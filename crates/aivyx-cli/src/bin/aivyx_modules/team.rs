//! `aivyx-pa team` CLI — Chapter J (the Nonagon).
//!
//! Two subcommands:
//!
//! - `aivyx-pa team roster` — render the default Nonagon (the 9 roles + their
//!   scopes/trust). Offline: pure rendering, no provider, no storage.
//! - `aivyx-pa team run "<mission>"` — assemble the team in-process and hand the
//!   mission to the **lead** agent. The lead drives via its orchestration
//!   tools (`decompose_task` → delegate → `verify`/`synthesize`); every
//!   specialist sub-turn is built by the [`SpecialistPool`] over the **same
//!   `AuditHook`**, so the whole run lands on the one HMAC chain. Wired from
//!   `run_async` (which owns the live provider + persistent audit).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use aivyx_capability::{CapabilitySet, Scope, TrustTier};
use aivyx_core::{
    Agent, AgentId, AuditHook, CancellationToken, ChannelContext, ChannelError, ChannelPlatform,
    ConcreteAgent, LlmPlanner, LlmPlannerConfig, Message, SessionId, StreamEvent, Tool,
    ToolRegistry, TurnOutcome, TurnSafety,
};
use aivyx_llm::LlmProvider;
use aivyx_team::{TeamAssembly, TeamConfig, default_nonagon};
use async_trait::async_trait;

/// Render the team roster as an operator-readable block. Pure — the unit of
/// `aivyx-pa team roster`.
pub fn render_roster(config: &TeamConfig) -> String {
    let specialists = config.specialists().count();
    let mut out = format!(
        "Team: {} — {}\n  lead: {} ({} specialist{})\n",
        config.name,
        if config.description.is_empty() {
            "(no description)"
        } else {
            &config.description
        },
        config.lead,
        specialists,
        if specialists == 1 { "" } else { "s" },
    );
    for m in &config.members {
        let tag = if m.name == config.lead {
            "lead "
        } else {
            "spec "
        };
        let scopes = if m.capability_scopes.is_empty() {
            "(none)".to_string()
        } else {
            m.capability_scopes.join(", ")
        };
        out.push_str(&format!(
            "  [{tag}] {:<12} {:<24} trust={}\n             scopes: {scopes}\n",
            m.name,
            m.role,
            trust_label(m.trust_ceiling),
        ));
    }
    out.push_str(
        "\nNote: scopes above are each member's own declared ask. Actual \
         grants are computed when the team runs (`aivyx-pa team run` /\n\
         `aivyx-pa team start`) against your own configured authority, and \
         may be narrower.\n",
    );
    out
}

fn trust_label(t: TrustTier) -> &'static str {
    match t {
        TrustTier::Untrusted => "Untrusted",
        TrustTier::SemiTrusted => "SemiTrusted",
        TrustTier::Trusted => "Trusted",
        TrustTier::Kernel => "Kernel",
    }
}

/// Load the team to run: a vertical pack's `TeamConfig` from `--config
/// <path.toml>`, or the default 9-role Nonagon when none is given.
fn load_team(config: Option<&str>) -> Result<TeamConfig, String> {
    match config {
        Some(path) => TeamConfig::load(path)
            .map_err(|e| format!("failed to load team config from {path:?}: {e}")),
        None => Ok(default_nonagon()),
    }
}

/// Load a team config, then clamp the lead's (and every specialist's)
/// pack-declared `capability_scopes` to `lead_scopes` — the CLI-run
/// equivalent of the daemon path's own `bind_lead_scopes` call
/// (`aivyx-channel`'s `team_mission_driver.rs`, used by
/// `TeamMissionService`'s `assemble_runtime`). Closes the CLI team-run
/// capability-floor gap: without this clamp, a vertical pack's own file
/// could grant its lead role — and therefore, via delegation, its
/// specialists — any `capability_scopes` it declares, regardless of the
/// operator's own real, already-configured authority.
fn load_and_clamp_team(config: Option<&str>, lead_scopes: &[String]) -> Result<TeamConfig, String> {
    let mut config = load_team(config)?;
    aivyx_channel::team_mission_driver::bind_lead_scopes(&mut config, lead_scopes);
    Ok(config)
}

