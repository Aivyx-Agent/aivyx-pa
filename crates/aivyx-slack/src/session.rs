//! `run_slack_session` — the Slack analogue of
//! `aivyx_channel::run_session` /
//! `aivyx_telegram::run_telegram_session` /
//! `aivyx_discord::run_discord_session`.
//!
//! Slack's Socket Mode is push-based like Discord's Gateway —
//! one outbound WebSocket connection from aivyx-pa, events arrive
//! continuously, no `get_updates` cursor. That puts Slack on
//! the **two-piece** session-driver shape Phase 107 named
//! (`docs/ADAPTER_PATTERN.md`'s three-data-point update):
//!
//! - Outer multiplexer pumps `transport.next_message()` and
//!   routes each inbound message to a per-channel mailbox.
//! - Per-channel inner mailbox task runs the agent turn loop
//!   for one `(team_id, channel_id)` partition.
//!
//! No `scan_for_cancel` probe (Slack events flow through the
//! same WebSocket; the inner task's biased select against
//! `mailbox.recv()` is the cancel detector). No
//! `get_updates` cursor (push-based protocol).
//!
//! ## What this module owns
//!
//! - Builds `ConcreteAgent` from [`SlackSessionConfig`] in each
//!   inner task. Why a separate config type: `aivyx-slack`
//!   cannot depend on `aivyx-channel` without creating a
//!   package cycle (the `aivyx-pa` binary lives in `aivyx-channel`
//!   and imports `aivyx_slack::run_slack_session`).
//! - Rotates the channel's cancellation token per turn (Phase
//!   3 monotonic-token fix).
//! - Runs `agent.turn(message, &channel).await` for each
//!   inbound message.
//!
//! Deliberately omitted (compared to `run_session`):
//! No banner, no prompt string, no session marker write, no
//! signal handling — same omissions Discord made for the same
//! reasons.
//!
//! ## /cancel semantics
//!
//! Mirrors Discord's mailbox path: `/cancel` mid-turn fires
//! the channel's per-turn token; `/cancel` with no turn
//! running is dropped at the mailbox boundary.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use aivyx_capability::CapabilitySet;
use aivyx_core::{
    agent::ConcreteAgent, llm_planner::LlmPlanner, planner::ToolRegistry, Agent, AgentId,
    AuditHook, CancellationToken, ChannelContext, LlmPlannerConfig, Message,
};
use aivyx_llm::LlmProvider;
use aivyx_storage::Storage;

use crate::slack_channel::SlackChannel;
use crate::transport::{IncomingMessage, SlackMorphismTransport, SlackTransport};

/// Per-channel mailbox capacity. Mirrors Telegram's
/// `CHAT_MAILBOX_CAPACITY` and Discord's
/// `CHANNEL_MAILBOX_CAPACITY` — same number, same rationale
/// (absorb a per-channel burst without backpressure on the
/// outer loop; sustained floods apply backpressure on the
/// problem channel only).
const CHANNEL_MAILBOX_CAPACITY: usize = 32;

// ---------------------------------------------------------------------------
// Per-session config + reports
// ---------------------------------------------------------------------------

