//! `aivyx-pa doctor` — first-run health check for the local on-ramp (Chapter P).
//!
//! Confirms the path actually works end to end and, when it doesn't, says
//! *why* and *what to do* — instead of leaving a new user with a blank first
//! turn. For the local (Ollama) provider it checks: Ollama reachable → the
//! configured model present → a real test generation that returns **non-empty**
//! text (the exact failure modes — empty thinking content, dropped tool calls,
//! starved `num_ctx` — that ruined first impressions). Lemonade Server gets a
//! reachability + model-downloaded check, aivyx-broker a reachability check
//! of the broker and the llama-server behind it, and embedded mistral.rs a
//! build-feature + model-file check. Cloud providers get a lighter
//! config-presence check. Read-only; no daemon, no passphrase.

use std::path::{Path, PathBuf};

use aivyx_config::{AivyxConfig, LoadOptions, MemoryProfile, ProviderKind};
use aivyx_llm::ollama::{
    AUTO_NUM_CTX_CAP, DEFAULT_OLLAMA_BASE_URL, OllamaConfig, OllamaOptions, OllamaProvider,
    RECOMMENDED_LOCAL_MODEL,
};
use aivyx_llm::{LlmMessage, LlmProvider, LlmRequest, LlmStreamEvent, LlmToolDescriptor};

const DOCTOR_TOML_PATH: &str = "aivyx-pa.toml";

/// `aivyx-pa doctor` — run the health checks and report.
pub async fn run_doctor() -> Result<(), String> {
    let cfg = load_config_for_inspection()?;
    println!("aivyx-pa doctor — checking your setup\n");

    let provider_ok = match cfg.provider.value {
        ProviderKind::Ollama => check_ollama(&cfg).await,
        ProviderKind::Lemonade => check_lemonade(&cfg).await,
        ProviderKind::Broker => check_broker(&cfg).await,
        ProviderKind::MistralRs => check_mistralrs(&cfg),
        other => check_cloud(other, &cfg),
    };

    // Chapter Engram — semantic-memory readiness (embedding provider + profile).
    let memory_ok = check_memory(&cfg).await;

    // Chapter Mise — the kitchen vertical health section, only when it's
    // installed (config present). Skipped silently otherwise.
    let kitchen_ok = match std::env::var_os("HOME").map(PathBuf::from) {
        Some(home) => check_kitchen(&home).await,
        None => true,
    };
    let all_ok = provider_ok && memory_ok && kitchen_ok;

    // Chapter Anchor — service-install status. Informational: not being
    // installed is fine for interactive use, so it never fails the doctor.
    check_service();

    // Chapter Gatehouse — VITRINE.md §13 operator UX note: a forgotten
    // Studio auth token used to mean grepping aivyx-pa.toml by hand to
    // recover it. Informational only, same as check_service() above —
    // never fails the doctor.
    check_gatehouse(&cfg);

    println!();
    if all_ok {
        println!("✓ Looks good — your agent is ready. Run `aivyx-pa` to start.");
        Ok(())
    } else {
        Err("one or more checks failed — see the notes above.".into())
    }
}

/// Chapter Anchor — report whether the daemon is installed as a persistent
/// service (and whether it's running). Purely informational: an interactive
/// user needs no service, so this never flips the doctor's exit status.
fn check_service() {
    println!("\nService:");
    match crate::daemon_service::installed_unit_path() {
        Some(path) => {
            pass(&format!("installed as a user service ({})", path.display()));
            match crate::daemon_service::is_active() {
                Some(true) => pass("running — survives logout/reboot"),
                Some(false) => println!(
                    "  ⚠ installed but not running\n     \
                     → start it: `systemctl --user start aivyx-pa-daemon` (or re-run `aivyx-pa daemon install`)"
                ),
                None => {}
            }
        }
        None => {
            println!(
                "  • not installed as a service — fine for interactive use.\n     \
                 → for a 'runs for days' agent: `aivyx-pa daemon install`"
            );
        }
    }
}

/// VITRINE.md §13 — "a future 'reveal/regenerate token' affordance (CLI
/// `aivyx-pa doctor` hint or Settings) would smooth this": an operator who
/// forgot their Studio auth token had to read `aivyx-pa.toml` directly to
/// recover it (that's exactly what a rig recovery looked like). This is
/// the "reveal" half — printing the token doctor already loaded to check
/// the interlock, rather than sending the operator to grep the TOML file
/// themselves. No new privilege: doctor already reads the full config,
/// and the operator already has direct read access to `aivyx-pa.toml` on
/// their own disk. "Regenerate" (writing a fresh token) is a mutating
/// action and stays out of scope for this read-only command — it's a
/// `[daemon] web_ui_auth_token` edit in `aivyx-pa.toml`, or a future
/// Settings-screen affordance.
/// The branch `check_gatehouse` prints, split out as pure/testable logic
/// (no `AivyxConfig` needed) from the `println!` side effects.
#[derive(Debug, PartialEq, Eq)]
enum GatehouseStatus {
    /// A token is set. `off_host` is `Some(host)` when bound beyond loopback.
    TokenSet { off_host: Option<std::net::IpAddr> },
    /// No token, and bound beyond loopback — the interlock-refusable case
    /// (unless `web_ui_insecure_no_auth` was set to bypass it).
    NoTokenOffHost { host: std::net::IpAddr },
    /// No operator token, loopback-only — the default: the daemon uses its
    /// automatic `studio-token` (unless `web_ui_insecure_no_auth`).
    NoTokenLoopback,
}

fn gatehouse_status(token: Option<&str>, host: Option<std::net::IpAddr>) -> GatehouseStatus {
    let off_host = host.filter(|h| !h.is_loopback());
    match (token, off_host) {
        (Some(_), _) => GatehouseStatus::TokenSet { off_host },
        (None, Some(h)) => GatehouseStatus::NoTokenOffHost { host: h },
        (None, None) => GatehouseStatus::NoTokenLoopback,
    }
}

