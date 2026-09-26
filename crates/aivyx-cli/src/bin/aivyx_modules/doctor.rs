//! `aivyx-pa doctor` — first-run health check for the local on-ramp (Chapter P).
//!
//! Confirms the path actually works end to end and, when it doesn't, says
//! *why* and *what to do* — instead of leaving a new user with a blank first
//! turn. For the local (Ollama) provider it checks: Ollama reachable → the
//! configured model present → a real test generation that returns **non-empty**
//! text (the exact failure modes — empty thinking content, dropped tool calls,
//! starved `num_ctx` — that ruined first impressions). Cloud providers get a
//! lighter config-presence check. Read-only; no daemon, no passphrase.

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
    /// No token, loopback-only — the default, fine posture.
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

fn check_gatehouse(cfg: &AivyxConfig) {
    println!("\nWeb UI (Gatehouse):");
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
        GatehouseStatus::NoTokenLoopback => {
            println!(
                "  • no auth token set — fine for loopback-only use.\n     \
                 → to expose the Studio off-host, set both `web_ui_host` and \
                 `web_ui_auth_token` in aivyx-pa.toml (see docs/GATEHOUSE.md)."
            );
        }
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
