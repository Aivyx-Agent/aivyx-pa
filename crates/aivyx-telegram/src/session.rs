//! `run_telegram_session` — the Telegram analogue of
//! [`aivyx_channel::run_session`].
//!
//! ## Why a sibling function and not a shared abstraction
//!
//! `aivyx_channel::run_session` takes `channel: LocalChannel<W>` +
//! `R: BufRead` as concrete parameters. A Telegram adapter has
//! neither: there is no local writer to hand it, and inbound messages
//! arrive via a long-poll cursor, not a line-oriented reader. Two
//! shapes to resolve this were considered in PHASE_8.md Task 4:
//!
//! 1. Generalize `run_session` to take `&dyn ChannelContext` plus an
//!    abstract input source trait.
//! 2. **Write a sibling `run_telegram_session` that owns its own
//!    long-poll loop.**
//!
//! We picked (2). The local and Telegram lifecycles are different
//! enough — pulled line-by-line vs. pushed long-poll batches — that
//! shoehorning them into one trait would invent an abstraction that
//! has exactly two implementations and would need to be rethought
//! the moment a third adapter (webhook Matrix? push-driven Discord
//! gateway?) arrives. The ~100 lines of "duplicated" wiring here is
//! honest — it's the price of keeping each adapter's event-pump
//! code legible in isolation.
//!
//! ## What this function owns (vs. `run_session`)
//!
//! Same as the local path:
//!
//! - Builds the `ConcreteAgent` from [`TelegramSessionConfig`] —
//!   the Telegram-flavored analogue of `aivyx_channel::SessionConfig`.
//!   Why a separate type: `aivyx-telegram` cannot depend on
//!   `aivyx-channel` without creating a package cycle (the `aivyx-pa`
//!   binary lives in `aivyx-channel` and will import the Telegram
//!   entry point). See [`TelegramSessionConfig`] for the exact
//!   shape; the binary converts its `SessionConfig` fields over
//!   field-by-field at the call site.
//! - Rotates the channel's cancellation token per turn (Phase 3
//!   monotonic-token fix: a cancelled turn must not poison turn N+1).
//! - Runs `agent.turn(message, &channel).await` for each inbound
//!   message.
//!
//! Deliberately omitted (compared to `run_session`):
//!
//! - **No session marker write under `KeyDomain::Sessions`.** The
//!   local session marker is a single-row "current session" record
//!   keyed on a fixed key. A Telegram bot serves many chats from one
//!   process; the analogous "per-chat session marker" would need a
//!   different schema (one row per chat_id) and a new key convention.
//!   Phase 8 defers that work — see PHASE_8.md Task 4 ship record.
//!   The `storage` field on `TelegramSessionConfig` is still carried
//!   through, so a future refinement can wire markers without
//!   changing the call-site shape.
//! - **No banner.** Telegram bots don't have a "session start"
//!   affordance the way a terminal REPL does; the first user message
//!   is the banner. A startup-ping message could be a Task 7 smoke-
//!   test concern.
//! - **No prompt string.** Same reason — Telegram's "prompt" is the
//!   user pressing Send, not a character printed by the bot.
//! - **No signal handling.** The binary still owns the `ctrl_c`
//!   listener task that cancels the `shutdown` token this function
//!   receives as a parameter; the loop only checks the token state
//!   at the top of each iteration.
//!
//! ## The long-poll cursor
//!
//! `get_updates(offset, timeout_secs)` is the Bot API's long-poll
//! entry point: the server holds the request open up to `timeout_secs`
//! seconds, and returns as soon as any updates with `update_id >=
//! offset` arrive (or an empty list on timeout). `offset` is the
//! **acknowledgment cursor** — passing `last_seen + 1` tells Telegram
//! "I've handled everything up to `last_seen`, don't send them again."
//!
//! We keep a local `i64` cursor initialized to 0. After each batch of
//! updates, we advance the cursor to `max(update_id) + 1`. A Telegram
//! server restart or offset reset does not cause replay — the cursor
//! is monotonic within the process lifetime.
//!
//! ## Chat filtering
//!
//! The current design binds one `TelegramChannel` to one `chat_id`
//! (Phase 8 Task 1's simplification). `get_updates` will return
//! updates for **every chat the bot is in**, not just the target one,
//! so we filter inbound messages by `chat_id` at the top of the loop.
//! Messages from other chats are silently dropped in Phase 8; a
//! multi-chat pump that spawns one `TelegramChannel` per chat_id and
//! routes accordingly is a Phase 9 concern.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use aivyx_capability::CapabilitySet;
use aivyx_core::{
    agent::ConcreteAgent, llm_planner::LlmPlanner, planner::ToolRegistry, Agent, AgentId,
    AuditHook, CancellationToken, ChannelContext, LlmPlannerConfig, Message,
};
use aivyx_llm::LlmProvider;
use aivyx_storage::Storage;

use crate::telegram_channel::TelegramChannel;
use crate::transport::{IncomingMessage, ReqwestTransport, TelegramTransport, TransportError};

