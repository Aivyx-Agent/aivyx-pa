//! `RoutedProvider`: an `LlmProvider` that picks a model per call with
//! `aivyx-route`'s shared `Router` and dispatches to a per-endpoint
//! provider pool, rewriting `LlmRequest.model`. Untagged requests
//! (`route == None`) go to the configured provider unchanged.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use aivyx_route::{EndpointRef, ModelKey, ModelProfile, RouteQuery, RouteRecord, Router};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::{ContentBlock, LlmError, LlmMessage, LlmProvider, LlmRequest, LlmStream};

/// Builds the provider for one endpoint. One provider serves every model
/// on its endpoint; the model travels in `LlmRequest.model`.
pub type ProviderFactory =
    Box<dyn Fn(&EndpointRef) -> Result<Arc<dyn LlmProvider>, String> + Send + Sync>;

/// Called after every successful routed dispatch (the daemon audits
/// through it).
pub type RouteObserver = Arc<dyn Fn(&RouteRecord) + Send + Sync>;

/// Re-runs discovery + merge, producing a fresh candidate list.
#[async_trait]
pub trait ProfileRefresher: Send + Sync {
    async fn refresh(&self) -> Vec<ModelProfile>;
}

pub struct RoutedProvider {
    default_key: ModelKey,
    default_provider: Arc<dyn LlmProvider>,
    router: Router,
    factory: ProviderFactory,
    pool: Mutex<HashMap<EndpointRef, Arc<dyn LlmProvider>>>,
    refresher: Option<Arc<dyn ProfileRefresher>>,
    observer: Option<RouteObserver>,
}

impl RoutedProvider {
    pub fn new(
        default_key: ModelKey,
        default_provider: Arc<dyn LlmProvider>,
        router: Router,
        factory: ProviderFactory,
    ) -> Self {
        RoutedProvider {
            default_key,
            default_provider,
            router,
            factory,
            pool: Mutex::new(HashMap::new()),
            refresher: None,
            observer: None,
        }
    }

    pub fn with_refresher(mut self, refresher: Arc<dyn ProfileRefresher>) -> Self {
        self.refresher = Some(refresher);
        self
    }

    pub fn with_observer(mut self, observer: RouteObserver) -> Self {
        self.observer = Some(observer);
        self
    }

    pub fn router(&self) -> &Router {
        &self.router
    }

    pub fn default_key(&self) -> &ModelKey {
        &self.default_key
    }

    /// Re-runs discovery + merge. Returns the new candidate count.
    pub async fn refresh(&self) -> Result<usize, String> {
        let refresher = self
            .refresher
            .as_ref()
            .ok_or_else(|| "routing has no discovery configured".to_string())?;
        let profiles = refresher.refresh().await;
        let count = profiles.len();
        self.router.set_profiles(profiles);
        Ok(count)
    }

    /// The default endpoint is served by the configured provider; every
    /// other endpoint's provider is built once, on first use.
    fn provider_for(&self, endpoint: &EndpointRef) -> Result<Arc<dyn LlmProvider>, String> {
        if *endpoint == self.default_key.endpoint {
            return Ok(Arc::clone(&self.default_provider));
        }
        let mut pool = self.pool.lock().unwrap();
        if let Some(provider) = pool.get(endpoint) {
            return Ok(Arc::clone(provider));
        }
        let provider = (self.factory)(endpoint)?;
        pool.insert(endpoint.clone(), Arc::clone(&provider));
        Ok(provider)
    }
}

fn has_image(messages: &[LlmMessage]) -> bool {
    messages.iter().any(|m| match m {
        LlmMessage::User { content } => content
            .iter()
            .any(|b| matches!(b, ContentBlock::ImageBase64 { .. })),
        _ => false,
    })
}

/// Connection trouble, a missing/unloadable model, or a server error: try
/// the next candidate. Anything else would fail the same way everywhere.
fn is_retryable(err: &LlmError) -> bool {
    match err {
        LlmError::Transport(_) | LlmError::UnknownModel(_) => true,
        LlmError::Api { status, .. } => matches!(status, 404 | 408 | 500..=599),
        _ => false,
    }
}

