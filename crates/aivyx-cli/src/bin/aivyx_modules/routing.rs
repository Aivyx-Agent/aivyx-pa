//! Model routing Part 3a — builds the `RoutedProvider` from `[routing]` at
//! startup. With routing off (`[routing]` absent or `enabled = false`),
//! `wrap_with_routing` hands back the configured provider untouched.
//!
//! The default endpoint (`default`) is `[agent] provider` + `model`. Extra
//! `[routing.endpoints.*]` must be local in 3a: cloud candidates are only
//! allowed on the default endpoint, and only when the configured provider
//! itself is cloud.
//!
//! Also the offline `aivyx-pa routing status|explain` subcommand: `status`
//! runs the same discovery + merge without a daemon; `explain` scans the
//! audit chain's `ModelRouted` entries like `aivyx-pa cost` scans `LlmCost`.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use aivyx_audit::{AuditEvent, PersistentAuditLog, SignedEntry};
use aivyx_config::ProviderKind;
use aivyx_llm::ollama::{OllamaConfig, OllamaProvider};
use aivyx_llm::openai::{OpenAiConfig, OpenAiProvider};
use aivyx_llm::{LlmProvider, ProfileRefresher, ProviderFactory, RouteObserver, RoutedProvider};
use aivyx_route::{
    Availability, Capability, DefaultEndpoint, EndpointKind, EndpointRef, Locality, ModelKey,
    ModelProfile, RosterEntry, Router, RoutingConfig, find, merge,
};
use aivyx_storage::Storage;

/// The `[agent] provider` + `model`, as a routing endpoint name.
pub(crate) const DEFAULT_ENDPOINT: &str = "default";

/// The default endpoint's kind, from `[agent] provider`. It decides the
/// locality of every roster entry without an `endpoint`, and whether
/// routing may choose cloud models at all.
pub(crate) fn default_endpoint(kind: ProviderKind, base_url: Option<&str>) -> DefaultEndpoint {
    let kind = match kind {
        ProviderKind::Anthropic => EndpointKind::Anthropic,
        ProviderKind::OpenAi if base_url.is_some_and(is_loopback_url) => EndpointKind::OpenaiCompat,
        ProviderKind::OpenAi => EndpointKind::Openai,
        ProviderKind::Ollama => EndpointKind::Ollama,
        ProviderKind::LlamaCpp
        | ProviderKind::Jan
        | ProviderKind::MistralRs
        | ProviderKind::Broker => EndpointKind::OpenaiCompat,
    };
    DefaultEndpoint {
        name: EndpointRef::new(DEFAULT_ENDPOINT),
        kind,
    }
}

/// `provider = "openai"` pointed at a local OpenAI-compatible server.
fn is_loopback_url(url: &str) -> bool {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .is_some_and(|host| matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]" | "::1"))
}

/// 3a allows no new cloud destinations, and `default` names `[agent]`.
pub(crate) fn check_routing_config(cfg: &RoutingConfig) -> Result<(), String> {
    for (name, endpoint) in &cfg.endpoints {
        if name == DEFAULT_ENDPOINT {
            return Err(format!(
                "[routing.endpoints.{DEFAULT_ENDPOINT}] is reserved — it means the [agent] \
                 provider and model; give this endpoint another name"
            ));
        }
        if endpoint.kind.locality() == Locality::Cloud {
            return Err(format!(
                "[routing.endpoints.{name}] is a cloud endpoint; cloud endpoints arrive with \
                 consent-gated escalation in a later release — remove it (routing may already \
                 pick other models on a cloud [agent] provider via [[routing.models]])"
            ));
        }
    }
    Ok(())
}

/// `cfg` plus an implicit roster entry for `[agent] model` on the default
/// endpoint, unless an entry names it already.
pub(crate) fn effective_routing_config(cfg: &RoutingConfig, model: &str) -> RoutingConfig {
    let mut config = cfg.clone();
    let named = config
        .models
        .iter()
        .any(|m| m.id == model && m.endpoint.as_deref().is_none_or(|e| e == DEFAULT_ENDPOINT));
    if !named {
        config.models.push(RosterEntry {
            id: model.to_string(),
            endpoint: None,
            locality: None,
            tier: None,
            strengths: None,
            priority: None,
            capabilities: Default::default(),
            capabilities_deny: Default::default(),
            context_window: None,
        });
    }
    config
}

