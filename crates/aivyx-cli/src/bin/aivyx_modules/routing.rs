//! Model routing Part 3a — builds the `RoutedProvider` from `[routing]` at
//! startup. With routing off (`[routing]` absent or `enabled = false`),
//! `wrap_with_routing` hands back the configured provider untouched.
//!
//! The default endpoint (`default`) is `[agent] provider` + `model`. Extra
//! `[routing.endpoints.*]` may be cloud (`anthropic`/`openai`) only when
//! `[routing.escalation] mode` isn't `never` (Part 3b), and then only with
//! the operator's own API key for that kind. Their models are kept out of
//! the local router: cloud candidates there stay limited to the default
//! endpoint, and only when the configured provider itself is cloud.
//!
//! Also the offline `aivyx-pa routing status|explain` subcommand: `status`
//! runs the same discovery + merge without a daemon; `explain` scans the
//! audit chain's `ModelRouted` entries like `aivyx-pa cost` scans `LlmCost`.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use aivyx_audit::{AuditEvent, PersistentAuditLog, SignedEntry};
use aivyx_config::{EscalationConfig, EscalationMode, ProviderKind};
use aivyx_llm::anthropic::{AnthropicConfig, AnthropicProvider};
use aivyx_llm::ollama::{AUTO_NUM_CTX_CAP, OllamaConfig, OllamaProvider};
use aivyx_llm::openai::{OpenAiConfig, OpenAiProvider};
use aivyx_llm::{LlmProvider, ProfileRefresher, ProviderFactory, RouteObserver, RoutedProvider};
use aivyx_route::{
    Availability, Capability, DefaultEndpoint, EndpointKind, EndpointRef, Locality, ModelKey,
    ModelProfile, RosterEntry, Router, RoutingConfig, find, merge,
};
use aivyx_storage::Storage;
use secrecy::SecretString;

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

/// `[routing.escalation]` plus the operator's own cloud API keys: what a
/// `[routing.endpoints.*]` cloud endpoint needs. The keys are the same
/// `anthropic_api_key`/`openai_api_key` the `[agent]` provider uses.
#[derive(Clone, Default)]
pub(crate) struct CloudAccess {
    pub escalation: EscalationConfig,
    pub anthropic_key: Option<SecretString>,
    pub openai_key: Option<SecretString>,
    /// The daemon's one routing guard (taint + consent), when escalation is
    /// active. Without it no escalation layer is built — the offline
    /// `routing status` path and tests leave it `None`.
    pub escalation_guard: Option<Arc<dyn aivyx_llm::EscalationGuard>>,
    /// Receives every escalation decision (the daemon audits through it).
    pub escalation_observer: Option<aivyx_llm::EscalationObserver>,
}

impl CloudAccess {
    /// The key a cloud endpoint kind needs, and its config name.
    fn key_for(&self, kind: EndpointKind) -> Option<(Option<&SecretString>, &'static str)> {
        match kind {
            EndpointKind::Anthropic => Some((self.anthropic_key.as_ref(), "anthropic_api_key")),
            EndpointKind::Openai => Some((self.openai_key.as_ref(), "openai_api_key")),
            EndpointKind::Ollama | EndpointKind::LlamaRouter | EndpointKind::OpenaiCompat => None,
        }
    }
}

/// Cloud endpoints only under escalation (`mode` not `never`), and
/// `default` names `[agent]`.
pub(crate) fn check_routing_config(
    cfg: &RoutingConfig,
    escalation: &EscalationConfig,
) -> Result<(), String> {
    for (name, endpoint) in &cfg.endpoints {
        if name == DEFAULT_ENDPOINT {
            return Err(format!(
                "[routing.endpoints.{DEFAULT_ENDPOINT}] is reserved — it means the [agent] \
                 provider and model; give this endpoint another name"
            ));
        }
        if endpoint.kind.locality() == Locality::Cloud && escalation.mode == EscalationMode::Never {
            return Err(format!(
                "[routing.endpoints.{name}] is a cloud endpoint, but [routing.escalation] mode \
                 is \"never\" — set it to \"ask\" or \"auto\" to allow escalating to it, or \
                 remove the endpoint (routing may already pick other models on a cloud [agent] \
                 provider via [[routing.models]])"
            ));
        }
    }
    Ok(())
}