/// Per-session knobs for the Slack loop. Analogue of
/// `aivyx_discord::DiscordSessionConfig` and
/// `aivyx_telegram::TelegramSessionConfig` minus their
/// platform-specific fields.
#[derive(Clone)]
pub struct SlackSessionConfig {
    pub model: String,
    pub system_prompt: String,
    pub max_tokens: u32,
    pub capabilities: CapabilitySet,
    pub tools: Arc<ToolRegistry>,
    pub storage: Arc<dyn Storage>,
    pub tool_allowlist: Option<std::collections::BTreeSet<String>>,
    pub memory_topic_prefix: Option<String>,
    /// Chapter Bridle (BR.4) — `[agent] turn_timeout_secs` override.
    /// Threaded into `TurnSafety::interactive(...)` at each construction
    /// site below, matching `daemon_agent`/`child_agent`. `None` preserves
    /// the turn loop's built-in default deadline.
    pub turn_timeout_secs: Option<u64>,
    /// `[agent] cycle_detection` — arm the small-cycle breaker. Threaded
    /// into `TurnSafety::interactive(...)` alongside `turn_timeout_secs`.
    pub cycle_detection: Option<bool>,
    /// `[agent] injection_scan_enabled` — Chapter Picket's active-scan
    /// on/off. See `aivyx_core::TurnSafety` for the full contract.
    pub injection_scan_enabled: bool,
    /// `[agent] injection_scan_exempt` — per-tool-name exemption list for
    /// the active scan. See `aivyx_core::TurnSafety` for the full contract.
    pub injection_scan_exempt: std::collections::BTreeSet<String>,
    /// Task 4 security-audit fix round 3 — `[access] confirm_destructive`.
    /// Threaded into `ConcreteAgent::with_confirm_destructive(...)` at the
    /// construction site below, same pattern as `injection_scan_enabled`.
    /// `false` preserves pre-fix behavior byte-for-byte.
    pub confirm_destructive: bool,
}

/// Per-channel session report. Returned by an inner mailbox
/// task when it exits.
#[derive(Debug, Clone)]
pub struct SlackSessionReport {
    pub turns_run: usize,
}

/// Multi-channel session report aggregated by the outer
/// multiplexer at shutdown.
#[derive(Debug, Clone, Default)]
pub struct SlackMultiSessionReport {
    /// Per-channel turn counts, keyed on the Q3a-resolved
    /// `"{team_id}:{channel_id}"` partition key.
    pub turns_by_partition: HashMap<String, usize>,
}

impl SlackMultiSessionReport {
    /// Sum of every per-channel `turns_run`.
    pub fn total_turns(&self) -> usize {
        self.turns_by_partition.values().sum()
    }
}

// ---------------------------------------------------------------------------
// Inner mailbox task — structurally parallel to
// `aivyx_discord::run_discord_session_with_mailbox`
// ---------------------------------------------------------------------------

/// Inner-task entry point for the multi-channel pump. One of
/// these runs per active Slack `(team_id, channel_id)`
/// partition, driven by messages posted to `mailbox` from the
/// outer multiplexer.
pub(crate) async fn run_slack_session_with_mailbox<T>(
    channel: Arc<SlackChannel<T>>,
    config: SlackSessionConfig,
    provider: Arc<dyn LlmProvider>,
    audit: Arc<dyn AuditHook>,
    checkpointer: Option<Arc<aivyx_core::GitCheckpointer>>,
    mut mailbox: mpsc::Receiver<IncomingMessage>,
    shutdown: CancellationToken,
) -> Result<SlackSessionReport, String>
where
    T: SlackTransport + 'static,
{
    let registry = config.tools;
    let _storage = config.storage;

    let provider_for_factory = Arc::clone(&provider);
    let registry_for_factory = Arc::clone(&registry);
    let planner_config = LlmPlannerConfig::new(config.model)
        .with_system_prompt(config.system_prompt)
        .with_max_tokens(config.max_tokens)
        .with_tool_allowlist(config.tool_allowlist.clone());

    let agent = ConcreteAgent::new(
        AgentId::new(),
        config.capabilities,
        registry,
        audit,
        move || {
            Box::new(LlmPlanner::new(
                Arc::clone(&provider_for_factory),
                Arc::clone(&registry_for_factory),
                planner_config.clone(),
            ))
        },
    )
    .with_tool_allowlist(config.tool_allowlist)
    .with_memory_topic_prefix(config.memory_topic_prefix)
    .with_checkpointer(checkpointer)
    // Task 4 security-audit fix round 3 — same [access] confirm_destructive
    // posture as every other agent construction path.
    .with_confirm_destructive(config.confirm_destructive);
    // Route through the shared per-turn-safety choke point with the
    // operator's configured values.
    let agent = aivyx_core::TurnSafety::interactive(
        config.turn_timeout_secs,
        config.cycle_detection,
        config.injection_scan_enabled,
        config.injection_scan_exempt,
    )
    .apply(agent);

    let mut turns_run: usize = 0;
    let mut pending: VecDeque<IncomingMessage> = VecDeque::new();
    let target_partition = channel.partition_key();

    loop {
        if shutdown.is_cancelled() {
            return Ok(SlackSessionReport { turns_run });
        }

        if pending.is_empty() {
            if channel.cancellation_token().is_cancelled() {
                return Ok(SlackSessionReport { turns_run });
            }

            tokio::select! {
                biased;

                _ = shutdown.cancelled() => {
                    return Ok(SlackSessionReport { turns_run });
                }

                maybe_msg = mailbox.recv() => {
                    match maybe_msg {
                        Some(msg) => {
                            if msg.text.trim() == "/cancel" {
                                continue;
                            }
                            // Defense-in-depth: the outer
                            // multiplexer routes by partition
                            // key, but drop foreign-partition
                            // messages if any slip through.
                            if msg.partition_key() != target_partition {
                                continue;
                            }
                            pending.push_back(msg);
                        }
                        None => {
                            return Ok(SlackSessionReport { turns_run });
                        }
                    }
                }
            }
        }

        let msg = pending.pop_front().expect("pending is non-empty here");

        channel.reset_cancellation();

        let message = Message::text(channel.session_id(), &msg.text);
        let turn_fut = agent.turn(message, channel.as_ref());
        tokio::pin!(turn_fut);

        let _outcome = loop {
            tokio::select! {
                biased;

                outcome = &mut turn_fut => {
                    break outcome;
                }

                maybe_msg = mailbox.recv() => {
                    match maybe_msg {
                        Some(msg) => {
                            if msg.partition_key() != target_partition {
                                continue;
                            }
                            if msg.text.trim() == "/cancel" {
                                channel.cancellation_token().cancel();
                            } else {
                                pending.push_back(msg);
                            }
                        }
                        None => {
                            channel.cancellation_token().cancel();
                        }
                    }
                }
            }
        };
        turns_run += 1;
    }
}