/// Per-session knobs for the Telegram loop. Analogue of
/// [`aivyx_channel::SessionConfig`], minus the local-only `prompt`
/// and `banner` fields. Kept as a separate type (rather than an
/// import) to avoid a package-level cycle between `aivyx-telegram`
/// and `aivyx-channel` — see the `Cargo.toml` comment for details.
///
/// Field semantics are identical to the corresponding fields on
/// `SessionConfig`. A future refactor that extracts the shared shape
/// into a third crate would collapse both into one type without
/// touching any call sites.
#[derive(Clone)]
pub struct TelegramSessionConfig {
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
    /// Threaded into `ConcreteAgent::with_confirm_destructive(...)` at each
    /// construction site below, same pattern as `injection_scan_enabled`.
    /// `false` preserves pre-fix behavior byte-for-byte.
    pub confirm_destructive: bool,
    /// `[agent] injection_scan_exempt` — per-tool-name exemption list for
    /// the active scan. See `aivyx_core::TurnSafety` for the full contract.
    pub injection_scan_exempt: std::collections::BTreeSet<String>,
}

/// How long to hold each `getUpdates` request open (seconds).
///
/// Telegram's Bot API allows up to 50; we pick a conservative 25 so a
/// bot that is killed mid-poll comes back within ~half a minute. Tests
/// override this via `run_telegram_session_with_transport` so they
/// don't wait on real-world timeouts.
const LONG_POLL_TIMEOUT_SECS: u32 = 25;

/// How long each `scan_for_cancel` `getUpdates` call holds the connection
/// open, in seconds. The Phase 8 Q8 design note called out ~2 seconds
/// as the sweet spot: long enough that a user typing `/cancel` during
/// a 30-second turn has multiple scan iterations to land on, short
/// enough that a normal fast turn doesn't pay a noticeable wait cost
/// on the losing select arm when the turn finishes quickly.
///
/// Tests override this via `run_telegram_session_with_transport_ex`.
const SCAN_FOR_CANCEL_TIMEOUT_SECS: u32 = 2;

/// Result of one `scan_for_cancel` probe. The session loop consumes
/// this to advance its shared `offset` cursor and to reshuffle any
/// updates the scan saw into the next turn's pending queue.
///
/// **Why the scan returns messages the turn loop has to re-queue, not
/// the whole next-turn decision.** The scan is a transport-layer
/// helper; the turn-loop logic of "what counts as a cancel" and "what
/// to do next" stays in `run_telegram_session_with_transport`. The
/// scan's only job is to answer: "did a `/cancel` land, and what
/// other target-chat messages arrived in the same batch?"
///
/// **Design note on queueing vs. redelivery (PHASE_8.md:1376–1380
/// open question).** We pick queueing: messages that arrive *alongside*
/// `/cancel` in the same scan batch are appended to the session
/// loop's pending queue in `update_id` order and drive subsequent
/// turns. The rejected alternative was "drop them, let Telegram
/// redeliver next poll" — simpler, but worse UX. A user who types
/// "do X" then immediately "/cancel" shouldn't lose the X; a user
/// who types "/cancel" then immediately "do Y" shouldn't lose the Y.
/// Queueing preserves both.
#[derive(Debug)]
enum ScanResult {
    /// The scan batch did not contain a `/cancel` from the target chat.
    /// `queued` is any target-chat messages the scan *did* see (the
    /// main loop prepends these to its pending queue; they're
    /// non-cancel messages that the main loop can process after the
    /// current turn finishes). `max_update_id` is the highest
    /// `update_id` the scan observed from *any* chat, which the main
    /// loop uses to advance the offset cursor past this batch so
    /// Telegram doesn't redeliver it on the next `get_updates` call.
    NoCancel {
        max_update_id: Option<i64>,
        queued: Vec<IncomingMessage>,
    },
    /// The scan batch contained a `/cancel` from the target chat. The
    /// session loop cancels the channel's per-turn token so the
    /// in-flight `agent.turn` resolves as `Cancelled`. `queued` is any
    /// target-chat messages in the same batch with `update_id` other
    /// than `cancel_update_id`, in batch order — they are re-queued
    /// for subsequent turns per the queueing-over-redelivery design.
    /// `cancel_update_id` is the highest `update_id` between `/cancel`
    /// itself and any observed queued-message update_ids, used to
    /// advance the offset cursor.
    FoundCancel {
        cancel_update_id: i64,
        queued: Vec<IncomingMessage>,
    },
}