#[async_trait]
impl LlmProvider for RoutedProvider {
    async fn chat_stream(
        &self,
        request: LlmRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<Box<dyn LlmStream>, LlmError> {
        let Some(hint) = request.route.clone() else {
            return self
                .default_provider
                .chat_stream(request, cancellation)
                .await;
        };
        let query = RouteQuery {
            task: hint.task.clone(),
            session: hint.session.clone(),
            tools: !request.tools.is_empty(),
            vision: has_image(request.messages),
            estimated_prompt_tokens: hint.estimated_prompt_tokens,
        };
        let plan = self.router.plan(&query, Instant::now()).map_err(|e| {
            LlmError::Routing(format!(
                "{e} — add a capable model to [[routing.models]] (see `aivyx-pa routing status`)"
            ))
        })?;
        let mut failures: Vec<String> = Vec::new();
        for key in &plan.chain {
            let provider = match self.provider_for(&key.endpoint) {
                Ok(provider) => provider,
                Err(why) => {
                    self.router.failed(key, Instant::now());
                    failures.push(format!("`{key}` ({why})"));
                    continue;
                }
            };
            let mut attempt = request.clone();
            attempt.model = &key.id;
            if *key != self.default_key {
                // Slot ids/hints describe the default server's KV cache.
                attempt.id_slot = None;
                attempt.slot_hint = None;
            }
            match provider.chat_stream(attempt, cancellation).await {
                Ok(stream) => {
                    let record = self.router.succeeded(&plan, key, &failures);
                    if let Some(observer) = &self.observer {
                        observer(&record);
                    }
                    return Ok(stream);
                }
                Err(err) if is_retryable(&err) => {
                    self.router.failed(key, Instant::now());
                    failures.push(format!("`{key}` ({err})"));
                }
                Err(err) => return Err(err),
            }
        }
        Err(LlmError::Routing(format!(
            "every candidate failed: {}",
            failures.join(", ")
        )))
    }

    async fn tool_call_family_hint(&self, model: &str) -> Option<String> {
        let endpoint = self
            .router
            .profiles()
            .into_iter()
            .filter(|p| p.id == model)
            .map(|p| p.endpoint)
            .min_by_key(|e| *e != self.default_key.endpoint)
            .unwrap_or_else(|| self.default_key.endpoint.clone());
        match self.provider_for(&endpoint) {
            Ok(provider) => provider.tool_call_family_hint(model).await,
            Err(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use aivyx_route::{
        Capability, EndpointRef, ModelKey, ModelProfile, RouteRecord, Router, TaskKind,
        TaskOverrides, Tier,
    };
    use async_trait::async_trait;
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::{
        ContentBlock, LlmError, LlmMessage, LlmProvider, LlmRequest, LlmStepEnd, LlmStream,
        LlmStreamEvent, LlmToolDescriptor, LlmUsage, RouteHint, SlotHint,
    };

    /// What one `chat_stream` call carried.
    #[derive(Debug, Clone, PartialEq)]
    struct Seen {
        model: String,
        id_slot: Option<u32>,
        slot_hint: Option<SlotHint>,
    }

    /// Records every request; answers with an empty successful stream, or
    /// with `LlmError::Api { status, .. }` when `fail` is set.
    struct Scripted {
        fail: Option<u16>,
        seen: Mutex<Vec<Seen>>,
    }

    impl Scripted {
        fn ok() -> Arc<Self> {
            Arc::new(Scripted {
                fail: None,
                seen: Mutex::new(Vec::new()),
            })
        }

        fn failing(status: u16) -> Arc<Self> {
            Arc::new(Scripted {
                fail: Some(status),
                seen: Mutex::new(Vec::new()),
            })
        }

        fn seen(&self) -> Vec<Seen> {
            self.seen.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl LlmProvider for Scripted {
        async fn chat_stream(
            &self,
            request: LlmRequest<'_>,
            _cancellation: &CancellationToken,
        ) -> Result<Box<dyn LlmStream>, LlmError> {
            self.seen.lock().unwrap().push(Seen {
                model: request.model.to_string(),
                id_slot: request.id_slot,
                slot_hint: request.slot_hint.clone(),
            });
            match self.fail {
                Some(status) => Err(LlmError::Api {
                    status,
                    message: "down".into(),
                }),
                None => Ok(Box::new(EmptyStream)),
            }
        }
    }

    struct EmptyStream;

    #[async_trait]
    impl LlmStream for EmptyStream {
        async fn next_event(&mut self) -> Result<Option<LlmStreamEvent>, LlmError> {
            Ok(None)
        }
        async fn finish(self: Box<Self>) -> Result<LlmStepEnd, LlmError> {
            Ok(LlmStepEnd::FinalMessage {
                text: String::new(),
                usage: LlmUsage::default(),
            })
        }
    }

    fn key(endpoint: &str, id: &str) -> ModelKey {
        ModelKey {
            endpoint: EndpointRef::new(endpoint),
            id: id.into(),
        }
    }

    fn profile(endpoint: &str, id: &str, tier: Tier, caps: &[Capability]) -> ModelProfile {
        let mut p = ModelProfile::new(id, EndpointRef::new(endpoint));
        p.tier = tier;
        p.capabilities.insert(Capability::Completion);
        p.capabilities.extend(caps.iter().copied());
        p
    }

    /// A `RoutedProvider` over `default@default` plus a pool of scripted
    /// endpoint providers, with a counter of factory builds.
    struct Fixture {
        routed: RoutedProvider,
        default: Arc<Scripted>,
        builds: Arc<AtomicUsize>,
    }

    fn fixture(profiles: Vec<ModelProfile>, pool: Vec<(&str, Arc<Scripted>)>) -> Fixture {
        let default = Scripted::ok();
        let builds = Arc::new(AtomicUsize::new(0));
        let pool: HashMap<EndpointRef, Arc<Scripted>> = pool
            .into_iter()
            .map(|(name, provider)| (EndpointRef::new(name), provider))
            .collect();
        let counter = Arc::clone(&builds);
        let factory: ProviderFactory = Box::new(move |endpoint: &EndpointRef| {
            counter.fetch_add(1, Ordering::SeqCst);
            pool.get(endpoint)
                .map(|p| Arc::clone(p) as Arc<dyn LlmProvider>)
                .ok_or_else(|| format!("no provider for `{endpoint}`"))
        });
        let routed = RoutedProvider::new(
            key("default", "default"),
            Arc::clone(&default) as Arc<dyn LlmProvider>,
            Router::new(profiles, TaskOverrides::default()),
            factory,
        );
        Fixture {
            routed,
            default,
            builds,
        }
    }

    fn default_profile() -> ModelProfile {
        profile("default", "default", Tier::Medium, &[])
    }

    fn routed(task: TaskKind, session: Option<&str>) -> (Vec<LlmMessage>, RouteHint) {
        (
            vec![LlmMessage::user_text("hi")],
            RouteHint {
                task,
                session: session.map(str::to_string),
                estimated_prompt_tokens: 0,
            },
        )
    }

    fn request<'a>(
        messages: &'a [LlmMessage],
        tools: &'a [LlmToolDescriptor],
        route: Option<RouteHint>,
    ) -> LlmRequest<'a> {
        LlmRequest {
            model: "whatever",
            system: None,
            messages,
            tools,
            max_tokens: 64,
            temperature: None,
            id_slot: None,
            slot_hint: None,
            route,
        }
    }

    async fn call(routed: &RoutedProvider, req: LlmRequest<'_>) -> Result<(), LlmError> {
        let stream = routed.chat_stream(req, &CancellationToken::new()).await?;
        stream.finish().await.map(|_| ())
    }

    fn tool() -> LlmToolDescriptor {
        LlmToolDescriptor {
            name: "echo".into(),
            description: "echo".into(),
            input_schema: json!({ "type": "object" }),
        }
    }

    #[tokio::test]
    async fn untagged_requests_go_to_the_default_provider_unchanged() {
        let gpu = Scripted::ok();
        let f = fixture(
            vec![default_profile(), profile("gpu", "big", Tier::Large, &[])],
            vec![("gpu", Arc::clone(&gpu))],
        );
        let messages = vec![LlmMessage::user_text("hi")];
        let mut req = request(&messages, &[], None);
        req.id_slot = Some(3);
        call(&f.routed, req).await.unwrap();

        assert_eq!(
            f.default.seen(),
            vec![Seen {
                model: "whatever".into(),
                id_slot: Some(3),
                slot_hint: None,
            }]
        );
        assert!(gpu.seen().is_empty());
        assert_eq!(f.builds.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn the_chosen_model_is_written_into_the_request() {
        let gpu = Scripted::ok();
        let f = fixture(
            vec![default_profile(), profile("gpu", "big", Tier::Large, &[])],
            vec![("gpu", Arc::clone(&gpu))],
        );
        let (messages, hint) = routed(TaskKind::CodeEdit, None);
        call(&f.routed, request(&messages, &[], Some(hint)))
            .await
            .unwrap();

        let seen = gpu.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].model, "big");
        assert!(f.default.seen().is_empty());
    }

    #[tokio::test]
    async fn tools_and_images_are_hard_needs() {
        let gpu = Scripted::ok();
        let f = fixture(
            vec![
                default_profile(),
                profile("gpu", "caller", Tier::Medium, &[Capability::Tools]),
                profile("gpu", "seer", Tier::Medium, &[Capability::Vision]),
            ],
            vec![("gpu", Arc::clone(&gpu))],
        );

        let (messages, hint) = routed(TaskKind::Chat, None);
        let tools = vec![tool()];
        call(&f.routed, request(&messages, &tools, Some(hint)))
            .await
            .unwrap();

        let (_, hint) = routed(TaskKind::Chat, None);
        let images = vec![LlmMessage::User {
            content: vec![
                ContentBlock::text("what is this?"),
                ContentBlock::ImageBase64 {
                    media_type: "image/png".into(),
                    data: "aGk=".into(),
                },
            ],
        }];
        call(&f.routed, request(&images, &[], Some(hint)))
            .await
            .unwrap();

        let models: Vec<String> = gpu.seen().into_iter().map(|s| s.model).collect();
        assert_eq!(models, vec!["caller".to_string(), "seer".to_string()]);
        assert!(f.default.seen().is_empty());
    }

    #[tokio::test]
    async fn only_the_default_model_keeps_slot_pinning() {
        let gpu = Scripted::ok();
        let f = fixture(
            vec![default_profile(), profile("gpu", "big", Tier::Large, &[])],
            vec![("gpu", Arc::clone(&gpu))],
        );
        let hint_slot = SlotHint {
            prefix_hash: "abc".into(),
            preferred_slot: None,
        };

        let (messages, hint) = routed(TaskKind::CodeEdit, None);
        let mut req = request(&messages, &[], Some(hint));
        req.id_slot = Some(2);
        req.slot_hint = Some(hint_slot.clone());
        call(&f.routed, req).await.unwrap();

        let (_, hint) = routed(TaskKind::Chat, None);
        let mut req = request(&messages, &[], Some(hint));
        req.id_slot = Some(2);
        req.slot_hint = Some(hint_slot.clone());
        call(&f.routed, req).await.unwrap();

        assert_eq!(
            gpu.seen(),
            vec![Seen {
                model: "big".into(),
                id_slot: None,
                slot_hint: None,
            }]
        );
        assert_eq!(
            f.default.seen(),
            vec![Seen {
                model: "default".into(),
                id_slot: Some(2),
                slot_hint: Some(hint_slot),
            }]
        );
    }

    #[tokio::test]
    async fn a_retryable_failure_falls_back_cools_down_and_is_observed() {
        let gpu = Scripted::failing(503);
        let records: Arc<Mutex<Vec<RouteRecord>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&records);
        let f = fixture(
            vec![default_profile(), profile("gpu", "big", Tier::Large, &[])],
            vec![("gpu", Arc::clone(&gpu))],
        );
        let routed_provider = f.routed.with_observer(Arc::new(move |r: &RouteRecord| {
            sink.lock().unwrap().push(r.clone())
        }));

        let (messages, hint) = routed(TaskKind::CodeEdit, None);
        call(&routed_provider, request(&messages, &[], Some(hint)))
            .await
            .unwrap();

        {
            let records = records.lock().unwrap();
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].model, key("default", "default"));
            assert!(
                records[0].reason.contains("fell back after"),
                "reason: {}",
                records[0].reason
            );
        }
        assert_eq!(gpu.seen().len(), 1);
        assert_eq!(f.default.seen().len(), 1);

        // `big` is cooling down: the next call goes straight to the default.
        let (_, hint) = routed(TaskKind::CodeEdit, None);
        call(&routed_provider, request(&messages, &[], Some(hint)))
            .await
            .unwrap();
        assert_eq!(gpu.seen().len(), 1);
        assert_eq!(f.default.seen().len(), 2);
        assert_eq!(records.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn non_retryable_errors_return_without_fallback() {
        let gpu = Scripted::failing(400);
        let f = fixture(
            vec![default_profile(), profile("gpu", "big", Tier::Large, &[])],
            vec![("gpu", Arc::clone(&gpu))],
        );
        let (messages, hint) = routed(TaskKind::CodeEdit, None);
        let err = call(&f.routed, request(&messages, &[], Some(hint)))
            .await
            .unwrap_err();

        assert!(
            matches!(err, LlmError::Api { status: 400, .. }),
            "got {err:?}"
        );
        assert_eq!(gpu.seen().len(), 1);
        assert!(f.default.seen().is_empty());
    }

    #[tokio::test]
    async fn no_route_is_an_actionable_routing_error() {
        let gpu = Scripted::ok();
        let f = fixture(
            vec![default_profile(), profile("gpu", "big", Tier::Large, &[])],
            vec![("gpu", Arc::clone(&gpu))],
        );
        let (messages, hint) = routed(TaskKind::Chat, None);
        let tools = vec![tool()];
        let err = call(&f.routed, request(&messages, &tools, Some(hint)))
            .await
            .unwrap_err();

        match err {
            LlmError::Routing(msg) => {
                assert!(msg.contains("tool calling"), "msg: {msg}");
                assert!(msg.contains("aivyx-pa routing status"), "msg: {msg}");
            }
            other => panic!("expected a routing error, got {other:?}"),
        }
        assert!(gpu.seen().is_empty());
        assert!(f.default.seen().is_empty());
    }

    #[tokio::test]
    async fn each_endpoint_provider_is_built_once() {
        let gpu = Scripted::ok();
        let f = fixture(
            vec![
                default_profile(),
                profile("gpu", "big", Tier::Large, &[]),
                profile("gpu", "tiny", Tier::Small, &[]),
            ],
            vec![("gpu", Arc::clone(&gpu))],
        );
        let (messages, hint) = routed(TaskKind::CodeEdit, None);
        call(&f.routed, request(&messages, &[], Some(hint)))
            .await
            .unwrap();
        let (_, hint) = routed(TaskKind::Summarize, None);
        call(&f.routed, request(&messages, &[], Some(hint)))
            .await
            .unwrap();

        let models: Vec<String> = gpu.seen().into_iter().map(|s| s.model).collect();
        assert_eq!(models, vec!["big".to_string(), "tiny".to_string()]);
        assert_eq!(f.builds.load(Ordering::SeqCst), 1);
    }

    struct FixedRefresher(Vec<ModelProfile>);

    #[async_trait]
    impl ProfileRefresher for FixedRefresher {
        async fn refresh(&self) -> Vec<ModelProfile> {
            self.0.clone()
        }
    }

    #[tokio::test]
    async fn refresh_replaces_candidates() {
        let f = fixture(vec![default_profile()], Vec::new());
        assert!(f.routed.refresh().await.is_err());

        let fresh = vec![
            default_profile(),
            profile("gpu", "big", Tier::Large, &[]),
            profile("gpu", "tiny", Tier::Small, &[]),
        ];
        let routed_provider = f
            .routed
            .with_refresher(Arc::new(FixedRefresher(fresh.clone())));
        assert_eq!(routed_provider.refresh().await, Ok(3));
        assert_eq!(routed_provider.router().profiles(), fresh);
        assert_eq!(routed_provider.default_key(), &key("default", "default"));
    }
}