/// First-run coherence A2 — the Studio sign-in link doctor shows, or
/// `None`: the Studio is off (`addr` is `None`), or it runs with no token
/// at all (`web_ui_insecure_no_auth`), or the automatic token hasn't been
/// created yet. An operator-set token wins over the automatic one, exactly
/// as in the daemon.
fn studio_sign_in_link(
    addr: Option<std::net::SocketAddr>,
    configured_token: Option<&str>,
    insecure_no_auth: bool,
    auto_token: Option<&str>,
) -> Option<String> {
    let addr = addr?;
    let token = match configured_token {
        Some(t) => t,
        None if insecure_no_auth => return None,
        None => auto_token?,
    };
    Some(aivyx_channel::studio_token::sign_in_url(
        addr.ip(),
        addr.port(),
        token,
    ))
}

/// First-run coherence A1 — the REPL banner's `Studio: <link>` line when
/// connected to a daemon: the sign-in link when `interactive` (stdout is a
/// terminal), otherwise the bare URL and a pointer to `aivyx-pa studio`, so
/// a redirected banner (`| tee chat.log`, `script`) never stores the token;
/// the bare URL under `web_ui_insecure_no_auth`; nothing when the Studio is
/// off or its token file doesn't exist yet.
pub fn repl_studio_line(
    addr: Option<std::net::SocketAddr>,
    configured_token: Option<&str>,
    insecure_no_auth: bool,
    auto_token: Option<&str>,
    interactive: bool,
) -> Option<String> {
    let addr = addr?;
    let url = aivyx_channel::studio_token::studio_url(addr.ip(), addr.port());
    if configured_token.is_none() && insecure_no_auth {
        return Some(format!("Studio: {url}"));
    }
    let link = studio_sign_in_link(Some(addr), configured_token, insecure_no_auth, auto_token)?;
    Some(if interactive {
        format!("Studio: {link}")
    } else {
        format!("Studio: {url} (sign-in link: run `aivyx-pa studio`)")
    })
}

/// `aivyx-pa studio [--token]` — what to print. With `token_only`, the bare
/// token (empty when the Studio runs with no token at all), for scripts and
/// the desktop app; otherwise the sign-in link (or the bare URL with no
/// token). The same token choice as the daemon and doctor.
fn studio_command_output(
    addr: Option<std::net::SocketAddr>,
    configured_token: Option<&str>,
    insecure_no_auth: bool,
    auto_token: Option<&str>,
    token_path: &Path,
    token_only: bool,
) -> Result<String, String> {
    let addr = addr.ok_or_else(|| "the Studio is off (`[daemon] web_ui = false`).".to_string())?;
    if configured_token.is_none() && !insecure_no_auth && auto_token.is_none() {
        return Err(format!(
            "no Studio sign-in token yet — start the daemon (`aivyx-pa daemon run`), \
             which creates {}.",
            token_path.display()
        ));
    }
    if token_only {
        let token = match configured_token {
            Some(t) => t,
            None if insecure_no_auth => "",
            None => auto_token.unwrap_or(""),
        };
        return Ok(token.to_string());
    }
    Ok(studio_sign_in_link(Some(addr), configured_token, insecure_no_auth, auto_token)
        .unwrap_or_else(|| aivyx_channel::studio_token::studio_url(addr.ip(), addr.port())))
}

/// `aivyx-pa studio [--token]`: print the Studio sign-in link (or, with
/// `--token`, just the token) for this config. Reads the `0600` token file
/// as the operator; never creates it (the daemon does). No daemon needed.
pub fn run_studio(token_only: bool) -> Result<(), String> {
    let cfg = load_config_for_inspection()?;
    let token_path = aivyx_channel::studio_token::token_path(&cfg.storage_path.value);
    let auto_token = aivyx_channel::studio_token::read_existing(&token_path);
    let out = studio_command_output(
        cfg.studio_addr(),
        cfg.web_ui_auth_token.as_deref(),
        cfg.web_ui_insecure_no_auth,
        auto_token.as_deref(),
        &token_path,
        token_only,
    )?;
    println!("{out}");
    Ok(())
}

fn check_gatehouse(cfg: &AivyxConfig) {
    println!("\nWeb UI (Gatehouse):");
    let Some(addr) = cfg.studio_addr() else {
        println!("  • the Studio is off (`[daemon] web_ui = false`).");
        return;
    };
    let token_path = aivyx_channel::studio_token::token_path(&cfg.storage_path.value);
    let auto_token = if cfg.web_ui_auth_token.is_none() && !cfg.web_ui_insecure_no_auth {
        aivyx_channel::studio_token::read_existing(&token_path)
    } else {
        None
    };
    match gatehouse_status(cfg.web_ui_auth_token.as_deref(), cfg.web_ui_host) {
        GatehouseStatus::TokenSet { off_host } => {
            let host_note = off_host.map_or(String::new(), |h| format!(" (bound off-host at {h})"));
            pass(&format!("auth token set{host_note}"));
            println!(
                "     token: {}",
                cfg.web_ui_auth_token.as_deref().unwrap_or("")
            );
        }
        GatehouseStatus::NoTokenOffHost { host } => {
            println!(
                "  ⚠ bound off-host ({host}) with no `[daemon] web_ui_auth_token` set\n     \
                 → anyone who can reach this host can drive the agent; set a token \
                 (e.g. `openssl rand -hex 32`) in aivyx-pa.toml, or \
                 `web_ui_insecure_no_auth = true` if a reverse proxy already \
                 authenticates. See docs/GATEHOUSE.md."
            );
        }
        GatehouseStatus::NoTokenLoopback if cfg.web_ui_insecure_no_auth => {
            println!(
                "  ⚠ no auth token (`web_ui_insecure_no_auth = true`) — any account \
                 on this machine can open the Studio at {}",
                aivyx_channel::studio_token::studio_url(addr.ip(), addr.port())
            );
        }
        GatehouseStatus::NoTokenLoopback => match &auto_token {
            Some(_) => pass(&format!("automatic sign-in token ({})", token_path.display())),
            None => println!(
                "  • no sign-in token yet — the daemon creates {} the first \
                 time it serves the Studio.",
                token_path.display()
            ),
        },
    }
    if let Some(link) = studio_sign_in_link(
        Some(addr),
        cfg.web_ui_auth_token.as_deref(),
        cfg.web_ui_insecure_no_auth,
        auto_token.as_deref(),
    ) {
        println!("     sign-in: {link}");
    }
}