/// Routing endpoints are configured like discovery sees them; the providers
/// append `/v1/chat/completions` (or `/api/chat`) themselves.
pub(crate) fn provider_base_url(base: &str) -> String {
    let base = base.trim_end_matches('/');
    base.strip_suffix("/v1").unwrap_or(base).to_string()
}

/// Builds the provider for one `[routing.endpoints.*]` entry. The default
/// endpoint never reaches here (`RoutedProvider` serves it with the
/// configured provider), and no API key is ever sent to a routing endpoint.
pub(crate) fn provider_factory(cfg: &RoutingConfig) -> ProviderFactory {
    let endpoints = cfg.endpoints.clone();
    Box::new(move |endpoint: &EndpointRef| {
        let config = endpoints
            .get(endpoint.as_str())
            .ok_or_else(|| format!("no [routing.endpoints.{endpoint}] is configured"))?;
        let base = provider_base_url(
            config
                .base_url()
                .ok_or_else(|| format!("[routing.endpoints.{endpoint}] has no base_url"))?,
        );
        let provider: Arc<dyn LlmProvider> = match config.kind {
            EndpointKind::Ollama => Arc::new(
                OllamaProvider::new(OllamaConfig::default_local().with_base_url(base))
                    .map_err(|e| format!("[routing.endpoints.{endpoint}]: {e}"))?,
            ),
            EndpointKind::LlamaRouter | EndpointKind::OpenaiCompat => Arc::new(
                OpenAiProvider::new(OpenAiConfig::without_api_key().with_base_url(base))
                    .map_err(|e| format!("[routing.endpoints.{endpoint}]: {e}"))?,
            ),
            EndpointKind::Anthropic | EndpointKind::Openai => {
                return Err(format!(
                    "[routing.endpoints.{endpoint}] is a cloud endpoint, which routing does \
                     not dispatch to yet"
                ));
            }
        };
        Ok(provider)
    })
}

/// A warning when the `[agent] model`'s tool support is undeclared while
/// another candidate's is known: aivyx-route ranks unknown capabilities
/// below known ones, so it would lose every tool-using call.
pub(crate) fn backend_caps_undeclared_warning(
    profiles: &[ModelProfile],
    default: &ModelKey,
) -> Option<String> {
    let configured = find(profiles, default)?;
    if !configured.unknown_capabilities.contains(&Capability::Tools) {
        return None;
    }
    let others_known = profiles
        .iter()
        .any(|p| p.key() != *default && p.capabilities.contains(&Capability::Tools));
    others_known.then(|| {
        format!(
            "the [agent] model `{}` has no declared capabilities, so routing ranks it below \
             every model known to call tools — add a [[routing.models]] entry for it with \
             capabilities = [\"tools\", ...] and context_window = <its served window>",
            default.id
        )
    })
}

/// Discovery (when `[routing] discover`) + merge, for startup and refresh.
struct DiscoveryRefresher {
    config: RoutingConfig,
    default_endpoint: DefaultEndpoint,
    client: aivyx_route::discovery::reqwest::Client,
}

#[async_trait::async_trait]
impl ProfileRefresher for DiscoveryRefresher {
    async fn refresh(&self) -> Vec<ModelProfile> {
        let reports = if self.config.discover {
            aivyx_route::discovery::discover_all(&self.config, &self.client).await
        } else {
            Vec::new()
        };
        merge(&self.config, &self.default_endpoint, &reports)
    }
}

/// What `wrap_with_routing` and `aivyx-pa routing status` both build from
/// `[routing]`: the effective config, the discovered + merged candidates,
/// and the refresher that re-runs that discovery.
struct Prepared {
    config: RoutingConfig,
    default: DefaultEndpoint,
    default_key: ModelKey,
    refresher: Arc<DiscoveryRefresher>,
    profiles: Vec<ModelProfile>,
}