/// Resolve the daemon's startup team (Chapter Roster RO.1). Resolution order:
///
/// 1. `[team] config_path` set → load that file (a relative path is resolved
///    against `base_dir`, the directory of the loaded `aivyx-pa.toml`).
/// 2. unset → the conventional `team.toml` beside `aivyx-pa.toml`, if it exists.
/// 3. neither → the built-in [`default_nonagon`] (byte-identical to the
///    pre-RO.1 daemon).
///
/// Never errors: a configured-but-broken (or unparseable conventional) file
/// logs a warning and falls back to the default Nonagon so the daemon still
/// boots. The write path ([Chapter U] machinery, RO.2) guarantees a valid
/// file; this reader is the defensive boot half.
pub fn resolve_daemon_team_config(configured: Option<&Path>, base_dir: &Path) -> TeamConfig {
    // Which file to try — the configured path (resolved) or the conventional
    // `team.toml`. An unset-and-absent conventional file means "no file".
    let candidate: Option<PathBuf> = match configured {
        Some(p) if p.is_absolute() => Some(p.to_path_buf()),
        Some(p) => Some(base_dir.join(p)),
        None => {
            let conventional = base_dir.join("team.toml");
            conventional.exists().then_some(conventional)
        }
    };
    let Some(path) = candidate else {
        return default_nonagon();
    };
    match TeamConfig::load(&path) {
        Ok(cfg) => {
            eprintln!("aivyx-pa team: loaded team config from {}", path.display());
            cfg
        }
        Err(e) => {
            eprintln!(
                "aivyx-pa team: WARNING — failed to load team config {} ({e}); \
                 falling back to the default Nonagon",
                path.display()
            );
            default_nonagon()
        }
    }
}

/// The team-config **write target** (Chapter Roster RO.2): the configured
/// `[team] config_path` (a relative path joined to `base_dir`, the `aivyx-pa.toml`
/// directory), else the conventional `team.toml` beside `aivyx-pa.toml`. Unlike
/// [`resolve_daemon_team_config`] this always yields a path — the file may not
/// exist yet, and the `SetTeamRoster` writer creates it.
pub fn team_write_target(configured: Option<&Path>, base_dir: &Path) -> PathBuf {
    match configured {
        Some(p) if p.is_absolute() => p.to_path_buf(),
        Some(p) => base_dir.join(p),
        None => base_dir.join("team.toml"),
    }
}

/// `aivyx-pa team roster [--config <path>]` — print a team. Offline.
pub fn run_roster(config: Option<&str>) -> Result<(), String> {
    print!("{}", render_roster(&load_team(config)?));
    Ok(())
}

/// Resolve the source team for `aivyx-pa team init`: the default Nonagon (no
/// `--pack` or `--pack default`) or a pack loaded from a TOML path. Pure.
fn init_source(pack: Option<&str>) -> Result<TeamConfig, String> {
    match pack {
        None | Some("default") => Ok(default_nonagon()),
        Some(path) => TeamConfig::load(path)
            .map_err(|e| format!("failed to load team pack from {path:?}: {e}")),
    }
}

/// `aivyx-pa team init [--pack <default|path.toml>] [--out <path>] [--force]` —
/// write a starter team config file the daemon adopts at startup (Chapter
/// Roster RO.1) and the Studio's Teams screen edits (RO.3). Offline; shares the
/// RO.2 writer (`write_team_config`: validate → `to_toml` → `0600`). Refuses to
/// overwrite an existing file without `--force`.
pub fn run_init(pack: Option<&str>, out: Option<&str>, force: bool) -> Result<(), String> {
    let roster = init_source(pack)?;
    let out_path = PathBuf::from(out.unwrap_or("team.toml"));
    if out_path.exists() && !force {
        return Err(format!(
            "{} already exists — pass --force to overwrite",
            out_path.display()
        ));
    }
    aivyx_channel::team_config_write::write_team_config(&out_path, &roster).map_err(
        |e| match e {
            aivyx_channel::team_config_write::TeamConfigWriteError::Invalid(m) => {
                format!("the team is invalid: {m}")
            }
            aivyx_channel::team_config_write::TeamConfigWriteError::Write(m) => {
                format!("failed to write {}: {m}", out_path.display())
            }
        },
    )?;
    println!(
        "Wrote team {:?} ({} members) to {}.\n\
         The daemon adopts it on the next start; edit it in the file or the Studio's Teams screen.",
        roster.name,
        roster.members.len(),
        out_path.display(),
    );
    Ok(())
}

