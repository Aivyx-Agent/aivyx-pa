//! `run_discord_session` — the Discord analogue of
//! [`aivyx_channel::run_session`] / `aivyx_telegram::run_telegram_session`.
//!
//! Discord's Gateway protocol fundamentally simplifies one piece
//! of the Telegram session machinery: there is no `scan_for_cancel`
//! probe and no `get_updates` cursor. The Gateway is a continuous
//! event stream — `transport.next_message().await` is the entire
//! "wait for input" surface, and Discord's API does not enforce
//! "one concurrent reader per token" the way Telegram's Bot API
//! 409 Conflict does.
//!
//! That collapses Phase 8/9's two-tier session shape (single-chat
//! seam + multi-chat outer multiplexer + per-chat inner task) into
//! a one-tier shape: one outer multiplexer pumps the shard, one
//! inner mailbox task per channel id. No degenerate "single
//! channel" entry point — Discord's natural shape is multi-channel
//! from one shard, and pretending otherwise would invent a
//! degenerate case.
//!
//! ## What this module owns (vs. `run_session`)
//!
//! Same as the Telegram path:
//!
//! - Builds the `ConcreteAgent` from [`DiscordSessionConfig`] in
//!   each inner task. Why a separate config type: `aivyx-discord`
//!   cannot depend on `aivyx-channel` without creating a package
//!   cycle (the `aivyx-pa` binary lives in `aivyx-channel` and
//!   imports `aivyx_discord::run_discord_session`). The shape
//!   mirrors `TelegramSessionConfig` and `aivyx_channel::SessionConfig`
//!   minus the local-only fields.
//! - Rotates the channel's cancellation token per turn
//!   (Phase 3 monotonic-token fix: a cancelled turn N must not
//!   poison turn N+1).
//! - Runs `agent.turn(message, &channel).await` for each inbound
//!   message.
//!
//! Deliberately omitted (compared to `run_session`):
//!
//! - No banner / no prompt — Discord bots don't have a
//!   session-start affordance. First user message is the banner.
//! - No session marker write — the multi-channel shape already
//!   namespaces per `channel_id` via `session_partition()`.
//! - No signal handling — the binary still owns the `ctrl_c`
//!   listener task that cancels the `shutdown` token this module
//!   receives.
//!
//! ## /cancel semantics
//!
//! Mirrors Telegram's mailbox path: a `/cancel` inbound while a
//! turn is in flight fires the channel's per-turn cancellation
//! token; a `/cancel` arriving with no turn running is a no-op
//! (dropped at the mailbox boundary). The biased `tokio::select!`
//! inside the inner task's per-turn loop is the cancel-listening
//! arm.

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

use crate::discord_channel::DiscordChannel;
use crate::transport::{DiscordTransport, IncomingMessage, TwilightTransport};

/// Per-channel mailbox capacity. Mirrors Telegram's
/// `CHAT_MAILBOX_CAPACITY` — absorbs a burst of messages from
/// one channel while the inner task is busy on a turn, applies
/// backpressure to the outer multiplexer if a single channel
/// floods past it.
const CHANNEL_MAILBOX_CAPACITY: usize = 32;

// ---------------------------------------------------------------------------
// Per-session config + reports
// ---------------------------------------------------------------------------

/// Per-session knobs for the Discord loop. Analogue of
/// `aivyx_telegram::TelegramSessionConfig` and
/// `aivyx_channel::SessionConfig` minus the local-only fields.
/// Kept as a separate type (rather than an import) to avoid a
/// package-level cycle between `aivyx-discord` and `aivyx-channel`.
///
/// Field semantics mirror the corresponding fields on
/// `SessionConfig` / `TelegramSessionConfig`. A future refactor
/// that extracts the shared shape into a leaf crate would collapse
/// all three into one type without touching any call sites.
#[derive(Clone)]
pub struct DiscordSessionConfig {
    pub model: String,
    pub system_prompt: String,
    pub max_tokens: u32,
    pub capabilities: CapabilitySet,
    pub tools: Arc<ToolRegistry>,
    pub storage: Arc<dyn Storage>,
    /// Phase 11 Task 4 — role-derived tool allowlist. See
    /// `aivyx_channel::SessionConfig::tool_allowlist` for semantics.
    /// `None` preserves legacy behavior (allow every registered tool).
    pub tool_allowlist: Option<std::collections::BTreeSet<String>>,
    /// Phase 11 Task 4 — role-derived memory-topic prefix. See
    /// `aivyx_channel::SessionConfig::memory_topic_prefix` for
    /// semantics. `None` preserves legacy behavior.
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
    /// Task 4 security-audit fix round 3 — `[access] confirm_destructive`.
    /// Threaded into `ConcreteAgent::with_confirm_destructive(...)` at the
    /// construction site below, same pattern as `injection_scan_enabled`.
    /// `false` preserves pre-fix behavior byte-for-byte.
    pub confirm_destructive: bool,
    /// `[agent] injection_scan_exempt` — per-tool-name exemption list for
    /// the active scan. See `aivyx_core::TurnSafety` for the full contract.
    pub injection_scan_exempt: std::collections::BTreeSet<String>,
}

