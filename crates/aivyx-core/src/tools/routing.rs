//! Model routing Part 3a — `routing.status` / `routing.explain` tools.
//! Read-only views of the daemon's `RoutedProvider`: the router's
//! candidate list, and the last routing decision for a session.
//! Infrastructure-tier (see `crates/aivyx-capability/src/lib.rs`'s
//! `KNOWN_BASES` doc comment). Registered only when `[routing] enabled`.

use std::sync::Arc;

use aivyx_capability::Scope;
use aivyx_llm::RoutedProvider;
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{Tool, ToolContext, ToolId, ToolOutcome, Verification};

// ---------------------------------------------------------------------------
// routing.status
// ---------------------------------------------------------------------------

/// `routing.status` — the router's default model and every candidate.
pub struct RoutingStatusTool {
    id: ToolId,
    schema: Value,
    routed: Arc<RoutedProvider>,
}

impl std::fmt::Debug for RoutingStatusTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoutingStatusTool")
            .field("id", &self.id)
            .finish()
    }
}

impl RoutingStatusTool {
    pub fn new(routed: Arc<RoutedProvider>) -> Self {
        RoutingStatusTool {
            id: ToolId::new(),
            schema: status_input_schema(),
            routed,
        }
    }
}

#[async_trait]
impl Tool for RoutingStatusTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "routing.status"
    }

    fn description(&self) -> &str {
        "Show the model router's candidates. Input is a JSON object (no \
         fields required). Returns a JSON object with `default` (the \
         configured model, as `id@endpoint`) and a `candidates` array — \
         each entry has `model` (`id@endpoint`), `tier`, `capabilities` \
         (known), `unknown_capabilities` (assumed but unconfirmed), \
         `context_window` (tokens, or null when unknown) and `availability` \
         (`available`, `unverified` or `unavailable`) — plus `residency` \
         (`vram`, `resident_endpoints`, and each candidate's load note: \
         `loaded`, `needs_load`, `wont_fit` or `null` when unknown)."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("routing.status").expect("routing.status must parse — it is in KNOWN_BASES")
    }

    // Read-only over the daemon's own router state, no model-supplied
    // path (the same risk profile as skill_defaults.list, already
    // floored) — included in the zero-config default role's capability
    // floor. See compute_backcompat_floor's own doc comment in aivyx.rs.
    fn auto_grantable_in_backcompat_floor(&self) -> bool {
        true
    }

    // Chapter Bulwark — model ids come from endpoint discovery (a local
    // server's `/api/tags` or `/v1/models`), i.e. external content.
    // Picket/Bulwark cover every call's output automatically.
    fn output_is_untrusted(&self) -> bool {
        true
    }

    async fn execute(&self, _input: Value, ctx: &ToolContext<'_>) -> ToolOutcome {
        let session = ctx.session_id.to_string();
        let status = routing_status(&self.routed, Some(&session)).await;
        ToolOutcome::Completed {
            output: status_tool_json(&status),
            verified: Verification::Verified,
        }
    }
}

// ---------------------------------------------------------------------------
// The shared status builder (Routing visibility B1)
// ---------------------------------------------------------------------------

/// The model router's state: what `routing.status` reports and what the
/// daemon's `GetRoutingStatus` query answers. ONE builder
/// ([`routing_status`]) feeds both, so the two can't drift.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutingStatus {
    /// The configured model, `id@endpoint`.
    pub default_model: String,
    pub candidates: Vec<RoutingCandidate>,
    /// Total / used / available-for-a-new-load VRAM, once a residency
    /// source has answered.
    pub vram: Option<RoutingVram>,
    /// Endpoints residency counts as fully loaded.
    pub resident_endpoints: Vec<String>,
    /// `None` when no cloud escalation is configured.
    pub escalation: Option<RoutingEscalation>,
    pub classifier_enabled: bool,
    /// The asked-about conversation, when one was given.
    pub conversation: Option<RoutingConversation>,
}

