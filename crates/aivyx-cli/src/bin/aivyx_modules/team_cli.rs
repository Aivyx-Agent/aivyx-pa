//! `aivyx-pa team` daemon control surface — Chapter L (L.5b).
//!
//! IPC-backed verbs for daemon-run Nonagon missions, siblings of the offline
//! `roster` / in-process `run` (Chapter J, in `team.rs`):
//!
//! - `aivyx-pa team start --plan <file.json>` — submit an explicit mission plan
//!   (the same friendly `{goal, steps}` spec `decompose_task` accepts; goal→
//!   plan LLM decomposition is a later increment per the L.4 decision).
//! - `aivyx-pa team list` / `aivyx-pa team status [<id>]` — poll the mission feed.
//! - `aivyx-pa team approve|reject <id> <step>` — resolve a human-approval gate.
//! - `aivyx-pa team abort <id>` (Chapter Belay) — stop a running mission; it
//!   halts gracefully at its next wave boundary, landing terminally in
//!   `Halted`.
//! - `aivyx-pa team pause <id>` / `aivyx-pa team resume <id>` (Chapter Mission
//!   Control) — pause a running mission at its next wave boundary (landing
//!   non-terminally in `Paused`) and later resume it from the preserved
//!   checkpoint.
//!
//! All eight (start, list, status, approve, reject, abort, pause, resume)
//! talk to the running daemon's `TeamMissionService`. The render helpers are
//! pure so they unit-test against fixtures without IPC.

use std::path::Path;

use aivyx_channel::daemon_client::{
    daemon_is_running, resolve_team_gate, team_mission_list, team_mission_status, team_run,
    team_run_goal,
};
use aivyx_channel::daemon_ipc::default_socket_path;
use aivyx_channel::team_mission::{TeamMissionPhase, TeamMissionRecord};
use aivyx_team::{StepKind, TeamConfig, parse_plan_spec};

use crate::TeamSubcommand;

/// `aivyx-pa team <daemon-subcommand>` — dispatch the IPC-backed verbs.
pub async fn run_team_daemon(sub: TeamSubcommand) -> Result<(), String> {
    let socket_path = default_socket_path()?;
    require_daemon_running(&socket_path).await?;

    match sub {
        TeamSubcommand::Start { plan_path, config } => {
            let plan = load_plan(&plan_path)?;
            let team = load_team_config(config.as_deref())?;
            let id = team_run(&socket_path, plan, team)
                .await
                .map_err(|e| format!("team start failed: {e}"))?;
            println!("started mission {id}");
            println!("track it with `aivyx-pa team status {id}`");
            Ok(())
        }
        TeamSubcommand::StartGoal { goal, config } => {
            let team = load_team_config(config.as_deref())?;
            println!("decomposing goal into a plan…");
            let id = team_run_goal(&socket_path, goal, team)
                .await
                .map_err(|e| format!("team start failed: {e}"))?;
            println!("started mission {id}");
            println!("track it with `aivyx-pa team status {id}`");
            Ok(())
        }
        TeamSubcommand::List => {
            let missions = team_mission_list(&socket_path)
                .await
                .map_err(|e| format!("team list failed: {e}"))?;
            print!("{}", render_mission_list(&missions));
            Ok(())
        }
        TeamSubcommand::Status {
            mission_id: Some(id),
        } => {
            let mission = team_mission_status(&socket_path, id.clone())
                .await
                .map_err(|e| format!("team status failed: {e}"))?;
            match mission {
                Some(record) => print!("{}", render_mission_status(&record)),
                None => return Err(format!("no mission with id `{id}`")),
            }
            Ok(())
        }
        TeamSubcommand::Status { mission_id: None } => {
            // No id → the whole feed (same as `list`).
            let missions = team_mission_list(&socket_path)
                .await
                .map_err(|e| format!("team status failed: {e}"))?;
            print!("{}", render_mission_list(&missions));
            Ok(())
        }
        TeamSubcommand::Approve { mission_id, step } => {
            resolve_gate(&socket_path, mission_id, step, true).await
        }
        TeamSubcommand::Reject { mission_id, step } => {
            resolve_gate(&socket_path, mission_id, step, false).await
        }
        TeamSubcommand::Abort { mission_id } => {
            let message =
                aivyx_channel::daemon_client::abort_team_mission(&socket_path, mission_id)
                    .await
                    .map_err(|e| format!("team abort failed: {e}"))?;
            println!("{message}");
            Ok(())
        }
        TeamSubcommand::Pause { mission_id } => {
            let message =
                aivyx_channel::daemon_client::pause_team_mission(&socket_path, mission_id)
                    .await
                    .map_err(|e| format!("team pause failed: {e}"))?;
            println!("{message}");
            Ok(())
        }
        TeamSubcommand::Resume { mission_id } => {
            let phase =
                aivyx_channel::daemon_client::resume_team_mission(&socket_path, mission_id.clone())
                    .await
                    .map_err(|e| format!("team resume failed: {e}"))?;
            println!("mission {mission_id} resumed (now {phase:?})");
            println!("track it with `aivyx-pa team status {mission_id}`");
            Ok(())
        }
        // The offline / in-process verbs are dispatched elsewhere (`team.rs`).
        TeamSubcommand::Roster { .. }
        | TeamSubcommand::Init { .. }
        | TeamSubcommand::Run { .. } => {
            Err("internal: non-daemon team subcommand routed to the daemon path".to_string())
        }
    }
}