// ---------------------------------------------------------------------------
// Outer multiplexer
// ---------------------------------------------------------------------------

/// One per-partition route: mpsc sender + the spawned inner
/// task's JoinHandle. Keyed on the Q3a partition string.
struct PartitionRoute {
    sender: mpsc::Sender<IncomingMessage>,
    handle: JoinHandle<Result<SlackSessionReport, String>>,
}

/// Drive a multi-channel Slack session to completion against
/// a real bot token + app-level token. Production entry point
/// for the `aivyx-pa --channel slack` binary path.
///
/// ## Parameters
///
/// - `channel_name` — base label spawned channels carry
///   through audit events.
/// - `bot_token` — `xoxb-...` Slack bot token. Passed to
///   `SlackMorphismTransport::connect`.
/// - `app_token` — `xapp-...` Slack app-level Socket Mode
///   token. Same destination.
/// - `config` — template [`SlackSessionConfig`] cloned per
///   inner task.
/// - `team_filter` — security-audit fix (Task 10, 2026-09-16). The
///   operator's configured `SlackConfig::team_id` workspace
///   constraint, if any. Consulted by `trust_tier()` alongside
///   `channel_filter` below; see `SlackChannel::trust_tier()`.
/// - `channel_filter` — security-audit fix (Task 10, 2026-09-16).
///   `Some(channel_id)` names the one Slack channel the operator has
///   allowlisted as `SemiTrusted`; every other channel (mismatched,
///   or `None` meaning no channel is allowlisted at all) is
///   `Untrusted` instead. Mirrors `aivyx_telegram`'s `chat_filter`/
///   `aivyx_discord`'s `channel_filter`.
/// - `provider` / `audit` — shared across all inner tasks.
/// - `shutdown` — ctrl-C-driven token; cancels the outer
///   loop and cascades into inner-task drain.
#[allow(clippy::too_many_arguments)]
pub async fn run_slack_session(
    channel_name: impl Into<String> + Clone,
    bot_token: &str,
    app_token: &str,
    team_filter: Option<String>,
    channel_filter: Option<String>,
    config: SlackSessionConfig,
    provider: Arc<dyn LlmProvider>,
    audit: Arc<dyn AuditHook>,
    checkpointer: Option<Arc<aivyx_core::GitCheckpointer>>,
    shutdown: CancellationToken,
) -> Result<SlackMultiSessionReport, String> {
    let transport = Arc::new(
        SlackMorphismTransport::connect(bot_token, app_token)
            .await
            .map_err(|e| format!("slack: connect failed: {e}"))?,
    );
    run_slack_session_with_transport(
        channel_name,
        transport,
        team_filter,
        channel_filter,
        config,
        provider,
        audit,
        checkpointer,
        shutdown,
    )
    .await
}