/// Checks `routing` (enabled), reports config issues, and runs discovery +
/// merge once.
async fn prepare(
    routing: &RoutingConfig,
    kind: ProviderKind,
    base_url: Option<&str>,
    model: &str,
) -> Result<Prepared, String> {
    check_routing_config(routing)?;
    let config = effective_routing_config(routing, model);
    let default = default_endpoint(kind, base_url);
    for issue in config.validate(&default) {
        eprintln!("aivyx-pa: routing config: {issue}");
    }
    let refresher = Arc::new(DiscoveryRefresher {
        config: config.clone(),
        default_endpoint: default.clone(),
        client: aivyx_route::discovery::reqwest::Client::new(),
    });
    let profiles = refresher.refresh().await;
    let default_key = ModelKey {
        endpoint: default.name.clone(),
        id: model.to_string(),
    };
    Ok(Prepared {
        config,
        default,
        default_key,
        refresher,
        profiles,
    })
}

/// Routing off (`None` or `enabled = false`) ⇒ `(provider, None)` with
/// `provider` untouched. Routing on ⇒ a `RoutedProvider` whose default
/// endpoint is `provider` serving `model`.
pub(crate) async fn wrap_with_routing(
    routing: Option<&RoutingConfig>,
    kind: ProviderKind,
    base_url: Option<&str>,
    model: &str,
    provider: Arc<dyn LlmProvider>,
    observer: Option<RouteObserver>,
) -> Result<(Arc<dyn LlmProvider>, Option<Arc<RoutedProvider>>), String> {
    let Some(routing) = routing.filter(|r| r.enabled) else {
        return Ok((provider, None));
    };
    let Prepared {
        config,
        default,
        default_key,
        refresher,
        profiles,
    } = prepare(routing, kind, base_url, model).await?;
    if let Some(warning) = backend_caps_undeclared_warning(&profiles, &default_key) {
        eprintln!("aivyx-pa: routing: {warning}");
    }
    let router = Router::new(profiles, config.tasks.clone())
        .with_allow_cloud(default.kind.locality() == Locality::Cloud);
    let mut routed = RoutedProvider::new(default_key, provider, router, provider_factory(&config))
        .with_refresher(refresher);
    if let Some(observer) = observer {
        routed = routed.with_observer(observer);
    }
    let routed = Arc::new(routed);
    Ok((Arc::clone(&routed) as Arc<dyn LlmProvider>, Some(routed)))
}

// ---------------------------------------------------------------------------
// `aivyx-pa routing status|explain` (offline)
// ---------------------------------------------------------------------------

/// One `ModelRouted` audit entry, for `aivyx-pa routing explain`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RoutedEntry {
    pub seq: u64,
    pub appended_at: SystemTime,
    pub session_id: Option<String>,
    pub model: String,
    pub task: String,
    pub reason: String,
}

/// `aivyx-pa routing status` output, in aivyx-coder's `/models` shape:
/// one line per candidate, `* ` marking the `[agent] model`, unknown
/// capabilities carrying a `?`. Pure.
pub(crate) fn render_status(
    profiles: &[ModelProfile],
    default: &ModelKey,
    warning: Option<&str>,
) -> String {
    let mut out = String::from("Routing candidates (* = the [agent] model):\n");
    for p in profiles {
        let key = p.key();
        let marker = if key == *default { "* " } else { "  " };
        let mut caps: Vec<String> = p.capabilities.iter().map(ToString::to_string).collect();
        caps.extend(p.unknown_capabilities.iter().map(|c| format!("{c}?")));
        let caps = if caps.is_empty() {
            "no known capabilities".to_string()
        } else {
            caps.join(" ")
        };
        let ctx = p
            .context_window
            .map_or_else(|| "?".to_string(), |n| n.to_string());
        let availability = match p.availability {
            Availability::Available => "available",
            Availability::Unverified => "unverified",
            Availability::Unavailable => "unavailable",
        };
        out.push_str(&format!(
            "{marker}{key} — {}, {caps}, ctx {ctx}, {availability}\n",
            p.tier
        ));
    }
    if let Some(warning) = warning {
        out.push_str(&format!("\n\u{26a0} {warning}\n"));
    }
    out
}