async fn resolve_gate(
    socket_path: &Path,
    mission_id: String,
    step: String,
    approve: bool,
) -> Result<(), String> {
    let verb = if approve { "approve" } else { "reject" };
    let phase = resolve_team_gate(socket_path, mission_id.clone(), step.clone(), approve)
        .await
        .map_err(|e| format!("team {verb} failed: {e}"))?;
    println!(
        "{verb}d gate `{step}` of mission {mission_id} → {}",
        phase_label(phase)
    );
    Ok(())
}

/// Load an optional vertical-pack `TeamConfig` from `--config <path.toml>`.
/// `None` ⇒ the daemon runs the mission on its default team (the Nonagon). The
/// CLI loads + sends the full config so the daemon needn't resolve the path.
fn load_team_config(config_path: Option<&str>) -> Result<Option<TeamConfig>, String> {
    match config_path {
        None => Ok(None),
        Some(path) => aivyx_team::load_team(path)
            .map(Some)
            .map_err(|e| format!("failed to load team config from {path:?}: {e}")),
    }
}

/// Read + parse a `{goal, steps}` plan-spec JSON file into a `MissionPlan`.
fn load_plan(plan_path: &str) -> Result<aivyx_team::MissionPlan, String> {
    let raw = std::fs::read_to_string(plan_path)
        .map_err(|e| format!("failed to read plan file {plan_path:?}: {e}"))?;
    let value: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| format!("plan file {plan_path:?} is not valid JSON: {e}"))?;
    parse_plan_spec(&value).map_err(|e| format!("invalid plan in {plan_path:?}: {e}"))
}

async fn require_daemon_running(socket_path: &Path) -> Result<(), String> {
    if daemon_is_running(socket_path).await {
        return Ok(());
    }
    Err(format!(
        "aivyx-pa team: no daemon running on socket {} — start the daemon \
         first with `aivyx-pa daemon run`",
        socket_path.display(),
    ))
}

fn phase_label(phase: TeamMissionPhase) -> &'static str {
    match phase {
        TeamMissionPhase::Planning => "planning",
        TeamMissionPhase::Executing => "executing",
        TeamMissionPhase::AwaitingApproval => "awaiting approval",
        TeamMissionPhase::Paused => "paused",
        TeamMissionPhase::Done => "done",
        TeamMissionPhase::Rejected => "rejected",
        TeamMissionPhase::Halted => "halted",
    }
}

