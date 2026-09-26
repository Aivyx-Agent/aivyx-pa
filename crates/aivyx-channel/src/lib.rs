//! # aivyx-channel
//!
//! The `LocalChannel` reference implementation and future remote-channel
//! adapters (Telegram/Discord/Slack/Matrix/Email).
//!
//! ## Where `ChannelContext` lives
//!
//! **The `ChannelContext` trait itself lives in `aivyx-core`**, not here.
//! This is a Phase 1 task 3 layering decision: `ToolContext` inside
//! `aivyx-core` needs to hold `&dyn ChannelContext`, and moving the trait
//! out of core would create a cycle (`aivyx-core` → `aivyx-channel` →
//! `aivyx-core`). The same reasoning applied to `ChannelPlatform` in
//! task 2.
//!
//! This crate re-exports the trait + helpers so that channel-crate
//! consumers have a stable import path (`use aivyx_channel::ChannelContext`).
//!
//! See DESIGN.md Deliverable 1 (the "no bypass" commitment) and
//! Deliverable 3 (the `ChannelContext` trait sketch).
//!
//! ## Status
//!
//! Phase 1 shipped re-exports only. Phase 3 task 1 adds [`LocalChannel`]
//! — the CLI reference implementation that streams tokens to stdout
//! (or any `std::io::Write` sink, for tests).

pub use aivyx_core::{
    AttachmentKind, ChannelContext, ChannelError, ChannelPlatform, StreamEvent,
};