/// `aivyx-pa team run "<mission>"` — assemble the default team and run the lead
/// over `mission`. Called from `run_async` with the live provider + the
/// persistent `AuditHook`, so specialist sub-turns land on the HMAC chain.
#[allow(clippy::too_many_arguments)]
pub async fn run_mission(
    provider: Arc<dyn LlmProvider>,
    model: &str,
    max_tokens: u32,
    audit: Arc<dyn AuditHook>,
    checkpointer: Option<Arc<aivyx_core::GitCheckpointer>>,
    lead_scopes: &[String],
    kv_cache_handles: Option<(
        Arc<aivyx_llm::KvSlotPool>,
        Arc<aivyx_kvcache::LlamaServerSlotStore>,
        String,
    )>,
    broker_slot_hint_mode: bool,
    base_tools: Vec<Arc<dyn Tool>>,
    mission: &str,
    config: Option<&str>,
    injection_scan_enabled: bool,
    injection_scan_exempt: std::collections::BTreeSet<String>,
    confirm_destructive: bool,
) -> Result<(), String> {
    let config = load_and_clamp_team(config, lead_scopes)?;
    let team_name = config.name.clone();
    let lead = config.lead_member().ok_or("team has no lead")?.clone();
    // The lead's own operational capabilities -- what its own ConcreteAgent
    // is mounted with below. It grants team.delegate + team.message, so
    // the lead's orchestration/dialogue tools are callable. NOT what
    // specialists are attenuated against (NT-02) -- that's `ceiling`,
    // built just below from the operator's own real, un-narrowed authority.
    let lead_caps = lead.declared_capabilities().map_err(|e| e.to_string())?;

    // The specialist ceiling is the operator's own real authority (the
    // raw floor this function was called with) -- not the lead's own
    // narrowed capability_scopes. Reusing the lead's own field here
    // would silently re-narrow every specialist down to whatever the
    // lead itself declared for its own direct use, defeating
    // bind_lead_scopes' own already-correct per-specialist floor
    // computation one hop downstream.
    let ceiling = CapabilitySet::from_scopes(lead_scopes.iter().filter_map(|s| Scope::parse(s)));

    let assembly = TeamAssembly::build(
        config,
        Arc::clone(&provider),
        model,
        max_tokens,
        Arc::clone(&audit),
        // The daemon's full tool set. Each specialist gets exactly the subset
        // its `tool_allowlist` names (least privilege), capability-attenuated
        // against `ceiling` -- the operator's own real authority (NT-02),
        // not the lead's own declared scopes. The lead itself stays
        // orchestration-only. (A vertical's *domain* tools — e.g. the
        // kitchen toolkit's RPCs — join this set once that toolkit crate
        // is wired in.)
        base_tools,
        ceiling,
        // Chapter Ensemble — the CLI lead-driven `team run` uses one shared
        // backend for all roles; per-role overrides are a daemon-mission path.
        std::collections::HashMap::new(),
        checkpointer.clone(),
        kv_cache_handles.clone(),
        broker_slot_hint_mode,
        // Interactively started via the CLI -- a real operator, not an
        // unattended trigger, so the recursive-scheduling guard doesn't
        // apply here.
        aivyx_core::MessageOrigin::Operator,
        injection_scan_enabled,
        injection_scan_exempt.clone(),
        confirm_destructive,
    )
    .map_err(|e| format!("failed to assemble team: {e}"))?;

    let registry = Arc::new(ToolRegistry::new(assembly.lead_tools()));
    let planner_provider = Arc::clone(&provider);
    let planner_registry = Arc::clone(&registry);
    let model_owned = model.to_string();
    let soul = lead.soul.clone();
    let planner_kv_cache_handles = kv_cache_handles.clone();
    let planner_broker_slot_hint_mode = broker_slot_hint_mode;
    let agent = ConcreteAgent::new(AgentId::new(), lead_caps, registry, audit, move || {
        let cfg = LlmPlannerConfig::new(&model_owned)
            .with_system_prompt(&soul)
            .with_max_tokens(max_tokens);
        let planner = LlmPlanner::new(
            Arc::clone(&planner_provider),
            Arc::clone(&planner_registry),
            cfg,
        );
        let planner = match &planner_kv_cache_handles {
            Some((pool, store, build_hash)) => planner.with_kv_cache(
                Arc::clone(pool),
                Arc::clone(store),
                "llama-server".to_string(),
                model_owned.clone(),
                build_hash.clone(),
            ),
            None => planner,
        };
        // GPU-slot broker coordination — mutually exclusive with the
        // `with_kv_cache` call above (see `SpecialistFactory`'s own
        // `broker_slot_hint_mode` doc comment): at most one of the two
        // ever fires.
        let planner = if planner_broker_slot_hint_mode {
            planner.with_broker_slot_hint()
        } else {
            planner
        };
        Box::new(planner)
    })
    .with_checkpointer(checkpointer)
    // Task 4 fix round 1 — same `[access] confirm_destructive` posture as
    // every specialist `TeamAssembly::build` just wired above; without
    // this the CLI `team run` lead agent's own D1 confirm-destructive gate
    // never fires even though its specialists' does.
    .with_confirm_destructive(confirm_destructive);
    // The lead orchestrates the mission autonomously (delegating to specialists
    // via team.delegate), so it takes the same autonomous safety posture as the
    // specialists (see SpecialistFactory::build): the small-cycle breaker as a
    // built-in floor, independent of the interactive `[agent] cycle_detection`.
    // The injection-scan posture carries the operator's own `[agent]` config
    // through, same as every specialist this mission builds.
    let agent = TurnSafety::autonomous(injection_scan_enabled, injection_scan_exempt).apply(agent);

    let channel = MissionChannel::new();
    let msg = Message::text(channel.session_id(), mission);
    eprintln!(
        "team: running mission on {} (lead: {})…",
        team_name, lead.name
    );
    match agent.turn(msg, &channel).await {
        TurnOutcome::Completed { final_message, .. } => {
            println!("{final_message}");
            Ok(())
        }
        TurnOutcome::Escalated { reason, .. } => {
            Err(format!("team mission escalated for approval: {reason}"))
        }
        TurnOutcome::TimedOut { .. } => Err("team mission timed out".to_string()),
        TurnOutcome::Cancelled { .. } => Err("team mission was cancelled".to_string()),
        _ => Err("team mission did not complete".to_string()),
    }
}

