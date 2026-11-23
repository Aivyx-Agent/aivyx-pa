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
//! build-feature + model-file check, and a generic OpenAI-compatible local
//! server the same check as llama.cpp. First, a `local-only` row checks that
//! every model URL is on this machine or the operator's own network (aivyx
//! is local-only). Read-only; no daemon, no passphrase.

use std::path::{Path, PathBuf};

use aivyx_config::{AivyxConfig, LoadOptions, MemoryProfile, ProviderKind};
use aivyx_instance::InstancePaths;
use aivyx_llm::ollama::{
    AUTO_NUM_CTX_CAP, DEFAULT_OLLAMA_BASE_URL, OllamaConfig, OllamaOptions, OllamaProvider,
    RECOMMENDED_LOCAL_MODEL,
};
use aivyx_llm::openai::{OpenAiConfig, OpenAiProvider};
use aivyx_llm::{LlmMessage, LlmProvider, LlmRequest, LlmStreamEvent, LlmToolDescriptor};

/// The operator's `aivyx-pa.toml` — see `aivyx_config::resolve_config_path`.
fn config_path() -> std::path::PathBuf {
    aivyx_config::resolve_config_path()
}

/// The model half of the doctor, for the end of `init`: is the provider
/// reachable and the model there? The rest (memory, service, Studio token)
/// is `aivyx-pa doctor`'s, not a first run's.
pub async fn check_provider_for_init() -> Result<bool, String> {
    let cfg = load_config_for_inspection()?;
    Ok(match cfg.provider.value {
        ProviderKind::Ollama => check_ollama(&cfg).await,
        ProviderKind::Lemonade => check_lemonade(&cfg).await,
        ProviderKind::Broker => check_broker(&cfg).await,
        ProviderKind::MistralRs => check_mistralrs(&cfg),
        local @ (ProviderKind::LlamaCpp | ProviderKind::Jan | ProviderKind::OpenAi) => {
            check_local_openai_server(local, &cfg).await
        }
    })
}

/// Nothing configured yet (no config file, no store): the checks would
/// only report defaults nobody chose, so say what to do instead — the
/// same wording bare `aivyx-pa` uses.
fn not_set_up_message(cfg: &AivyxConfig) -> Option<String> {
    let path = config_path();
    (!path.exists() && !cfg.storage_path.value.exists()).then(|| {
        format!(
            "not set up yet — there's no config at {}.\n\nRun `aivyx-pa init` to set it up.",
            path.display()
        )
    })
}

/// `aivyx-pa.toml` files in the current directory and in the installed
/// service's working directory that are no longer read (configs come from
/// `AIVYX_PA_CONFIG_PATH` or the standard path only). One line each, saying
/// how to point at the file explicitly.
fn ignored_local_config_notices(
    candidates: &[PathBuf],
    active: &std::path::Path,
    explicit: bool,
) -> Vec<String> {
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut out = Vec::new();
    for c in candidates {
        let file = c.join(aivyx_config::CONFIG_FILE_NAME);
        let canon = std::fs::canonicalize(&file).unwrap_or_else(|_| file.clone());
        let active_canon = std::fs::canonicalize(active).unwrap_or_else(|_| active.to_path_buf());
        if !file.is_file() || canon == active_canon || seen.contains(&canon) || explicit {
            continue;
        }
        seen.push(canon);
        out.push(format!(
            "{} exists but is no longer read (configs are not loaded from a working \
             directory). To use it, set {}={} (for the service: re-run `aivyx-pa daemon \
             install` from that folder), or move it to {}.",
            file.display(),
            aivyx_config::ENV_CONFIG_PATH,
            file.display(),
            active.display()
        ));
    }
    out
}

fn print_ignored_local_configs() {
    let mut dirs: Vec<PathBuf> = std::env::current_dir().into_iter().collect();
    if let Some(unit) = super::daemon_service::installed_unit_path()
        && let Ok(text) = std::fs::read_to_string(unit)
        && let Some(wd) = text.lines().find_map(|l| l.strip_prefix("WorkingDirectory="))
    {
        dirs.push(PathBuf::from(wd.trim()));
    }
    let explicit = std::env::var_os(aivyx_config::ENV_CONFIG_PATH).is_some_and(|v| !v.is_empty());
    for note in ignored_local_config_notices(&dirs, &config_path(), explicit) {
        println!("  \u{26a0} {note}");
    }
}

/// `aivyx-pa doctor` — run the health checks and report.
pub async fn run_doctor() -> Result<(), String> {
    print_ignored_local_configs();
    let (cfg, role_fix) = load_config_for_doctor()?;
    if let Some(message) = not_set_up_message(&cfg) {
        return Err(message);
    }
    println!("aivyx-pa doctor — checking your setup\n");
    let mut warnings = cfg.warnings.clone();
    if let Some(role) = &role_fix {
        warnings.insert(0, missing_start_role_warning(role));
    }
    print!("{}", config_warnings_section(&warnings, role_fix.is_none()));

    // 2.0 — every model URL must be local. The load above skipped this
    // check so a failure is reported here, as its own row.
    let urls = cfg.model_urls();
    let toml_path = config_path();
    let toml_text = std::fs::read_to_string(&toml_path).ok();
    let team_path = toml_path.clone();
    let team_text = toml_text.clone();
    let (local_section, local_ok) = tokio::task::spawn_blocking(move || {
        let team: Vec<String> = team_text
            .as_deref()
            .and_then(|t| team_pack_local_failure(t, &team_path))
            .into_iter()
            .collect();
        local_only_section(&urls, &team, &aivyx_route::SystemResolver)
    })
    .await
    .map_err(|e| format!("local-only check failed: {e}"))?;
    print!("{local_section}");
    for note in unused_cloud_key_notes(
        &cfg,
        std::env::var_os(ENV_ANTHROPIC_API_KEY).is_some_and(|v| !v.is_empty()),
        toml_text.as_deref().is_some_and(toml_has_anthropic_key),
    ) {
        println!("  note: {note}");
    }
    println!();

    // The provider and memory checks talk to those URLs, so they wait
    // until every one is local.
    let (provider_ok, memory_ok) = if local_ok {
        let provider_ok = match cfg.provider.value {
            ProviderKind::Ollama => check_ollama(&cfg).await,
            ProviderKind::Lemonade => check_lemonade(&cfg).await,
            ProviderKind::Broker => check_broker(&cfg).await,
            ProviderKind::MistralRs => check_mistralrs(&cfg),
            local @ (ProviderKind::LlamaCpp | ProviderKind::Jan | ProviderKind::OpenAi) => {
                check_local_openai_server(local, &cfg).await
            }
        };
        // Chapter Engram — semantic-memory readiness (embedding provider + profile).
        (provider_ok, check_memory(&cfg).await)
    } else {
        println!("Provider: not checked until every model URL is local.");
        (false, true)
    };

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

    // aivyx-confine's carved-home limit — informational, never fails the
    // doctor; printed only when it applies.
    if let Some(home) = std::env::var_os("HOME")
        && let Some(notice) = aivyx_core::carved_home_notice(&cfg.fs_root.value, &PathBuf::from(home))
    {
        println!("\nSandbox:");
        println!("  \u{26a0} {notice}");
    }

    // aivyx-checkpoint's refusal — informational, never fails the doctor;
    // printed only for a git repository whose checkpoints are refused.
    check_checkpoints(&cfg).await;

    println!();
    let summary = doctor_summary(all_ok, role_fix.as_deref());
    if let Ok(line) = &summary {
        println!("{line}");
    }
    summary.map(|_| ())
}