fn pass(label: &str) {
    println!("  ✓ {label}");
}
fn fail(label: &str, hint: &str) {
    println!("  ✗ {label}\n     → {hint}");
}

/// Run the local-Ollama checks. Returns `true` iff every check passed.
async fn check_ollama(cfg: &AivyxConfig) -> bool {
    let base_url = cfg
        .openai_base_url
        .as_ref()
        .map(|s| s.value.clone())
        .unwrap_or_else(|| DEFAULT_OLLAMA_BASE_URL.to_string());
    let model = cfg.model.value.clone();
    println!("Provider: ollama (model `{model}`, {base_url})\n");

    // 1. Ollama reachable.
    if !crate::init::detect_ollama(&base_url).await {
        fail(
            &format!("Ollama is not reachable at {base_url}"),
            "Start it with `ollama serve` (and install Ollama if you haven't: https://ollama.com).",
        );
        return false;
    }
    pass("Ollama is running");

    // 2. The configured model is pulled.
    let models = crate::init::list_ollama_models(&base_url)
        .await
        .unwrap_or_default();
    if !models.iter().any(|m| m == &model) {
        fail(
            &format!("model `{model}` is not downloaded"),
            &format!("Pull it with `ollama pull {model}` (or `aivyx-pa init` to pick/pull a model)."),
        );
        return false;
    }
    pass(&format!("model `{model}` is available"));

    // 3. A real test generation returns non-empty text — the check that
    //    catches empty-content / dropped-tool / starved-context regressions.
    match test_generation(&base_url, &model).await {
        Ok(text) if !text.trim().is_empty() => {
            pass(&format!("test reply OK: \"{}\"", truncate(&text, 60)));
            // Capable-hardware tip — surface the context posture so an operator
            // on a big GPU (e.g. a 24GB card) isn't silently under-using it.
            match cfg.ollama_options.num_ctx {
                Some(n) => println!("  context: num_ctx = {n} (explicit)"),
                None => println!(
                    "  context: num_ctx = auto (≤{AUTO_NUM_CTX_CAP}) — on a capable GPU, \
                     size the model up and raise `[ollama] num_ctx`: see docs/LOCAL_HOSTING.md"
                ),
            }
            true
        }
        Ok(_) => {
            fail(
                "the model returned an EMPTY reply",
                &format!(
                    "The classic local-model failure. Try the recommended model: \
                     `ollama pull {RECOMMENDED_LOCAL_MODEL}` — it's verified to work \
                     with the agent's tool-calling."
                ),
            );
            false
        }
        Err(e) => {
            fail(
                &format!("test generation failed: {e}"),
                "Check that Ollama is healthy and the model loads.",
            );
            false
        }
    }
}

/// One real generation through the native provider — same path a turn uses
/// (auto-`num_ctx` + thinking handling apply), with a tool present so the
/// thinking-empty mode is exercised. Returns the collected text.
async fn test_generation(base_url: &str, model: &str) -> Result<String, String> {
    let provider = OllamaProvider::new(
        OllamaConfig::default_local()
            .with_base_url(base_url.to_string())
            .with_options(OllamaOptions::default()),
    )
    .map_err(|e| format!("build provider: {e}"))?;

    // A dummy tool makes the request shape match a real agent turn (the
    // empty-content bug only shows when `tools` is present).
    let tools = vec![LlmToolDescriptor {
        name: "noop".to_string(),
        description: "A tool you do not need to call for this.".to_string(),
        input_schema: serde_json::json!({ "type": "object", "properties": {} }),
    }];
    let messages = vec![LlmMessage::user_text("Reply with exactly the word: OK")];
    let request = LlmRequest {
        model,
        system: Some("You are a helpful assistant."),
        messages: &messages,
        tools: &tools,
        max_tokens: 64,
        temperature: None,
        id_slot: None,
        slot_hint: None,
        route: None,
    };

    let token = aivyx_core::CancellationToken::new();
    let mut stream = provider
        .chat_stream(request, &token)
        .await
        .map_err(|e| format!("{e}"))?;
    let mut text = String::new();
    while let Some(event) = stream.next_event().await.map_err(|e| format!("{e}"))? {
        if let LlmStreamEvent::TextChunk(chunk) = event {
            text.push_str(&chunk);
        }
    }
    // Drain the terminal; the text-so-far is what we assert on.
    let _ = Box::new(stream).finish().await;
    Ok(text)
}

/// What doctor concluded about a Lemonade Server, split out as pure logic
/// from the probing and the `println!` side effects.
#[derive(Debug, PartialEq, Eq)]
enum LemonadeVerdict {
    Unreachable(String),
    /// Reachable, but the configured model is not downloaded.
    ModelMissing { downloaded: Vec<String> },
    /// Reachable with the model downloaded; `loaded` is whichever model
    /// Lemonade holds right now (it holds one LLM at a time).
    Ready { loaded: Option<String> },
}