/// Transport-generic multi-channel driver. Production callers
/// go through [`run_slack_session`]; tests call this directly
/// with a `ScriptedTransport`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_slack_session_with_transport<T>(
    channel_name: impl Into<String> + Clone,
    transport: Arc<T>,
    team_filter: Option<String>,
    channel_filter: Option<String>,
    config: SlackSessionConfig,
    provider: Arc<dyn LlmProvider>,
    audit: Arc<dyn AuditHook>,
    checkpointer: Option<Arc<aivyx_core::GitCheckpointer>>,
    shutdown: CancellationToken,
) -> Result<SlackMultiSessionReport, String>
where
    T: SlackTransport + 'static,
{
    let base_name: String = channel_name.into();
    let mut routes: HashMap<String, PartitionRoute> = HashMap::new();

    loop {
        if shutdown.is_cancelled() {
            break;
        }

        let msg = tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            res = transport.next_message() => match res {
                Ok(m) => m,
                Err(e) => {
                    eprintln!(
                        "aivyx-slack(multi): next_message failed ({e}); backing off 1s"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    continue;
                }
            }
        };

        let partition = msg.partition_key();

        // Lazy-spawn the inner task the first time we see
        // this (team_id, channel_id). Each inner task gets
        // its own SlackChannel<T> bound to the partition, its
        // own mailbox receiver, and cloned shared state.
        let route = routes.entry(partition.clone()).or_insert_with(|| {
            let (tx, rx) = mpsc::channel::<IncomingMessage>(CHANNEL_MAILBOX_CAPACITY);
            let schannel = Arc::new(SlackChannel::new(
                base_name.clone(),
                msg.team_id.clone(),
                msg.channel_id.clone(),
                team_filter.clone(),
                channel_filter.clone(),
                Arc::clone(&transport),
            ));
            let config_clone = config.clone();
            let provider_clone = Arc::clone(&provider);
            let audit_clone = Arc::clone(&audit);
            let checkpointer_clone = checkpointer.clone();
            let shutdown_clone = shutdown.clone();
            let handle = tokio::spawn(async move {
                run_slack_session_with_mailbox(
                    schannel,
                    config_clone,
                    provider_clone,
                    audit_clone,
                    checkpointer_clone,
                    rx,
                    shutdown_clone,
                )
                .await
            });
            PartitionRoute { sender: tx, handle }
        });

        if let Err(e) = route.sender.send(msg).await {
            eprintln!(
                "aivyx-slack(multi): partition {partition} mailbox send failed ({e}); dropping route"
            );
            routes.remove(&partition);
        }
    }

    // Shutdown drain: same shape as Discord — drop every
    // sender so inner tasks see mailbox close, then join +
    // aggregate.
    let mut turns_by_partition: HashMap<String, usize> = HashMap::new();
    let drained: Vec<(String, PartitionRoute)> = routes.drain().collect();
    for (partition, PartitionRoute { sender, handle }) in drained {
        drop(sender);
        match handle.await {
            Ok(Ok(report)) => {
                turns_by_partition.insert(partition, report.turns_run);
            }
            Ok(Err(e)) => {
                eprintln!(
                    "aivyx-slack(multi): partition {partition} inner task errored: {e}"
                );
            }
            Err(join_err) => {
                eprintln!(
                    "aivyx-slack(multi): partition {partition} inner task join failed: {join_err}"
                );
            }
        }
    }

    Ok(SlackMultiSessionReport { turns_by_partition })
}