/// Probe the Bot API for a short window, looking for a `/cancel`
/// command from `target_chat`. This is the "scanning arm" of the
/// `tokio::select!` inside the session loop's per-turn block — the
/// other arm is `agent.turn(...)` itself.
///
/// **Cancellation-safety invariant the caller relies on.** When the
/// turn arm wins the `select!`, this future is dropped mid-`await` on
/// `get_updates`. For the production `ReqwestTransport`, dropping the
/// reqwest future cancels the in-flight HTTP request *before* the Bot
/// API's server-side cursor advances — so the next main-loop
/// `get_updates(offset, ...)` with the same `offset` reproduces the
/// same batch (or a superset). For the test `ScriptedTransport`,
/// `get_updates` eagerly drains its internal queue, which is a test-
/// artifact that does *not* model production precisely; the tests
/// compensate by driving the scan to completion and reading the
/// returned `ScanResult` rather than relying on select-drop.
///
/// **`/cancel` detection.** A message counts as a cancel iff its
/// `text.trim() == "/cancel"` (case-sensitive, no arguments). Bot
/// Mention forms like `/cancel@MyBotName` are a Phase 9+ refinement —
/// they require reading the bot's `getMe` username, which this seam
/// doesn't carry. A future task can thread the username through
/// `TelegramSessionConfig` if the simpler form proves insufficient.
///
/// **Non-target-chat messages.** A scan batch can return messages
/// for *any* chat this bot is in (Bot API behavior). Anything that
/// isn't for `target_chat` is silently dropped here, mirroring the
/// main-loop filter. Its `update_id` still contributes to
/// `max_update_id` so the cursor advances past it.
async fn scan_for_cancel<T: TelegramTransport + ?Sized>(
    transport: &T,
    offset: i64,
    target_chat: i64,
    scan_timeout_secs: u32,
) -> Result<ScanResult, TransportError> {
    let batch = transport.get_updates(offset, scan_timeout_secs).await?;

    let mut max_update_id: Option<i64> = None;
    let mut queued: Vec<IncomingMessage> = Vec::new();
    let mut cancel_update_id: Option<i64> = None;

    for msg in batch {
        max_update_id = Some(max_update_id.map_or(msg.update_id, |m| m.max(msg.update_id)));

        if msg.chat_id != target_chat {
            continue;
        }

        if msg.text.trim() == "/cancel" {
            // Remember only the *first* /cancel in the batch. A batch
            // with multiple cancels is a pathological case (the user
            // mashed the command), and one cancel is enough to fire
            // the branch. Subsequent cancels are dropped (they'd
            // cancel an already-cancelled turn).
            if cancel_update_id.is_none() {
                cancel_update_id = Some(msg.update_id);
            }
            // Do NOT queue the /cancel message itself — it is a
            // control signal, not a prompt. A user shouldn't see the
            // bot respond to "/cancel" as if it were a question.
        } else {
            queued.push(msg);
        }
    }

    if let Some(cancel_id) = cancel_update_id {
        // The cancel_update_id returned to the caller is the *cursor
        // advance target*: the highest update_id the scan observed,
        // whether that's the cancel itself, a queued normal message,
        // or a non-target-chat message. The main loop will advance
        // `offset` to `cancel_update_id + 1`.
        let advance_to = max_update_id.unwrap_or(cancel_id).max(cancel_id);
        Ok(ScanResult::FoundCancel {
            cancel_update_id: advance_to,
            queued,
        })
    } else {
        Ok(ScanResult::NoCancel {
            max_update_id,
            queued,
        })
    }
}

/// Summary of what one Telegram session did, returned after the long-
/// poll cursor is shut down. Matches the shape of
/// [`aivyx_channel::SessionReport`] deliberately — a future refactor
/// that unifies the two session functions would fold these into one
/// type. Kept separate for now so the Phase 8 empty-diff streak on
/// `aivyx-channel` isn't touched.
#[derive(Debug, Clone)]
pub struct TelegramSessionReport {
    /// Number of inbound text messages that drove a turn to completion.
    pub turns_run: usize,
}

/// Phase 9 Task 2 multi-chat report. One aivyx-pa process can now drive N
/// chats concurrently through a single outer multiplexer; this report
/// collapses each inner task's [`TelegramSessionReport`] into a per-
/// chat map plus a total.
///
/// The per-chat map key is `chat_id`. A chat appears in the map iff an
/// inner task was spawned for it during the session — chats that never
/// sent a message never incur an entry, which matches the lazy-spawn
/// policy in the multiplexer.
#[derive(Debug, Clone, Default)]
pub struct TelegramMultiSessionReport {
    /// Per-chat turn counts, keyed on `chat_id`.
    pub turns_by_chat: HashMap<i64, usize>,
}

impl TelegramMultiSessionReport {
    /// Sum of all per-chat `turns_run` values. Useful for tests and
    /// for operator-facing "how many turns did this process serve"
    /// logs without having to spell out the full map shape.
    pub fn total_turns(&self) -> usize {
        self.turns_by_chat.values().sum()
    }
}

/// Drive a Telegram session to completion against a real bot token.
///
/// This is the production entry point for the `aivyx-pa --channel
/// telegram` binary path. It builds a [`TelegramChannel`] over the
/// production `ReqwestTransport`, then delegates to the generic
/// [`run_telegram_session_with_transport`] that unit tests also call.
///
/// ## Parameters
///
/// - `channel_name` — human label for the channel, surfaced in audit
///   events and error messages. Defaults usefully to `"aivyx-telegram"`
///   in the binary.
/// - `token` — the Bot API token. Passed straight through to
///   `frankenstein::client_reqwest::Bot::new`; nothing in this crate
///   logs or echoes it. The caller is responsible for sourcing it
///   safely (the binary reads `AIVYX_PA_TELEGRAM_TOKEN` from the
///   environment — see PHASE_8.md Q1 resolution).
/// - `chat_id` — the Telegram chat this session is bound to. One
///   channel per chat_id is Phase 8's simplification.
/// - `config` — reused `SessionConfig` from the local path. The
///   `banner` and `prompt` fields are ignored for Telegram (see the
///   module doc).
/// - `provider` / `audit` — same trait objects the local binary
///   hands to `run_session`. Identical contracts.
/// - `shutdown` — an external cancellation token the binary's ctrl-C
///   signal handler can cancel to tell the session to exit after the
///   current long-poll batch drains. Passed separately from the
///   channel's own cancellation token because the channel's token
///   rotates per-turn (a turn N cancel must not poison turn N+1, see
///   PHASE_8.md Q5 and `LocalChannel::reset_cancellation`), so it
///   isn't a stable shutdown signal. This token is checked at the
///   top of each loop iteration.
pub async fn run_telegram_session(
    channel_name: impl Into<String>,
    token: &str,
    chat_id: i64,
    config: TelegramSessionConfig,
    provider: Arc<dyn LlmProvider>,
    audit: Arc<dyn AuditHook>,
    shutdown: CancellationToken,
) -> Result<TelegramSessionReport, String> {
    let transport = Arc::new(ReqwestTransport::new(token));
    let channel = Arc::new(TelegramChannel::new(
        channel_name,
        chat_id,
        Arc::clone(&transport),
    ));
    run_telegram_session_with_transport(
        channel,
        config,
        provider,
        audit,
        LONG_POLL_TIMEOUT_SECS,
        shutdown,
    )
    .await
}