/// The doctor's last line: ready only when every check passed and the
/// config names a role pa can start in. A missing start role is blocking
/// (plain `aivyx-pa` fails to load the config), unlike the advisory config
/// warnings, so the summary says so and repeats the line to add.
fn doctor_summary(all_ok: bool, role_fix: Option<&str>) -> Result<String, String> {
    match (all_ok, role_fix) {
        (true, None) => Ok("✓ Looks good — your agent is ready. Run `aivyx-pa` to start.".into()),
        (_, Some(role)) => Err(format!(
            "not ready: aivyx-pa won't start until the config names its role. Add this line \
             under [agent] in {}: role = \"{role}\"{}",
            config_path().display(),
            if all_ok { "" } else { " — and see the failed checks above." }
        )),
        (false, None) => Err("one or more checks failed — see the notes above.".into()),
    }
}

/// The `local-only` row: ✓ when every model URL passes `require_local`,
/// otherwise one ✗ line per failing URL with its config key and reason.
/// Returns the section text and whether every URL passed. Pure apart from
/// `resolver` (blocking DNS for names that aren't local by spelling).
fn local_only_section(
    urls: &[(String, String)],
    extra_failures: &[String],
    resolver: &dyn aivyx_route::Resolve,
) -> (String, bool) {
    let mut out = String::from("Local-only:\n");
    let mut failures: Vec<String> = urls
        .iter()
        .filter_map(|(key, url)| {
            aivyx_route::require_local_with(url, resolver)
                .err()
                .map(|e| format!("{key}: {e}"))
        })
        .collect();
    failures.extend(extra_failures.iter().cloned());
    if failures.is_empty() {
        out.push_str("  \u{2713} local-only: every model URL is on this machine or your network\n");
        return (out, true);
    }
    for failure in &failures {
        out.push_str(&format!("  \u{2717} local-only: {failure}\n"));
    }
    out.push_str(&format!(
        "     \u{2192} edit {} so {} at a local server\n",
        config_path().display(),
        if failures.len() == 1 { "it points" } else { "they point" },
    ));
    (out, false)
}

/// The `[team]` pack's local-only failure, if any: the pack is loaded
/// through `aivyx_team::load_team`, which refuses a member `base_url` that
/// isn't local, and the refusal (naming the member and the reason) is
/// returned. Other load failures are `check_team_pack`'s, not this row's.
/// May do blocking DNS.
fn team_pack_local_failure(toml_text: &str, toml_path: &Path) -> Option<String> {
    let doc: toml_edit::DocumentMut = toml_text.parse().ok()?;
    let rel = ["roster_path", "config_path"].iter().find_map(|key| {
        doc.get("team")
            .and_then(|t| t.get(key))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    })?;
    let pack = {
        let p = Path::new(rel);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            toml_path.parent().unwrap_or(Path::new(".")).join(p)
        }
    };
    match aivyx_team::load_team(&pack) {
        Err(e) if e.to_string().contains("is not local") => {
            Some(format!("[team] pack `{rel}`: {e}"))
        }
        _ => None,
    }
}

/// Whether the config text itself carries `[anthropic] api_key`, whatever
/// the loader's winning source is (a bare env var outranks it).
fn toml_has_anthropic_key(toml_text: &str) -> bool {
    toml_text
        .parse::<toml_edit::DocumentMut>()
        .ok()
        .and_then(|d| {
            d.get("anthropic")
                .and_then(|a| a.get("api_key"))
                .and_then(|k| k.as_str().map(|s| !s.is_empty()))
        })
        .unwrap_or(false)
}

/// aivyx's own Anthropic key variable (the config loader's name for it).
const ENV_ANTHROPIC_API_KEY: &str = "AIVYX_PA_ANTHROPIC_API_KEY";

/// Notes for a stored cloud API key aivyx no longer uses (2.0 removed the
/// cloud providers). Informational: the key is never deleted. Covers
/// `[anthropic] api_key` in the config and `AIVYX_PA_ANTHROPIC_API_KEY`
/// (`prefixed_env_set`); a bare `ANTHROPIC_API_KEY` in the environment is
/// likely another tool's, so it is not mentioned. The OS keyring holds
/// only the master passphrase, and keys in the encrypted store need the
/// passphrase, which the doctor doesn't ask for.
fn unused_cloud_key_notes(
    cfg: &AivyxConfig,
    prefixed_env_set: bool,
    toml_has_key: bool,
) -> Vec<String> {
    let mut places: Vec<String> = Vec::new();
    // The raw TOML key counts even when an env var won the load.
    if toml_has_key
        || cfg
            .anthropic_api_key
            .as_ref()
            .is_some_and(|k| matches!(k.source, aivyx_config::FieldSource::Toml))
    {
        places.push("`[anthropic] api_key` in the config".to_string());
    }
    if prefixed_env_set {
        places.push(format!("`{ENV_ANTHROPIC_API_KEY}` in the environment"));
    }
    places
        .into_iter()
        .map(|place| {
            format!(
                "an Anthropic API key is stored ({place}) but unused — aivyx is local-only \
                 since 2.0. It is left as it is; remove it yourself if you no longer need it."
            )
        })
        .collect()
}