/// Pure renderer — the mission feed as an operator-readable list, most-recently-
/// updated first so live work is at the top.
fn render_mission_list(missions: &[TeamMissionRecord]) -> String {
    if missions.is_empty() {
        return "No team missions. Start one with `aivyx-pa team start --plan \
                <file.json>`.\n"
            .to_string();
    }
    let mut sorted: Vec<&TeamMissionRecord> = missions.iter().collect();
    sorted.sort_by_key(|m| std::cmp::Reverse(m.updated_at_unix_ms));

    let mut out = format!("{} team mission(s):\n", sorted.len());
    for m in sorted {
        let done = m.outputs.len();
        let total = m.plan.steps.len();
        out.push_str(&format!(
            "  {:<36} [{:<17}] {}/{} steps — {}\n",
            m.id,
            phase_label(m.phase),
            done,
            total,
            m.goal,
        ));
        if let Some(gate) = &m.pending_gate {
            out.push_str(&format!(
                "      ↳ awaiting `aivyx-pa team approve|reject {} {gate}`\n",
                m.id
            ));
        }
        // Chapter Mission Control — a paused mission's next move is resume,
        // mirroring the pending-gate hint above.
        if m.phase == TeamMissionPhase::Paused {
            out.push_str(&format!(
                "      ↳ resume with `aivyx-pa team resume {}`\n",
                m.id
            ));
        }
    }
    out
}

/// Pure renderer — one mission's detail: phase, goal, and per-step state.
fn render_mission_status(record: &TeamMissionRecord) -> String {
    let mut out = format!(
        "Mission {}\n  goal:  {}\n  phase: {}\n",
        record.id,
        record.goal,
        phase_label(record.phase),
    );
    if let Some(gate) = &record.pending_gate {
        out.push_str(&format!(
            "  gate:  `{gate}` awaiting your decision \
             (`aivyx-pa team approve|reject {} {gate}`)\n",
            record.id
        ));
    }
    // Chapter Belay — when halted, show *why* (budget cap vs operator abort).
    if record.phase == TeamMissionPhase::Halted {
        if let Some(reason) = &record.halt_reason {
            out.push_str(&format!("  reason: {reason}\n"));
        }
    }
    // Chapter Mission Control — a paused mission's next move is resume,
    // mirroring the `gate:` hint above.
    if record.phase == TeamMissionPhase::Paused {
        out.push_str(&format!(
            "  ↳ resume with `aivyx-pa team resume {}`\n",
            record.id
        ));
    }
    out.push_str("  steps:\n");
    for step in &record.plan.steps {
        let (kind, member) = match &step.kind {
            StepKind::Delegate { specialist, .. } => ("delegate", specialist.as_str()),
            StepKind::Gate { reviewer, .. } => ("gate", reviewer.as_str()),
        };
        let state = step_state_label(record, &step.id);
        out.push_str(&format!(
            "    [{:<9}] {:<14} {} ({})\n",
            state, step.id, member, kind,
        ));
    }
    out
}