/// Transport-generic session driver. The production path calls this
/// with a `TelegramChannel<ReqwestTransport>`; unit tests call it with
/// a `TelegramChannel<ScriptedTransport>`. The same function body
/// drives both.
///
/// Visibility is `pub(crate)` rather than `pub` because the private
/// `TelegramTransport` trait appears in the bound — exposing this
/// publicly would leak the trait. The public surface for production
/// callers is [`run_telegram_session`] above; the test surface is
/// this function, called from within the crate's own `tests` module.
pub(crate) async fn run_telegram_session_with_transport<T>(
    channel: Arc<TelegramChannel<T>>,
    config: TelegramSessionConfig,
    provider: Arc<dyn LlmProvider>,
    audit: Arc<dyn AuditHook>,
    long_poll_timeout_secs: u32,
    shutdown: CancellationToken,
) -> Result<TelegramSessionReport, String>
where
    T: TelegramTransport + 'static,
{
    // ---- Agent stack --------------------------------------------------
    // Exactly the same shape as `run_session`: a fresh `LlmPlanner`
    // per turn, captured by the factory closure. Keeping this in lock-
    // step with the local path means a planner-state bug that shows
    // up locally also shows up through Telegram (and vice versa),
    // which is the invariant we want.
    let registry = config.tools;
    let _storage = config.storage; // Kept alive; not used for markers this phase.

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

    // ---- Long-poll loop ----------------------------------------------
    //
    // `offset` is the "give me everything with update_id >= offset"
    // cursor. 0 on first iteration is the Bot API's "send me everything
    // you've got buffered for this bot" sentinel; subsequent iterations
    // advance to `max(update_id) + 1`.
    //
    // `pending` is a per-target-chat queue of messages waiting to be
    // turned into agent turns. It is usually refilled from the main
    // `get_updates` call at the top of each outer iteration, but the
    // `/cancel` scan arm can also push messages it saw *during* a turn
    // onto this queue (at the front, if a cancel arrived and pre-cancel
    // messages need to run before the current turn's replacement; at
    // the back, otherwise). This is the queueing-over-redelivery
    // choice documented on `ScanResult`.
    let mut offset: i64 = 0;
    let mut turns_run: usize = 0;
    let mut pending: VecDeque<IncomingMessage> = VecDeque::new();
    let target_chat = channel.chat_id();
    let transport = channel.transport();

    loop {
        // Process-wide shutdown signal is checked every outer iteration:
        // a ctrl-C received between turns (or between long-polls) must
        // exit immediately rather than stalling up to
        // `long_poll_timeout_secs` seconds waiting for the Bot API to
        // return an empty batch.
        if shutdown.is_cancelled() {
            return Ok(TelegramSessionReport { turns_run });
        }

        // If there's nothing pending, refill from a fresh long-poll.
        // When the scan arm has queued messages from a mid-turn batch,
        // we skip this — we'd prefer to drain the scan-provided queue
        // first before paying for another round-trip.
        if pending.is_empty() {
            // Channel-token fast path: the per-turn token is rotated
            // at the top of each turn (see `reset_cancellation` below)
            // so it is a turn-internal signal, not an inter-turn one.
            // Between turns the token is free to carry the previous
            // turn's cancelled state — we must only check it when
            // we're about to *block* on a long-poll, so that a ctrl-C
            // equivalent that happened to land between a turn and its
            // long-poll can short-circuit the wait. The
            // `tests::run_telegram_session_drives_two_scripted_turns`
            // test relies on this: it cancels the channel token
            // externally (simulating a shutdown that lands between
            // turns) and expects the session to exit on the next
            // pre-poll check.
            //
            // Before Phase 9 Task 1 this check lived above the
            // `pending.is_empty()` branch and fired on *every* outer
            // iteration, which was fine when the old loop also did
            // all per-turn work inside the same outer iteration. With
            // Task 1's `pending: VecDeque` carrying across outer
            // iterations, that placement was subtly wrong: it would
            // observe the still-cancelled per-turn token *between*
            // turns of one long-poll batch and exit before turn N+1
            // got a chance to rotate the slot. Moving it here — only
            // on the pre-long-poll path — restores the Phase 8 Task 5
            // "cancelled turn 1, then turn 2 runs normally" invariant.
            if channel.cancellation_token().is_cancelled() {
                return Ok(TelegramSessionReport { turns_run });
            }

            let updates = match transport
                .get_updates(offset, long_poll_timeout_secs)
                .await
            {
                Ok(batch) => batch,
                Err(e) => {
                    // Platform errors at the poll layer are non-fatal:
                    // Bot API 5xx, transient network flakiness, rate-
                    // limit 429s. Log to stderr and back off briefly
                    // so we don't hot-loop a broken network. Same
                    // shape as the session-marker error handling in
                    // `run_session`: a degraded transport should
                    // never take down the bot's ability to serve
                    // later messages.
                    eprintln!("aivyx-telegram: get_updates failed ({e}); backing off 1s");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };

            if updates.is_empty() {
                // Empty batch — Bot API long-poll timed out with no
                // new messages. Loop immediately to re-arm.
                continue;
            }

            for msg in updates {
                // Advance the cursor regardless of whether we handle
                // the message, so a malformed message from a chat
                // we're not targeting doesn't cause the same update
                // to be redelivered next poll.
                offset = offset.max(msg.update_id + 1);

                if msg.chat_id != target_chat {
                    // Multi-chat pumping is a Phase 9 concern; for
                    // now, drop anything that isn't for this
                    // channel's chat.
                    continue;
                }

                // A stray top-of-loop `/cancel` with no turn running
                // is a no-op — there's nothing to cancel. We drop it
                // rather than queueing it (a user shouldn't see the
                // bot respond to "/cancel" as if it were a question;
                // same rationale as the scan arm).
                if msg.text.trim() == "/cancel" {
                    continue;
                }

                pending.push_back(msg);
            }

            if pending.is_empty() {
                // Entire batch was non-target-chat noise or /cancels
                // with no turn to cancel. Skip straight to the next
                // long-poll without trying to run a turn.
                continue;
            }
        }

        // Dequeue the next message and run a turn for it, racing
        // `scan_for_cancel` against the turn to watch for an in-band
        // `/cancel`. The scan arm never completes a turn itself — its
        // only jobs are (a) advancing `offset` past any batch it sees
        // and (b) cancelling the channel's per-turn token when it
        // observes `/cancel`, which then lets the biased turn arm win
        // the next select iteration with `TurnOutcome::Cancelled`.
        let msg = pending.pop_front().expect("pending is non-empty here");

        // Rotate cancellation per turn — identical rationale to
        // `LocalChannel::reset_cancellation` in the local path.
        // `tokio_util::CancellationToken` is monotonic, so a
        // previously-cancelled turn would poison turn N+1 if we
        // didn't swap the slot.
        channel.reset_cancellation();

        // Phase 45 — construct the right message type depending on
        // whether the inbound update carried an image payload.
        let message = match msg.image {
            Some(ref img) if msg.text.is_empty() => {
                Message::image(channel.session_id(), &img.media_type, img.data.clone())
            }
            Some(ref img) => Message::text_with_image(
                channel.session_id(),
                &msg.text,
                &img.media_type,
                img.data.clone(),
            ),
            None => Message::text(channel.session_id(), &msg.text),
        };
        let turn_fut = agent.turn(message, channel.as_ref());
        tokio::pin!(turn_fut);

        let _outcome = loop {
            tokio::select! {
                // Biased — the turn arm is checked first each poll.
                // If the turn has already resolved (common case on a
                // fast turn), we never even arm the scan and the
                // `scan_for_cancel` future is constructed and dropped
                // synchronously, paying no network round-trip.
                biased;

                outcome = &mut turn_fut => {
                    // Turn completed (or was cancelled by a previous
                    // scan-arm cancellation). Exit the per-turn select
                    // loop with the outcome. Any `ScanResult` the scan
                    // arm may have *also* seen on this iteration is
                    // discarded by the drop here — which is fine,
                    // because that batch either (a) hasn't been
                    // fetched yet (scan arm still awaiting) or
                    // (b) was fetched, and scripted-transport-drains-
                    // eagerly corner cases aside, the main loop's
                    // next `get_updates(offset, ...)` will re-fetch
                    // the same window on production `ReqwestTransport`.
                    break outcome;
                }

                scan = scan_for_cancel(
                    transport.as_ref(),
                    offset,
                    target_chat,
                    SCAN_FOR_CANCEL_TIMEOUT_SECS,
                ) => {
                    match scan {
                        Ok(ScanResult::NoCancel { max_update_id, queued }) => {
                            // No cancel this scan window; advance the
                            // cursor past whatever we observed and
                            // push any queued target-chat messages to
                            // the *back* of `pending` so the current
                            // turn finishes first, then those queued
                            // messages drive subsequent turns in
                            // arrival order.
                            if let Some(m) = max_update_id {
                                offset = offset.max(m + 1);
                            }
                            for q in queued {
                                pending.push_back(q);
                            }
                            // Loop back to arm another scan against
                            // the still-in-flight turn.
                        }
                        Ok(ScanResult::FoundCancel { cancel_update_id, queued }) => {
                            // Cancel! Advance the cursor past the
                            // cancel (and any same-batch queued
                            // messages). Prepend the queued messages
                            // to `pending` so they run *before* any
                            // messages the user types after the
                            // cancelled turn's finalize — preserving
                            // arrival order from the user's point of
                            // view.
                            offset = offset.max(cancel_update_id + 1);
                            for q in queued.into_iter().rev() {
                                pending.push_front(q);
                            }
                            // Fire the per-turn cancel. The planner's
                            // own `tokio::select!` against the
                            // channel's cancellation token (see
                            // `llm_planner.rs:176`) will win the next
                            // scheduling step and `turn_fut` will
                            // resolve as `TurnOutcome::Cancelled`,
                            // which the biased branch above then
                            // catches on the next loop iteration.
                            channel.cancellation_token().cancel();
                        }
                        Err(e) => {
                            // A scan-layer transport failure is not
                            // fatal — the turn is still running and
                            // should be allowed to finish (the user
                            // didn't ask for a cancel as far as we
                            // know). Back off briefly to avoid hot-
                            // spinning on a consistently broken scan
                            // and loop to re-arm.
                            eprintln!(
                                "aivyx-telegram: scan_for_cancel failed ({e}); dropping scan this round"
                            );
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                }
            }
        };
        turns_run += 1;
    }
}

// ============================================================================
// Phase 9 Task 2 — multi-chat pumping
// ============================================================================
//
// Everything below this line is the multi-chat path. The single-chat
// [`run_telegram_session_with_transport`] above is left structurally
// untouched so Phase 8's test suite keeps passing verbatim — zero
// regression risk on the Phase 8 cancel/finalize/scan behavior.
//
// The multi-chat design has two moving parts:
//
// 1. **Outer multiplexer** — [`run_telegram_multi_session`]. Owns the
//    single `get_updates` cursor (Bot API 409 Conflict forbids more
//    than one concurrent `getUpdates` per bot token), holds a
//    `HashMap<i64, ChatRoute>` of per-chat mailboxes and join handles,
//    and routes each inbound message to its chat's inner task via an
//    `mpsc::Sender<IncomingMessage>`.
//
// 2. **Inner task** — [`run_telegram_session_with_mailbox`]. Structurally
//    a copy of `run_telegram_session_with_transport`, but the inbound
//    source is `mpsc::Receiver<IncomingMessage>` instead of
//    `transport.get_updates`. Per-turn cancel detection is a biased
//    select between `turn_fut` and `mailbox.recv()` — a `/cancel`
//    message on the mailbox fires the per-turn token; a normal message
//    goes to the pending queue. No `scan_for_cancel` probe needed: the
//    outer multiplexer is already the only thing polling the network,
//    and it hands us pre-parsed `IncomingMessage`s synchronously.
//
// This pair preserves the Phase 8 queueing-over-redelivery contract on
// a per-chat basis: a user who types "do X" then "/cancel" then "do Y"
// in one chat has X queued before the cancel lands and Y queued after.
// Users in *other* chats are completely unaffected — their inner tasks
// have their own mailboxes, their own pending queues, and their own
// channel cancellation tokens.

/// Per-chat mailbox capacity. Sized to absorb a short burst of messages
/// from one chat while the inner task is busy on a turn, without
/// applying backpressure to the outer multiplexer's `get_updates`
/// loop. If a single chat floods past this, the outer multiplexer's
/// `.send(msg).await` will block briefly, which is the intended
/// backpressure: a misbehaving chat should throttle itself, not other
/// chats (the outer loop drains the *current* batch before looping,
/// so a brief block here doesn't starve other chats — it only delays
/// the *next* `get_updates` by the unblock time).
const CHAT_MAILBOX_CAPACITY: usize = 32;

/// Inner-task entry point for the multi-chat pump. One of these runs
/// per active chat, driven by messages posted to `mailbox` from the
/// outer [`run_telegram_multi_session`] multiplexer.
///
/// Returns the per-chat `turns_run` count so the outer multiplexer
/// can aggregate a [`TelegramMultiSessionReport`].
///
/// ## How this differs from `run_telegram_session_with_transport`
///
/// - **Inbound source:** `mpsc::Receiver<IncomingMessage>` instead of
///   `transport.get_updates(offset, timeout)`. The offset cursor lives
///   in the outer multiplexer, not here.
/// - **`/cancel` detection:** per-turn biased select races `turn_fut`
///   against `mailbox.recv()`. A `/cancel` on the mailbox fires the
///   channel token; a normal message is pushed to `pending` (front if
///   the batch also contained `/cancel`, back otherwise — matching
///   the single-chat `scan_for_cancel` `FoundCancel` vs `NoCancel`
///   semantics).
/// - **Shutdown:** the inner task exits when (a) the shutdown token
///   fires, or (b) the mailbox sender is dropped — which the outer
///   multiplexer does at shutdown to signal "no more messages coming
///   for your chat." Either path drains the pending queue into
///   completed turns before returning so a user who just typed
///   something isn't silently dropped on ctrl-C.
///
/// ## Cancel-message semantics at the mailbox boundary
///
/// A `/cancel` received while no turn is running is a no-op (same as
/// the single-chat top-of-loop drop). A `/cancel` received *during* a
/// turn fires the channel token and is itself consumed — it never
/// becomes a prompt.
pub(crate) async fn run_telegram_session_with_mailbox<T>(
    channel: Arc<TelegramChannel<T>>,
    config: TelegramSessionConfig,
    provider: Arc<dyn LlmProvider>,
    audit: Arc<dyn AuditHook>,
    checkpointer: Option<Arc<aivyx_core::GitCheckpointer>>,
    mut mailbox: mpsc::Receiver<IncomingMessage>,
    shutdown: CancellationToken,
) -> Result<TelegramSessionReport, String>
where
    T: TelegramTransport + 'static,
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
    let target_chat = channel.chat_id();

    loop {
        if shutdown.is_cancelled() {
            return Ok(TelegramSessionReport { turns_run });
        }

        // If there's nothing pending, block on the mailbox for the next
        // inbound message. A drop of the sender (outer multiplexer
        // shutting down this chat) returns `None`, which we treat as
        // "drain and exit."
        if pending.is_empty() {
            // Same channel-token pre-wait check as the single-chat
            // path: a cancel that landed between turns must be observed
            // before we block. See the long comment in
            // `run_telegram_session_with_transport` for the Phase 9
            // Task 1 placement rationale.
            if channel.cancellation_token().is_cancelled() {
                return Ok(TelegramSessionReport { turns_run });
            }

            tokio::select! {
                biased;

                _ = shutdown.cancelled() => {
                    return Ok(TelegramSessionReport { turns_run });
                }

                maybe_msg = mailbox.recv() => {
                    match maybe_msg {
                        Some(msg) => {
                            // Top-of-loop /cancel with no turn running
                            // is a no-op — drop it. Same rationale as
                            // the single-chat path.
                            if msg.text.trim() == "/cancel" {
                                continue;
                            }
                            // Messages from foreign chats should never
                            // reach this mailbox (the outer
                            // multiplexer routes by chat_id), but
                            // defense-in-depth: drop them if they do.
                            if msg.chat_id != target_chat {
                                continue;
                            }
                            pending.push_back(msg);
                        }
                        None => {
                            // Sender dropped — outer multiplexer is
                            // shutting us down. Nothing pending, so
                            // exit cleanly.
                            return Ok(TelegramSessionReport { turns_run });
                        }
                    }
                }
            }
        }

        let msg = pending.pop_front().expect("pending is non-empty here");

        channel.reset_cancellation();

        // Phase 45 — same image-aware dispatch as the single-chat path.
        let message = match msg.image {
            Some(ref img) if msg.text.is_empty() => {
                Message::image(channel.session_id(), &img.media_type, img.data.clone())
            }
            Some(ref img) => Message::text_with_image(
                channel.session_id(),
                &msg.text,
                &img.media_type,
                img.data.clone(),
            ),
            None => Message::text(channel.session_id(), &msg.text),
        };
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
                            // Defense-in-depth chat_id filter.
                            if msg.chat_id != target_chat {
                                continue;
                            }
                            if msg.text.trim() == "/cancel" {
                                // Fire the per-turn cancel; the turn
                                // arm will win the next iteration with
                                // TurnOutcome::Cancelled. /cancel is
                                // itself consumed — it never becomes
                                // a prompt.
                                channel.cancellation_token().cancel();
                            } else {
                                // Normal message during a running
                                // turn — queue it for after the
                                // current turn finishes. push_back so
                                // it runs in arrival order relative
                                // to anything else that comes in
                                // before the turn resolves.
                                pending.push_back(msg);
                            }
                        }
                        None => {
                            // Sender dropped mid-turn. Cancel the
                            // current turn so the inner task can
                            // finalize and exit promptly — this is
                            // the multi-chat analogue of the
                            // single-chat shutdown path waiting for
                            // the in-flight turn to resolve.
                            channel.cancellation_token().cancel();
                        }
                    }
                }
            }
        };
        turns_run += 1;
    }
}