/// A one-shot operator-facing channel for a `team run`: a fresh session at the
/// local Trusted tier. Streaming the lead's progress to the operator is the
/// J.7 Fleet panel; here we just collect the final deliverable.
struct MissionChannel {
    session: SessionId,
    token: CancellationToken,
}

impl MissionChannel {
    fn new() -> Self {
        MissionChannel {
            session: SessionId::new(),
            token: CancellationToken::new(),
        }
    }
}

#[async_trait]
impl ChannelContext for MissionChannel {
    fn channel_name(&self) -> &str {
        "team"
    }
    fn platform(&self) -> ChannelPlatform {
        ChannelPlatform::Local
    }
    fn trust_tier(&self) -> TrustTier {
        TrustTier::Trusted
    }
    fn session_id(&self) -> SessionId {
        self.session
    }
    async fn stream_event(&self, _event: StreamEvent<'_>) -> Result<(), ChannelError> {
        Ok(())
    }
    async fn finalize(&self, _outcome: &TurnOutcome) -> Result<(), ChannelError> {
        Ok(())
    }
    fn cancellation_token(&self) -> CancellationToken {
        self.token.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roster_renders_the_default_nonagon() {
        let out = render_roster(&default_nonagon());
        // Header: 9 roles → 8 specialists, coordinator lead.
        assert!(out.contains("Team: default-nonagon"));
        assert!(out.contains("lead: coordinator (8 specialists)"));
        // Every role is listed.
        for name in [
            "coordinator",
            "researcher",
            "analyst",
            "coder",
            "writer",
            "reviewer",
            "planner",
            "verifier",
            "archivist",
        ] {
            assert!(out.contains(name), "roster missing {name}");
        }
        // The lead is tagged distinctly and shows its orchestration scope.
        assert!(out.contains("[lead ]"));
        assert!(out.contains("[spec ]"));
        assert!(out.contains("team.delegate"));
        // J.5 roster wiring: every member can talk on the bus.
        assert!(out.contains("team.message"));
    }

    #[test]
    fn load_team_defaults_to_the_nonagon_and_errors_on_a_bad_path() {
        // No --config → the default 9-role Nonagon.
        assert_eq!(load_team(None).unwrap().lead, "coordinator");
        // A missing pack path is a clean error, not a panic.
        let err = load_team(Some("/no/such/team.toml")).unwrap_err();
        assert!(err.contains("failed to load team config"), "error: {err}");
    }

    // --- Chapter Roster (RO.1): the daemon's load-or-default resolver --------

    /// A unique scratch dir under the system temp root (no tempfile dev-dep).
    fn scratch(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!(
            "aivyx-roster-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Write a distinguishable (non-default) `[team]`-rooted file. Returns the path.
    fn write_custom_team(path: &Path) {
        use aivyx_team::config::{DialogueConfig, TeamConfig, TeamMember};
        let m = |name: &str| TeamMember {
            name: name.into(),
            role: "R".into(),
            soul: "s".into(),
            tool_allowlist: vec![],
            capability_scopes: vec![],
            trust_ceiling: TrustTier::Trusted,
            model: None,
            base_url: None,
        };
        let cfg = TeamConfig {
            name: "custom-team".into(),
            description: String::new(),
            lead: "boss".into(),
            members: vec![m("boss"), m("helper")],
            dialogue: DialogueConfig::default(),
        };
        std::fs::write(path, cfg.to_toml().unwrap()).unwrap();
    }

    #[test]
    fn resolve_unset_and_no_file_is_the_default_nonagon() {
        let dir = scratch("none");
        let team = resolve_daemon_team_config(None, &dir);
        assert_eq!(team.name, "default-nonagon");
        assert_eq!(team.lead, "coordinator");
    }

    #[test]
    fn resolve_unset_loads_the_conventional_team_toml() {
        let dir = scratch("conventional");
        write_custom_team(&dir.join("team.toml"));
        let team = resolve_daemon_team_config(None, &dir);
        assert_eq!(team.name, "custom-team");
        assert_eq!(team.lead, "boss");
    }

    #[test]
    fn resolve_configured_relative_path_is_joined_to_base_dir() {
        let dir = scratch("relative");
        write_custom_team(&dir.join("my-team.toml"));
        let team = resolve_daemon_team_config(Some(Path::new("my-team.toml")), &dir);
        assert_eq!(team.name, "custom-team");
    }

    #[test]
    fn resolve_configured_absolute_path_ignores_base_dir() {
        let dir = scratch("absolute");
        let elsewhere = scratch("absolute-target");
        let abs = elsewhere.join("packed.toml");
        write_custom_team(&abs);
        let team = resolve_daemon_team_config(Some(&abs), &dir);
        assert_eq!(team.name, "custom-team");
    }

    #[test]
    fn resolve_configured_broken_path_falls_back_to_default_not_panic() {
        let dir = scratch("broken");
        let team = resolve_daemon_team_config(Some(Path::new("/no/such/team.toml")), &dir);
        assert_eq!(team.name, "default-nonagon");
    }

    // --- Chapter Roster (RO.4): `aivyx-pa team init` -----------------------------

    #[test]
    fn init_source_defaults_to_nonagon_and_loads_a_pack_path() {
        assert_eq!(init_source(None).unwrap().name, "default-nonagon");
        assert_eq!(
            init_source(Some("default")).unwrap().name,
            "default-nonagon"
        );
        let dir = scratch("init-src");
        let p = dir.join("pack.toml");
        write_custom_team(&p);
        assert_eq!(
            init_source(Some(p.to_str().unwrap())).unwrap().name,
            "custom-team"
        );
        assert!(init_source(Some("/no/such.toml")).is_err());
    }

    #[test]
    fn run_init_writes_then_refuses_overwrite_without_force() {
        let dir = scratch("init-write");
        let out = dir.join("team.toml");
        let out_s = out.to_str().unwrap();
        run_init(None, Some(out_s), false).unwrap();
        assert!(out.exists());
        // The written file is the default Nonagon, loadable back.
        assert_eq!(TeamConfig::load(&out).unwrap().name, "default-nonagon");
        // A second write without --force is refused.
        let err = run_init(None, Some(out_s), false).unwrap_err();
        assert!(err.contains("already exists"), "err: {err}");
        // With --force it overwrites cleanly.
        run_init(None, Some(out_s), true).unwrap();
    }

    #[test]
    fn roster_handles_a_single_specialist_plural() {
        use aivyx_team::config::{DialogueConfig, TeamConfig, TeamMember};
        let m = |name: &str| TeamMember {
            name: name.into(),
            role: "R".into(),
            soul: "s".into(),
            tool_allowlist: vec![],
            capability_scopes: vec![],
            trust_ceiling: TrustTier::Trusted,
            model: None,
            base_url: None,
        };
        let cfg = TeamConfig {
            name: "duo".into(),
            description: String::new(),
            lead: "lead".into(),
            members: vec![m("lead"), m("helper")],
            dialogue: DialogueConfig::default(),
        };
        let out = render_roster(&cfg);
        assert!(
            out.contains("1 specialist)"),
            "singular, not '1 specialists'"
        );
        assert!(out.contains("(no description)"));
        assert!(out.contains("scopes: (none)"));
    }

    #[test]
    fn roster_shows_a_declared_not_guaranteed_caveat() {
        let out = render_roster(&default_nonagon());
        assert!(
            out.contains("declared") && out.contains("aivyx-pa team run"),
            "roster output should caveat that scopes are declared, not \
             guaranteed, and point at where the real grant happens: {out}"
        );
    }

    #[test]
    fn load_and_clamp_team_strips_a_lead_scope_the_floor_does_not_grant() {
        use aivyx_team::config::{DialogueConfig, TeamConfig, TeamMember};

        let dir = scratch("clamp");
        let path = dir.join("pack.toml");
        let m = |name: &str, scopes: Vec<&str>| TeamMember {
            name: name.into(),
            role: "R".into(),
            soul: "s".into(),
            tool_allowlist: vec![],
            capability_scopes: scopes.into_iter().map(String::from).collect(),
            trust_ceiling: TrustTier::Trusted,
            model: None,
            base_url: None,
        };
        let cfg = TeamConfig {
            name: "unaudited-pack".into(),
            description: String::new(),
            lead: "boss".into(),
            // The lead declares a domain scope well beyond team
            // orchestration — exactly the shape an unaudited third-party
            // pack might ship, and exactly what this fix must strip.
            members: vec![m("boss", vec!["shell.exec:cwd:/etc/**", "team.delegate"])],
            dialogue: DialogueConfig::default(),
        };
        std::fs::write(&path, cfg.to_toml().unwrap()).unwrap();

        // The floor grants only team orchestration markers — no shell.exec
        // at all. This is the operator's own real authority; the pack's
        // file must not be able to exceed it.
        let floor = vec!["team.message".to_string(), "team.delegate".to_string()];
        let clamped = load_and_clamp_team(Some(path.to_str().unwrap()), &floor).unwrap();
        let lead = clamped.lead_member().unwrap();

        assert!(
            !lead
                .capability_scopes
                .iter()
                .any(|s| s.starts_with("shell.exec")),
            "lead scopes should not include the out-of-floor domain scope: {:?}",
            lead.capability_scopes
        );
        assert!(
            lead.capability_scopes
                .contains(&"team.delegate".to_string()),
            "the legitimate orchestration marker must still flow through: {:?}",
            lead.capability_scopes
        );
    }
}