/// The "Config:" section: each config warning (unknown keys, deprecated
/// spellings, conflicting settings) as a warning line, or a pass line when
/// there are none. Warning-level: it never fails the doctor, matching the
/// start-up banner, which prints the same list.
/// `still_runs` is false when one of them stops pa from starting (the
/// missing start role), and the hint then doesn't claim otherwise.
fn config_warnings_section(warnings: &[String], still_runs: bool) -> String {
    let mut out = String::from("Config:\n");
    if warnings.is_empty() {
        out.push_str("  \u{2713} no unknown, deprecated or conflicting settings\n");
        return out;
    }
    for warning in warnings {
        out.push_str(&format!("  \u{26a0} {warning}\n"));
        // 2.1 — the tier's old spelling gets its own fix line.
        if warning == aivyx_config::OLD_LOCALITY_NOTE {
            out.push_str(&format!(
                "     \u{2192} change locality = \"cloud\" to \"shared\" in {}\n",
                config_path().display()
            ));
        }
    }
    // The old-spelling note already printed its own fix line; one arrow per
    // row is enough when it is the only warning.
    if warnings.iter().all(|w| w == aivyx_config::OLD_LOCALITY_NOTE) {
        out.push('\n');
        return out;
    }
    out.push_str(&format!(
        "     \u{2192} edit {} to clear {}{}\n",
        config_path().display(),
        if warnings.len() == 1 { "it" } else { "them" },
        if still_runs { "; the agent still runs" } else { "" }
    ));
    out.push('\n');
    out
}

/// The roots the agent checkpoints (`fs_root` and every `[git] repos`
/// entry) that are git repositories but whose checkpoints aivyx-checkpoint
/// refuses: the agent then has no undo or rollback there, which otherwise
/// looks exactly like a folder that isn't a repository. Informational.
async fn check_checkpoints(cfg: &AivyxConfig) {
    let mut roots: Vec<PathBuf> = vec![cfg.fs_root.value.clone()];
    if let Some(git) = &cfg.git {
        roots.extend(git.repos.iter().map(|repo| repo.value.clone()));
    }
    let mut refused: Vec<String> = Vec::new();
    for root in roots {
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        if let aivyx_core::DetectOutcome::Refused(reason) =
            aivyx_core::GitCheckpointer::detect_outcome(&root, Vec::new()).await
        {
            let line = checkpoints_refused_line(&root, &reason);
            if !refused.contains(&line) {
                refused.push(line);
            }
        }
    }
    if !refused.is_empty() {
        println!("\nCheckpoints:");
        for line in refused {
            println!("  \u{26a0} {line}");
        }
    }
}

/// A checkpoint refusal reason as it may reach a terminal: control
/// characters (escape sequences, newlines) dropped, cut to 300 characters
/// so a long list of settings stays one readable line.
pub(crate) fn shown_checkpoint_reason(reason: &str) -> String {
    const MAX_CHARS: usize = 300;
    let clean: String = reason.chars().filter(|c| !c.is_control()).collect();
    let clean = clean.trim();
    let mut detail: String = clean.chars().take(MAX_CHARS).collect();
    if clean.chars().count() > MAX_CHARS {
        detail.push('…');
    }
    detail
}