/// Drive a multi-chat Telegram session to completion against a real
/// bot token. This is the Phase 9 Task 2 production entry point for
/// `aivyx-pa --channel telegram` — one aivyx-pa process, N chats, N
/// [`TelegramChannel`] instances, one shared audit chain and memory
/// store.
///
/// ## Parameters
///
/// - `channel_name` — base label for spawned channels. Each inner
///   task's channel is constructed with this name verbatim; per-chat
///   disambiguation happens via `session_partition()` on the channel
///   context, not by mangling names.
/// - `token` — Bot API token, passed straight to the production
///   `ReqwestTransport`.
/// - `chat_filter` — Phase 8 compatibility knob. `Some(chat_id)` makes
///   the outer multiplexer drop inbound messages for any chat other
///   than `chat_id` (matching Phase 8 single-chat semantics). `None`
///   accepts all chats this bot is in.
/// - `config` — template [`TelegramSessionConfig`] cloned per inner
///   task at spawn time. All heavy fields are `Arc`'d, so each clone
///   is a handful of refcount bumps.
/// - `provider` / `audit` — shared across all inner tasks. The audit
///   log is the cross-chat audit chain that records turns from every
///   chat in interleaved order.
/// - `shutdown` — ctrl-C-driven token. When cancelled, the outer loop
///   stops polling and drops all per-chat mpsc senders, which in turn
///   cancels each inner task's in-flight turn (via the mailbox-close
///   branch above) and lets them drain to completion.
#[allow(clippy::too_many_arguments)]
pub async fn run_telegram_multi_session(
    channel_name: impl Into<String> + Clone,
    token: &str,
    chat_filter: Option<i64>,
    config: TelegramSessionConfig,
    provider: Arc<dyn LlmProvider>,
    audit: Arc<dyn AuditHook>,
    checkpointer: Option<Arc<aivyx_core::GitCheckpointer>>,
    shutdown: CancellationToken,
) -> Result<TelegramMultiSessionReport, String> {
    let transport = Arc::new(ReqwestTransport::new(token));
    run_telegram_multi_session_with_transport(
        channel_name,
        transport,
        chat_filter,
        config,
        provider,
        audit,
        checkpointer,
        LONG_POLL_TIMEOUT_SECS,
        shutdown,
    )
    .await
}