fn lemonade_verdict(
    discovery: aivyx_route::DiscoveryOutcome,
    residency: &aivyx_route::ResidencySnapshot,
    model: &str,
) -> LemonadeVerdict {
    let downloaded: Vec<String> = match discovery {
        aivyx_route::DiscoveryOutcome::Reached(models) => {
            models.into_iter().map(|m| m.id).collect()
        }
        aivyx_route::DiscoveryOutcome::Unreachable(why) => {
            return LemonadeVerdict::Unreachable(why);
        }
        aivyx_route::DiscoveryOutcome::NotProbed => {
            return LemonadeVerdict::Unreachable("not probed".into());
        }
    };
    if !downloaded.iter().any(|id| id == model) {
        return LemonadeVerdict::ModelMissing { downloaded };
    }
    let loaded = residency.models.iter().find_map(|(key, r)| {
        matches!(r, aivyx_route::ModelResidency::Loaded { .. }).then(|| key.id.clone())
    });
    LemonadeVerdict::Ready { loaded }
}

/// Lemonade Server checks: reachable, and the configured model is
/// downloaded (through aivyx-route's own Lemonade discovery + residency,
/// the same view routing gets). Returns `true` iff both passed.
async fn check_lemonade(cfg: &AivyxConfig) -> bool {
    let base_url = cfg
        .openai_base_url
        .as_ref()
        .map(|s| s.value.clone())
        .unwrap_or_else(|| crate::DEFAULT_LEMONADE_BASE_URL.to_string());
    let model = cfg.model.value.clone();
    println!("Provider: lemonade (model `{model}`, {base_url})\n");
    check_lemonade_at(&base_url, &model).await
}

async fn check_lemonade_at(base_url: &str, model: &str) -> bool {
    let client = aivyx_route::discovery::reqwest::Client::new();
    let endpoint = aivyx_route::EndpointRef::new("default");
    let config = aivyx_route::EndpointConfig {
        kind: aivyx_route::EndpointKind::Lemonade,
        base_url: Some(base_url.to_string()),
    };
    let report = aivyx_route::discovery::discover(&endpoint, &config, &client).await;
    let residency =
        aivyx_route::discovery::residency::collect(&[(endpoint, config)], None, None, &client)
            .await;
    match lemonade_verdict(report.outcome, &residency, model) {
        LemonadeVerdict::Unreachable(why) => {
            fail(
                &format!("Lemonade Server is not reachable at {base_url} ({why})"),
                "Start it (`lemond`, or its service), and keep the `/api` suffix on \
                 `[openai] base_url` (default http://127.0.0.1:13305/api).",
            );
            false
        }
        LemonadeVerdict::ModelMissing { downloaded } => {
            let have = if downloaded.is_empty() {
                "none".to_string()
            } else {
                downloaded.join(", ")
            };
            fail(
                &format!("model `{model}` is not downloaded (downloaded: {have})"),
                &format!("Download it with `lemonade pull {model}`, or set `[agent] model` to one of those."),
            );
            false
        }
        LemonadeVerdict::Ready { loaded } => {
            pass("Lemonade Server is running");
            pass(&format!("model `{model}` is downloaded"));
            match loaded {
                Some(id) if id == model => println!("  loaded now: `{id}`"),
                Some(id) => println!(
                    "  loaded now: `{id}` — Lemonade holds one LLM at a time, so the \
                     first turn swaps to `{model}`"
                ),
                None => println!("  no model loaded yet — the first turn loads `{model}`"),
            }
            true
        }
    }
}

/// How long doctor waits on a local server before calling it unreachable.
const LOCAL_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// aivyx-broker checks: the broker answers `GET /status`, and the
/// llama-server it proxies to answers `GET /health` (a running broker in
/// front of a stopped llama-server is the likely failure). Returns `true`
/// iff both answered.
async fn check_broker(cfg: &AivyxConfig) -> bool {
    let base_url = cfg
        .broker_base_url
        .clone()
        .unwrap_or_else(|| crate::DEFAULT_BROKER_BASE_URL.to_string());
    println!("Provider: broker (model `{}`, {base_url})\n", cfg.model.value);
    check_broker_at(&base_url).await
}

async fn check_broker_at(base_url: &str) -> bool {
    let client = reqwest::Client::new();
    let base = base_url.trim_end_matches('/');
    let status: Result<serde_json::Value, String> = async {
        client
            .get(format!("{base}/status"))
            .timeout(LOCAL_PROBE_TIMEOUT)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }
    .await;
    let status = match status {
        Ok(v) => v,
        Err(why) => {
            fail(
                &format!("aivyx-broker is not reachable at {base_url} ({why})"),
                "Start it (`aivyx-broker`, see its README), or set `[broker] base_url` \
                 (default http://127.0.0.1:8899).",
            );
            return false;
        }
    };
    pass("aivyx-broker is running");
    let Some(upstream) = status.get("llama_server_url").and_then(|v| v.as_str()) else {
        println!("  (the broker did not say which llama-server it fronts)");
        return true;
    };
    let upstream = upstream.trim_end_matches('/');
    let health = client
        .get(format!("{upstream}/health"))
        .timeout(LOCAL_PROBE_TIMEOUT)
        .send()
        .await
        .and_then(|r| r.error_for_status());
    match health {
        Ok(_) => {
            pass(&format!("its llama-server is answering ({upstream})"));
            true
        }
        Err(e) => {
            fail(
                &format!("the broker's llama-server is not answering at {upstream} ({e})"),
                "Start llama-server (or check the broker's `--llama-server-url`); \
                 the broker only forwards to it.",
            );
            false
        }
    }
}