/// One doctor line for a refused repository.
fn checkpoints_refused_line(root: &Path, reason: &str) -> String {
    format!(
        "checkpoints are off for {}, so agent changes there can't be undone or rolled back: \
         {}",
        root.display(),
        shown_checkpoint_reason(reason)
    )
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
/// `[studio] auth_token` edit in `aivyx-pa.toml`, or a future
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

/// Where the Studio really is, from what the daemon recorded
/// ([`aivyx_channel::studio_token::StudioState`]) rather than from the
/// config alone: a Studio that failed to bind (its port taken) or whose
/// daemon isn't running gets no address, hence no link. `cfg_addr` is `None`
/// when the config turns the Studio off.
pub fn studio_reach(
    cfg_addr: Option<std::net::SocketAddr>,
    state: &aivyx_channel::studio_token::StudioState,
) -> Result<std::net::SocketAddr, String> {
    use aivyx_channel::studio_token::{not_running_message, StudioState};
    cfg_addr.ok_or_else(|| "the Studio is off (`[studio] enabled = false`).".to_string())?;
    match state {
        StudioState::Bound(addr) => Ok(*addr),
        StudioState::Failed(port) => Err(not_running_message(*port)),
        StudioState::Unknown => Err("The Studio isn't running: the daemon isn't up. Run \
             `aivyx-pa` (it starts the daemon when it can get your passphrase) or \
             `aivyx-pa daemon run`, then `aivyx-pa studio` again."
            .to_string()),
    }
}

/// The REPL banner's Studio line for a daemon's recorded state: the link
/// for a Studio that bound, the plain failure sentence for one that did
/// not, nothing when there is no record (the Studio is off, or the daemon
/// has not got that far).
pub fn repl_studio_banner(
    state: &aivyx_channel::studio_token::StudioState,
    configured_token: Option<&str>,
    insecure_no_auth: bool,
    auto_token: Option<&str>,
    interactive: bool,
) -> Option<String> {
    use aivyx_channel::studio_token::{not_running_message, StudioState};
    match state {
        StudioState::Bound(addr) => repl_studio_line(
            Some(*addr),
            configured_token,
            insecure_no_auth,
            auto_token,
            interactive,
        ),
        StudioState::Failed(port) => Some(not_running_message(*port)),
        StudioState::Unknown => None,
    }
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
    let addr = addr.ok_or_else(|| "the Studio is off (`[studio] enabled = false`).".to_string())?;
    if configured_token.is_none() && !insecure_no_auth && auto_token.is_none() {
        return Err(format!(
            "the Studio isn't running yet — its daemon hasn't started. Run `aivyx-pa` \
             (it starts the daemon when it can get your passphrase) or `aivyx-pa daemon run`, \
             then `aivyx-pa studio` again. (The daemon creates the sign-in token at {}.)",
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

/// The recorded Studio state of the current instance's daemon.
fn studio_state_for_cli() -> aivyx_channel::studio_token::StudioState {
    match aivyx_channel::daemon_ipc::default_socket_path() {
        Ok(sp) => aivyx_channel::studio_token::read_status(&sp),
        Err(_) => aivyx_channel::studio_token::StudioState::Unknown,
    }
}

/// `aivyx-pa studio [--token]`: print the Studio sign-in link (or, with
/// `--token`, just the token) for this config. Reads the `0600` token file
/// as the operator; never creates it (the daemon does). No daemon needed.
pub fn run_studio(token_only: bool) -> Result<(), String> {
    let cfg = load_config_for_inspection()?;
    if let Some(message) = not_set_up_message(&cfg) {
        return Err(message);
    }
    let token_path = aivyx_channel::studio_token::token_path(&cfg.storage_path.value);
    let auto_token = aivyx_channel::studio_token::read_existing(&token_path);
    // The link only for a Studio that really bound; `--token` stays a pure
    // read of the token (the desktop app and scripts use it).
    let addr = if token_only {
        cfg.studio_addr()
    } else {
        let state = studio_state_for_cli();
        Some(studio_reach(cfg.studio_addr(), &state)?)
    };
    let out = studio_command_output(
        addr,
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
    println!("\nStudio (web UI):");
    let Some(addr) = cfg.studio_addr() else {
        println!("  • the Studio is off (`[studio] enabled = false`).");
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
                "  ⚠ bound off-host ({host}) with no `[studio] auth_token` set\n     \
                 → anyone who can reach this host can drive the agent; set a token \
                 (e.g. `openssl rand -hex 32`) in aivyx-pa.toml, or \
                 `[studio] insecure_no_auth = true` if a reverse proxy already \
                 authenticates. See https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/GATEHOUSE.md"
            );
        }
        GatehouseStatus::NoTokenLoopback if cfg.web_ui_insecure_no_auth => {
            println!(
                "  ⚠ no auth token (`[studio] insecure_no_auth = true`) — any account \
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
    // The link only for a Studio that really bound, at the address it is on.
    let bound_addr = match studio_reach(Some(addr), &studio_state_for_cli()) {
        Ok(a) => a,
        Err(msg) => {
            println!("  ⚠ {msg}");
            return;
        }
    };
    if let Some(link) = studio_sign_in_link(
        Some(bound_addr),
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
                     size the model up and raise `[ollama] num_ctx`: see https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/LOCAL_HOSTING.md"
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
    reply_from(&provider, model, false).await
}

/// Onboarding audit 3 (L6) — one real reply from a local OpenAI-compatible
/// server (llama.cpp, Jan, Lemonade, Docker Model Runner) through the
/// provider a turn uses, so "ready" means the model answers, not only that
/// the server is up. Asked to answer without reasoning first, so a thinking
/// model's reply fits the small budget.
async fn openai_test_generation(
    base_url: &str,
    model: &str,
    api_key: Option<&str>,
) -> Result<String, String> {
    let config = match api_key {
        Some(key) => OpenAiConfig::new(key.to_string()),
        None => OpenAiConfig::without_api_key(),
    }
    .with_base_url(base_url.to_string());
    let provider = OpenAiProvider::new(config).map_err(|e| format!("build provider: {e}"))?;
    reply_from(&provider, model, true).await
}

/// Report an OpenAI-compatible server's test reply; true when it answered.
fn report_test_reply(reply: Result<String, String>) -> bool {
    match reply {
        Ok(text) if !text.trim().is_empty() => {
            pass(&format!("the model answers: \"{}\"", truncate(&text, 60)));
            true
        }
        Ok(_) => {
            fail(
                "the model returned an EMPTY reply",
                "The server is up but the model said nothing: try another model, or see \
                 the server's log.",
            );
            false
        }
        Err(e) => {
            fail(
                &format!("the model didn't answer: {e}"),
                "The server is up but the model didn't reply: check that it loads (the \
                 server's log says why).",
            );
            false
        }
    }
}

/// The test request every check sends: one short user message, with a
/// dummy tool so the request has a real turn's shape.
async fn reply_from(
    provider: &dyn LlmProvider,
    model: &str,
    no_thinking: bool,
) -> Result<String, String> {
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
        no_thinking,
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
    let Some(client) = crate::routing::discovery_client() else {
        fail(
            "couldn't build the local-only client to reach Lemonade Server",
            "This is a TLS-init failure on this machine; see the error above.",
        );
        return false;
    };
    let endpoint = aivyx_route::EndpointRef::new("default");
    let config = crate::routing::operator_backend(
        aivyx_route::EndpointKind::Lemonade,
        base_url.to_string(),
    );
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
                None => println!("  no model loaded yet — loading `{model}` for a test reply"),
            }
            report_test_reply(openai_test_generation(base_url, model, None).await)
        }
    }
}

/// How long doctor waits on a local server before calling it unreachable.
const LOCAL_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Why a local server's model list couldn't be read.
#[derive(Debug, PartialEq, Eq)]
enum ProbeError {
    /// 401/403: the server was started with an API key.
    Unauthorized,
    Other(String),
}

/// What doctor concluded about a local OpenAI-compatible server
/// (llama.cpp's `llama-server`, Jan), split out as pure logic.
#[derive(Debug, PartialEq, Eq)]
enum LocalServerVerdict {
    Unreachable(String),
    NeedsKey,
    /// Reachable, but it doesn't serve the configured model.
    ModelMissing { served: Vec<String> },
    Ready { note: Option<String> },
}

fn local_server_verdict(
    provider: ProviderKind,
    models: Result<Vec<String>, ProbeError>,
    model: &str,
) -> LocalServerVerdict {
    let served = match models {
        Ok(served) => served,
        Err(ProbeError::Unauthorized) => return LocalServerVerdict::NeedsKey,
        Err(ProbeError::Other(why)) => return LocalServerVerdict::Unreachable(why),
    };
    if served.is_empty() || served.iter().any(|m| m == model) {
        return LocalServerVerdict::Ready { note: None };
    }
    match provider {
        // llama-server serves the one model it was started with, whatever
        // name a request carries — `[agent] model` is just a label there.
        ProviderKind::LlamaCpp => LocalServerVerdict::Ready {
            note: Some(format!("it serves `{}` (requests use that, whatever the name)", served.join("`, `"))),
        },
        _ => LocalServerVerdict::ModelMissing { served },
    }
}

/// llama.cpp / Jan / an OpenAI-compatible local server: no API key is
/// needed unless it was started with one — check it answers and serves the
/// model instead. (`openai` always has a `base_url`: the config requires it.)
async fn check_local_openai_server(provider: ProviderKind, cfg: &AivyxConfig) -> bool {
    let default = match provider {
        ProviderKind::Jan => crate::init::DEFAULT_JAN_BASE_URL,
        _ => aivyx_config::DEFAULT_LLAMACPP_BASE_URL,
    };
    let base_url = cfg
        .openai_base_url
        .as_ref()
        .map(|s| s.value.clone())
        .unwrap_or_else(|| default.to_string());
    let model = cfg.model.value.clone();
    println!("Provider: {provider} (model `{model}`, {base_url})\n");
    let key = cfg
        .openai_api_key
        .as_ref()
        .map(|k| secrecy::ExposeSecret::expose_secret(&k.value).to_string());
    check_local_openai_server_at(provider, &base_url, &model, key.as_deref()).await
}

async fn check_local_openai_server_at(
    provider: ProviderKind,
    base_url: &str,
    model: &str,
    api_key: Option<&str>,
) -> bool {
    let name = match provider {
        ProviderKind::Jan => "Jan",
        ProviderKind::OpenAi => "OpenAI-compatible server",
        _ => "llama.cpp server",
    };
    // Same joining the OpenAI provider uses for chat: `{base}/v1/...`.
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    let models: Result<Vec<String>, ProbeError> = async {
        let client = aivyx_llm::local_only::model_client_builder()
            .build()
            .map_err(|e| ProbeError::Other(format!("couldn't build the local-only client: {e}")))?;
        let mut request = client.get(&url).timeout(LOCAL_PROBE_TIMEOUT);
        if let Some(key) = api_key {
            request = request.bearer_auth(key);
        }
        let response = request.send().await.map_err(|e| ProbeError::Other(e.to_string()))?;
        if matches!(response.status().as_u16(), 401 | 403) {
            return Err(ProbeError::Unauthorized);
        }
        let body: serde_json::Value = response
            .error_for_status()
            .map_err(|e| ProbeError::Other(e.to_string()))?
            .json()
            .await
            .map_err(|e| ProbeError::Other(e.to_string()))?;
        Ok(body["data"]
            .as_array()
            .map(|a| a.iter().filter_map(|m| m["id"].as_str().map(str::to_string)).collect())
            .unwrap_or_default())
    }
    .await;
    match local_server_verdict(provider, models, model) {
        LocalServerVerdict::Unreachable(why) => {
            fail(
                &format!("{name} is not reachable at {base_url} ({why})"),
                "Start it, or set `[openai] base_url` to where it listens.",
            );
            false
        }
        LocalServerVerdict::NeedsKey => {
            fail(
                &format!("{name} at {base_url} asks for an API key"),
                "It was started with one: put the same key in `[openai] api_key` \
                 (or AIVYX_PA_OPENAI_API_KEY).",
            );
            false
        }
        LocalServerVerdict::ModelMissing { served } => {
            fail(
                &format!("{name} doesn't serve model `{model}` (serves: {})", served.join(", ")),
                "Set `[agent] model` to one of those, or load it in the server.",
            );
            false
        }
        LocalServerVerdict::Ready { note } => {
            pass(&format!("{name} is running"));
            if api_key.is_none() {
                println!("  no API key needed — it runs on your own machine or network");
            }
            if let Some(note) = note {
                println!("  {note}");
            }
            report_test_reply(openai_test_generation(base_url, model, api_key).await)
        }
    }
}

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
    let client = match aivyx_llm::local_only::model_client_builder().build() {
        Ok(c) => c,
        Err(e) => {
            fail(
                &format!("couldn't build the local-only client to reach aivyx-broker ({e})"),
                "This is a TLS-init failure on this machine; see the error above.",
            );
            return false;
        }
    };
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
                 {model}, or another local embedding server) and set `profile = smart` for semantic recall.",
                model = crate::init::RECOMMENDED_EMBED_MODEL
            );
        } else if matches!(cfg.memory_profile, MemoryProfile::Smart) {
            println!(
                "  ⚠ `[memory] profile = smart` is set but there is no \
                 [embedding] provider, so semantic recall is inert.\n     → add \
                 an [embedding] section (a local Ollama running {model}, or \
                 another local embedding server), or use `profile = lite` for embedding-free recall.",
                model = crate::init::RECOMMENDED_EMBED_MODEL
            );
        } else {
            println!(
                "  memory recall is off (no [embedding] provider, no [memory] profile)\n     → \
                 set `profile = \"lite\"` under [memory] for keyword recall with no setup, \
                 or add an [embedding] section (a local Ollama running {model}, or another \
                 local embedding server) for recall by meaning.",
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
        // A local server elsewhere on the network (the local-only row passed):
        // not probed here, just confirm it's wired.
        pass("embedding provider configured (on your network; not probed)");
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

/// `~/.aivyx-pa/tool-processes/kitchen/config.toml` (or instance-specific path) —
/// the kitchen vertical's presence marker. `Some(path)` iff the file exists (the vertical is installed).
fn kitchen_config_path(home: &Path) -> Option<PathBuf> {
    // Use home-based paths for tests (when home is a temp dir), otherwise use instance
    let p = InstancePaths::with_home(home)
        .ok()?
        .tool_process_dir("kitchen")?
        .join("config.toml");
    p.exists().then_some(p)
}

/// Chapter Mise — the kitchen vertical health section. Returns `true` when the
/// vertical is **not installed** (skipped silently) or when every check passes;
/// `false` if installed-but-broken. Checks: config parses → `[[tool_process]]`
/// wired → `[team] roster_path` points at a loadable pack → KitchenDB reachable.
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

    // 2. [[tool_process]] kitchen wired + 3. [team] roster_path → a loadable pack.
    match crate::connect::find_aivyx_toml() {
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

/// Check `[team] roster_path` (or its deprecated alias `config_path`, R22)
/// is set to a pack file that loads as a valid `TeamConfig` (resolved
/// against the `aivyx-pa.toml` directory). The new spelling wins, as in the
/// loader.
fn check_team_pack(doc: &toml_edit::DocumentMut, toml_path: &Path) -> bool {
    let Some(rel) = ["roster_path", "config_path"].iter().find_map(|key| {
        doc.get("team")
            .and_then(|t| t.get(key))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }) else {
        fail(
            "no [team] roster_path — the kitchen brigade won't auto-load",
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
    match aivyx_team::load_team(&pack) {
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
    load_config_with_role(None)
}

fn load_config_with_role(role_override: Option<String>) -> Result<AivyxConfig, String> {
    AivyxConfig::load_from_env_and_toml(&inspection_opts(role_override))
        .map_err(|e| format!("failed to load {}: {e}", config_path().display()))
}

/// The doctor's own load: without the local-only check, which the doctor
/// reports as its `local-only` row instead of failing to load.
fn load_for_doctor_with_role(role_override: Option<String>) -> Result<AivyxConfig, String> {
    AivyxConfig::load_for_diagnostics(&inspection_opts(role_override))
        .map_err(|e| format!("failed to load {}: {e}", config_path().display()))
}

fn inspection_opts(role_override: Option<String>) -> LoadOptions {
    LoadOptions {
        toml_path: Some(config_path()),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override,
    }
}

/// The doctor's load. A config made from a template before `init` wrote
/// `[agent] role` defines one role that isn't `default`, and nothing picks
/// it, so it doesn't load. Then the doctor loads it with that role, to
/// check the rest, and returns the role so the Config section can name the
/// line to add.
fn load_config_for_doctor() -> Result<(AivyxConfig, Option<String>), String> {
    match AivyxConfig::load_for_diagnostics(&inspection_opts(None)) {
        Ok(cfg) => Ok((cfg, None)),
        // Set means non-empty, the loader's rule for every env var.
        Err(e) => match start_role_fix(
            &e,
            std::env::var("AIVYX_PA_ROLE").is_ok_and(|v| !v.is_empty()),
        ) {
            Some(role) => Ok((load_for_doctor_with_role(Some(role.clone()))?, Some(role))),
            None => Err(format!("failed to load {}: {e}", config_path().display())),
        },
    }
}

/// The role to name in `[agent] role` when `err` is "the default role
/// isn't defined" and exactly one role is, with no `AIVYX_PA_ROLE` set.
/// Pure.
fn start_role_fix(err: &aivyx_config::ConfigError, role_env_set: bool) -> Option<String> {
    match err {
        aivyx_config::ConfigError::UnknownRole { name, known }
            if !role_env_set && name == aivyx_config::DEFAULT_ROLE_NAME && known.len() == 1 =>
        {
            Some(known[0].clone())
        }
        _ => None,
    }
}

/// The Config-section warning for [`start_role_fix`], with the exact line.
fn missing_start_role_warning(role: &str) -> String {
    format!(
        "no role is chosen and this config has no `default` role, so aivyx-pa won't start \
         without `--role {role}`. Add this line under [agent]: role = \"{role}\""
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Task 5c part 3: a config with one role that isn't `default`, and
    /// nothing choosing it, gets a fix naming the exact line; any other
    /// unknown-role error (two roles, `AIVYX_PA_ROLE` set) does not.
    #[test]
    fn a_config_with_one_non_default_role_gets_the_line_to_add() {
        let err = |name: &str, known: &[&str]| aivyx_config::ConfigError::UnknownRole {
            name: name.to_string(),
            known: known.iter().map(|k| k.to_string()).collect(),
        };
        assert_eq!(start_role_fix(&err("default", &["coder"]), false).as_deref(), Some("coder"));
        assert_eq!(start_role_fix(&err("default", &["coder"]), true), None);
        assert_eq!(start_role_fix(&err("default", &["a", "b"]), false), None);
        assert_eq!(start_role_fix(&err("typo", &["coder"]), false), None);
        let line = missing_start_role_warning("coder");
        assert!(line.ends_with("Add this line under [agent]: role = \"coder\""), "{line}");
    }

    /// Re-review fix: a missing start role is blocking — the summary is
    /// never "ready" with it, and repeats the line to add.
    #[test]
    fn a_missing_start_role_means_not_ready() {
        assert!(doctor_summary(true, None).unwrap().contains("ready"));
        for all_ok in [true, false] {
            let err = doctor_summary(all_ok, Some("coder")).unwrap_err();
            assert!(err.contains("not ready") && err.contains("role = \"coder\""), "{err}");
        }
        assert!(doctor_summary(false, None).unwrap_err().contains("checks failed"));
    }

    /// Task 5c item 10: doctor lists every config warning, warning-level.
    #[test]
    fn doctor_lists_config_warnings() {
        let none = config_warnings_section(&[], true);
        assert!(none.starts_with("Config:\n  \u{2713}"), "{none}");
        let warnings = vec![
            "unknown config key `ollama.base_url`".to_string(),
            "`daemon.web_ui_port` is deprecated; use `studio.port`".to_string(),
        ];
        let out = config_warnings_section(&warnings, true);
        for w in &warnings {
            assert!(out.contains(&format!("  \u{26a0} {w}\n")), "{out}");
        }
        assert!(out.contains("to clear them; the agent still runs"), "{out}");
        let blocked = config_warnings_section(&warnings[..1], false);
        assert!(blocked.contains("to clear it\n"), "{blocked}");
        assert!(!out.contains('\u{2717}'), "a config warning never fails the doctor: {out}");
    }

    /// 2.1 — the old tier spelling is a warning row with its own fix.
    #[test]
    fn the_old_locality_note_is_a_warning_row_with_its_fix() {
        let warnings = vec![aivyx_config::OLD_LOCALITY_NOTE.to_string()];
        let out = config_warnings_section(&warnings, true);
        assert!(
            out.contains(
                "  \u{26a0} locality = \"cloud\" is now \"shared\"; the old name works until 3.0\n"
            ),
            "{out}"
        );
        assert!(
            out.contains(&format!(
                "     \u{2192} change locality = \"cloud\" to \"shared\" in {}\n",
                config_path().display()
            )),
            "{out}"
        );
        assert!(!out.contains('\u{2717}'), "{out}");
    }

    #[tokio::test]
    async fn a_refused_repository_gets_a_checkpoints_line() {
        let dir = tempfile::tempdir().unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "core.sshCommand", "SECRET-VALUE"],
        ] {
            let status = std::process::Command::new("git")
                .args(&args)
                .current_dir(dir.path())
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        }
        let reason =
            match aivyx_core::GitCheckpointer::detect_outcome(dir.path(), Vec::new()).await {
                aivyx_core::DetectOutcome::Refused(reason) => reason,
                _ => panic!("expected the repository to be refused"),
            };
        let line = checkpoints_refused_line(dir.path(), &reason);
        assert!(line.contains("core.sshcommand"), "{line}");
        assert!(line.contains("undone"), "{line}");
        assert!(!line.contains("SECRET-VALUE"), "{line}");
    }

    #[test]
    fn a_checkpoint_reason_is_stripped_of_control_characters_and_cut() {
        let line = checkpoints_refused_line(
            Path::new("/r"),
            &format!("bad\u{1b}[2K\nsetting {}", "x".repeat(1000)),
        );
        assert!(!line.contains('\u{1b}') && !line.contains('\n'), "{line:?}");
        assert!(line.ends_with('…'), "{line}");
        assert!(line.chars().count() < 450, "{}", line.chars().count());
    }

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
        assert!(off.contains("[studio] enabled = false"), "{off}");
        let addr = Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7843));
        let missing = studio_command_output(addr, None, false, None, p, false).unwrap_err();
        assert!(missing.contains("aivyx-pa daemon run") && missing.contains("/s/studio-token"), "{missing}");
    }

    #[test]
    fn studio_reach_and_banner_follow_what_the_daemon_recorded() {
        use aivyx_channel::studio_token::StudioState;
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        let cfg = Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7843));
        let bound = StudioState::Bound(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9000));
        // Bound: the link is for the port it really bound, not the config's.
        assert_eq!(studio_reach(cfg, &bound).unwrap().port(), 9000);
        assert_eq!(
            repl_studio_banner(&bound, None, false, Some("auto"), true).as_deref(),
            Some("Studio: http://127.0.0.1:9000/?token=auto")
        );
        // Failed: the plain sentence, never a link or a token.
        let failed = StudioState::Failed(7843);
        let err = studio_reach(cfg, &failed).unwrap_err();
        assert!(err.starts_with("The Studio isn't running: port 7843 is in use"), "{err}");
        assert!(err.contains("`[studio] port`") && err.contains("`--studio-port`"), "{err}");
        let line = repl_studio_banner(&failed, None, false, Some("auto"), true).unwrap();
        assert_eq!(line, err);
        assert!(!line.contains("token") && !line.contains("http"), "{line}");
        // Unknown: no daemon record, no link, no banner line.
        assert!(studio_reach(cfg, &StudioState::Unknown).unwrap_err().contains("daemon isn't up"));
        assert_eq!(repl_studio_banner(&StudioState::Unknown, None, false, Some("auto"), true), None);
        // Off in the config.
        assert!(studio_reach(None, &bound).unwrap_err().contains("[studio] enabled = false"));
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

    /// A stub resolver: `gpu.lan` needs no lookup; `pub.example.com`
    /// answers a public address; anything else doesn't resolve.
    struct Stub;

    impl aivyx_route::Resolve for Stub {
        fn resolve(&self, host: &str, _port: u16) -> std::io::Result<Vec<std::net::IpAddr>> {
            match host {
                "pub.example.com" => Ok(vec!["93.184.216.34".parse().unwrap()]),
                _ => Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no such host")),
            }
        }
    }

    fn urls(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, u)| (k.to_string(), u.to_string())).collect()
    }

    #[test]
    fn the_local_only_row_passes_when_every_url_is_local() {
        let (out, ok) = local_only_section(
            &urls(&[
                ("[openai] base_url", "http://127.0.0.1:8080"),
                ("[embedding] base_url", "http://gpu.lan:11434"),
                ("[routing.endpoints.box] base_url", "http://10.0.0.7:8000"),
            ]),
            &[],
            &Stub,
        );
        assert!(ok);
        assert_eq!(
            out,
            "Local-only:\n  \u{2713} local-only: every model URL is on this machine or your network\n"
        );
        // No URLs at all (every provider at its built-in local default).
        assert!(local_only_section(&[], &[], &Stub).1);
    }

    #[test]
    fn the_local_only_row_names_each_failing_url_with_its_reason() {
        let (out, ok) = local_only_section(
            &urls(&[
                ("[openai] base_url", "https://api.openai.com"),
                ("[broker] base_url", "http://127.0.0.1:8899"),
                ("[embedding] base_url", "http://pub.example.com:11434"),
                ("[routing.endpoints.x] base_url", "http://nowhere.example:1"),
            ]),
            &[],
            &Stub,
        );
        assert!(!ok);
        let tail = " — aivyx is local-only; point it at a model server on this machine or your own network";
        for line in [
            format!("  \u{2717} local-only: [openai] base_url: https://api.openai.com/ is not local (cloud host){tail}\n"),
            format!(
                "  \u{2717} local-only: [embedding] base_url: http://pub.example.com:11434/ is not local \
                 (resolves to a public address 93.184.216.34){tail}\n"
            ),
            format!(
                "  \u{2717} local-only: [routing.endpoints.x] base_url: http://nowhere.example:1/ is not \
                 local (doesn't resolve){tail}\n"
            ),
        ] {
            assert!(out.contains(&line), "missing {line:?} in\n{out}");
        }
        assert!(!out.contains("[broker]"), "a local URL is not listed: {out}");
        assert_eq!(out.matches('\u{2717}').count(), 3);
        assert!(out.contains("so they point at a local server"), "{out}");
    }

    /// A config loaded from a minimal TOML (no `[anthropic]`, so the key
    /// is set by hand below).
    fn cfg_with_anthropic_key(source: aivyx_config::FieldSource) -> AivyxConfig {
        let path = std::env::temp_dir().join(format!(
            "aivyx-doctor-key-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, "[agent]\nprovider = \"llamacpp\"\nmodel = \"m\"\n").unwrap();
        let mut opts = inspection_opts(None);
        opts.toml_path = Some(path.clone());
        let mut cfg = AivyxConfig::load_for_diagnostics(&opts).unwrap();
        let _ = std::fs::remove_file(&path);
        cfg.anthropic_api_key = Some(aivyx_config::SourcedSecret::new(
            secrecy::SecretString::from("sk-ant-x".to_string()),
            source,
        ));
        cfg
    }

    #[test]
    fn a_stored_anthropic_key_gets_a_note_and_is_never_shown() {
        use aivyx_config::FieldSource;
        let notes = unused_cloud_key_notes(&cfg_with_anthropic_key(FieldSource::Toml), false, false);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("`[anthropic] api_key` in the config"), "{}", notes[0]);
        assert!(notes[0].contains("unused"), "{}", notes[0]);
        assert!(notes[0].contains("left as it is"), "{}", notes[0]);
        assert!(!notes[0].contains("sk-ant-x"), "the key itself is never printed");

        let notes = unused_cloud_key_notes(&cfg_with_anthropic_key(FieldSource::Env), true, false);
        assert!(notes[0].contains("`AIVYX_PA_ANTHROPIC_API_KEY` in the environment"));
        // A bare ANTHROPIC_API_KEY is likely another tool's: no note.
        assert!(unused_cloud_key_notes(&cfg_with_anthropic_key(FieldSource::Env), false, false).is_empty());
        // A bare env var won the load, but the TOML still holds a key: noted,
        // and the key is never printed.
        let notes = unused_cloud_key_notes(&cfg_with_anthropic_key(FieldSource::Env), false, true);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("`[anthropic] api_key` in the config"), "{}", notes[0]);
        assert!(!notes[0].contains("sk-ant-x"));
        assert!(toml_has_anthropic_key("[anthropic]\napi_key = \"sk-ant-x\"\n"));
        assert!(!toml_has_anthropic_key("[agent]\nmodel = \"m\"\n"));
        // No key: no note.
        let mut cfg = cfg_with_anthropic_key(FieldSource::Toml);
        cfg.anthropic_api_key = None;
        assert!(unused_cloud_key_notes(&cfg, false, false).is_empty());
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

    #[test]
    fn a_local_openai_server_needs_no_api_key() {
        use LocalServerVerdict::*;
        let models = |ids: &[&str]| Ok(ids.iter().map(|s| s.to_string()).collect());
        assert_eq!(
            local_server_verdict(ProviderKind::LlamaCpp, models(&["qwen.gguf"]), "qwen.gguf"),
            Ready { note: None }
        );
        // llama-server answers whatever model name it's sent.
        assert!(matches!(
            local_server_verdict(ProviderKind::LlamaCpp, models(&["qwen.gguf"]), "other"),
            Ready { note: Some(n) } if n.contains("qwen.gguf")
        ));
        // Jan serves the model it's asked for, so a missing one fails.
        assert!(matches!(
            local_server_verdict(ProviderKind::Jan, models(&["a"]), "b"),
            ModelMissing { served } if served == vec!["a".to_string()]
        ));
        assert_eq!(
            local_server_verdict(ProviderKind::LlamaCpp, Err(ProbeError::Unauthorized), "m"),
            NeedsKey
        );
        assert!(matches!(
            local_server_verdict(ProviderKind::Jan, Err(ProbeError::Other("refused".into())), "m"),
            Unreachable(w) if w == "refused"
        ));
    }

    #[tokio::test]
    async fn check_local_openai_server_fails_when_unreachable() {
        assert!(!check_local_openai_server_at(ProviderKind::LlamaCpp, "http://127.0.0.1:1", "m", None).await);
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

    /// Onboarding audit 3 (L6): a server that lists the model but whose
    /// model doesn't answer is not "ready"; one that answers is.
    #[tokio::test]
    async fn a_local_server_is_ready_only_when_its_model_answers() {
        let models = r#"{"data":[{"id":"m"}]}"#.to_string();
        let sse = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"}}]}\n\n\
                   data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
                   data: [DONE]\n\n"
            .to_string();
        let answers = serve_canned(vec![("200 OK", models.clone()), ("200 OK", sse)]).await;
        assert!(check_local_openai_server_at(ProviderKind::LlamaCpp, &answers, "m", None).await);

        let broken = serve_canned(vec![
            ("200 OK", models),
            ("500 Internal Server Error", r#"{"error":"failed to load model"}"#.to_string()),
        ])
        .await;
        assert!(!check_local_openai_server_at(ProviderKind::LlamaCpp, &broken, "m", None).await);
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

    #[test]
    fn a_cloud_team_member_fails_the_local_only_row() {
        let dir = std::env::temp_dir().join(format!("doctor-teamlocal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let member = |url: &str| {
            format!(
                "[team]\nname = \"t\"\nlead = \"chef\"\n\n[[team.member]]\nname = \"chef\"\n\
                 role = \"R\"\nsoul = \"s\"\ntrust_ceiling = \"Trusted\"\nbase_url = \"{url}\"\n"
            )
        };
        let cfg_path = dir.join("aivyx-pa.toml");
        let cfg_text = "[team]\nroster_path = \"pack.toml\"\n";
        std::fs::write(dir.join("pack.toml"), member("https://api.openai.com/v1")).unwrap();
        let failure = team_pack_local_failure(cfg_text, &cfg_path).expect("cloud member refused");
        assert!(failure.contains("chef"), "{failure}");
        assert!(failure.contains("cloud host"), "{failure}");
        let (out, ok) = local_only_section(&[], &[failure], &Stub);
        assert!(!ok);
        assert!(out.contains("\u{2717} local-only: [team] pack"), "{out}");
        std::fs::write(dir.join("pack.toml"), member("http://127.0.0.1:8080")).unwrap();
        assert!(team_pack_local_failure(cfg_text, &cfg_path).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_config_in_a_working_directory_is_reported_as_no_longer_read() {
        let dir = std::env::temp_dir().join(format!("doctor-localcfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let active = dir.join("standard/aivyx-pa.toml");
        assert!(ignored_local_config_notices(std::slice::from_ref(&dir), &active, false).is_empty());
        std::fs::write(dir.join("aivyx-pa.toml"), "").unwrap();
        let notes = ignored_local_config_notices(&[dir.clone(), dir.clone()], &active, false);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("AIVYX_PA_CONFIG_PATH"), "{notes:?}");
        // The file that IS the active config, or an explicit override, is silent.
        assert!(ignored_local_config_notices(std::slice::from_ref(&dir), &dir.join("aivyx-pa.toml"), false).is_empty());
        assert!(ignored_local_config_notices(std::slice::from_ref(&dir), &active, true).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