/// Per-channel session report. Returned by an inner mailbox task
/// when it exits.
#[derive(Debug, Clone)]
pub struct DiscordSessionReport {
    pub turns_run: usize,
}

/// Multi-channel session report aggregated by the outer
/// multiplexer at shutdown. Mirrors
/// `aivyx_telegram::TelegramMultiSessionReport`.
#[derive(Debug, Clone, Default)]
pub struct DiscordMultiSessionReport {
    /// Per-channel turn counts, keyed on Discord `channel_id`
    /// (snowflake `u64`).
    pub turns_by_channel: HashMap<u64, usize>,
}

impl DiscordMultiSessionReport {
    /// Sum of every per-channel `turns_run`. Useful for tests
    /// and for operator-facing "how many turns did this process
    /// serve" logs without spelling out the full map shape.
    pub fn total_turns(&self) -> usize {
        self.turns_by_channel.values().sum()
    }
}

// ---------------------------------------------------------------------------
// Inner mailbox task — structurally parallel to
// `aivyx_telegram::run_telegram_session_with_mailbox`
// ---------------------------------------------------------------------------

/// Inner-task entry point for the multi-channel pump. One of
/// these runs per active Discord channel id, driven by messages
/// posted to `mailbox` from the outer
/// [`run_discord_multi_session_with_transport`] multiplexer.
///
/// Returns the per-channel `turns_run` count so the outer
/// multiplexer can aggregate a [`DiscordMultiSessionReport`].
///
/// ## /cancel handling
///
/// Same shape as Telegram's mailbox path:
///
/// - `/cancel` received while no turn is running: no-op
///   (dropped at the mailbox boundary).
/// - `/cancel` received during a turn: fires the channel's
///   per-turn cancellation token; the planner's biased
///   `tokio::select!` against that token resolves the in-flight
///   `agent.turn` as `TurnOutcome::Cancelled`. The `/cancel`
///   message itself is consumed — it never becomes a prompt.
/// - Other messages received during a turn: appended to the
///   pending queue in arrival order, drive subsequent turns
///   after the current one finalizes.
///
/// ## Shutdown handling
///
/// Inner task exits when (a) the shared shutdown token fires,
/// or (b) the mailbox sender is dropped — which the outer
/// multiplexer does at shutdown to signal "no more messages
/// coming for your channel." Either path drains the pending
/// queue into completed turns before returning so a user who
/// just typed a message isn't silently dropped on ctrl-C.
pub(crate) async fn run_discord_session_with_mailbox<T>(
    channel: Arc<DiscordChannel<T>>,
    config: DiscordSessionConfig,
    provider: Arc<dyn LlmProvider>,
    audit: Arc<dyn AuditHook>,
    checkpointer: Option<Arc<aivyx_core::GitCheckpointer>>,
    mut mailbox: mpsc::Receiver<IncomingMessage>,
    shutdown: CancellationToken,
) -> Result<DiscordSessionReport, String>
where
    T: DiscordTransport + 'static,
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
    let target_channel = channel.channel_id();

    loop {
        if shutdown.is_cancelled() {
            return Ok(DiscordSessionReport { turns_run });
        }

        // If there's nothing pending, block on the mailbox for
        // the next inbound message. Mirrors the Telegram
        // mailbox-path Phase 9 Task 1 placement of the channel-
        // token check: only on the pre-block path, so a per-
        // turn token left in a cancelled state by turn N
        // doesn't pre-cancel turn N+1.
        if pending.is_empty() {
            if channel.cancellation_token().is_cancelled() {
                return Ok(DiscordSessionReport { turns_run });
            }

            tokio::select! {
                biased;

                _ = shutdown.cancelled() => {
                    return Ok(DiscordSessionReport { turns_run });
                }

                maybe_msg = mailbox.recv() => {
                    match maybe_msg {
                        Some(msg) => {
                            // Top-of-loop /cancel with no turn
                            // running is a no-op. /cancel is a
                            // control signal, not a prompt —
                            // dropping it keeps the bot from
                            // responding to "/cancel" as if it
                            // were a question.
                            if msg.text.trim() == "/cancel" {
                                continue;
                            }
                            // Defense-in-depth: the outer
                            // multiplexer routes by channel_id,
                            // but drop foreign-channel messages
                            // if any slip through.
                            if msg.channel_id != target_channel {
                                continue;
                            }
                            pending.push_back(msg);
                        }
                        None => {
                            // Sender dropped — outer
                            // multiplexer is shutting us down.
                            // Nothing pending, exit cleanly.
                            return Ok(DiscordSessionReport { turns_run });
                        }
                    }
                }
            }
        }

        let msg = pending.pop_front().expect("pending is non-empty here");

        // Rotate cancellation per turn — Phase 3 monotonic-
        // token-poisons-next-turn fix. Same rationale as
        // LocalChannel::reset_cancellation and
        // TelegramChannel::reset_cancellation.
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
                            // Defense-in-depth channel_id filter.
                            if msg.channel_id != target_channel {
                                continue;
                            }
                            if msg.text.trim() == "/cancel" {
                                // Fire the per-turn cancel. The
                                // planner's own select against
                                // the channel's cancellation
                                // token (llm_planner.rs) will
                                // win the next scheduling step
                                // and resolve turn_fut as
                                // TurnOutcome::Cancelled, which
                                // the biased branch above
                                // catches on the next iteration.
                                channel.cancellation_token().cancel();
                            } else {
                                // Normal message during a
                                // running turn — queue it for
                                // after the current turn
                                // finishes. push_back so it
                                // runs in arrival order.
                                pending.push_back(msg);
                            }
                        }
                        None => {
                            // Sender dropped mid-turn. Cancel
                            // the current turn so the inner
                            // task can finalize and exit
                            // promptly. Multi-channel analogue
                            // of Telegram's mailbox-close path.
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
// Outer multiplexer — one Gateway shard pumps, fanned out per
// channel_id into mailbox-driven inner tasks
// ---------------------------------------------------------------------------

/// One per-channel route: the mpsc sender the outer loop uses
/// to deliver messages to the inner task, plus the JoinHandle
/// the outer loop awaits at shutdown. Bundled so the `HashMap`
/// stays a single lookup.
struct ChannelRoute {
    sender: mpsc::Sender<IncomingMessage>,
    handle: JoinHandle<Result<DiscordSessionReport, String>>,
}

/// Drive a multi-channel Discord session to completion against
/// a real bot token. Production entry point for the `aivyx-pa
/// --channel discord` binary path.
///
/// One aivyx-pa process, one Gateway shard, N Discord channels.
/// Each channel id the bot sees gets its own [`DiscordChannel`]
/// instance with a stable `session_partition()` keyed on the
/// snowflake — so memory partitions and audit-chain events
/// stay distinct per conversation, the same way Telegram's
/// Phase 9 multi-chat pump partitions per `chat_id`.
///
/// ## Parameters
///
/// - `channel_name` — base label spawned channels carry through
///   audit events. Per-channel disambiguation happens via
///   `session_partition()` on the channel context, not by
///   mangling names.
/// - `token` — Bot API token. Passed straight to
///   `TwilightTransport::new`; nothing in this crate logs or
///   echoes it.
/// - `config` — template [`DiscordSessionConfig`] cloned per
///   inner task at spawn time. All heavy fields are `Arc`'d.
/// - `provider` / `audit` — shared across all inner tasks. The
///   audit log is the cross-channel audit chain that records
///   turns from every channel in interleaved order.
/// - `channel_filter` — security-audit fix (Task 10, 2026-09-16).
///   `Some(channel_id)` names the one Discord channel the operator
///   has allowlisted as `SemiTrusted`; every other channel this bot
///   receives messages from (`channel_filter` mismatched, or `None`
///   meaning no channel is allowlisted at all) is `Untrusted`
///   instead. Mirrors `aivyx_telegram`'s `chat_filter`. Unlike
///   Telegram, this does not drop messages from non-matching
///   channels at the routing layer — Discord's Gateway intents
///   already gate which channels the bot's connection can see at
///   all, so a mismatched/`None` channel is still processed, just at
///   the `Untrusted` ceiling rather than silently trusted.
/// - `shutdown` — ctrl-C-driven token. When cancelled, the
///   outer loop stops pumping the shard and drops all per-
///   channel mpsc senders, cascading into each inner task's
///   mailbox-close branch.
#[allow(clippy::too_many_arguments)]
pub async fn run_discord_session(
    channel_name: impl Into<String> + Clone,
    token: &str,
    channel_filter: Option<u64>,
    config: DiscordSessionConfig,
    provider: Arc<dyn LlmProvider>,
    audit: Arc<dyn AuditHook>,
    checkpointer: Option<Arc<aivyx_core::GitCheckpointer>>,
    shutdown: CancellationToken,
) -> Result<DiscordMultiSessionReport, String> {
    let transport = Arc::new(TwilightTransport::new(token));
    run_discord_session_with_transport(
        channel_name,
        transport,
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
/// go through [`run_discord_session`]; tests call this directly
/// with a `ScriptedTransport`.
///
/// Mirrors `aivyx_telegram::run_telegram_multi_session_with_transport`'s
/// `chat_filter` knob (security-audit fix, Task 10, 2026-09-16) —
/// see `run_discord_session`'s doc for the one behavioral difference
/// (no message-dropping at the routing layer).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_discord_session_with_transport<T>(
    channel_name: impl Into<String> + Clone,
    transport: Arc<T>,
    channel_filter: Option<u64>,
    config: DiscordSessionConfig,
    provider: Arc<dyn LlmProvider>,
    audit: Arc<dyn AuditHook>,
    checkpointer: Option<Arc<aivyx_core::GitCheckpointer>>,
    shutdown: CancellationToken,
) -> Result<DiscordMultiSessionReport, String>
where
    T: DiscordTransport + 'static,
{
    let base_name: String = channel_name.into();
    let mut routes: HashMap<u64, ChannelRoute> = HashMap::new();

    // Outer event-stream loop. The single reader of the Gateway
    // shard — twilight handles heartbeat / sequence / resume
    // inside `next_message`, and any non-MessageCreate events
    // drain silently inside the transport.
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
                        "aivyx-discord(multi): next_message failed ({e}); backing off 1s"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    continue;
                }
            }
        };

        let channel_id = msg.channel_id;

        // Lazy-spawn the inner task the first time we see this
        // channel id. Each inner task gets its own
        // `DiscordChannel<T>` bound to the channel_id, its own
        // mailbox receiver, and cloned shared state.
        let route = routes.entry(channel_id).or_insert_with(|| {
            let (tx, rx) = mpsc::channel::<IncomingMessage>(CHANNEL_MAILBOX_CAPACITY);
            let dchannel = Arc::new(DiscordChannel::new(
                base_name.clone(),
                channel_id,
                channel_filter,
                Arc::clone(&transport),
            ));
            let config_clone = config.clone();
            let provider_clone = Arc::clone(&provider);
            let audit_clone = Arc::clone(&audit);
            let checkpointer_clone = checkpointer.clone();
            let shutdown_clone = shutdown.clone();
            let handle = tokio::spawn(async move {
                run_discord_session_with_mailbox(
                    dchannel,
                    config_clone,
                    provider_clone,
                    audit_clone,
                    checkpointer_clone,
                    rx,
                    shutdown_clone,
                )
                .await
            });
            ChannelRoute { sender: tx, handle }
        });

        // Deliver the message to the inner task. A `.send().await`
        // blocks briefly if the mailbox is full, applying the
        // intended per-channel backpressure.
        if let Err(e) = route.sender.send(msg).await {
            eprintln!(
                "aivyx-discord(multi): channel {channel_id} mailbox send failed ({e}); dropping route"
            );
            routes.remove(&channel_id);
        }
    }

    // Shutdown drain: drop every sender so inner tasks see
    // mailbox close, then join each handle and aggregate turn
    // counts. Dropping senders is the signal; joining collects
    // the reports.
    let mut turns_by_channel: HashMap<u64, usize> = HashMap::new();
    let drained: Vec<(u64, ChannelRoute)> = routes.drain().collect();
    for (channel_id, ChannelRoute { sender, handle }) in drained {
        drop(sender);
        match handle.await {
            Ok(Ok(report)) => {
                turns_by_channel.insert(channel_id, report.turns_run);
            }
            Ok(Err(e)) => {
                eprintln!(
                    "aivyx-discord(multi): channel {channel_id} inner task errored: {e}"
                );
            }
            Err(join_err) => {
                eprintln!(
                    "aivyx-discord(multi): channel {channel_id} inner task join failed: {join_err}"
                );
            }
        }
    }

    Ok(DiscordMultiSessionReport { turns_by_channel })
}