/// One routing candidate. Every field is already in its wire spelling.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutingCandidate {
    /// `id@endpoint`.
    pub model: String,
    pub tier: String,
    pub capabilities: Vec<String>,
    pub unknown_capabilities: Vec<String>,
    pub context_window: Option<u32>,
    /// `available`, `unverified` or `unavailable`.
    pub availability: String,
    /// `loaded`, `needs_load` or `wont_fit`; `None` when residency has no
    /// opinion.
    pub residency: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutingVram {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
}

/// The cloud escalation settings.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutingEscalation {
    /// `ask`, `auto` or `never`.
    pub mode: String,
    pub no_local_candidate: bool,
    pub tiers: Vec<String>,
    pub on_failure: bool,
    pub cloud_candidates: Vec<RoutingCandidate>,
}

/// One conversation's routing state.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutingConversation {
    pub session: String,
    /// Its pin, else the model routing made it stick to (`id@endpoint`).
    pub current_model: Option<String>,
    pub pinned: Option<String>,
    /// The last routed call: `(id@endpoint, task, reason)`.
    pub last_decision: Option<(String, String, String)>,
    /// The taint reason (only tracked while escalation is configured).
    pub tainted: Option<String>,
    pub cloud_allowed: bool,
    /// A16 — the *next* turn will escalate (the ARMED set).
    pub armed: bool,
}

/// Builds [`RoutingStatus`] from the daemon's `RoutedProvider`; `session`
/// adds that conversation's block.
pub async fn routing_status(routed: &RoutedProvider, session: Option<&str>) -> RoutingStatus {
    let router = routed.router();
    let profiles = router.profiles();
    // Model routing Part 4 — the router's residency snapshot, refreshed
    // in the background every 5s; empty until a source responds.
    let snapshot = router.residency();
    let candidates = profiles.iter().map(|p| candidate(p, Some(&snapshot))).collect();
    let vram = snapshot.vram.map(|v| RoutingVram {
        total_bytes: v.total_bytes,
        used_bytes: v.used_bytes,
        available_bytes: snapshot.available_vram().unwrap_or(0),
    });
    let escalation = routed
        .escalation_settings()
        .map(|(mode, no_local_candidate, tiers)| RoutingEscalation {
            mode: mode.name().to_string(),
            no_local_candidate,
            tiers: tiers.iter().map(|t| t.name().to_string()).collect(),
            on_failure: routed.escalation_on_failure().unwrap_or(false),
            cloud_candidates: routed
                .escalation_candidates()
                .iter()
                .map(|p| candidate(p, None))
                .collect(),
        });
    let conversation = match session {
        None => None,
        Some(session) => {
            // Model routing Part 3b — taint / consent are tracked only while
            // escalation is configured.
            let (tainted, cloud_allowed) = if escalation.is_some() {
                routed
                    .escalation_state(session)
                    .await
                    .unwrap_or((None, false))
            } else {
                (None, false)
            };
            Some(RoutingConversation {
                session: session.to_string(),
                current_model: router.current(session).map(|k| k.to_string()),
                pinned: router.pinned(session).map(|k| k.to_string()),
                last_decision: router.last_decision(session).map(|r| {
                    (r.model.to_string(), r.task.name().to_string(), r.reason)
                }),
                tainted,
                cloud_allowed,
                // A16 — "armed" here means the *next* turn will escalate
                // (the ARMED set), not that a call is escalating mid-turn
                // right now (the ACTIVE set `EscalationGuard::armed` reads).
                armed: escalation.is_some() && routed.escalation_armed_next(session),
            })
        }
    };
    RoutingStatus {
        default_model: routed.default_key().to_string(),
        candidates,
        vram,
        resident_endpoints: snapshot
            .resident_endpoints
            .iter()
            .map(ToString::to_string)
            .collect(),
        escalation,
        classifier_enabled: routed.classifier_enabled(),
        conversation,
    }
}