/// The model file mistral.rs would load, or why it can't: `model_path`
/// must be set and exist; a directory needs `model_file` inside it, or
/// else at least one `.gguf`.
pub(crate) fn mistralrs_model_check(
    opts: &aivyx_config::MistralRsOptions,
) -> Result<PathBuf, String> {
    let path = opts
        .model_path
        .as_ref()
        .ok_or_else(|| "[mistralrs] model_path is not set".to_string())?;
    if path.is_file() {
        return Ok(path.clone());
    }
    if !path.is_dir() {
        return Err(format!("[mistralrs] model_path {} does not exist", path.display()));
    }
    match &opts.model_file {
        Some(file) => {
            let full = path.join(file);
            if full.is_file() {
                Ok(full)
            } else {
                Err(format!("[mistralrs] model_file {} does not exist", full.display()))
            }
        }
        None => {
            let has_gguf = std::fs::read_dir(path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?
                .filter_map(Result::ok)
                .any(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("gguf")));
            if has_gguf {
                Ok(path.clone())
            } else {
                Err(format!("no .gguf file in {}", path.display()))
            }
        }
    }
}

/// Embedded mistral.rs checks: this binary was built with the
/// `provider-mistral-rs` feature, and the configured GGUF exists. The model
/// itself is not loaded (that can take minutes). Returns `true` iff both hold.
fn check_mistralrs(cfg: &AivyxConfig) -> bool {
    println!("Provider: mistralrs (embedded, model `{}`)\n", cfg.model.value);
    if !cfg!(feature = "provider-mistral-rs") {
        fail(
            "this aivyx-pa was built without mistral.rs support",
            "Rebuild with `--features provider-mistral-rs` (or `recommended-providers`).",
        );
        return false;
    }
    pass("built with mistral.rs support");
    match mistralrs_model_check(&cfg.mistralrs_options) {
        Ok(path) => {
            pass(&format!("model found: {}", path.display()));
            println!("  (not loaded here; the first turn loads it, which can take a while)");
            true
        }
        Err(why) => {
            fail(
                &why,
                "Set `[mistralrs] model_path` to a GGUF file, or a directory of them \
                 (plus `model_file` to pick one).",
            );
            false
        }
    }
}

/// Lighter check for cloud providers — confirm the config is wired (a key is
/// present). The full credential round-trip lives in the `init` wizard.
fn check_cloud(provider: ProviderKind, cfg: &AivyxConfig) -> bool {
    println!("Provider: {provider} (model `{}`)\n", cfg.model.value);
    let has_key = match provider {
        ProviderKind::Anthropic => cfg.anthropic_api_key.is_some(),
        ProviderKind::OpenAi | ProviderKind::LlamaCpp | ProviderKind::Jan => {
            cfg.openai_api_key.is_some()
        }
        _ => true,
    };
    if has_key {
        pass("an API key is configured");
        println!("  (run `aivyx-pa init` to re-verify the key against the provider)");
        true
    } else {
        fail(
            "no API key configured for this provider",
            "Run `aivyx-pa init` to set one, or switch to a local model with `[agent] provider = \"ollama\"`.",
        );
        false
    }
}

/// Chapter Engram — the semantic-memory readiness section. Reports the
/// embedding provider + active profile, and (on the local path) checks the
/// embedding model is actually pulled — the most common "configured but dark"
/// failure. A config with no `[embedding]` is valid (semantic recall is simply
/// off), so that path notes how to enable it and does **not** fail.
async fn check_memory(cfg: &AivyxConfig) -> bool {
    println!("\nMemory:");
    let profile = match cfg.memory_profile {
        MemoryProfile::Off => "off",
        MemoryProfile::Lite => "lite",
        MemoryProfile::Smart => "smart",
    };

    let Some(emb) = &cfg.embedding else {
        // Backlog #1 — `profile = smart` arms the semantic stack but, with
        // no `[embedding]`, the semantic source is inert. Call out that
        // mismatch loudly (it's a misconfig, not a deliberate "off"); `off`
        // and the embedding-free `lite` profile just note how to enable it.
        if matches!(cfg.memory_profile, MemoryProfile::Lite) {
            // Chapter Ember — lite is embedding-free BY DESIGN: BM25 lexical +
            // co-occurrence recall over existing memory, zero setup. Report it
            // as active, not "off".
            pass("lite recall active (lexical BM25 + co-occurrence, no embeddings)");
            println!(
                "     → add an [embedding] section (a local Ollama running \
                 {model}, or OpenAI) and set `profile = smart` for semantic recall.",
                model = crate::init::RECOMMENDED_EMBED_MODEL
            );
        } else if matches!(cfg.memory_profile, MemoryProfile::Smart) {
            println!(
                "  ⚠ `[memory] profile = smart` is set but there is no \
                 [embedding] provider, so semantic recall is inert.\n     → add \
                 an [embedding] section (a local Ollama running {model}, or \
                 OpenAI), or use `profile = lite` for embedding-free recall.",
                model = crate::init::RECOMMENDED_EMBED_MODEL
            );
        } else {
            println!(
                "  semantic memory is off (no [embedding] provider)\n     → run \
                 `aivyx-pa init`, or add an [embedding] section (a local Ollama running \
                 {model}, or OpenAI) to enable recall.",
                model = crate::init::RECOMMENDED_EMBED_MODEL
            );
        }
        return true;
    };

    println!(
        "  embedding: `{}` ({} dims) at {}",
        emb.model, emb.dimensions, emb.base_url
    );
    println!("  profile: {profile}");

    // On the local path, confirm the embedding model is pulled and the server
    // is up — without that, recall is silently dark.
    if is_local_base_url(&emb.base_url) {
        if !crate::init::detect_ollama(&emb.base_url).await {
            fail(
                &format!("embedding server not reachable at {}", emb.base_url),
                "Start it with `ollama serve`.",
            );
            return false;
        }
        let models = crate::init::list_ollama_models(&emb.base_url)
            .await
            .unwrap_or_default();
        let present = models
            .iter()
            .any(|m| m == &emb.model || m.starts_with(&format!("{}:", emb.model)));
        if !present {
            fail(
                &format!("embedding model `{}` is not downloaded", emb.model),
                &format!("Pull it with `ollama pull {}`.", emb.model),
            );
            return false;
        }
        pass(&format!("embedding model `{}` is available", emb.model));
    } else {
        // Cloud embedding — don't make a paid call; just confirm it's wired.
        pass("embedding provider configured");
    }

    if matches!(cfg.memory_profile, MemoryProfile::Off) {
        println!(
            "  note: [embedding] is set but [memory] profile is off — set \
             profile = \"smart\" (or \"lite\") to use graph-augmented recall."
        );
    }
    true
}