pub mod daemon_client;
pub mod daemon_ipc;
pub mod daemon_scheduler;
pub mod daemon_server;
pub mod document_browse;
pub mod mcp_status;
pub mod mission;
pub mod mission_meter;
pub mod mission_tool;
pub mod schedule;
pub mod schedule_tool;
pub mod trigger;
pub mod webhook;
pub mod webhook_listener;
pub mod webhook_tool;
pub mod file_watch;
pub mod file_watch_tool;
pub mod file_watcher;
pub mod conversation_window;
pub mod skill_trigger_context;
/// Chapter L — durable persistence for daemon-run Nonagon team missions
/// (`TeamMissionRecord` over `KeyDomain::TeamMissions`), the checkpoint/resume
/// state behind the live TUI Missions feed.
pub mod team_mission;
/// Chapter Roster (RO.2) — the team-config file writer (`SetTeamRoster`).
pub mod team_config_write;
/// Chapter L (L.4) — the daemon-side mission driver: `SharedMissionState`
/// (registry over the store) plus `team_run` / `resolve_team_gate`, driving
/// the engine's `run_until_pause` with checkpoint persistence.
pub mod team_mission_driver;
/// Chapter K (K.4.2) — the concrete pre-call dollar gate
/// (`ChannelBudgetGate`) for the interactive / team turn loop, over the
/// `aivyx_core::BudgetGate` trait.
pub mod budget_gate;
pub mod rate_gate;
pub mod routing_guard;
pub mod cooccurrence_ledger;
pub mod graph_query_tool;
pub mod conflict_dismissals;
pub mod contradiction;
pub mod soul_contradiction;
pub mod knowledge_graph;
pub mod knowledge_wiki;
/// Phase 172 — the structural correction-signal detector +
/// durable decayed correction ledger + consolidation pass.
/// Closes the Aivyx Agent Review §5.8 self-improvement gap:
/// the agent now notices when the operator corrects it and
/// files an operator-gated Persona proposal. `correction_detect`
/// is the pure detector; `correction_ledger` the durable view;
/// `correction_consolidation` the proposal actuator.
pub mod correction_consolidation;
pub mod correction_detect;
pub mod correction_judgment;
pub mod correction_ledger;
/// Phase 173 — the autonomous-loop backlog substrate (the
/// Aivyx Ralph loop). HMAC-chained, append-only ordered story
/// list (the `prd.json` analog) the loop driver works through
/// one fresh-context iteration at a time.
pub mod loop_backlog;
/// Phase 183 — durable one-shot reminder store + the pure
/// `due_now` selector (everyday-PA breadth #1).
pub mod reminder_store;
pub mod reminder_tool;
pub mod reminder_driver;
/// Phase 173 — the autonomous-loop backlog agent tools
/// (`loop.next` / `loop.complete`), channel-tier like
/// `mission.*`. The only new agent-facing surface the loop
/// needs; the fifteen-tool substrate core is untouched.
pub mod loop_tool;
/// Phase 173 — the autonomous-loop driver. A background task
/// (sibling of `reflection_scheduler`) that fires a fresh-context
/// `TriggerSource::Loop` turn per iteration over the backlog,
/// re-arming until the backlog is empty or a hard cap is hit.
pub mod loop_driver;
pub mod loop_resume;
pub mod digest;
/// Phase 174 — driver-side gate verification. The `GateRunner`
/// trait + the production `ShellGateRunner` that runs the
/// operator-configured gate command (build/tests) so the driver
/// stops a run the moment the tree goes red.
pub mod loop_gate;
pub mod completion_judge;
pub mod task_complexity;
pub mod helpfulness_ledger;
pub mod skill_authoring;
pub mod skill_effectiveness;
pub mod skill_refinement;
pub mod memory_embedding;
pub mod memory_gc_tool;
pub mod memory_recall;
pub mod recall_feedback;
pub mod recall_gate;
pub mod recall_insights;
pub mod recall_judgment;
pub mod recall_log;
pub mod prune_sink;
pub mod reflection_tool;
pub mod role_overrides;
pub mod role_update_tool;
pub mod turn_history_tool;
pub mod tools_list_tool;
pub mod ollama_tools;
/// Phase 62 — Agent-Initiated Outbound Notifications. The
/// dispatcher and `NotifyBackend` trait live here; per-kind
/// backend impls live in `notify_telegram` (Phase 62 Task 5) and
/// `notify_webhook` (Phase 62 Task 6); the `notify.send` tool
/// lives in `notify_tool` (Phase 62 Task 7).
pub mod notify_dispatcher;
pub mod notify_email;
pub mod notify_telegram;
pub mod notify_webhook;
pub mod notify_webui;
pub mod notify_tool;
/// Phase 64 — Identity export/import format. Closes the Phase 60
/// deferral; lets operators move Profile + Persona between hosts
/// by re-signing the chain on import (Q1(a) at sign-off).
pub mod identity_export;
mod daemon_session;
mod local;
pub mod keyring_store;
pub mod passphrase;
pub mod persona;
pub mod persona_consolidation;
pub mod persona_context;
pub mod persona_lifecycle;
pub mod persona_proposal;
pub mod persona_seed_draft;
pub mod profile_draft;
pub mod team_template_draft;
pub mod proactive_detect;
pub mod proactive_log;
pub mod profile_prompt;
pub mod proposal_grouping;
pub mod recall_fusion;
pub mod reflection_scheduler;
pub mod token_budget;
pub mod workspace_journal;
mod render;
mod role_envelope;
mod role_render;
mod session;
pub mod telegram_daemon_frontend;
// Phase 111 — Discord and Slack daemon-frontends mirroring
// telegram_daemon_frontend.rs's Phase 19 pattern. Each adapter
// crate (aivyx-discord, aivyx-slack) ships the in-process
// channel + session driver; the daemon-frontend modules below
// bridge the daemon IPC protocol to those crates' transport
// surfaces. The `parse_gate_command` helper extracted to
// `gate_command.rs` is shared between all three (Telegram +
// Discord + Slack) daemon-frontends.
pub mod discord_daemon_frontend;
pub mod gate_command;
pub mod team_command;
pub mod team_dispatch;
pub mod team_trigger_state;
pub mod skill_auto_proposer;
pub mod skill_edit;
pub mod skill_tool;
pub mod relevance_prompt_refiner;
pub mod tool_relevance_ledger;
pub mod slack_daemon_frontend;
pub mod web_ui;

pub use local::LocalChannel;
pub use render::{render_finalize, render_stream_event, RenderMode};
pub use profile_prompt::assemble_session_prompt;
pub use role_envelope::{assemble_role_envelope, MAX_INHERITANCE_DEPTH};
pub use role_render::{render_role_envelope, ChannelKind};
pub use daemon_session::{run_daemon_session, run_daemon_session_connected, DaemonSessionConfig};
pub use session::{
    build_agent_stack, run_session, AgentStackSpec, SessionConfig, SessionReport,
};