/// One candidate; `residency` is `None` for the escalation router's cloud
/// candidates (residency is a local-hardware signal).
fn candidate(
    p: &aivyx_route::ModelProfile,
    residency: Option<&aivyx_route::ResidencySnapshot>,
) -> RoutingCandidate {
    RoutingCandidate {
        model: p.key().to_string(),
        tier: p.tier.to_string(),
        capabilities: p.capabilities.iter().map(ToString::to_string).collect(),
        unknown_capabilities: p
            .unknown_capabilities
            .iter()
            .map(ToString::to_string)
            .collect(),
        context_window: p.context_window,
        availability: wire_name(&p.availability),
        residency: residency
            .and_then(|r| r.cost(p).1)
            .map(|note| wire_name(&note)),
    }
}

/// A unit enum's serde (snake_case) spelling.
fn wire_name<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(s)) => s,
        _ => String::new(),
    }
}

/// `routing.status`'s JSON output (its shape is the tool's contract).
fn status_tool_json(status: &RoutingStatus) -> Value {
    let escalation = match &status.escalation {
        None => Value::Null,
        Some(esc) => {
            let this = status.conversation.as_ref();
            json!({
                "mode": esc.mode,
                "no_local_candidate": esc.no_local_candidate,
                "tiers": esc.tiers,
                "on_failure": esc.on_failure,
                "cloud_candidates": esc
                    .cloud_candidates
                    .iter()
                    .map(candidate_json)
                    .collect::<Vec<_>>(),
                "this_conversation": {
                    "tainted": this.and_then(|c| c.tainted.clone()),
                    "cloud_allowed": this.is_some_and(|c| c.cloud_allowed),
                    "armed": this.is_some_and(|c| c.armed),
                },
            })
        }
    };
    let vram = status.vram.map(|v| {
        json!({
            "total_bytes": v.total_bytes,
            "used_bytes": v.used_bytes,
            "available_bytes": v.available_bytes,
        })
    });
    let mut residency = serde_json::Map::new();
    for c in &status.candidates {
        residency.insert(c.model.clone(), json!(c.residency));
    }
    json!({
        "default": status.default_model,
        "candidates": status.candidates.iter().map(candidate_json).collect::<Vec<_>>(),
        "escalation": escalation,
        "classifier": { "enabled": status.classifier_enabled },
        "residency": {
            "vram": vram,
            "resident_endpoints": status.resident_endpoints,
            "candidates": residency,
        },
    })
}

/// One routing candidate as `routing.status` reports it.
fn candidate_json(c: &RoutingCandidate) -> Value {
    json!({
        "model": c.model,
        "tier": c.tier,
        "capabilities": c.capabilities,
        "unknown_capabilities": c.unknown_capabilities,
        "context_window": c.context_window,
        "availability": c.availability,
    })
}