/// The operator-facing state of one step, derived from the checkpoint: a step
/// in `outputs` is done (or the pending/rejected gate), else pending.
fn step_state_label(record: &TeamMissionRecord, step_id: &str) -> &'static str {
    if record.pending_gate.as_deref() == Some(step_id) {
        return "awaiting";
    }
    match record.outputs.get(step_id) {
        Some(v) if v.starts_with("rejected") => "rejected",
        Some(_) => "done",
        None => "pending",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_team::{MissionPlan, Step};

    fn sample(phase: TeamMissionPhase, pending: Option<&str>) -> TeamMissionRecord {
        let plan = MissionPlan::new(
            "ship the note",
            vec![
                Step::delegate("research", "researcher", "gather"),
                Step::human_gate("approve", "reviewer", "ok?").after(["research"]),
                Step::delegate("write", "writer", "draft").after(["approve"]),
            ],
        );
        let mut r = TeamMissionRecord::new("m-1", "ship the note", plan);
        r.phase = phase;
        r.pending_gate = pending.map(str::to_string);
        if pending.is_some() {
            r.outputs.insert("research".into(), "12 findings".into());
        }
        r
    }

    #[test]
    fn empty_list_guides_the_operator() {
        let out = render_mission_list(&[]);
        assert!(out.contains("No team missions"));
        assert!(out.contains("team start --plan"));
    }

    #[test]
    fn list_shows_phase_progress_and_pending_gate() {
        let recs = vec![sample(TeamMissionPhase::AwaitingApproval, Some("approve"))];
        let out = render_mission_list(&recs);
        assert!(out.contains("m-1"));
        assert!(out.contains("awaiting approval"));
        assert!(out.contains("1/3 steps"));
        assert!(out.contains("approve|reject m-1 approve"));
    }

    #[test]
    fn status_derives_per_step_state() {
        let rec = sample(TeamMissionPhase::AwaitingApproval, Some("approve"));
        let out = render_mission_status(&rec);
        assert!(out.contains("[done     ] research"));
        assert!(out.contains("[awaiting ] approve"));
        assert!(out.contains("[pending  ] write"));
        assert!(out.contains("researcher (delegate)"));
        assert!(out.contains("reviewer (gate)"));
    }

    #[test]
    fn halted_shows_the_real_reason_not_a_hardcoded_budget() {
        // Chapter Belay / backlog #10 — Halted now has >1 cause; the renderer
        // must show the actual reason, not always say "(budget)".
        let mut rec = sample(TeamMissionPhase::Halted, None);
        rec.halt_reason = Some("aborted by operator".to_string());
        let out = render_mission_status(&rec);
        assert!(out.contains("phase: halted"));
        assert!(!out.contains("(budget)"), "must not hardcode budget");
        assert!(out.contains("reason: aborted by operator"));
        // The list view stays columnar — just the short phase, no reason spill.
        let list = render_mission_list(&[rec]);
        assert!(list.contains("halted"));
        assert!(!list.contains("aborted by operator"));
    }

    #[test]
    fn paused_mission_shows_a_resume_hint_in_list_and_status() {
        // Chapter Mission Control (Fix D.1) — a paused mission's next
        // operator move is resume, mirroring the pending-gate hint.
        let rec = sample(TeamMissionPhase::Paused, None);
        let status = render_mission_status(&rec);
        assert!(status.contains("phase: paused"));
        assert!(status.contains("resume with `aivyx-pa team resume m-1`"));

        let list = render_mission_list(&[rec]);
        assert!(list.contains("↳ resume with `aivyx-pa team resume m-1`"));
    }

    #[test]
    fn rejected_gate_renders_as_rejected() {
        let mut rec = sample(TeamMissionPhase::Rejected, None);
        rec.outputs
            .insert("approve".into(), "rejected by operator".into());
        let out = render_mission_status(&rec);
        assert!(out.contains("[rejected ] approve"));
        assert!(out.contains("[pending  ] write"), "the dependent never ran");
    }

    #[test]
    fn load_plan_parses_the_friendly_spec() {
        let dir = std::env::temp_dir().join(format!("aivyx-plan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("plan.json");
        std::fs::write(
            &path,
            r#"{"goal":"g","steps":[
                {"id":"a","specialist":"researcher","prompt":"go"},
                {"id":"g","reviewer":"reviewer","criteria":"ok?","mode":"human","deps":["a"]}
            ]}"#,
        )
        .unwrap();
        let plan = load_plan(path.to_str().unwrap()).unwrap();
        assert_eq!(plan.goal, "g");
        assert_eq!(plan.steps.len(), 2);
        assert!(plan.step("g").unwrap().is_human_gate());
    }

    #[test]
    fn load_plan_rejects_bad_json() {
        let dir = std::env::temp_dir().join(format!("aivyx-plan-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(load_plan(path.to_str().unwrap()).is_err());
    }
}