/// One per-chat route: the mpsc sender the outer loop uses to deliver
/// messages to the inner task, plus the JoinHandle the outer loop
/// awaits at shutdown. Bundled so the `HashMap` stays a single lookup.
struct ChatRoute {
    sender: mpsc::Sender<IncomingMessage>,
    handle: JoinHandle<Result<TelegramSessionReport, String>>,
}

/// Transport-generic multi-chat driver. Production callers go through
/// [`run_telegram_multi_session`]; tests call this directly with a
/// `ScriptedTransport`. Mirrors the single-chat
/// [`run_telegram_session_with_transport`] seam.
///
/// The 8-argument shape mirrors the single-chat variant (7 args) plus
/// a `chat_filter`, and is expected to stay wide: channel_name, token-
/// or-transport, per-chat filter, config, provider, audit, timeout,
/// shutdown are all independent knobs that don't collapse into a
/// struct without hurting call-site legibility. Same scoped allow the
/// binary's `run_async` uses for its wide top-level.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_telegram_multi_session_with_transport<T>(
    channel_name: impl Into<String> + Clone,
    transport: Arc<T>,
    chat_filter: Option<i64>,
    config: TelegramSessionConfig,
    provider: Arc<dyn LlmProvider>,
    audit: Arc<dyn AuditHook>,
    checkpointer: Option<Arc<aivyx_core::GitCheckpointer>>,
    long_poll_timeout_secs: u32,
    shutdown: CancellationToken,
) -> Result<TelegramMultiSessionReport, String>
where
    T: TelegramTransport + 'static,
{
    let base_name: String = channel_name.into();
    let mut routes: HashMap<i64, ChatRoute> = HashMap::new();
    let mut offset: i64 = 0;

    // Outer long-poll loop. The single call site of `get_updates` in
    // the multi-chat path — Bot API 409 Conflict would reject any
    // concurrent call on the same token.
    loop {
        if shutdown.is_cancelled() {
            break;
        }

        let updates = tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            res = transport.get_updates(offset, long_poll_timeout_secs) => match res {
                Ok(batch) => batch,
                Err(e) => {
                    eprintln!(
                        "aivyx-telegram(multi): get_updates failed ({e}); backing off 1s"
                    );
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            }
        };

        if updates.is_empty() {
            continue;
        }

        for msg in updates {
            offset = offset.max(msg.update_id + 1);

            // Phase 8 compatibility: if a chat filter is set, drop
            // messages from other chats at the outer boundary so the
            // rest of the pipeline only ever sees the one allowed
            // chat. When unset, every chat is accepted.
            if let Some(allowed) = chat_filter {
                if msg.chat_id != allowed {
                    continue;
                }
            }

            let chat_id = msg.chat_id;

            // Lazy-spawn the inner task the first time we see this
            // chat. Each inner task gets:
            //   - its own `TelegramChannel<T>` bound to the chat_id
            //     (constructed inside this crate, which is why
            //     `TelegramChannel::new` can stay `pub(crate)`)
            //   - its own mailbox receiver
            //   - cloned shared state (provider, audit, config)
            //   - a clone of the shutdown token — when it fires, the
            //     inner task also notices and exits
            let route = routes.entry(chat_id).or_insert_with(|| {
                let (tx, rx) = mpsc::channel::<IncomingMessage>(CHAT_MAILBOX_CAPACITY);
                let channel = Arc::new(TelegramChannel::new(
                    base_name.clone(),
                    chat_id,
                    Arc::clone(&transport),
                ));
                let config_clone = config.clone();
                let provider_clone = Arc::clone(&provider);
                let audit_clone = Arc::clone(&audit);
                let checkpointer_clone = checkpointer.clone();
                let shutdown_clone = shutdown.clone();
                let handle = tokio::spawn(async move {
                    run_telegram_session_with_mailbox(
                        channel,
                        config_clone,
                        provider_clone,
                        audit_clone,
                        checkpointer_clone,
                        rx,
                        shutdown_clone,
                    )
                    .await
                });
                ChatRoute { sender: tx, handle }
            });

            // Deliver the message to the inner task. A `.send().await`
            // blocks briefly if the mailbox is full, applying the
            // intended per-chat backpressure.
            if let Err(e) = route.sender.send(msg).await {
                // The inner task's receiver has been dropped — it
                // exited for some reason (likely a panic, since the
                // normal shutdown path has the outer loop dropping
                // *senders* rather than the inner dropping
                // *receivers*). Remove the dead route so a
                // subsequent message for the same chat_id respawns.
                eprintln!(
                    "aivyx-telegram(multi): chat {chat_id} mailbox send failed ({e}); dropping route"
                );
                routes.remove(&chat_id);
            }
        }
    }

    // Shutdown drain: drop every sender so inner tasks see mailbox
    // close, then join each handle and aggregate turn counts. Dropping
    // the senders is the signal; joining collects the reports.
    let mut turns_by_chat: HashMap<i64, usize> = HashMap::new();
    let drained: Vec<(i64, ChatRoute)> = routes.drain().collect();
    for (chat_id, ChatRoute { sender, handle }) in drained {
        drop(sender);
        match handle.await {
            Ok(Ok(report)) => {
                turns_by_chat.insert(chat_id, report.turns_run);
            }
            Ok(Err(e)) => {
                eprintln!(
                    "aivyx-telegram(multi): chat {chat_id} inner task errored: {e}"
                );
            }
            Err(join_err) => {
                eprintln!(
                    "aivyx-telegram(multi): chat {chat_id} inner task join failed: {join_err}"
                );
            }
        }
    }

    Ok(TelegramMultiSessionReport { turns_by_chat })
}