/// Model routing Part 3b — cloud escalation is live: `[routing]` is
/// enabled, `[routing.escalation] mode` isn't `never`, and at least one
/// `[routing.endpoints.*]` is a cloud endpoint. Only then does the daemon
/// run the taint machinery; otherwise nothing is marked (the
/// compatibility invariant — behaviour stays byte-identical to 3a).
pub(crate) fn escalation_active(
    cfg: Option<&RoutingConfig>,
    escalation: &EscalationConfig,
) -> bool {
    escalation.mode != EscalationMode::Never
        && cfg.filter(|c| c.enabled).is_some_and(|c| {
            c.endpoints
                .values()
                .any(|e| e.kind.locality() == Locality::Cloud)
        })
}

/// Every cloud endpoint has the operator's key for its kind.
pub(crate) fn check_cloud_keys(cfg: &RoutingConfig, access: &CloudAccess) -> Result<(), String> {
    for (name, endpoint) in &cfg.endpoints {
        if let Some((None, key_name)) = access.key_for(endpoint.kind) {
            return Err(format!(
                "[routing.endpoints.{name}] is a cloud endpoint but no {key_name} is \
                 configured — set it (the same key the [agent] provider would use) or remove \
                 the endpoint"
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
/// configured provider). Local endpoints never get an API key; a cloud
/// endpoint gets the operator's own key for its kind (and, for `openai`,
/// its `base_url` if set).
pub(crate) fn provider_factory(cfg: &RoutingConfig, access: &CloudAccess) -> ProviderFactory {
    let endpoints = cfg.endpoints.clone();
    let access = access.clone();
    Box::new(move |endpoint: &EndpointRef| {
        let config = endpoints
            .get(endpoint.as_str())
            .ok_or_else(|| format!("no [routing.endpoints.{endpoint}] is configured"))?;
        let err = |e| format!("[routing.endpoints.{endpoint}]: {e}");
        if let Some((key, key_name)) = access.key_for(config.kind) {
            let key = key.cloned().ok_or_else(|| {
                format!(
                    "[routing.endpoints.{endpoint}] is a cloud endpoint but no {key_name} is \
                     configured"
                )
            })?;
            let provider: Arc<dyn LlmProvider> = if config.kind == EndpointKind::Anthropic {
                Arc::new(AnthropicProvider::new(AnthropicConfig::new(key)).map_err(err)?)
            } else {
                let mut openai = OpenAiConfig::new(key);
                if let Some(base) = config.base_url() {
                    openai = openai.with_base_url(provider_base_url(base));
                }
                Arc::new(OpenAiProvider::new(openai).map_err(err)?)
            };
            return Ok(provider);
        }
        let base = provider_base_url(
            config
                .base_url()
                .ok_or_else(|| format!("[routing.endpoints.{endpoint}] has no base_url"))?,
        );
        let provider: Arc<dyn LlmProvider> = match config.kind {
            EndpointKind::Ollama => Arc::new(
                OllamaProvider::new(OllamaConfig::default_local().with_base_url(base))
                    .map_err(err)?,
            ),
            _ => Arc::new(
                OpenAiProvider::new(OpenAiConfig::without_api_key().with_base_url(base))
                    .map_err(err)?,
            ),
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
        let mut profiles = merge(&self.config, &self.default_endpoint, &reports);
        clamp_ollama_windows(&mut profiles, &self.config);
        without_cloud_endpoints(&mut profiles, &self.config);
        profiles
    }
}

/// The models on `[routing.endpoints.*]` cloud endpoints — the escalation
/// candidates. Cloud endpoints are never probed, so these are exactly the
/// roster entries naming them (merged as unverified).
fn cloud_candidates(config: &RoutingConfig, default: &DefaultEndpoint) -> Vec<ModelProfile> {
    merge(config, default, &[])
        .into_iter()
        .filter(|p| {
            config
                .endpoints
                .get(p.endpoint.as_str())
                .is_some_and(|e| e.kind.locality() == Locality::Cloud)
        })
        .collect()
}

fn llm_mode(mode: EscalationMode) -> aivyx_llm::EscalationMode {
    match mode {
        EscalationMode::Never => aivyx_llm::EscalationMode::Never,
        EscalationMode::Ask => aivyx_llm::EscalationMode::Ask,
        EscalationMode::Auto => aivyx_llm::EscalationMode::Auto,
    }
}

/// Drops the models on `[routing.endpoints.*]` cloud endpoints: they're
/// escalation targets, never local-router candidates, so the local router
/// behaves exactly as 3a's whether or not a cloud endpoint is configured.
/// (Cloud models on the default endpoint are unaffected.)
fn without_cloud_endpoints(profiles: &mut Vec<ModelProfile>, config: &RoutingConfig) {
    profiles.retain(|p| {
        config
            .endpoints
            .get(p.endpoint.as_str())
            .is_none_or(|e| e.kind.locality() != Locality::Cloud)
    });
}

/// Routed Ollama endpoints run with `OllamaConfig::default_local()`, whose
/// provider sends `num_ctx = min(native, AUTO_NUM_CTX_CAP)` per request —
/// so that, not the trained window discovery reports, is what the model
/// actually gets. A roster `context_window` (already merged into the
/// profile) can only lower it. An unknown window stays unknown.
fn clamp_ollama_windows(profiles: &mut [ModelProfile], config: &RoutingConfig) {
    for p in profiles {
        let ollama = config
            .endpoints
            .get(p.endpoint.as_str())
            .is_some_and(|e| e.kind == EndpointKind::Ollama);
        if ollama {
            p.context_window = p.context_window.map(|w| w.min(AUTO_NUM_CTX_CAP));
        }
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
    access: &CloudAccess,
    kind: ProviderKind,
    base_url: Option<&str>,
    model: &str,
) -> Result<Prepared, String> {
    check_routing_config(routing, &access.escalation)?;
    check_cloud_keys(routing, access)?;
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
    access: &CloudAccess,
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
    } = prepare(routing, access, kind, base_url, model).await?;
    if let Some(warning) = backend_caps_undeclared_warning(&profiles, &default_key) {
        eprintln!("aivyx-pa: routing: {warning}");
    }
    let router = Router::new(profiles, config.tasks.clone())
        .with_allow_cloud(default.kind.locality() == Locality::Cloud);
    let mut routed = RoutedProvider::new(
        default_key,
        provider,
        router,
        provider_factory(&config, access),
    )
    .with_refresher(refresher);
    if let Some(observer) = observer {
        routed = routed.with_observer(observer);
    }
    // Part 3b — escalation only when it's active and the daemon handed us
    // its guard; the cloud models live on their own router, never the
    // local one.
    if escalation_active(Some(routing), &access.escalation)
        && let Some(guard) = &access.escalation_guard
    {
        let cloud = cloud_candidates(&config, &default);
        if cloud.is_empty() {
            eprintln!(
                "aivyx-pa: routing: a cloud [routing.endpoints] entry is configured but no \
                 [[routing.models]] entry names it — nothing to escalate to"
            );
        } else {
            let esc = &access.escalation;
            routed = routed.with_escalation(aivyx_llm::EscalationSetup {
                router: Router::new(cloud, config.tasks.clone()).with_allow_cloud(true),
                mode: llm_mode(esc.mode),
                no_local_candidate: esc.no_local_candidate,
                tiers: esc
                    .tiers
                    .iter()
                    .map(|t| t.parse().unwrap_or_else(|never| match never {}))
                    .collect(),
                guard: Arc::clone(guard),
                observer: access
                    .escalation_observer
                    .clone()
                    .unwrap_or_else(|| Arc::new(|_: &aivyx_llm::EscalationRecord| {})),
            });
        }
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
    access: &CloudAccess,
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
    let prepared = prepare(routing, access, kind, base_url, model).await?;
    let warning = backend_caps_undeclared_warning(&prepared.profiles, &prepared.default_key);
    print!(
        "{}",
        render_status(
            &prepared.profiles,
            &prepared.default_key,
            warning.as_deref()
        )
    );
    if escalation_active(Some(routing), &access.escalation) {
        let cloud = cloud_candidates(&prepared.config, &prepared.default);
        print!("{}", render_escalation(&access.escalation, &cloud));
    }
    Ok(())
}

/// `aivyx-pa routing explain [--limit N]` — offline, the same cold-start
/// posture as `aivyx-pa cost`: scan the audit chain's routing entries
/// (`ModelRouted`, and Part 3b's escalation / consent / taint entries) and
/// print the newest `limit` of them.
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

/// A routing-related entry — `ModelRouted`, or (Part 3b)
/// `CloudEscalation` / `CloudConsentGranted` / `ConversationTainted`
/// shown with `escalation` / `consent` / `taint` in the task column —
/// else `None`. Never includes content (escalations carry only a hash).
fn routed_entry(entry: &SignedEntry) -> Option<RoutedEntry> {
    let (session_id, model, task, reason) = match &entry.event {
        AuditEvent::ModelRouted {
            session_id,
            model,
            task,
            reason,
        } => (session_id.clone(), model.clone(), task.clone(), reason.clone()),
        AuditEvent::CloudEscalation {
            session_id,
            model,
            trigger,
            mode,
            outcome,
            payload_hash,
        } => (
            session_id.clone(),
            model.clone().unwrap_or_else(|| "-".to_string()),
            "escalation".to_string(),
            format!(
                "{outcome} (trigger {trigger}, mode {mode}; payload {})",
                &payload_hash[..payload_hash.len().min(12)]
            ),
        ),
        AuditEvent::CloudConsentGranted { session_id, via } => (
            Some(session_id.clone()),
            "-".to_string(),
            "consent".to_string(),
            format!("cloud escalation allowed via {via}"),
        ),
        AuditEvent::ConversationTainted { session_id, reason } => (
            Some(session_id.clone()),
            "-".to_string(),
            "taint".to_string(),
            format!("tainted by {reason} — never escalates to the cloud"),
        ),
        _ => return None,
    };
    Some(RoutedEntry {
        seq: entry.seq,
        appended_at: entry.appended_at,
        session_id,
        model,
        task,
        reason,
    })
}

/// Part 3b — the escalation section of `aivyx-pa routing status`. Pure.
pub(crate) fn render_escalation(esc: &EscalationConfig, cloud: &[ModelProfile]) -> String {
    let mode = match esc.mode {
        EscalationMode::Never => "never",
        EscalationMode::Ask => "ask",
        EscalationMode::Auto => "auto",
    };
    let mut out = format!(
        "\nCloud escalation: mode {mode}; triggers: {}{}\n",
        if esc.no_local_candidate {
            "no_local_candidate"
        } else {
            "(no_local_candidate off)"
        },
        if esc.tiers.is_empty() {
            String::new()
        } else {
            format!(", tiers [{}]", esc.tiers.join(", "))
        }
    );
    for p in cloud {
        out.push_str(&format!("  {} (cloud)\n", p.key()));
    }
    if esc.mode == EscalationMode::Ask {
        out.push_str(
            "  In `ask` mode a turn that needs the cloud stops and asks; send /allow-cloud in \
             that conversation (or `aivyx-pa routing allow-cloud <session>`), then resend.\n",
        );
    }
    out.push_str(
        "  A conversation that touched sensitive data never escalates, in any mode.\n",
    );
    out
}

/// `aivyx-pa routing allow-cloud <session>` — allow cloud escalation for
/// one conversation on the running daemon (in-memory there; a daemon
/// restart re-asks). Never overrides a routing taint.
pub(crate) async fn run_allow_cloud(session: &str) -> Result<(), String> {
    let socket_path = aivyx_channel::daemon_ipc::default_socket_path()?;
    if !aivyx_channel::daemon_client::daemon_is_running(&socket_path).await {
        return Err("the daemon isn't running — start it, then allow cloud escalation for the \
                    conversation (or send /allow-cloud in it)"
            .to_string());
    }
    match aivyx_channel::daemon_client::allow_cloud_escalation(&socket_path, session).await {
        Ok(true) => {
            println!("Cloud escalation allowed for conversation {session} (until the daemon restarts).");
            Ok(())
        }
        Ok(false) => Err("cloud escalation is not enabled on the running daemon".to_string()),
        Err(e) => Err(format!("failed to reach the daemon: {e}")),
    }
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

    fn escalation(mode: EscalationMode) -> EscalationConfig {
        EscalationConfig {
            mode,
            ..EscalationConfig::default()
        }
    }

    /// Escalation at its default (`ask`) with both cloud keys present.
    fn cloud_access() -> CloudAccess {
        CloudAccess {
            escalation: EscalationConfig::default(),
            anthropic_key: Some(SecretString::from("sk-ant-test")),
            openai_key: Some(SecretString::from("sk-test")),
            ..CloudAccess::default()
        }
    }

    #[test]
    fn escalation_is_active_only_with_a_cloud_endpoint_and_a_non_never_mode() {
        let cloud =
            parse("[routing]\nenabled = true\n[routing.endpoints.claude]\nkind = \"anthropic\"\n");
        let local = parse(
            "[routing]\nenabled = true\n[routing.endpoints.box]\nkind = \"ollama\"\n\
             base_url = \"http://127.0.0.1:11434\"\n",
        );
        for mode in [EscalationMode::Ask, EscalationMode::Auto] {
            assert!(
                escalation_active(Some(&cloud), &escalation(mode)),
                "{mode:?}"
            );
            assert!(
                !escalation_active(Some(&local), &escalation(mode)),
                "{mode:?}"
            );
            assert!(!escalation_active(None, &escalation(mode)), "{mode:?}");
        }
        assert!(!escalation_active(
            Some(&cloud),
            &escalation(EscalationMode::Never)
        ));
        let disabled = RoutingConfig {
            enabled: false,
            ..cloud.clone()
        };
        assert!(!escalation_active(
            Some(&disabled),
            &escalation(EscalationMode::Ask)
        ));
    }

    #[test]
    fn cloud_endpoints_need_escalation_and_the_reserved_name_is_rejected() {
        let never = escalation(EscalationMode::Never);
        for kind in ["anthropic", "openai"] {
            let cfg = parse(&format!("[routing.endpoints.claude]\nkind = \"{kind}\"\n"));
            let err = check_routing_config(&cfg, &never).unwrap_err();
            assert!(err.contains("claude"), "{err}");
            assert!(err.contains("[routing.escalation] mode"), "{err}");
            for mode in [EscalationMode::Ask, EscalationMode::Auto] {
                assert!(
                    check_routing_config(&cfg, &escalation(mode)).is_ok(),
                    "{kind} under {mode:?}"
                );
            }
        }

        for mode in [EscalationMode::Never, EscalationMode::Ask] {
            let cfg = parse("[routing.endpoints.default]\nkind = \"ollama\"\n");
            let err = check_routing_config(&cfg, &escalation(mode)).unwrap_err();
            assert!(err.contains("reserved"), "{err}");

            // Local endpoints never need escalation.
            let cfg = parse("[routing.endpoints.gpu]\nkind = \"ollama\"\n");
            assert!(check_routing_config(&cfg, &escalation(mode)).is_ok());
        }
    }

    #[test]
    fn a_cloud_endpoint_without_its_key_is_an_error_naming_the_key() {
        for (kind, key_name) in [
            ("anthropic", "anthropic_api_key"),
            ("openai", "openai_api_key"),
        ] {
            let cfg = parse(&format!("[routing.endpoints.cloud]\nkind = \"{kind}\"\n"));
            let err = check_cloud_keys(&cfg, &CloudAccess::default()).unwrap_err();
            assert!(err.contains(key_name), "{err}");
            assert!(err.contains("cloud"), "{err}");
            assert!(check_cloud_keys(&cfg, &cloud_access()).is_ok(), "{kind}");
        }
        // Only the key the endpoint's kind needs.
        let cfg = parse("[routing.endpoints.cloud]\nkind = \"anthropic\"\n");
        let only_anthropic = CloudAccess {
            openai_key: None,
            ..cloud_access()
        };
        assert!(check_cloud_keys(&cfg, &only_anthropic).is_ok());
        // Local endpoints need no key.
        let cfg = parse("[routing.endpoints.gpu]\nkind = \"ollama\"\n");
        assert!(check_cloud_keys(&cfg, &CloudAccess::default()).is_ok());
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
            let provider =
                provider_factory(&cfg, &CloudAccess::default())(&EndpointRef::new("gpu")).unwrap();
            let line = request_line(listener, provider).await;
            assert!(line.starts_with(expected), "{kind:?}: {line}");
        }
    }

    #[test]
    fn the_factory_builds_an_anthropic_provider_with_the_operators_key() {
        let mut cfg = RoutingConfig::default();
        cfg.endpoints
            .insert("claude".into(), endpoint(EndpointKind::Anthropic, None));
        let factory = provider_factory(&cfg, &cloud_access());
        assert!(factory(&EndpointRef::new("claude")).is_ok());
        // No key, no provider (the startup key check is the first line).
        let err = provider_factory(&cfg, &CloudAccess::default())(&EndpointRef::new("claude"))
            .err()
            .unwrap();
        assert!(err.contains("anthropic_api_key"), "{err}");
    }

    #[tokio::test]
    async fn the_factory_builds_an_openai_provider_on_the_endpoints_base_url() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let mut cfg = RoutingConfig::default();
        cfg.endpoints
            .insert("gpt".into(), endpoint(EndpointKind::Openai, Some(&base)));
        let provider = provider_factory(&cfg, &cloud_access())(&EndpointRef::new("gpt")).unwrap();
        let line = request_line(listener, provider).await;
        assert!(line.starts_with("POST /v1/chat/completions "), "{line}");

        let err = provider_factory(&cfg, &CloudAccess::default())(&EndpointRef::new("gpt"))
            .err()
            .unwrap();
        assert!(err.contains("openai_api_key"), "{err}");
    }

    #[test]
    fn the_factory_errors_for_unknown_or_urlless_endpoints() {
        let mut cfg = RoutingConfig::default();
        cfg.endpoints
            .insert("bare".into(), endpoint(EndpointKind::OpenaiCompat, None));
        let factory = provider_factory(&cfg, &CloudAccess::default());
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
            &CloudAccess::default(),
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
            &CloudAccess::default(),
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
            &CloudAccess::default(),
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
            &CloudAccess::default(),
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
    async fn a_cloud_endpoint_fails_startup_under_mode_never_or_without_its_key() {
        let cfg =
            parse("[routing]\nenabled = true\n[routing.endpoints.claude]\nkind = \"anthropic\"\n");
        let never = CloudAccess {
            escalation: escalation(EscalationMode::Never),
            ..cloud_access()
        };
        let keyless = CloudAccess::default();
        for (access, expected) in [
            (&never, "[routing.escalation] mode"),
            (&keyless, "anthropic_api_key"),
        ] {
            let err = wrap_with_routing(
                Some(&cfg),
                access,
                ProviderKind::Ollama,
                None,
                "qwen3:8b",
                unused_provider(),
                None,
            )
            .await
            .err()
            .unwrap();
            assert!(err.contains(expected), "{err}");
        }
    }

    /// Until escalation dispatch lands (a separate escalation router), a
    /// configured cloud endpoint's models stay out of the local router:
    /// routing behaves exactly as 3a's.
    struct NoTaint;

    #[async_trait::async_trait]
    impl aivyx_llm::EscalationGuard for NoTaint {
        async fn taint(&self, _session: &str) -> Option<String> {
            None
        }
        fn consented(&self, _session: &str) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn escalation_gets_the_cloud_endpoint_models_only_with_a_guard() {
        let cfg = parse(
            "[routing]\nenabled = true\ndiscover = false\n\
             [routing.endpoints.claude]\nkind = \"anthropic\"\n\
             [[routing.models]]\nid = \"claude-big\"\nendpoint = \"claude\"\ntier = \"large\"\n\
             capabilities = [\"completion\", \"tools\"]\n",
        );
        let with_guard = CloudAccess {
            escalation_guard: Some(Arc::new(NoTaint)),
            ..cloud_access()
        };
        for (access, expected) in [
            (with_guard, vec!["claude-big@claude".to_string()]),
            (cloud_access(), vec![]),
        ] {
            let (_, routed) = wrap_with_routing(
                Some(&cfg),
                &access,
                ProviderKind::Ollama,
                None,
                "qwen3:8b",
                unused_provider(),
                None,
            )
            .await
            .unwrap();
            let routed = routed.expect("routing is on");
            let cloud: Vec<String> = routed
                .escalation_candidates()
                .iter()
                .map(|p| p.key().to_string())
                .collect();
            assert_eq!(cloud, expected);
            // Never on the local router.
            assert!(
                routed
                    .router()
                    .profiles()
                    .iter()
                    .all(|p| p.endpoint.as_str() != "claude")
            );
        }
    }

    #[tokio::test]
    async fn cloud_endpoint_models_are_kept_out_of_the_local_router() {
        let local = "[routing]\nenabled = true\ndiscover = false\n\
             [[routing.models]]\nid = \"qwen3:32b\"\ntier = \"large\"\n";
        let with_cloud = format!(
            "{local}[routing.endpoints.claude]\nkind = \"anthropic\"\n\
             [[routing.models]]\nid = \"claude-big\"\nendpoint = \"claude\"\ntier = \"large\"\n\
             capabilities = [\"completion\", \"tools\"]\ncontext_window = 200000\n"
        );
        let mut keys_per_config = Vec::new();
        for src in [local.to_string(), with_cloud] {
            let cfg = parse(&src);
            let (_, routed) = wrap_with_routing(
                Some(&cfg),
                &cloud_access(),
                ProviderKind::Ollama,
                None,
                "qwen3:8b",
                unused_provider(),
                None,
            )
            .await
            .unwrap();
            let routed = routed.expect("routing is on");
            let keys: Vec<String> = routed
                .router()
                .profiles()
                .iter()
                .map(|p| p.key().to_string())
                .collect();
            keys_per_config.push(keys);
        }
        assert_eq!(
            keys_per_config[0],
            vec!["qwen3:32b@default", "qwen3:8b@default"]
        );
        assert_eq!(keys_per_config[1], keys_per_config[0]);
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

    #[test]
    fn escalation_consent_and_taint_entries_are_extracted_without_content() {
        let entry = |seq, event| SignedEntry {
            seq,
            appended_at: std::time::UNIX_EPOCH,
            event,
            prev_mac: [0; 32],
            mac: [0; 32],
        };
        let esc = routed_entry(&entry(
            7,
            AuditEvent::CloudEscalation {
                session_id: Some("s".into()),
                model: Some("claude@cloud".into()),
                trigger: "no_local_candidate".into(),
                mode: "ask".into(),
                outcome: "consent_requested".into(),
                payload_hash: "ab".repeat(32),
            },
        ))
        .expect("CloudEscalation is extracted");
        assert_eq!(esc.task, "escalation");
        assert_eq!(esc.model, "claude@cloud");
        assert_eq!(esc.session_id.as_deref(), Some("s"));
        assert!(esc.reason.contains("consent_requested"), "{}", esc.reason);
        assert!(esc.reason.contains("no_local_candidate"), "{}", esc.reason);
        assert!(esc.reason.contains("ask"), "{}", esc.reason);

        let blocked = routed_entry(&entry(
            8,
            AuditEvent::CloudEscalation {
                session_id: Some("s".into()),
                model: None,
                trigger: "tier".into(),
                mode: "auto".into(),
                outcome: "blocked_taint".into(),
                payload_hash: "cd".repeat(32),
            },
        ))
        .unwrap();
        assert_eq!(blocked.model, "-");

        let consent = routed_entry(&entry(
            9,
            AuditEvent::CloudConsentGranted {
                session_id: "s".into(),
                via: "chat".into(),
            },
        ))
        .unwrap();
        assert_eq!(consent.task, "consent");
        assert!(consent.reason.contains("chat"), "{}", consent.reason);

        let taint = routed_entry(&entry(
            10,
            AuditEvent::ConversationTainted {
                session_id: "s".into(),
                reason: "gmail.search output".into(),
            },
        ))
        .unwrap();
        assert_eq!(taint.task, "taint");
        assert!(taint.reason.contains("gmail.search output"), "{}", taint.reason);
    }

    #[test]
    fn render_status_lists_escalation_settings_and_cloud_candidates() {
        let mut claude = ModelProfile::new("claude-big", EndpointRef::new("claude"));
        claude.capabilities.insert(Capability::Tools);
        let esc = EscalationConfig {
            mode: EscalationMode::Ask,
            no_local_candidate: true,
            tiers: vec!["plan".into()],
        };
        let text = render_escalation(&esc, &[claude]);
        assert!(text.contains("mode ask"), "{text}");
        assert!(text.contains("no_local_candidate"), "{text}");
        assert!(text.contains("plan"), "{text}");
        assert!(text.contains("claude-big@claude"), "{text}");
        assert!(text.contains("/allow-cloud"), "{text}");
    }

    #[test]
    fn routed_ollama_windows_declared_or_not_are_capped_at_the_served_num_ctx() {
        let cfg = parse(
            "[routing.endpoints.box]\nkind = \"ollama\"\nbase_url = \"http://127.0.0.1:11434\"\n\
             [routing.endpoints.gpu]\nkind = \"openai_compat\"\nbase_url = \"http://127.0.0.1:8080\"\n\
             [[routing.models]]\nid = \"declared\"\nendpoint = \"box\"\ncontext_window = 65536\n\
             [[routing.models]]\nid = \"lowered\"\nendpoint = \"box\"\ncontext_window = 4096\n",
        );
        let with_window = |endpoint: &str, id: &str, window: Option<u32>| {
            let mut p = ModelProfile::new(id, EndpointRef::new(endpoint));
            p.context_window = window;
            p
        };
        let mut profiles = vec![
            with_window("box", "big", Some(131_072)),
            with_window("box", "small", Some(8_192)),
            with_window("box", "unknown", None),
            // Roster windows arrive merged into the profile.
            with_window("box", "declared", Some(65_536)),
            with_window("box", "lowered", Some(4_096)),
            with_window("gpu", "big", Some(131_072)),
            with_window("default", "big", Some(131_072)),
        ];
        clamp_ollama_windows(&mut profiles, &cfg);

        let windows: Vec<Option<u32>> = profiles.iter().map(|p| p.context_window).collect();
        let cap = aivyx_llm::ollama::AUTO_NUM_CTX_CAP;
        assert_eq!(
            windows,
            vec![
                Some(cap),
                Some(8_192),
                None,
                Some(cap),
                Some(4_096),
                Some(131_072),
                Some(131_072),
            ]
        );
    }
}