/// Resolves a model argument against the router's candidates, as
/// `aivyx-coder`'s `/model` does: `id@endpoint`, or a bare id served by
/// exactly one endpoint. The error texts are the operator-facing replies.
pub fn resolve_model(
    profiles: &[aivyx_route::ModelProfile],
    arg: &str,
) -> Result<aivyx_route::ModelKey, String> {
    use aivyx_route::{ModelKey, ModelProfile};
    if let Some((id, endpoint)) = arg.rsplit_once('@') {
        let k = ModelKey {
            endpoint: aivyx_route::EndpointRef::new(endpoint),
            id: id.to_string(),
        };
        return aivyx_route::find(profiles, &k)
            .map(ModelProfile::key)
            .ok_or_else(|| format!("No model `{arg}` — see /models."));
    }
    let matches: Vec<ModelKey> = profiles
        .iter()
        .filter(|p| p.id == arg)
        .map(ModelProfile::key)
        .collect();
    match matches.as_slice() {
        [] => Err(format!("No model `{arg}` — see /models.")),
        [one] => Ok(one.clone()),
        many => {
            let names: Vec<String> = many.iter().map(ToString::to_string).collect();
            Err(format!(
                "`{arg}` is served by several endpoints ({}) — use id@endpoint.",
                names.join(", ")
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// routing.explain
// ---------------------------------------------------------------------------

/// `routing.explain` — the router's last decision for a session (the
/// calling turn's own session when `session` is omitted).
pub struct RoutingExplainTool {
    id: ToolId,
    schema: Value,
    routed: Arc<RoutedProvider>,
}

impl std::fmt::Debug for RoutingExplainTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoutingExplainTool")
            .field("id", &self.id)
            .finish()
    }
}

impl RoutingExplainTool {
    pub fn new(routed: Arc<RoutedProvider>) -> Self {
        RoutingExplainTool {
            id: ToolId::new(),
            schema: explain_input_schema(),
            routed,
        }
    }
}

#[async_trait]
impl Tool for RoutingExplainTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "routing.explain"
    }

    fn description(&self) -> &str {
        "Explain which model the router last chose for a conversation, and \
         why. Input is a JSON object with an optional `session` field (a \
         session id; defaults to the current conversation). Returns a JSON \
         object with `session`, `model` (`id@endpoint`), `task` (the kind \
         of call that was routed) and `reason`, or `decision: null` when \
         nothing has been routed for that session yet."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("routing.read").expect("routing.read must parse — it is in KNOWN_BASES")
    }

    // Read-only, same rationale as RoutingStatusTool.
    fn auto_grantable_in_backcompat_floor(&self) -> bool {
        true
    }

    // Chapter Bulwark — the model id and the reason (which names models)
    // come from endpoint discovery, i.e. external content.
    fn output_is_untrusted(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, ctx: &ToolContext<'_>) -> ToolOutcome {
        let session = match input.get("session") {
            None | Some(Value::Null) => ctx.session_id.to_string(),
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            Some(_) => {
                return ToolOutcome::Failed(crate::AivyxError::Tool {
                    tool: self.id,
                    detail: "routing.explain: `session` must be a non-empty string".to_string(),
                });
            }
        };
        let output = match self.routed.router().last_decision(&session) {
            Some(record) => json!({
                "session": session,
                "model": record.model.to_string(),
                "task": record.task.name(),
                "reason": record.reason,
            }),
            None => json!({ "session": session, "decision": null }),
        };
        ToolOutcome::Completed {
            output,
            verified: Verification::Verified,
        }
    }
}

// ---------------------------------------------------------------------------
// Schemas
// ---------------------------------------------------------------------------

fn status_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {},
        "required": []
    })
}