/// Heuristic: does this embedding `base_url` point at a local server (Ollama)?
/// Used only to decide whether the doctor can cheaply verify the model is
/// pulled; a false negative just downgrades to the "configured" note.
fn is_local_base_url(url: &str) -> bool {
    url.contains("localhost")
        || url.contains("127.0.0.1")
        || url.contains("0.0.0.0")
        || url.contains(":11434")
}

/// `~/.aivyx-pa/tool-processes/kitchen/config.toml` — the kitchen vertical's
/// presence marker. `Some(path)` iff the file exists (the vertical is installed).
fn kitchen_config_path(home: &Path) -> Option<PathBuf> {
    let p = home
        .join(".aivyx-pa")
        .join("tool-processes")
        .join("kitchen")
        .join("config.toml");
    p.exists().then_some(p)
}

/// Chapter Mise — the kitchen vertical health section. Returns `true` when the
/// vertical is **not installed** (skipped silently) or when every check passes;
/// `false` if installed-but-broken. Checks: config parses → `[[tool_process]]`
/// wired → `[team] config_path` points at a loadable pack → KitchenDB reachable.
async fn check_kitchen(home: &Path) -> bool {
    let Some(cfg_path) = kitchen_config_path(home) else {
        return true; // vertical not installed — nothing to report.
    };
    println!("\nKitchen vertical:\n");

    // 1. config.toml parses as [kitchen_db].
    let db = match aivyx_kitchen_toolkit::load_config(&cfg_path) {
        Ok(d) => {
            pass(&format!("kitchen config at {}", cfg_path.display()));
            d
        }
        Err(e) => {
            fail("kitchen config is unreadable", &format!("{e}"));
            return false;
        }
    };

    let mut ok = true;

    // 2. [[tool_process]] kitchen wired + 3. [team] config_path → a loadable pack.
    match crate::connect::find_aivyx_toml(home) {
        Some(toml_path) => match std::fs::read_to_string(&toml_path)
            .ok()
            .and_then(|b| b.parse::<toml_edit::DocumentMut>().ok())
        {
            Some(doc) => {
                if crate::connect::tool_process_present(&doc, "kitchen") {
                    pass("[[tool_process]] kitchen is wired");
                } else {
                    fail(
                        "the kitchen tool process isn't wired in aivyx-pa.toml",
                        "Run `aivyx-pa connect kitchen` to wire it.",
                    );
                    ok = false;
                }
                ok &= check_team_pack(&doc, &toml_path);
            }
            None => {
                fail(
                    &format!("could not parse {}", toml_path.display()),
                    "Fix the TOML, then re-run `aivyx-pa doctor`.",
                );
                ok = false;
            }
        },
        None => {
            fail(
                "no aivyx-pa.toml found to wire the kitchen tool process into",
                "Run `aivyx-pa connect kitchen` from your config directory.",
            );
            ok = false;
        }
    }

    // 4. KitchenDB reachable (reuses the connect-kitchen probe).
    match crate::connect_kitchen::probe_kitchen_db(&db.base_url, &db.api_key, &db.organization_id)
        .await
    {
        Ok(n) => pass(&format!("KitchenDB reachable ({n} supplier(s) visible)")),
        Err(hint) => {
            fail("KitchenDB is not reachable", &hint);
            ok = false;
        }
    }
    ok
}