/// `aivyx-pa routing explain` output: `entries` in the order given
/// (the caller passes newest first), each with its age relative to `now`
/// and the router's reason. Pure.
pub(crate) fn render_explain(entries: &[RoutedEntry], now: SystemTime) -> String {
    if entries.is_empty() {
        return "Routing decisions\n  no routing decisions recorded yet (is [routing] enabled?)\n"
            .to_string();
    }
    let mut out = String::from("Routing decisions (newest first)\n");
    for e in entries {
        let age = now.duration_since(e.appended_at).map_or_else(
            |_| "just now".to_string(),
            |d| format!("{} ago", fmt_age(d)),
        );
        out.push_str(&format!(
            "  #{:<6} {age:<9} {:<10} → {}",
            e.seq, e.task, e.model
        ));
        if let Some(session) = &e.session_id {
            out.push_str(&format!("  (session {session})"));
        }
        out.push_str(&format!("\n          {}\n", e.reason));
    }
    out
}

/// Coarse age: `30s`, `5m`, `2h`, `3d`.
fn fmt_age(d: Duration) -> String {
    let secs = d.as_secs();
    match secs {
        0..60 => format!("{secs}s"),
        60..3_600 => format!("{}m", secs / 60),
        3_600..86_400 => format!("{}h", secs / 3_600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// `aivyx-pa routing status` — offline: load `[routing]`, run the same
/// discovery + merge the daemon runs at startup (no daemon, no store),
/// and print the candidates.
pub(crate) async fn run_routing_status(
    routing: Option<&RoutingConfig>,
    kind: ProviderKind,
    base_url: Option<&str>,
    model: &str,
) -> Result<(), String> {
    let Some(routing) = routing.filter(|r| r.enabled) else {
        println!(
            "Model routing is off — every call uses the [agent] model `{model}`. Add \
             [routing] enabled = true to aivyx-pa.toml to turn it on."
        );
        return Ok(());
    };
    let prepared = prepare(routing, kind, base_url, model).await?;
    let warning = backend_caps_undeclared_warning(&prepared.profiles, &prepared.default_key);
    print!(
        "{}",
        render_status(
            &prepared.profiles,
            &prepared.default_key,
            warning.as_deref()
        )
    );
    Ok(())
}

/// `aivyx-pa routing explain [--limit N]` — offline, the same cold-start
/// posture as `aivyx-pa cost`: scan the audit chain's `ModelRouted`
/// entries and print the newest `limit` of them.
pub(crate) async fn run_routing_explain(
    storage: Arc<dyn Storage>,
    audit_chain_key: [u8; 32],
    limit: usize,
) -> Result<(), String> {
    const PAGE_SIZE: usize = 1024;
    let log = PersistentAuditLog::open(storage, audit_chain_key)
        .await
        .map_err(|e| format!("failed to open audit chain: {e}"))?;
    let mut entries: Vec<RoutedEntry> = Vec::new();
    let mut cursor = 0u64;
    loop {
        let batch = log
            .entries_range(cursor, PAGE_SIZE)
            .map_err(|e| format!("audit-chain read failed at seq={cursor}: {e}"))?;
        if batch.is_empty() {
            break;
        }
        entries.extend(batch.iter().filter_map(routed_entry));
        cursor = batch.last().map(|e| e.seq + 1).unwrap_or(cursor);
    }
    entries.reverse();
    entries.truncate(limit);
    print!("{}", render_explain(&entries, SystemTime::now()));
    Ok(())
}

/// A `ModelRouted` entry, else `None`.
fn routed_entry(entry: &SignedEntry) -> Option<RoutedEntry> {
    let AuditEvent::ModelRouted {
        session_id,
        model,
        task,
        reason,
    } = &entry.event
    else {
        return None;
    };
    Some(RoutedEntry {
        seq: entry.seq,
        appended_at: entry.appended_at,
        session_id: session_id.clone(),
        model: model.clone(),
        task: task.clone(),
        reason: reason.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_route::{EndpointConfig, RouteQuery, TaskKind};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn parse(toml_src: &str) -> RoutingConfig {
        #[derive(serde::Deserialize)]
        struct Doc {
            #[serde(default)]
            routing: RoutingConfig,
        }
        toml::from_str::<Doc>(toml_src).unwrap().routing
    }

    fn key(endpoint: &str, id: &str) -> ModelKey {
        ModelKey {
            endpoint: EndpointRef::new(endpoint),
            id: id.into(),
        }
    }

    fn endpoint(kind: EndpointKind, base_url: Option<&str>) -> EndpointConfig {
        EndpointConfig {
            kind,
            base_url: base_url.map(str::to_string),
        }
    }

    /// A provider that never gets called (routing only holds it).
    fn unused_provider() -> Arc<dyn LlmProvider> {
        Arc::new(
            OpenAiProvider::new(
                OpenAiConfig::without_api_key().with_base_url("http://127.0.0.1:9"),
            )
            .unwrap(),
        )
    }

    #[test]
    fn provider_base_url_drops_v1_and_trailing_slashes() {
        assert_eq!(provider_base_url("http://h:11434"), "http://h:11434");
        assert_eq!(provider_base_url("http://h:11434/"), "http://h:11434");
        assert_eq!(provider_base_url("http://h:1337/v1"), "http://h:1337");
        assert_eq!(provider_base_url("http://h:1337/v1/"), "http://h:1337");
    }

    #[test]
    fn default_endpoint_kind_follows_the_provider() {
        let kind = |p, url| default_endpoint(p, url).kind;
        assert_eq!(kind(ProviderKind::Anthropic, None), EndpointKind::Anthropic);
        assert_eq!(kind(ProviderKind::OpenAi, None), EndpointKind::Openai);
        assert_eq!(
            kind(ProviderKind::OpenAi, Some("https://api.openai.com")),
            EndpointKind::Openai
        );
        for local in [
            "http://localhost:8000",
            "http://127.0.0.1:8000/v1",
            "http://[::1]:8000",
        ] {
            assert_eq!(
                kind(ProviderKind::OpenAi, Some(local)),
                EndpointKind::OpenaiCompat,
                "{local}"
            );
        }
        assert_eq!(kind(ProviderKind::Ollama, None), EndpointKind::Ollama);
        for p in [
            ProviderKind::LlamaCpp,
            ProviderKind::Jan,
            ProviderKind::MistralRs,
            ProviderKind::Broker,
        ] {
            assert_eq!(kind(p, None), EndpointKind::OpenaiCompat, "{p:?}");
        }
        assert_eq!(
            default_endpoint(ProviderKind::Ollama, None).name.as_str(),
            DEFAULT_ENDPOINT
        );
    }

    #[test]
    fn cloud_endpoints_and_the_reserved_name_are_rejected() {
        for kind in ["anthropic", "openai"] {
            let cfg = parse(&format!("[routing.endpoints.claude]\nkind = \"{kind}\"\n"));
            let err = check_routing_config(&cfg).unwrap_err();
            assert!(err.contains("claude"), "{err}");
            assert!(err.contains("consent-gated escalation"), "{err}");
        }

        let cfg = parse("[routing.endpoints.default]\nkind = \"ollama\"\n");
        let err = check_routing_config(&cfg).unwrap_err();
        assert!(err.contains("reserved"), "{err}");

        let cfg = parse("[routing.endpoints.gpu]\nkind = \"ollama\"\n");
        assert!(check_routing_config(&cfg).is_ok());
    }

    #[test]
    fn the_configured_model_becomes_an_implicit_roster_entry() {
        let cfg = parse("[[routing.models]]\nid = \"other\"\n");
        let c = effective_routing_config(&cfg, "qwen3:8b");
        let ids: Vec<(&str, Option<&str>)> = c
            .models
            .iter()
            .map(|m| (m.id.as_str(), m.endpoint.as_deref()))
            .collect();
        assert_eq!(ids, vec![("other", None), ("qwen3:8b", None)]);
    }

    #[test]
    fn an_existing_entry_for_the_configured_model_is_not_duplicated() {
        for endpoint in ["", "endpoint = \"default\"\n"] {
            let cfg = parse(&format!(
                "[[routing.models]]\nid = \"qwen3:8b\"\n{endpoint}tier = \"small\"\n"
            ));
            assert_eq!(
                effective_routing_config(&cfg, "qwen3:8b").models.len(),
                1,
                "{endpoint:?}"
            );
        }
        // The same id on another endpoint is a different model.
        let cfg = parse("[[routing.models]]\nid = \"qwen3:8b\"\nendpoint = \"gpu\"\n");
        assert_eq!(effective_routing_config(&cfg, "qwen3:8b").models.len(), 2);
    }

    /// Sends one request through `provider` to a one-shot local HTTP
    /// listener and returns the request line it received.
    async fn request_line(
        listener: tokio::net::TcpListener,
        provider: Arc<dyn LlmProvider>,
    ) -> String {
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = sock.read(&mut chunk).await.unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            sock.write_all(
                b"HTTP/1.1 400 Bad Request\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();
            let text = String::from_utf8_lossy(&buf).into_owned();
            text.lines().next().unwrap_or_default().to_string()
        });
        let messages = [aivyx_llm::LlmMessage::user_text("hi")];
        let request = aivyx_llm::LlmRequest {
            model: "m",
            system: None,
            messages: &messages,
            tools: &[],
            max_tokens: 8,
            temperature: None,
            id_slot: None,
            slot_hint: None,
            route: None,
        };
        let _ = provider
            .chat_stream(request, &aivyx_core::CancellationToken::new())
            .await;
        server.await.unwrap()
    }

    #[tokio::test]
    async fn the_factory_builds_a_provider_per_endpoint_kind() {
        for (kind, suffix, expected) in [
            // Native Ollama API (`/api/show` for auto num_ctx, then `/api/chat`).
            (EndpointKind::Ollama, "", "POST /api/"),
            (
                EndpointKind::OpenaiCompat,
                "/v1",
                "POST /v1/chat/completions ",
            ),
            (EndpointKind::LlamaRouter, "/", "POST /v1/chat/completions "),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}{suffix}", listener.local_addr().unwrap());
            let mut cfg = RoutingConfig::default();
            cfg.endpoints
                .insert("gpu".into(), endpoint(kind, Some(&base)));
            let provider = provider_factory(&cfg)(&EndpointRef::new("gpu")).unwrap();
            let line = request_line(listener, provider).await;
            assert!(line.starts_with(expected), "{kind:?}: {line}");
        }
    }

    #[test]
    fn the_factory_errors_for_unknown_or_urlless_endpoints() {
        let mut cfg = RoutingConfig::default();
        cfg.endpoints
            .insert("bare".into(), endpoint(EndpointKind::OpenaiCompat, None));
        let factory = provider_factory(&cfg);
        let err = factory(&EndpointRef::new("nowhere")).err().unwrap();
        assert!(err.contains("nowhere"), "{err}");
        let err = factory(&EndpointRef::new("bare")).err().unwrap();
        assert!(err.contains("bare") && err.contains("base_url"), "{err}");
    }

    fn profiles(entries: &[(&str, &str, &[Capability], &[Capability])]) -> Vec<ModelProfile> {
        entries
            .iter()
            .map(|(ep, id, known, unknown)| {
                let mut p = ModelProfile::new(*id, EndpointRef::new(*ep));
                p.capabilities.extend(known.iter().copied());
                p.unknown_capabilities.extend(unknown.iter().copied());
                p
            })
            .collect()
    }

    #[test]
    fn an_undeclared_default_model_is_warned_about() {
        use Capability::{Completion, Tools};
        let default = key("default", "qwen3:8b");
        let ps = profiles(&[
            ("default", "qwen3:8b", &[], &[Completion, Tools]),
            ("gpu", "coder", &[Completion, Tools], &[]),
        ]);
        let warning = backend_caps_undeclared_warning(&ps, &default).expect("should warn");
        assert!(warning.contains("qwen3:8b"), "{warning}");
        assert!(warning.contains("[[routing.models]]"), "{warning}");
        assert!(warning.contains("capabilities"), "{warning}");

        let ps = profiles(&[
            ("default", "qwen3:8b", &[Completion, Tools], &[]),
            ("gpu", "coder", &[Completion, Tools], &[]),
        ]);
        assert_eq!(backend_caps_undeclared_warning(&ps, &default), None);

        let ps = profiles(&[
            ("default", "qwen3:8b", &[], &[Completion, Tools]),
            ("gpu", "coder", &[], &[Tools]),
        ]);
        assert_eq!(backend_caps_undeclared_warning(&ps, &default), None);
    }

    #[tokio::test]
    async fn routing_off_returns_the_same_provider() {
        let provider = unused_provider();
        let (wrapped, routed) = wrap_with_routing(
            None,
            ProviderKind::Ollama,
            None,
            "qwen3:8b",
            Arc::clone(&provider),
            None,
        )
        .await
        .unwrap();
        assert!(Arc::ptr_eq(&wrapped, &provider));
        assert!(routed.is_none());

        // Present but disabled is the same as absent.
        let cfg = parse("[routing]\n[[routing.models]]\nid = \"x\"\n");
        assert!(!cfg.enabled);
        let (wrapped, routed) = wrap_with_routing(
            Some(&cfg),
            ProviderKind::Ollama,
            None,
            "qwen3:8b",
            Arc::clone(&provider),
            None,
        )
        .await
        .unwrap();
        assert!(Arc::ptr_eq(&wrapped, &provider));
        assert!(routed.is_none());
    }

    #[tokio::test]
    async fn routing_on_without_discovery_offers_the_roster() {
        let cfg = parse(
            "[routing]\nenabled = true\ndiscover = false\n\
             [[routing.models]]\nid = \"qwen3:32b\"\ntier = \"large\"\n",
        );
        let provider = unused_provider();
        let (wrapped, routed) = wrap_with_routing(
            Some(&cfg),
            ProviderKind::Ollama,
            None,
            "qwen3:8b",
            Arc::clone(&provider),
            None,
        )
        .await
        .unwrap();
        let routed = routed.expect("routing is on");
        assert!(!Arc::ptr_eq(&wrapped, &provider));
        assert_eq!(routed.default_key(), &key("default", "qwen3:8b"));
        let keys: Vec<String> = routed
            .router()
            .profiles()
            .iter()
            .map(|p| p.key().to_string())
            .collect();
        assert_eq!(keys, vec!["qwen3:32b@default", "qwen3:8b@default"]);
    }

    async fn plan_on_cloud_default(kind: ProviderKind) -> Result<ModelKey, String> {
        let cfg = parse(
            "[routing]\nenabled = true\ndiscover = false\n\
             [[routing.models]]\nid = \"claude-big\"\ntier = \"large\"\nlocality = \"cloud\"\n\
             capabilities = [\"completion\", \"tools\"]\ncontext_window = 200000\n",
        );
        let (_, routed) = wrap_with_routing(
            Some(&cfg),
            kind,
            None,
            "claude-small",
            unused_provider(),
            None,
        )
        .await
        .unwrap();
        let routed = routed.unwrap();
        routed
            .router()
            .plan(&RouteQuery::new(TaskKind::Plan), std::time::Instant::now())
            .map(|plan| plan.chain[0].clone())
            .map_err(|e| e.to_string())
    }

    #[tokio::test]
    async fn a_cloud_provider_lets_routing_pick_another_cloud_model_on_it() {
        assert_eq!(
            plan_on_cloud_default(ProviderKind::Anthropic).await,
            Ok(key("default", "claude-big"))
        );
        // Control: on a local provider the same cloud-tagged entry is never
        // chosen — routing adds no cloud destination the operator lacks.
        assert_ne!(
            plan_on_cloud_default(ProviderKind::Ollama).await,
            Ok(key("default", "claude-big"))
        );
    }

    #[tokio::test]
    async fn a_cloud_endpoint_fails_startup() {
        let cfg =
            parse("[routing]\nenabled = true\n[routing.endpoints.claude]\nkind = \"anthropic\"\n");
        let err = wrap_with_routing(
            Some(&cfg),
            ProviderKind::Ollama,
            None,
            "qwen3:8b",
            unused_provider(),
            None,
        )
        .await
        .err()
        .unwrap();
        assert!(err.contains("consent-gated escalation"), "{err}");
    }
    #[test]
    fn render_status_prints_one_line_per_candidate_and_marks_the_default() {
        use Capability::{Completion, Tools, Vision};
        let mut ps = profiles(&[
            ("default", "qwen3:8b", &[Completion], &[Tools]),
            ("gpu", "coder", &[Completion, Tools], &[Vision]),
        ]);
        ps[1].tier = aivyx_route::Tier::Large;
        ps[1].context_window = Some(32_768);
        ps[1].availability = aivyx_route::Availability::Available;
        let text = render_status(&ps, &key("default", "qwen3:8b"), Some("heads up"));

        let default = text
            .lines()
            .find(|l| l.contains("qwen3:8b@default"))
            .unwrap();
        assert!(default.starts_with("* "), "{default}");
        assert!(default.contains("completion tools?"), "{default}");
        assert!(default.contains("ctx ?"), "{default}");
        let coder = text.lines().find(|l| l.contains("coder@gpu")).unwrap();
        assert!(coder.starts_with("  "), "{coder}");
        assert!(coder.contains("large"), "{coder}");
        assert!(coder.contains("completion tools vision?"), "{coder}");
        assert!(coder.contains("ctx 32768"), "{coder}");
        assert!(coder.contains("available"), "{coder}");
        assert_eq!(
            text.lines().filter(|l| l.contains('@')).count(),
            2,
            "{text}"
        );
        assert!(text.contains("heads up"), "{text}");
    }

    #[test]
    fn render_status_says_when_no_capability_is_known() {
        let ps = profiles(&[("default", "m", &[], &[])]);
        let text = render_status(&ps, &key("default", "m"), None);
        assert!(text.contains("no known capabilities"), "{text}");
    }

    fn sample_entry(seq: u64, secs_ago: u64, session: Option<&str>) -> RoutedEntry {
        RoutedEntry {
            seq,
            appended_at: std::time::UNIX_EPOCH + std::time::Duration::from_secs(10_000 - secs_ago),
            session_id: session.map(str::to_string),
            model: format!("m{seq}@gpu"),
            task: "chat".into(),
            reason: format!("reason {seq}"),
        }
    }

    #[test]
    fn render_explain_lists_decisions_in_the_given_order_with_reasons() {
        let now = std::time::UNIX_EPOCH + std::time::Duration::from_secs(10_000);
        let text = render_explain(
            &[
                sample_entry(9, 30, Some("sess-1")),
                sample_entry(4, 7_200, None),
            ],
            now,
        );
        let first = text.find("m9@gpu").expect("newest listed");
        let second = text.find("m4@gpu").expect("older listed");
        assert!(first < second, "{text}");
        assert!(text.contains("#9"), "{text}");
        assert!(text.contains("30s ago"), "{text}");
        assert!(text.contains("2h ago"), "{text}");
        assert!(text.contains("chat"), "{text}");
        assert!(
            text.contains("reason 9") && text.contains("reason 4"),
            "{text}"
        );
        assert!(text.contains("sess-1"), "{text}");
    }

    #[test]
    fn render_explain_with_no_decisions_says_so() {
        let text = render_explain(&[], std::time::SystemTime::now());
        assert!(text.contains("no routing decisions recorded"), "{text}");
    }
    #[test]
    fn only_model_routed_entries_are_extracted() {
        let entry = |seq, event| SignedEntry {
            seq,
            appended_at: std::time::UNIX_EPOCH,
            event,
            prev_mac: [0; 32],
            mac: [0; 32],
        };
        let routed = entry(
            3,
            AuditEvent::ModelRouted {
                session_id: None,
                model: "big@gpu".into(),
                task: "plan".into(),
                reason: "plan prefers a large model".into(),
            },
        );
        let got = routed_entry(&routed).expect("ModelRouted is extracted");
        assert_eq!(got.seq, 3);
        assert_eq!(got.model, "big@gpu");
        assert_eq!(got.task, "plan");
        assert_eq!(got.session_id, None);

        let other = entry(
            4,
            AuditEvent::ModelRouted {
                session_id: Some("s".into()),
                model: "m".into(),
                task: "chat".into(),
                reason: "r".into(),
            },
        );
        assert_eq!(
            routed_entry(&other).unwrap().session_id.as_deref(),
            Some("s")
        );
        let unrelated = entry(
            5,
            AuditEvent::LlmCost {
                turn_id: aivyx_core::TurnId::new(),
                model: "m".into(),
                usage: Default::default(),
            },
        );
        assert_eq!(routed_entry(&unrelated), None);
    }
}