fn explain_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "session": {
                "type": "string",
                "description": "Session id to explain; omit for the current conversation."
            }
        },
        "required": []
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod routing_tool_tests {
    use super::*;
    use crate::{AgentId, CancellationToken, MessageOrigin, NullAuditHook, SessionId, TurnId};
    use aivyx_llm::{
        LlmError, LlmMessage, LlmProvider, LlmRequest, LlmStepEnd, LlmStream, LlmStreamEvent,
        LlmUsage, ProviderFactory, RouteHint,
    };
    use aivyx_route::{
        Availability, Capability, EndpointRef, ModelKey, ModelProfile, Router, TaskKind,
        TaskOverrides, Tier,
    };

    struct NoopChannel {
        session: SessionId,
        token: CancellationToken,
    }

    #[async_trait]
    impl crate::ChannelContext for NoopChannel {
        fn channel_name(&self) -> &str {
            "routing-test"
        }
        fn platform(&self) -> crate::ChannelPlatform {
            crate::ChannelPlatform::Local
        }
        fn trust_tier(&self) -> aivyx_capability::TrustTier {
            aivyx_capability::TrustTier::Trusted
        }
        fn session_id(&self) -> SessionId {
            self.session
        }
        async fn stream_event(
            &self,
            _event: crate::StreamEvent<'_>,
        ) -> Result<(), crate::ChannelError> {
            Ok(())
        }
        async fn finalize(&self, _outcome: &crate::TurnOutcome) -> Result<(), crate::ChannelError> {
            Ok(())
        }
        fn cancellation_token(&self) -> CancellationToken {
            self.token.clone()
        }
    }

    async fn run_execute(tool: &dyn Tool, session: SessionId, input: Value) -> ToolOutcome {
        let channel = NoopChannel {
            session,
            token: CancellationToken::new(),
        };
        let audit = NullAuditHook;
        let ctx = ToolContext {
            agent_id: AgentId::new(),
            session_id: channel.session,
            turn_id: TurnId::new(),
            channel: &channel,
            audit: &audit,
            cancellation: &channel.token,
            message_origin: MessageOrigin::Operator,
        };
        tool.execute(input, &ctx).await
    }

    fn completed(outcome: ToolOutcome) -> Value {
        match outcome {
            ToolOutcome::Completed { output, .. } => output,
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    /// Answers every call with an empty successful stream.
    struct Ok_;

    #[async_trait]
    impl LlmProvider for Ok_ {
        async fn chat_stream(
            &self,
            _request: LlmRequest<'_>,
            _cancellation: &CancellationToken,
        ) -> Result<Box<dyn LlmStream>, LlmError> {
            Ok(Box::new(EmptyStream))
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

    /// `small@default` (the configured model) plus `big@gpu`, a large
    /// tool-calling model with a known context window.
    fn routed() -> Arc<RoutedProvider> {
        let mut small = ModelProfile::new("small", EndpointRef::new("default"));
        small.tier = Tier::Small;
        small.capabilities.insert(Capability::Completion);
        small.unknown_capabilities.insert(Capability::Tools);
        let mut big = ModelProfile::new("big", EndpointRef::new("gpu"));
        big.tier = Tier::Large;
        big.capabilities
            .extend([Capability::Completion, Capability::Tools]);
        big.context_window = Some(32_768);
        big.availability = Availability::Available;
        let factory: ProviderFactory =
            Box::new(|_: &EndpointRef| Ok(Arc::new(Ok_) as Arc<dyn LlmProvider>));
        Arc::new(RoutedProvider::new(
            ModelKey {
                endpoint: EndpointRef::new("default"),
                id: "small".into(),
            },
            Arc::new(Ok_),
            Router::new(vec![small, big], TaskOverrides::default()),
            factory,
        ))
    }

    /// One routed `Plan` call tagged with `session`.
    async fn route_once(routed: &RoutedProvider, session: &str) {
        let messages = [LlmMessage::user_text("hi")];
        let request = LlmRequest {
            model: "small",
            system: None,
            messages: &messages,
            tools: &[],
            max_tokens: 8,
            temperature: None,
            id_slot: None,
            slot_hint: None,
            route: Some(RouteHint {
                task: TaskKind::Plan,
                session: Some(session.to_string()),
                estimated_prompt_tokens: 0,
            }),
        };
        let stream = routed
            .chat_stream(request, &CancellationToken::new())
            .await
            .unwrap();
        stream.finish().await.unwrap();
    }

    #[tokio::test]
    async fn status_lists_the_default_and_every_candidate() {
        let tool = RoutingStatusTool::new(routed());
        let output = completed(run_execute(&tool, SessionId::new(), json!({})).await);
        assert_eq!(output["default"], "small@default");
        let candidates = output["candidates"].as_array().unwrap();
        assert_eq!(candidates.len(), 2);
        let big = candidates
            .iter()
            .find(|c| c["model"] == "big@gpu")
            .expect("big@gpu listed");
        assert_eq!(big["tier"], "large");
        assert_eq!(big["capabilities"], json!(["completion", "tools"]));
        assert_eq!(big["unknown_capabilities"], json!([]));
        assert_eq!(big["context_window"], 32_768);
        assert_eq!(big["availability"], "available");
        let small = candidates
            .iter()
            .find(|c| c["model"] == "small@default")
            .expect("small@default listed");
        assert_eq!(small["unknown_capabilities"], json!(["tools"]));
        assert_eq!(small["context_window"], Value::Null);
    }

    #[tokio::test]
    async fn status_reports_residency_signals() {
        use aivyx_route::{ModelResidency, ResidencySnapshot, Vram};

        let routed = routed();
        let tool = RoutingStatusTool::new(Arc::clone(&routed));

        // No poll has landed yet: every candidate's note is null, VRAM is
        // null — residency has no opinion, so ranking is unaffected.
        let output = completed(run_execute(&tool, SessionId::new(), json!({})).await);
        let residency = &output["residency"];
        assert_eq!(residency["vram"], Value::Null);
        assert_eq!(residency["candidates"]["small@default"], Value::Null);
        assert_eq!(residency["candidates"]["big@gpu"], Value::Null);
        assert_eq!(residency["resident_endpoints"], json!([]));

        // `big@gpu` is explicitly loaded; the `default` endpoint counts as
        // resident too (a single-model server), so `small@default` is
        // loaded via that endpoint signal. 24 GiB total VRAM, none used.
        let mut snapshot = ResidencySnapshot::default();
        snapshot.models.insert(
            ModelKey {
                endpoint: EndpointRef::new("gpu"),
                id: "big".into(),
            },
            ModelResidency::Loaded {
                vram_bytes: Some(4 << 30),
            },
        );
        snapshot
            .resident_endpoints
            .insert(EndpointRef::new("default"));
        snapshot.vram = Some(Vram {
            total_bytes: 24u64 << 30,
            used_bytes: 0,
        });
        routed.router().set_residency(snapshot);

        let output = completed(run_execute(&tool, SessionId::new(), json!({})).await);
        let residency = &output["residency"];
        assert_eq!(residency["candidates"]["big@gpu"], "loaded");
        assert_eq!(residency["candidates"]["small@default"], "loaded");
        assert_eq!(residency["resident_endpoints"], json!(["default"]));
        assert_eq!(residency["vram"]["total_bytes"], 24u64 << 30);
        assert_eq!(residency["vram"]["used_bytes"], 0);
        assert_eq!(residency["vram"]["available_bytes"], 24u64 << 30);
    }

    struct GuardFor {
        tainted: String,
        allowed: String,
        armed: String,
    }

    #[async_trait]
    impl aivyx_llm::EscalationGuard for GuardFor {
        async fn taint(&self, session: &str) -> Option<String> {
            (session == self.tainted).then(|| "gmail.search output".to_string())
        }
        fn consented(&self, session: &str) -> bool {
            session == self.allowed
        }
        fn armed_next(&self, session: &str) -> bool {
            session == self.armed
        }
    }

    #[tokio::test]
    async fn status_reports_no_escalation_when_none_is_configured() {
        let tool = RoutingStatusTool::new(routed());
        let output = completed(run_execute(&tool, SessionId::new(), json!({})).await);
        assert_eq!(output["escalation"], Value::Null);
    }

    #[tokio::test]
    async fn status_reports_the_classifiers_enabled_state() {
        let tool = RoutingStatusTool::new(routed());
        let output = completed(run_execute(&tool, SessionId::new(), json!({})).await);
        assert_eq!(output["classifier"]["enabled"], false);

        let base = Arc::try_unwrap(routed()).ok().expect("sole owner");
        let with_classifier = Arc::new(base.with_classifier(aivyx_llm::ClassifierSetup {
            timeout: std::time::Duration::from_millis(1500),
        }));
        let tool = RoutingStatusTool::new(with_classifier);
        let output = completed(run_execute(&tool, SessionId::new(), json!({})).await);
        assert_eq!(output["classifier"]["enabled"], true);
    }

    #[tokio::test]
    async fn status_reports_escalation_and_this_conversations_state() {
        let tainted = SessionId::new();
        let allowed = SessionId::new();
        let armed = SessionId::new();
        let mut claude = ModelProfile::new("claude", EndpointRef::new("cloud"));
        claude.tier = Tier::Large;
        claude.capabilities.insert(Capability::Completion);
        claude.locality = aivyx_route::Locality::Cloud;
        let base = Arc::try_unwrap(routed()).ok().expect("sole owner");
        let routed = Arc::new(base.with_escalation(aivyx_llm::EscalationSetup {
            router: Router::new(vec![claude], TaskOverrides::default()).with_allow_cloud(true),
            mode: aivyx_llm::EscalationMode::Ask,
            no_local_candidate: true,
            tiers: vec![TaskKind::Plan],
            on_failure: true,
            guard: Arc::new(GuardFor {
                tainted: tainted.to_string(),
                allowed: allowed.to_string(),
                armed: armed.to_string(),
            }),
            observer: Arc::new(|_: &aivyx_llm::EscalationRecord| {}),
        }));
        let tool = RoutingStatusTool::new(Arc::clone(&routed));

        let output = completed(run_execute(&tool, tainted, json!({})).await);
        let esc = &output["escalation"];
        assert_eq!(esc["mode"], "ask");
        assert_eq!(esc["no_local_candidate"], true);
        assert_eq!(esc["tiers"], json!(["plan"]));
        assert_eq!(esc["on_failure"], true);
        assert_eq!(esc["cloud_candidates"][0]["model"], "claude@cloud");
        assert_eq!(esc["this_conversation"]["tainted"], "gmail.search output");
        assert_eq!(esc["this_conversation"]["cloud_allowed"], false);
        assert_eq!(esc["this_conversation"]["armed"], false);

        let output = completed(run_execute(&tool, allowed, json!({})).await);
        let this = &output["escalation"]["this_conversation"];
        assert_eq!(this["tainted"], Value::Null);
        assert_eq!(this["cloud_allowed"], true);
        assert_eq!(this["armed"], false);

        let output = completed(run_execute(&tool, armed, json!({})).await);
        let this = &output["escalation"]["this_conversation"];
        assert_eq!(this["armed"], true, "the armed session's next turn will escalate");
    }

    #[tokio::test]
    async fn status_reports_on_failure_false_when_configured_off() {
        let session = SessionId::new();
        let mut claude = ModelProfile::new("claude", EndpointRef::new("cloud"));
        claude.locality = aivyx_route::Locality::Cloud;
        let base = Arc::try_unwrap(routed()).ok().expect("sole owner");
        let routed = Arc::new(base.with_escalation(aivyx_llm::EscalationSetup {
            router: Router::new(vec![claude], TaskOverrides::default()).with_allow_cloud(true),
            mode: aivyx_llm::EscalationMode::Auto,
            no_local_candidate: false,
            tiers: vec![],
            on_failure: false,
            guard: Arc::new(GuardFor {
                tainted: "nobody".to_string(),
                allowed: "nobody".to_string(),
                armed: "nobody".to_string(),
            }),
            observer: Arc::new(|_: &aivyx_llm::EscalationRecord| {}),
        }));
        let tool = RoutingStatusTool::new(routed);
        let output = completed(run_execute(&tool, session, json!({})).await);
        assert_eq!(output["escalation"]["on_failure"], false);
        assert_eq!(output["escalation"]["this_conversation"]["armed"], false);
    }

    #[tokio::test]
    async fn explain_is_null_before_any_routed_call() {
        let tool = RoutingExplainTool::new(routed());
        let session = SessionId::new();
        let output = completed(run_execute(&tool, session, json!({})).await);
        assert_eq!(output["decision"], Value::Null);
        assert!(output.get("decision").is_some());
        assert_eq!(output["session"], session.to_string());
    }

    #[tokio::test]
    async fn explain_defaults_to_the_calling_turns_session() {
        let routed = routed();
        let session = SessionId::new();
        route_once(&routed, &session.to_string()).await;
        let tool = RoutingExplainTool::new(Arc::clone(&routed));
        let output = completed(run_execute(&tool, session, json!({})).await);
        assert_eq!(output["model"], "big@gpu");
        assert_eq!(output["task"], "plan");
        assert!(!output["reason"].as_str().unwrap().is_empty());
        assert!(output.get("decision").is_none());

        // Another session has no decision of its own.
        let other = completed(run_execute(&tool, SessionId::new(), json!({})).await);
        assert_eq!(other["decision"], Value::Null);
    }

    #[tokio::test]
    async fn explain_honours_an_explicit_session() {
        let routed = routed();
        route_once(&routed, "sess-1").await;
        let tool = RoutingExplainTool::new(Arc::clone(&routed));
        let output =
            completed(run_execute(&tool, SessionId::new(), json!({ "session": "sess-1" })).await);
        assert_eq!(output["session"], "sess-1");
        assert_eq!(output["model"], "big@gpu");
    }

    #[tokio::test]
    async fn explain_rejects_a_non_string_session() {
        let tool = RoutingExplainTool::new(routed());
        match run_execute(&tool, SessionId::new(), json!({ "session": 7 })).await {
            ToolOutcome::Failed(crate::AivyxError::Tool { detail, .. }) => {
                assert!(detail.contains("session"), "{detail}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    // ---- Routing visibility B1: the shared builder + pin resolution ----

    #[tokio::test]
    async fn the_shared_builder_reports_this_conversations_model_pin_and_last_decision() {
        let routed = routed();
        route_once(&routed, "sess-1").await;
        let status = routing_status(&routed, Some("sess-1")).await;
        assert_eq!(status.default_model, "small@default");
        assert_eq!(status.candidates.len(), 2);
        assert_eq!(status.escalation, None);
        assert!(!status.classifier_enabled);
        let conv = status.conversation.expect("a session was asked about");
        assert_eq!(conv.session, "sess-1");
        // A `plan` call isn't sticky: routing ranks every call afresh.
        assert_eq!(conv.current_model, None);
        assert_eq!(conv.pinned, None);
        let (model, task, reason) = conv.last_decision.expect("one routed call");
        assert_eq!(model, "big@gpu");
        assert_eq!(task, "plan");
        assert!(!reason.is_empty());
        assert_eq!(conv.tainted, None);
        assert!(!conv.cloud_allowed);

        routed.router().pin(
            "sess-1",
            ModelKey {
                endpoint: EndpointRef::new("default"),
                id: "small".into(),
            },
        );
        let conv = routing_status(&routed, Some("sess-1")).await.conversation.unwrap();
        assert_eq!(conv.pinned.as_deref(), Some("small@default"));
        assert_eq!(conv.current_model.as_deref(), Some("small@default"));

        // No session asked about: no conversation block.
        assert_eq!(routing_status(&routed, None).await.conversation, None);
    }

    #[test]
    fn resolve_model_accepts_bare_ids_and_disambiguates() {
        let mut a = ModelProfile::new("qwen3:8b", EndpointRef::new("a-gpu"));
        a.tier = Tier::Small;
        let b = ModelProfile::new("qwen3:8b", EndpointRef::new("b-gpu"));
        let coder = ModelProfile::new("coder", EndpointRef::new("a-gpu"));
        let ps = vec![a, b, coder];
        let key = |e: &str, id: &str| ModelKey {
            endpoint: EndpointRef::new(e),
            id: id.into(),
        };
        assert_eq!(resolve_model(&ps, "coder"), Ok(key("a-gpu", "coder")));
        assert_eq!(resolve_model(&ps, "qwen3:8b@b-gpu"), Ok(key("b-gpu", "qwen3:8b")));
        assert_eq!(
            resolve_model(&ps, "qwen3:8b"),
            Err("`qwen3:8b` is served by several endpoints (qwen3:8b@a-gpu, qwen3:8b@b-gpu) \
                 — use id@endpoint."
                .to_string())
        );
        assert_eq!(
            resolve_model(&ps, "nope"),
            Err("No model `nope` — see /models.".to_string())
        );
        assert_eq!(
            resolve_model(&ps, "coder@b-gpu"),
            Err("No model `coder@b-gpu` — see /models.".to_string())
        );
    }

    #[test]
    fn scopes_floor_trust_and_metadata() {
        let status = RoutingStatusTool::new(routed());
        let explain = RoutingExplainTool::new(routed());
        assert_eq!(status.required_scope(&json!({})).as_str(), "routing.status");
        assert_eq!(explain.required_scope(&json!({})).as_str(), "routing.read");
        for tool in [&status as &dyn Tool, &explain] {
            assert!(tool.auto_grantable_in_backcompat_floor(), "{}", tool.name());
            assert!(tool.output_is_untrusted(), "{}", tool.name());
            assert_eq!(super::super::check_tool_quality(tool), Vec::<String>::new());
        }
    }
}