/// Check `[team] config_path` is set to a pack file that loads as a valid
/// `TeamConfig` (resolved against the `aivyx-pa.toml` directory).
fn check_team_pack(doc: &toml_edit::DocumentMut, toml_path: &Path) -> bool {
    let Some(rel) = doc
        .get("team")
        .and_then(|t| t.get("config_path"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        fail(
            "no [team] config_path — the kitchen brigade won't auto-load",
            "Run `aivyx-pa connect kitchen` (it plants kitchen-boh.toml and sets the pointer).",
        );
        return false;
    };
    let pack = {
        let p = Path::new(rel);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            toml_path.parent().unwrap_or(Path::new(".")).join(p)
        }
    };
    match aivyx_team::TeamConfig::load(&pack) {
        Ok(team) => {
            pass(&format!(
                "team pack `{}` loads ({} members)",
                rel,
                team.members.len()
            ));
            true
        }
        Err(e) => {
            fail(
                &format!("[team] config_path `{rel}` does not load"),
                &format!("{e}"),
            );
            false
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}

fn load_config_for_inspection() -> Result<AivyxConfig, String> {
    let opts = LoadOptions {
        toml_path: Some(Path::new(DOCTOR_TOML_PATH).to_path_buf()),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    AivyxConfig::load_from_env_and_toml(&opts)
        .map_err(|e| format!("failed to load {DOCTOR_TOML_PATH}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_shortens_long_strings() {
        assert_eq!(truncate("hello", 60), "hello");
        assert_eq!(
            truncate(&"x".repeat(100), 10),
            format!("{}…", "x".repeat(10))
        );
    }

    #[test]
    fn gatehouse_status_covers_all_six_combinations() {
        // VITRINE.md §13 — the "reveal token" doctor hint's branch logic.
        use std::net::{IpAddr, Ipv4Addr};
        let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let lan = IpAddr::V4(Ipv4Addr::new(10, 80, 80, 148));

        assert_eq!(
            gatehouse_status(Some("tok"), None),
            GatehouseStatus::TokenSet { off_host: None }
        );
        assert_eq!(
            gatehouse_status(Some("tok"), Some(loopback)),
            GatehouseStatus::TokenSet { off_host: None },
            "an explicit loopback host is still loopback, not off-host"
        );
        assert_eq!(
            gatehouse_status(Some("tok"), Some(lan)),
            GatehouseStatus::TokenSet {
                off_host: Some(lan)
            }
        );
        assert_eq!(
            gatehouse_status(None, None),
            GatehouseStatus::NoTokenLoopback
        );
        assert_eq!(
            gatehouse_status(None, Some(loopback)),
            GatehouseStatus::NoTokenLoopback
        );
        assert_eq!(
            gatehouse_status(None, Some(lan)),
            GatehouseStatus::NoTokenOffHost { host: lan }
        );
    }

    #[test]
    fn studio_command_prints_the_link_or_the_token() {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        let addr = Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7843));
        let p = Path::new("/s/studio-token");
        assert_eq!(
            studio_command_output(addr, None, false, Some("auto"), p, false).unwrap(),
            "http://127.0.0.1:7843/?token=auto"
        );
        assert_eq!(
            studio_command_output(addr, None, false, Some("auto"), p, true).unwrap(),
            "auto"
        );
        // An operator-set token wins, as in the daemon.
        assert_eq!(
            studio_command_output(addr, Some("mine"), false, Some("auto"), p, true).unwrap(),
            "mine"
        );
        // No token at all: the bare URL, and an empty token.
        assert_eq!(
            studio_command_output(addr, None, true, None, p, false).unwrap(),
            "http://127.0.0.1:7843/"
        );
        assert_eq!(studio_command_output(addr, None, true, None, p, true).unwrap(), "");
        // `web_ui_insecure_no_auth` means no token even when a stale token
        // file is still on disk — the same choice the daemon makes.
        assert_eq!(
            studio_command_output(addr, None, true, Some("stale"), p, true).unwrap(),
            ""
        );
        assert_eq!(
            studio_command_output(addr, None, true, Some("stale"), p, false).unwrap(),
            "http://127.0.0.1:7843/"
        );
    }

    #[test]
    fn repl_studio_line_mirrors_the_sign_in_link_choice() {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        let addr = Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7843));
        assert_eq!(
            repl_studio_line(addr, None, false, Some("auto"), true).as_deref(),
            Some("Studio: http://127.0.0.1:7843/?token=auto")
        );
        assert_eq!(
            repl_studio_line(addr, Some("mine"), false, Some("auto"), true).as_deref(),
            Some("Studio: http://127.0.0.1:7843/?token=mine"),
            "a configured token wins"
        );
        assert_eq!(
            repl_studio_line(addr, None, true, None, true).as_deref(),
            Some("Studio: http://127.0.0.1:7843/"),
            "insecure_no_auth: the bare URL"
        );
        assert_eq!(repl_studio_line(addr, None, false, None, true), None, "no token file yet");
        assert_eq!(repl_studio_line(None, Some("mine"), false, Some("auto"), true), None, "Studio off");
        // stdout redirected (`| tee chat.log`, `script`): never the token.
        let piped = repl_studio_line(addr, None, false, Some("auto"), false).unwrap();
        assert_eq!(piped, "Studio: http://127.0.0.1:7843/ (sign-in link: run `aivyx-pa studio`)");
        assert!(!piped.contains("auto"));
    }

    #[test]
    fn studio_command_explains_an_off_studio_and_a_missing_token() {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        let p = Path::new("/s/studio-token");
        let off = studio_command_output(None, None, false, Some("auto"), p, false).unwrap_err();
        assert!(off.contains("web_ui = false"), "{off}");
        let addr = Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7843));
        let missing = studio_command_output(addr, None, false, None, p, false).unwrap_err();
        assert!(missing.contains("aivyx-pa daemon run") && missing.contains("/s/studio-token"), "{missing}");
    }

    #[test]
    fn studio_sign_in_link_follows_the_daemons_token_choice() {
        // First-run coherence A2 — doctor shows the sign-in link for the
        // token the daemon would actually use.
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        let addr = Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7843));
        assert_eq!(
            studio_sign_in_link(addr, None, false, Some("auto")).as_deref(),
            Some("http://127.0.0.1:7843/?token=auto"),
            "the token file exists and no token is configured"
        );
        assert_eq!(
            studio_sign_in_link(addr, Some("mine"), false, Some("auto")).as_deref(),
            Some("http://127.0.0.1:7843/?token=mine"),
            "an operator token wins"
        );
        assert_eq!(studio_sign_in_link(addr, None, false, None), None, "no file yet");
        assert_eq!(
            studio_sign_in_link(addr, None, true, Some("auto")),
            None,
            "insecure_no_auth: no token at all"
        );
        assert_eq!(studio_sign_in_link(None, None, false, Some("auto")), None, "Studio off");
    }

    #[test]
    fn is_local_base_url_recognizes_local_endpoints() {
        // Chapter Engram — only local endpoints get the cheap model-pulled check.
        assert!(is_local_base_url("http://localhost:11434"));
        assert!(is_local_base_url("http://127.0.0.1:11434"));
        assert!(is_local_base_url("http://0.0.0.0:11434"));
        // Non-default-port local Ollama still recognized via the port.
        assert!(is_local_base_url("http://my-box:11434"));
        // Cloud endpoints are not local.
        assert!(!is_local_base_url("https://api.openai.com"));
    }

    fn lemonade_model(id: &str) -> aivyx_route::DiscoveredModel {
        aivyx_route::DiscoveredModel {
            id: id.to_string(),
            capabilities: Default::default(),
            unknown_capabilities: Default::default(),
            context_window: None,
        }
    }

    fn residency_with_loaded(id: &str) -> aivyx_route::ResidencySnapshot {
        let mut snap = aivyx_route::ResidencySnapshot::default();
        snap.models.insert(
            aivyx_route::ModelKey {
                endpoint: aivyx_route::EndpointRef::new("default"),
                id: id.to_string(),
            },
            aivyx_route::ModelResidency::Loaded { vram_bytes: None },
        );
        snap
    }

    #[test]
    fn lemonade_verdict_covers_unreachable_missing_and_ready() {
        use aivyx_route::DiscoveryOutcome;
        let none = aivyx_route::ResidencySnapshot::default();
        assert_eq!(
            lemonade_verdict(DiscoveryOutcome::Unreachable("refused".into()), &none, "m"),
            LemonadeVerdict::Unreachable("refused".into())
        );
        let reached = || DiscoveryOutcome::Reached(vec![lemonade_model("a"), lemonade_model("b")]);
        assert_eq!(
            lemonade_verdict(reached(), &none, "m"),
            LemonadeVerdict::ModelMissing {
                downloaded: vec!["a".into(), "b".into()]
            }
        );
        assert_eq!(
            lemonade_verdict(reached(), &none, "a"),
            LemonadeVerdict::Ready { loaded: None }
        );
        assert_eq!(
            lemonade_verdict(reached(), &residency_with_loaded("b"), "a"),
            LemonadeVerdict::Ready {
                loaded: Some("b".into())
            }
        );
    }

    #[tokio::test]
    async fn check_lemonade_fails_when_the_server_is_unreachable() {
        assert!(!check_lemonade_at("http://127.0.0.1:1/api", "some-model").await);
    }

    /// Serves one canned HTTP response per connection, for `n` connections.
    async fn serve_canned(bodies: Vec<(&'static str, String)>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for (status, body) in bodies {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn check_broker_fails_when_the_broker_is_unreachable() {
        assert!(!check_broker_at("http://127.0.0.1:1").await);
    }

    #[tokio::test]
    async fn check_broker_fails_when_its_llama_server_is_down() {
        let base = serve_canned(vec![(
            "200 OK",
            r#"{"llama_server_url":"http://127.0.0.1:1","queue_depth":0}"#.to_string(),
        )])
        .await;
        assert!(!check_broker_at(&base).await);
    }

    #[tokio::test]
    async fn check_broker_passes_when_the_broker_and_its_llama_server_answer() {
        let upstream = serve_canned(vec![("200 OK", r#"{"status":"ok"}"#.to_string())]).await;
        let base = serve_canned(vec![(
            "200 OK",
            format!(r#"{{"llama_server_url":"{upstream}","queue_depth":0}}"#),
        )])
        .await;
        assert!(check_broker_at(&base).await);
    }

    #[test]
    fn mistralrs_model_check_covers_unset_missing_file_and_dir() {
        let dir = tempfile_dir("doctor-mistralrs");
        let opts = |path: Option<PathBuf>, file: Option<&str>| aivyx_config::MistralRsOptions {
            model_path: path,
            model_file: file.map(str::to_string),
            ..Default::default()
        };
        assert!(mistralrs_model_check(&opts(None, None)).is_err());
        assert!(mistralrs_model_check(&opts(Some(dir.join("nope.gguf")), None)).is_err());
        // A directory with no GGUF in it.
        assert!(mistralrs_model_check(&opts(Some(dir.clone()), None)).is_err());
        std::fs::write(dir.join("m.gguf"), b"x").unwrap();
        assert_eq!(
            mistralrs_model_check(&opts(Some(dir.clone()), None)).unwrap(),
            dir
        );
        // A named model_file must exist inside the directory.
        assert!(mistralrs_model_check(&opts(Some(dir.clone()), Some("other.gguf"))).is_err());
        assert_eq!(
            mistralrs_model_check(&opts(Some(dir.clone()), Some("m.gguf"))).unwrap(),
            dir.join("m.gguf")
        );
        // A single file path.
        assert_eq!(
            mistralrs_model_check(&opts(Some(dir.join("m.gguf")), None)).unwrap(),
            dir.join("m.gguf")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    fn tempfile_dir(prefix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("{prefix}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn test_generation_unreachable_is_error() {
        // No Ollama → a clear Err the check turns into actionable output.
        let res = test_generation("http://127.0.0.1:1", RECOMMENDED_LOCAL_MODEL).await;
        assert!(res.is_err(), "unreachable Ollama must error");
    }

    #[tokio::test]
    async fn check_kitchen_skips_when_not_installed() {
        // A home with no kitchen config → the section is skipped + counts as OK
        // (the vertical is simply not installed).
        let home = std::env::temp_dir().join(format!("doctor-nokitchen-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        assert!(kitchen_config_path(&home).is_none());
        assert!(
            check_kitchen(&home).await,
            "absent vertical must not fail doctor"
        );
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn check_team_pack_flags_missing_and_unloadable() {
        // No [team] config_path → fail.
        let doc: toml_edit::DocumentMut = "".parse().unwrap();
        assert!(!check_team_pack(&doc, Path::new("/tmp/aivyx-pa.toml")));
        // Set but pointing at a nonexistent file → fail.
        let doc: toml_edit::DocumentMut = "[team]\nconfig_path = \"no-such-pack.toml\"\n"
            .parse()
            .unwrap();
        assert!(!check_team_pack(&doc, Path::new("/tmp/aivyx-pa.toml")));
    }
}
