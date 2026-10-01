# Command Center Logbook Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Studio's stat-card home screen with a logbook. The page
has an instrument strip, a greeting, "Needs you" cards, a first-person log of
what happened since the operator was last here, and a "Coming up" list. All
of it is assembled by the daemon from the record.

**Architecture:**

- **Daemon.** A new `GetBriefing` query is answered in the daemon connection
  loop by `aivyx_channel::briefing`:
  - an async **gatherer** reads existing stores into `Facts`;
  - a pure **composer** turns `Facts` into a wire `Briefing` of ready-made
    sentences.
- **Last activity.** `aivyx_channel::activity::ActivityClock` records the
  operator's last action, grouped into visits. The reader task stamps it for
  every operator-action frame. It is persisted in the encrypted
  `ChannelState` domain.
- **Studio.** The Studio renders the briefing from a new `command_center.rs`
  module.

**Tech Stack:**

- Rust 2024, tokio, serde, redb-backed `aivyx-storage`
- Dioxus 0.6 (wasm32) for the Studio
- `just build-web` for the committed bundle

**Spec:** `docs/superpowers/specs/2026-10-01-command-center-logbook-design.md`.
Its "Refinements from planning" section takes precedence over the sections
above it.

## Global Constraints

### Commits and checks

- **Branch.** All work happens on branch `feat/command-center-logbook`, made
  from `main` and using no worktree. Every commit uses `git commit -s`, and
  its message ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- **Native checks before each commit:**
  - `cargo clippy --workspace --all-targets -- -D warnings` reports zero
    warnings;
  - the touched crate's tests pass.

  Task 7 runs `cargo test --workspace`.
- **Studio checks.** The Studio compiles only for wasm. Use this PATH:
  `PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$HOME/.cargo/bin:$PATH"`
  - `just check-web` compiles the wasm;
  - `cargo test -p aivyx-web` runs the pure helper tests natively.

### Wire protocol

- `aivyx-ipc` must stay wasm32-clean: no tokio, no std::fs and no chrono in
  `briefing.rs`.
- All wire changes are additive.

### Copy rules

- First person, plain words, no exclamation marks, and no model-written text.
- Empty lines, exactly:
  - "Nothing needs you."
  - "All quiet since you were last here."
  - "Nothing scheduled."
- The log keeps the 12 newest lines, shown oldest first. Anything beyond
  them becomes "and N more → Audit".

### Time windows

- The visit gap is 30 min (`VISIT_GAP_SECS = 1800`).
- The fallback window is 24 h, and the window is capped at 7 days.
- Spend is summed over a rolling 24 h.

### Visual rules

- Fraunces is used for the greeting only.
- IBM Plex Mono label-tech is used for times, the date line and the
  instruments.
- Surfaces are flat, with ruled 1px borders and no blur.
- Brass `#c9a24b` marks primary actions and the "Needs you" label; rust marks
  broken and offline states.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/aivyx-ipc/src/briefing.rs` (new) | Wire types: `Briefing`, `NeedsYouItem`, `NeedsYouAction`, `LogEntry`, `UpcomingItem` |
| `crates/aivyx-ipc/src/protocol.rs` | `QueryPayload::{GetBriefing, CompleteReminder, SnoozeReminder}`, `QueryResponsePayload::{Briefing, ReminderUpdated}` |
| `crates/aivyx-channel/src/activity.rs` (new) | `Activity` visit math, `ActivityClock` persistence, `is_operator_action` |
| `crates/aivyx-channel/src/briefing.rs` (new) | `Facts` and its parts, `window`, `compose`, `audit_facts`, `gather`, `BriefingSources` |
| `crates/aivyx-channel/src/daemon_server.rs` | Reminder command arms, `activity_store` config, the clock in the connection context, reader-task stamping, the `GetBriefing` intercept |
| `crates/aivyx-channel/tests/briefing_e2e.rs` (new) | `GetBriefing` and reminder commands over real IPC |
| `crates/aivyx-cli/src/bin/aivyx.rs` | Passes `activity_store: Some(storage.domain(KeyDomain::ChannelState))` |
| `crates/aivyx-web/src/command_center.rs` (new) | Pure helpers (greeting, ago, clock, labels) and the five components |
| `crates/aivyx-web/src/main.rs` | `BriefingState` signal, ws routing, the briefing poll, and `CommandPanel` rewritten to use the new components |
| `crates/aivyx-web/assets/stitch.css` | `.cc-*` styles |
| `docs/DAEMON_IPC.md`, `docs/guide/*`, `CHANGELOG.md` | Docs |

---

### Task 1: Wire types and the reminder commands

**Files:**
- Create: `crates/aivyx-ipc/src/briefing.rs`
- Modify:
  - `crates/aivyx-ipc/src/lib.rs` (add `pub mod briefing;` next to `pub mod insights;`)
  - `crates/aivyx-ipc/src/protocol.rs` (add the `QueryPayload` variants after `GetReminders` at ~line 847; add the `QueryResponsePayload` variants after `Reminders { .. }` at ~line 1574)
  - `crates/aivyx-channel/src/daemon_server.rs` (handle_query arms next to `QueryPayload::GetReminders` at ~line 4709; helper next to `reminders_query_response` at ~line 8184)
- Test:
  - `crates/aivyx-ipc/src/briefing.rs` (serde round trip)
  - `crates/aivyx-channel/src/daemon_server.rs` tests module (reminder commands, next to `reminders_query_response_lists_pending_soonest_first` at ~line 9996)

**Interfaces:**
- Produces:
  - `aivyx_ipc::briefing::{Briefing, NeedsYouItem, NeedsYouAction, LogEntry, UpcomingItem}` (exact fields below)
  - `QueryPayload::GetBriefing`
  - `QueryPayload::CompleteReminder { id: String }`
  - `QueryPayload::SnoozeReminder { id: String, secs: u64 }`
  - `QueryResponsePayload::Briefing { briefing: crate::briefing::Briefing }`
  - `QueryResponsePayload::ReminderUpdated { id: String, ok: bool, due_unix: Option<i64> }`
  - `async fn reminder_command(store: Option<&SharedReminderStore>, id: String, snooze_secs: Option<u64>, now_unix: i64) -> QueryResponsePayload` (private to `daemon_server.rs`)

- [ ] **Step 1: Create the branch**

```bash
cd /home/julian/Projects/Rust/aivyx-pa && git checkout -b feat/command-center-logbook
```

- [ ] **Step 2: Write the wire types and a failing round-trip test**

Create `crates/aivyx-ipc/src/briefing.rs`:

```rust
//! The Command Center's briefing — what needs the operator, what the
//! assistant did since they were last here, and what's coming up. Composed
//! by the daemon from the record (never by a model); the Studio only
//! renders it. Wasm-clean: plain data, no I/O.

use serde::{Deserialize, Serialize};

/// Answer to [`crate::protocol::QueryPayload::GetBriefing`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Briefing {
    /// The end of the operator's previous visit — what "last here" shows.
    /// `None` until the operator has acted at least once before this visit.
    pub last_active_unix: Option<i64>,
    /// Where the log window starts.
    pub window_start_unix: i64,
    /// `true` when the window was cut to 7 days.
    pub window_capped: bool,
    /// Priced `LlmCost` spend over the last 24 h; `None` if the audit chain
    /// couldn't be read.
    pub spend_24h_usd: Option<f64>,
    /// Operator-visible memory topics; `None` without a memory substrate.
    pub memory_topics: Option<u64>,
    pub needs_you: Vec<NeedsYouItem>,
    /// Oldest first, at most 12.
    pub log: Vec<LogEntry>,
    /// Older log lines left out of `log`.
    pub log_more: u32,
    pub coming_up: Vec<UpcomingItem>,
}

/// One card under "Needs you".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NeedsYouItem {
    /// Stable per item (the Studio's list key).
    pub key: String,
    pub sentence: String,
    /// A second, quieter line (a proposal's reason, a gate's scope).
    pub detail: Option<String>,
    pub action: NeedsYouAction,
    /// The Studio view slug that owns this item (`View::from_slug`).
    pub link: String,
}

/// What the card's buttons do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum NeedsYouAction {
    /// Approve / Deny → `FrontendMessage::ResolveGate`.
    MissionGate { mission_id: String, gate_id: String },
    /// Approve / Deny → `QueryPayload::ResolveTeamGate`.
    TeamGate { mission_id: String, step: String },
    /// Review → open `link`.
    Review,
    /// Done / Snooze 1 h → `CompleteReminder` / `SnoozeReminder`.
    Reminder { id: String },
    /// Nothing to press here; the link explains.
    Look,
}

/// One line under "Since you were last here".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogEntry {
    pub at_unix: i64,
    pub sentence: String,
    /// Something went wrong (the time is shown in the warn colour).
    pub warn: bool,
    pub link: String,
}

/// One line under "Coming up".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpcomingItem {
    /// `None` for work already in progress.
    pub at_unix: Option<i64>,
    pub sentence: String,
    pub link: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{QueryPayload, QueryResponsePayload};

    #[test]
    fn briefing_and_reminder_messages_round_trip() {
        let b = Briefing {
            last_active_unix: Some(10),
            window_start_unix: 10,
            window_capped: false,
            spend_24h_usd: Some(0.25),
            memory_topics: Some(3),
            needs_you: vec![NeedsYouItem {
                key: "reminder:r1".into(),
                sentence: "Reminder: call mom".into(),
                detail: None,
                action: NeedsYouAction::Reminder { id: "r1".into() },
                link: "reminders".into(),
            }],
            log: vec![LogEntry { at_unix: 20, sentence: "I ran the routine digest.".into(), warn: false, link: "schedules".into() }],
            log_more: 0,
            coming_up: vec![UpcomingItem { at_unix: None, sentence: "Working on “ship it”.".into(), link: "mission-control".into() }],
        };
        let resp = QueryResponsePayload::Briefing { briefing: b };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(serde_json::from_str::<QueryResponsePayload>(&json).unwrap(), resp);

        for q in [
            QueryPayload::GetBriefing,
            QueryPayload::CompleteReminder { id: "r1".into() },
            QueryPayload::SnoozeReminder { id: "r1".into(), secs: 3600 },
        ] {
            let json = serde_json::to_string(&q).unwrap();
            assert_eq!(serde_json::from_str::<QueryPayload>(&json).unwrap(), q);
        }
        let upd = QueryResponsePayload::ReminderUpdated { id: "r1".into(), ok: true, due_unix: Some(99) };
        let json = serde_json::to_string(&upd).unwrap();
        assert_eq!(serde_json::from_str::<QueryResponsePayload>(&json).unwrap(), upd);
    }
}
```

Add `pub mod briefing;` to `crates/aivyx-ipc/src/lib.rs`, next to `pub mod insights;`.

Check whether `QueryPayload` derives `PartialEq`:

```bash
grep -n "pub enum QueryPayload" -B3 crates/aivyx-ipc/src/protocol.rs
```

It does (`#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]`), so the
test compiles once the variants exist.

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p aivyx-ipc briefing_and_reminder_messages_round_trip`
Expected: FAIL to compile, with `no variant named GetBriefing` (and the
same for the other variants).

- [ ] **Step 4: Add the protocol variants**

In `crates/aivyx-ipc/src/protocol.rs`, add this immediately after `GetReminders,` inside `QueryPayload`:

```rust
    /// The Command Center's briefing. Answered by the daemon's connection
    /// loop (it needs the connection's activity clock). Replies with
    /// [`QueryResponsePayload::Briefing`].
    GetBriefing,
    /// Mark a reminder done (removes it). Replies with
    /// [`QueryResponsePayload::ReminderUpdated`]; an unknown id → `ok: false`.
    CompleteReminder { id: String },
    /// Move a reminder's due time to `now + secs`, keeping its message and
    /// targets. Replies with [`QueryResponsePayload::ReminderUpdated`].
    SnoozeReminder { id: String, secs: u64 },
```

Add this immediately after `Reminders { reminders: Vec<ReminderView> },` inside `QueryResponsePayload`:

```rust
    /// Response to [`QueryPayload::GetBriefing`].
    Briefing { briefing: crate::briefing::Briefing },
    /// Response to [`QueryPayload::CompleteReminder`] /
    /// [`QueryPayload::SnoozeReminder`]. `due_unix` is the new due time
    /// after a snooze, `None` after a completion or a failure.
    ReminderUpdated { id: String, ok: bool, due_unix: Option<i64> },
```

- [ ] **Step 5: Run the IPC test to verify it passes**

Run: `cargo test -p aivyx-ipc briefing_and_reminder_messages_round_trip`
Expected: PASS

- [ ] **Step 6: Write failing daemon tests for the reminder commands**

In the `daemon_server.rs` tests module, add these after
`reminders_query_response_none_store_is_empty_not_an_error`. They reuse the
existing `open_reminder_store()` fixture.

```rust
    #[tokio::test]
    async fn complete_removes_and_snooze_moves_a_reminder() {
        let store = open_reminder_store().await;
        for id in ["r1", "r2"] {
            store
                .set(&crate::reminder_store::Reminder {
                    id: id.into(),
                    due_unix: 100,
                    message: format!("msg {id}"),
                    notify_targets: vec!["telegram:1".into()],
                    created_unix: 0,
                })
                .await
                .unwrap();
        }

        let done = reminder_command(Some(&store), "r1".into(), None, 1_000).await;
        assert_eq!(
            done,
            QueryResponsePayload::ReminderUpdated { id: "r1".into(), ok: true, due_unix: None }
        );

        let snoozed = reminder_command(Some(&store), "r2".into(), Some(3_600), 1_000).await;
        assert_eq!(
            snoozed,
            QueryResponsePayload::ReminderUpdated { id: "r2".into(), ok: true, due_unix: Some(4_600) }
        );

        let left = store.list().await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, "r2");
        assert_eq!(left[0].due_unix, 4_600);
        assert_eq!(left[0].message, "msg r2");
        assert_eq!(left[0].notify_targets, vec!["telegram:1".to_string()]);
    }

    #[tokio::test]
    async fn reminder_commands_on_an_unknown_id_or_no_store_report_not_ok() {
        let store = open_reminder_store().await;
        for snooze in [None, Some(60)] {
            assert_eq!(
                reminder_command(Some(&store), "nope".into(), snooze, 0).await,
                QueryResponsePayload::ReminderUpdated { id: "nope".into(), ok: false, due_unix: None }
            );
            assert_eq!(
                reminder_command(None, "nope".into(), snooze, 0).await,
                QueryResponsePayload::ReminderUpdated { id: "nope".into(), ok: false, due_unix: None }
            );
        }
    }
```

- [ ] **Step 7: Run to verify it fails**

Run: `cargo test -p aivyx-channel --lib reminder_command`
Expected: FAIL to compile with ``cannot find function `reminder_command` ``.
(`handle_query`'s match is also non-exhaustive at this point.)

- [ ] **Step 8: Implement the helper and the handle_query arms**

Add this right after `reminders_query_response` (~line 8199):

```rust
/// `CompleteReminder` (`snooze_secs = None`) / `SnoozeReminder`. A missing
/// store, an unknown id, or a storage error → `ok: false`.
async fn reminder_command(
    reminder_store: Option<&crate::reminder_tool::SharedReminderStore>,
    id: String,
    snooze_secs: Option<u64>,
    now_unix: i64,
) -> QueryResponsePayload {
    let failed = |id: String| QueryResponsePayload::ReminderUpdated { id, ok: false, due_unix: None };
    let Some(store) = reminder_store else {
        return failed(id);
    };
    match snooze_secs {
        None => match store.cancel(&id).await {
            Ok(true) => QueryResponsePayload::ReminderUpdated { id, ok: true, due_unix: None },
            _ => failed(id),
        },
        Some(secs) => {
            let Ok(all) = store.list().await else {
                return failed(id);
            };
            let Some(mut r) = all.into_iter().find(|r| r.id == id) else {
                return failed(id);
            };
            r.due_unix = now_unix.saturating_add(secs.min(i64::MAX as u64) as i64);
            match store.set(&r).await {
                Ok(()) => QueryResponsePayload::ReminderUpdated { id, ok: true, due_unix: Some(r.due_unix) },
                Err(_) => failed(id),
            }
        }
    }
}
```

In `handle_query`, add these right after the `QueryPayload::GetReminders => ...` arm:

```rust
        QueryPayload::CompleteReminder { id } => {
            reminder_command(reminder_store, id, None, now_unix_secs()).await
        }
        QueryPayload::SnoozeReminder { id, secs } => {
            reminder_command(reminder_store, id, Some(secs), now_unix_secs()).await
        }
        // Answered by the connection loop, which holds the activity clock
        // (see `handle_connection`). Only a caller that bypasses it lands here.
        QueryPayload::GetBriefing => QueryResponsePayload::QueryError {
            code: "briefing_unavailable".into(),
            message: "the briefing is answered by the connection loop".into(),
        },
```

Check whether a seconds-clock helper already exists:

```bash
grep -n "fn now_unix_secs\|fn now_secs\|fn unix_now" crates/aivyx-channel/src/daemon_server.rs
```

If none exists, add this near `reminder_command`:

```rust
fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
```

If one does exist, use it in place of `now_unix_secs()` above.

- [ ] **Step 9: Make every other exhaustive `QueryPayload` match compile**

Run: `cargo build --workspace 2>&1 | grep -B2 -A8 "non-exhaustive"`

For each hit outside `handle_query`, add the new variants to the arm that
handles other read-only queries. Look in the TUI or CLI query-name helpers,
if any; a `match` that maps a query to a label gets `"GetBriefing"`,
`"CompleteReminder"` and `"SnoozeReminder"`. Then run `just check-web` too,
because the Studio may match `QueryResponsePayload` exhaustively. Expected:
both build clean.

- [ ] **Step 10: Run the tests and clippy**

Run:

```bash
cargo test -p aivyx-ipc && cargo test -p aivyx-channel --lib reminder && cargo clippy --workspace --all-targets -- -D warnings
```

Expected: PASS, with zero warnings.

- [ ] **Step 11: Commit**

```bash
git add crates/aivyx-ipc crates/aivyx-channel/src/daemon_server.rs
git add -u
git commit -s -m "feat(ipc): briefing wire types + CompleteReminder/SnoozeReminder

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: The activity clock

**Files:**
- Create: `crates/aivyx-channel/src/activity.rs`
- Modify: `crates/aivyx-channel/src/lib.rs` (add `pub mod activity;` after `pub mod approval_prompt;`)
- Test: `crates/aivyx-channel/src/activity.rs`

**Interfaces:**
- Consumes: `QueryPayload::{ResolveTeamGate, CompleteReminder, SnoozeReminder}` (Task 1)
- Produces:
  - `pub const VISIT_GAP_SECS: i64 = 1800`
  - `#[derive(Clone, Copy, Default, PartialEq, Eq, Debug, Serialize, Deserialize)] pub struct Activity { pub last_action: Option<i64>, pub anchor: Option<i64> }`
  - `Activity::touched(self, now: i64) -> Activity`
  - `Activity::last_here(self, now: i64) -> Option<i64>`
  - `pub struct ActivityClock`, with:
    - `ActivityClock::load(store: Option<DomainHandle>) -> ActivityClock` (async)
    - `ActivityClock::snapshot(&self) -> Activity`
    - `ActivityClock::touch(&self, now: i64)` (async)
  - `pub fn is_operator_action(msg: &FrontendMessage) -> bool`
  - `pub fn now_unix() -> i64`

- [ ] **Step 1: Write the failing tests and the module skeleton**

Create `crates/aivyx-channel/src/activity.rs`. Write the tests first; the
functions they call come in Step 3.

```rust
//! The operator's last activity, for the Command Center's "since you were
//! last here". Only things the operator *does* count (see
//! [`is_operator_action`]); looking doesn't. Actions less than
//! [`VISIT_GAP_SECS`] apart are one visit, and "last here" is the end of the
//! previous visit — so acting on the Command Center never empties its own
//! log. Persisted in the encrypted `ChannelState` domain.

use std::sync::Mutex;

use aivyx_storage::DomainHandle;
use serde::{Deserialize, Serialize};

use crate::daemon_ipc::{FrontendMessage, QueryPayload};

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn actions_within_a_visit_keep_the_previous_visit_as_last_here() {
        let a = Activity::default();
        assert_eq!(a.last_here(0), None);

        // First ever action: nothing before it.
        let a = a.touched(1_000);
        assert_eq!(a.last_here(1_010), None);

        // Same visit: still nothing before it.
        let a = a.touched(1_500);
        assert_eq!(a.last_here(1_600), None);

        // Away for over 30 min: the last action becomes "last here" at once.
        assert_eq!(a.last_here(1_500 + VISIT_GAP_SECS + 1), Some(1_500));

        // Acting in the new visit keeps showing the old visit's end.
        let later = 1_500 + VISIT_GAP_SECS + 100;
        let a = a.touched(later);
        assert_eq!(a.last_here(later + 5), Some(1_500));
        let a = a.touched(later + 60);
        assert_eq!(a.last_here(later + 70), Some(1_500));
    }

    #[test]
    fn operator_actions_are_the_things_you_do() {
        let yes = [
            FrontendMessage::SubmitInput {
                session_id: "s".into(),
                text: "hi".into(),
                mission_id: None,
                attachments: Vec::new(),
                headless: false,
            },
            FrontendMessage::ResolveApproval { request_id: "a".into(), approved: true },
            FrontendMessage::ResolveGate { mission_id: "m".into(), gate_id: "g".into(), approved: false },
            FrontendMessage::Query { id: "x".into(), payload: QueryPayload::CompleteReminder { id: "r".into() } },
            FrontendMessage::Query { id: "x".into(), payload: QueryPayload::SnoozeReminder { id: "r".into(), secs: 60 } },
            FrontendMessage::Query {
                id: "x".into(),
                payload: QueryPayload::ResolveTeamGate { mission_id: "m".into(), step: "s".into(), approve: true },
            },
        ];
        for m in &yes {
            assert!(is_operator_action(m), "{m:?}");
        }
        let no = [
            FrontendMessage::Query { id: "x".into(), payload: QueryPayload::GetBriefing },
            FrontendMessage::Query { id: "x".into(), payload: QueryPayload::GetReminders },
            FrontendMessage::SetApprovals { enabled: true },
        ];
        for m in &no {
            assert!(!is_operator_action(m), "{m:?}");
        }
    }

    #[tokio::test]
    async fn the_clock_survives_a_reopen() {
        use aivyx_crypto::MasterKey;
        use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};
        let dir = std::env::temp_dir().join(format!("aivyx-activity-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("store.redb");
        {
            let store: Arc<dyn Storage> =
                RedbStorage::open(StorageConfig::new(path.clone()), MasterKey::from_raw([5u8; 32]))
                    .await
                    .unwrap();
            let clock = ActivityClock::load(Some(store.domain(KeyDomain::ChannelState))).await;
            clock.touch(1_000).await;
            clock.touch(1_000 + VISIT_GAP_SECS + 10).await;
        }
        let store: Arc<dyn Storage> =
            RedbStorage::open(StorageConfig::new(path), MasterKey::from_raw([5u8; 32])).await.unwrap();
        let clock = ActivityClock::load(Some(store.domain(KeyDomain::ChannelState))).await;
        assert_eq!(
            clock.snapshot(),
            Activity { last_action: Some(1_000 + VISIT_GAP_SECS + 10), anchor: Some(1_000) }
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn without_a_store_the_clock_still_works_in_memory() {
        let clock = ActivityClock::load(None).await;
        clock.touch(5).await;
        assert_eq!(clock.snapshot().last_action, Some(5));
    }
}
```

The `MasterKey` import path must match what the reminder-store test uses.
Copy it from `crates/aivyx-channel/src/reminder_store.rs`'s test module:

```bash
grep -n "MasterKey" crates/aivyx-channel/src/reminder_store.rs
```

If the reopen fails because redb holds a file lock until the `Arc` drops,
the inner block above already drops it. If it still fails, add
`drop(clock); drop(store);` explicitly inside the block.

Add `pub mod activity;` to `crates/aivyx-channel/src/lib.rs`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p aivyx-channel --lib activity::`
Expected: FAIL to compile (`Activity`, `ActivityClock` and
`is_operator_action` are undefined).

- [ ] **Step 3: Implement**

Insert this above the `#[cfg(test)]` block:

```rust
/// Actions closer together than this are one visit.
pub const VISIT_GAP_SECS: i64 = 30 * 60;

const KEY: &[u8] = b"operator.activity";

/// The persisted pair: the newest action, and the end of the visit before
/// the current one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activity {
    pub last_action: Option<i64>,
    pub anchor: Option<i64>,
}

impl Activity {
    /// Record an action at `now`. A gap longer than a visit starts a new
    /// visit, whose anchor is the previous visit's last action.
    pub fn touched(self, now: i64) -> Activity {
        match self.last_action {
            Some(t) if now - t <= VISIT_GAP_SECS => Activity { last_action: Some(now), anchor: self.anchor },
            previous => Activity { last_action: Some(now), anchor: previous },
        }
    }

    /// When "since you were last here" starts, seen at `now`: once the
    /// newest action is a visit-gap old, it is the end of the last visit.
    pub fn last_here(self, now: i64) -> Option<i64> {
        match self.last_action {
            Some(t) if now - t > VISIT_GAP_SECS => Some(t),
            _ => self.anchor,
        }
    }
}

/// The daemon's one clock, shared by every connection.
pub struct ActivityClock {
    state: Mutex<Activity>,
    store: Option<DomainHandle>,
}

impl ActivityClock {
    /// Read the persisted state (unreadable or absent → fresh).
    pub async fn load(store: Option<DomainHandle>) -> ActivityClock {
        let state = match &store {
            Some(s) => match s.get(KEY).await {
                Ok(Some(bytes)) => serde_json::from_slice(&bytes).unwrap_or_default(),
                _ => Activity::default(),
            },
            None => Activity::default(),
        };
        ActivityClock { state: Mutex::new(state), store }
    }

    pub fn snapshot(&self) -> Activity {
        *self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record an operator action. A failed write is logged, not fatal: the
    /// in-memory value still serves this run.
    pub async fn touch(&self, now: i64) {
        let next = {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            *s = s.touched(now);
            *s
        };
        if let Some(store) = &self.store
            && let Ok(bytes) = serde_json::to_vec(&next)
            && let Err(e) = store.put(KEY, &bytes).await
        {
            eprintln!("aivyx-pa daemon: failed to persist operator activity: {e}");
        }
    }
}

/// Frames that are the operator doing something (any daemon-socket front
/// end: CLI chat, TUI, Studio). Reads — `GetBriefing` included — don't count.
pub fn is_operator_action(msg: &FrontendMessage) -> bool {
    match msg {
        FrontendMessage::SubmitInput { .. }
        | FrontendMessage::ResolveApproval { .. }
        | FrontendMessage::ResolveGate { .. }
        | FrontendMessage::ResolvePersonaProposal { .. } => true,
        FrontendMessage::Query { payload, .. } => matches!(
            payload,
            QueryPayload::ResolveTeamGate { .. }
                | QueryPayload::CompleteReminder { .. }
                | QueryPayload::SnoozeReminder { .. }
        ),
        _ => false,
    }
}

/// Wall-clock seconds.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
```

Check two things before building:

- **`DomainHandle` methods.** Confirm `get` and `put` are inherent methods
  (`aivyx-storage/src/lib.rs` ~lines 815 and 857); they are.
- **Imports.** If `DomainHandle` isn't re-exported at the crate root, import
  it the way `reminder_store.rs` does (`use aivyx_storage::DomainHandle;`).

- [ ] **Step 4: Run to verify it passes**

Run:

```bash
cargo test -p aivyx-channel --lib activity:: && cargo clippy -p aivyx-channel --all-targets -- -D warnings
```

Expected: 4 tests PASS, with zero warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-channel/src/activity.rs crates/aivyx-channel/src/lib.rs
git commit -s -m "feat(channel): operator activity clock (visits, persisted)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: The composer

**Files:**
- Create: `crates/aivyx-channel/src/briefing.rs` (facts plus composer; the gatherer comes in Task 4)
- Modify: `crates/aivyx-channel/src/lib.rs` (add `pub mod briefing;` after `pub mod activity;`)
- Test: `crates/aivyx-channel/src/briefing.rs`

**Interfaces:**
- Consumes: `aivyx_ipc::briefing::*` (Task 1)
- Produces:
  - **Fact structs** (fields below):
    - `pub struct Facts`
    - `pub struct RoutineRun`
    - `pub struct NotifyFact`
    - `pub struct GateFact`
    - `pub struct TeamGateFact`
    - `pub struct ProposalFact`
    - `pub struct ReminderFact`
    - `pub struct UpcomingFact`
  - **Functions and constants:**
    - `pub fn window(last_here: Option<i64>, now: i64) -> (i64, bool)`
    - `pub fn compose(f: &Facts, now: i64) -> Briefing`
    - `pub const LOG_CAP: usize = 12`

- [ ] **Step 1: Write the module with its facts types and failing tests**

Create `crates/aivyx-channel/src/briefing.rs`:

```rust
//! The Command Center briefing: [`gather`] reads what the record already
//! holds into [`Facts`]; [`compose`] (pure) writes every sentence. No model
//! is involved — the wording is fixed templates, in the assistant's voice.

use aivyx_ipc::briefing::{Briefing, LogEntry, NeedsYouAction, NeedsYouItem, UpcomingItem};

/// How many log lines the page shows.
pub const LOG_CAP: usize = 12;
const DAY: i64 = 24 * 3600;
const WEEK: i64 = 7 * DAY;

/// A routine (or other trigger) run inside the window.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutineRun {
    /// "routine digest", "webhook trigger deploy".
    pub what: String,
    pub at_unix: i64,
    /// `Some(true)` failed, `Some(false)` succeeded, `None` unknown (the run
    /// wasn't wrapped in a mission, so only "it ran" is on record).
    pub failed: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NotifyFact {
    pub target: String,
    pub at_unix: i64,
    /// `None` = delivered; `Some(kind)` = failed with that error kind.
    pub error_kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GateFact {
    pub mission_id: String,
    pub gate_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TeamGateFact {
    pub mission_id: String,
    pub step: String,
    pub goal: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProposalFact {
    pub id: String,
    /// The category in words ("communication style").
    pub category: String,
    pub is_skill: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReminderFact {
    pub id: String,
    pub message: String,
    pub due_unix: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UpcomingFact {
    pub name: String,
    pub at_unix: i64,
}

/// Everything the composer needs, already read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Facts {
    pub last_here: Option<i64>,
    pub window_start: i64,
    pub window_capped: bool,
    pub routine_runs: Vec<RoutineRun>,
    pub notifications: Vec<NotifyFact>,
    /// Completed non-read-only tool calls in the window: (tool base, at).
    pub changes: Vec<(String, i64)>,
    /// Memory writes in the window (their times).
    pub memories_saved: Vec<i64>,
    pub mission_gates: Vec<GateFact>,
    pub team_gates: Vec<TeamGateFact>,
    pub proposals: Vec<ProposalFact>,
    pub due_reminders: Vec<ReminderFact>,
    pub upcoming: Vec<UpcomingFact>,
    /// Goals of work in progress.
    pub in_progress: Vec<String>,
    pub spend_24h_usd: Option<f64>,
    pub memory_topics: Option<u64>,
    /// Sources that couldn't be read ("reminders", "the audit trail", …).
    pub source_errors: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000;

    fn base() -> Facts {
        let (window_start, window_capped) = window(Some(NOW - 3_600), NOW);
        Facts { last_here: Some(NOW - 3_600), window_start, window_capped, ..Default::default() }
    }

    #[test]
    fn the_window_falls_back_to_a_day_and_caps_at_a_week() {
        assert_eq!(window(None, NOW), (NOW - 24 * 3600, false));
        assert_eq!(window(Some(NOW - 60), NOW), (NOW - 60, false));
        assert_eq!(window(Some(NOW - 30 * 24 * 3600), NOW), (NOW - 7 * 24 * 3600, true));
    }

    #[test]
    fn an_empty_record_composes_an_empty_briefing() {
        let b = compose(&base(), NOW);
        assert!(b.needs_you.is_empty());
        assert!(b.log.is_empty());
        assert!(b.coming_up.is_empty());
        assert_eq!(b.log_more, 0);
        assert_eq!(b.last_active_unix, Some(NOW - 3_600));
    }

    #[test]
    fn needs_you_is_approvals_then_proposals_then_problems_then_reminders() {
        let mut f = base();
        f.due_reminders = vec![ReminderFact { id: "r1".into(), message: "call mom".into(), due_unix: NOW - 5 }];
        f.notifications = vec![
            NotifyFact { target: "telegram".into(), at_unix: NOW - 50, error_kind: Some("auth".into()) },
            NotifyFact { target: "telegram".into(), at_unix: NOW - 40, error_kind: Some("auth".into()) },
        ];
        f.routine_runs = vec![RoutineRun { what: "routine trend-scan".into(), at_unix: NOW - 30, failed: Some(true) }];
        f.proposals = vec![
            ProposalFact { id: "p1".into(), category: "learned skill".into(), is_skill: true, reason: Some("you ask for this often".into()) },
            ProposalFact { id: "p2".into(), category: "communication style".into(), is_skill: false, reason: None },
        ];
        f.team_gates = vec![TeamGateFact { mission_id: "t1".into(), step: "deploy".into(), goal: "ship the note".into() }];
        f.mission_gates = vec![GateFact { mission_id: "m1".into(), gate_id: "g1".into(), reason: "send the weekly digest".into() }];
        f.source_errors = vec!["reminders".into()];

        let b = compose(&f, NOW);
        let sentences: Vec<&str> = b.needs_you.iter().map(|n| n.sentence.as_str()).collect();
        assert_eq!(
            sentences,
            vec![
                "A mission is waiting for your go-ahead: send the weekly digest.",
                "The team is waiting for your go-ahead on “ship the note” (step deploy).",
                "I'd like to add or refine a skill.",
                "I'd like to update my communication style.",
                "The routine trend-scan failed.",
                "I couldn't send 2 notifications to telegram (auth).",
                "I couldn't read reminders just now.",
                "Reminder: call mom",
            ]
        );
        assert_eq!(b.needs_you[0].action, NeedsYouAction::MissionGate { mission_id: "m1".into(), gate_id: "g1".into() });
        assert_eq!(b.needs_you[0].link, "missions");
        assert_eq!(b.needs_you[1].action, NeedsYouAction::TeamGate { mission_id: "t1".into(), step: "deploy".into() });
        assert_eq!(b.needs_you[1].link, "mission-control");
        assert_eq!(b.needs_you[2].action, NeedsYouAction::Review);
        assert_eq!(b.needs_you[2].detail.as_deref(), Some("you ask for this often"));
        assert_eq!(b.needs_you[2].link, "agents");
        assert_eq!(b.needs_you[4].link, "missions");
        assert_eq!(b.needs_you[5].link, "notifications");
        assert_eq!(b.needs_you[7].action, NeedsYouAction::Reminder { id: "r1".into() });
        assert_eq!(b.needs_you[7].link, "reminders");
        // Keys are unique.
        let mut keys: Vec<&str> = b.needs_you.iter().map(|n| n.key.as_str()).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), b.needs_you.len());
    }

    #[test]
    fn a_single_failed_notification_reads_naturally() {
        let mut f = base();
        f.notifications = vec![NotifyFact { target: "email".into(), at_unix: NOW - 5, error_kind: Some("transport".into()) }];
        let b = compose(&f, NOW);
        assert_eq!(b.needs_you[0].sentence, "I couldn't send a notification to email (transport).");
    }

    #[test]
    fn the_log_reads_in_the_first_person_oldest_first() {
        let mut f = base();
        f.routine_runs = vec![
            RoutineRun { what: "routine digest".into(), at_unix: NOW - 300, failed: Some(false) },
            RoutineRun { what: "routine trend-scan".into(), at_unix: NOW - 100, failed: Some(true) },
            RoutineRun { what: "routine tidy".into(), at_unix: NOW - 200, failed: None },
        ];
        f.notifications = vec![
            NotifyFact { target: "telegram".into(), at_unix: NOW - 250, error_kind: None },
            NotifyFact { target: "email".into(), at_unix: NOW - 90, error_kind: Some("auth".into()) },
        ];
        f.changes = vec![
            ("fs.write".into(), NOW - 280),
            ("calendar.create".into(), NOW - 150),
            ("fs.write".into(), NOW - 80),
        ];
        f.memories_saved = vec![NOW - 260, NOW - 70];

        let b = compose(&f, NOW);
        let lines: Vec<(i64, &str, bool)> =
            b.log.iter().map(|l| (NOW - l.at_unix, l.sentence.as_str(), l.warn)).collect();
        assert_eq!(
            lines,
            vec![
                (300, "I ran the routine digest.", false),
                (250, "I sent a notification to telegram.", false),
                (200, "I ran the routine tidy.", false),
                (100, "The routine trend-scan failed.", true),
                (90, "A notification to email failed (auth).", true),
                (80, "I made 3 changes: fs.write ×2, calendar.create ×1.", false),
                (70, "I saved 2 memories.", false),
            ]
        );
        assert_eq!(b.log[0].link, "schedules");
        assert_eq!(b.log[1].link, "notifications");
        assert_eq!(b.log[5].link, "audit");
        assert_eq!(b.log[6].link, "memory");
    }

    #[test]
    fn singular_and_many_tools_read_naturally() {
        let mut f = base();
        f.changes = vec![
            ("a.x".into(), NOW - 9),
            ("b.x".into(), NOW - 8),
            ("c.x".into(), NOW - 7),
            ("d.x".into(), NOW - 6),
        ];
        f.memories_saved = vec![NOW - 5];
        let b = compose(&f, NOW);
        assert_eq!(b.log[0].sentence, "I made 4 changes: a.x ×1, b.x ×1, c.x ×1, and others.");
        assert_eq!(b.log[1].sentence, "I saved 1 memory.");

        let mut f = base();
        f.changes = vec![("fs.write".into(), NOW - 9)];
        assert_eq!(compose(&f, NOW).log[0].sentence, "I made 1 change: fs.write ×1.");
    }

    #[test]
    fn the_log_keeps_the_newest_twelve() {
        let mut f = base();
        f.routine_runs = (0..15)
            .map(|i| RoutineRun { what: format!("routine r{i}"), at_unix: NOW - 1_000 + i, failed: Some(false) })
            .collect();
        let b = compose(&f, NOW);
        assert_eq!(b.log.len(), LOG_CAP);
        assert_eq!(b.log_more, 3);
        assert_eq!(b.log[0].sentence, "I ran the routine r3.");
        assert_eq!(b.log[11].sentence, "I ran the routine r14.");
    }

    #[test]
    fn coming_up_is_the_next_three_routines_then_work_in_progress() {
        let mut f = base();
        f.upcoming = vec![
            UpcomingFact { name: "c".into(), at_unix: NOW + 300 },
            UpcomingFact { name: "a".into(), at_unix: NOW + 100 },
            UpcomingFact { name: "d".into(), at_unix: NOW + 400 },
            UpcomingFact { name: "b".into(), at_unix: NOW + 200 },
        ];
        f.in_progress = vec!["ship the note".into()];
        let b = compose(&f, NOW);
        let s: Vec<(Option<i64>, &str)> =
            b.coming_up.iter().map(|u| (u.at_unix.map(|t| t - NOW), u.sentence.as_str())).collect();
        assert_eq!(
            s,
            vec![
                (Some(100), "a"),
                (Some(200), "b"),
                (Some(300), "c"),
                (None, "Working on “ship the note”."),
            ]
        );
        assert_eq!(b.coming_up[0].link, "schedules");
        assert_eq!(b.coming_up[3].link, "mission-control");
    }

    #[test]
    fn instruments_and_window_pass_through() {
        let mut f = base();
        f.spend_24h_usd = Some(0.42);
        f.memory_topics = Some(12);
        let b = compose(&f, NOW);
        assert_eq!(b.spend_24h_usd, Some(0.42));
        assert_eq!(b.memory_topics, Some(12));
        assert_eq!(b.window_start_unix, NOW - 3_600);
        assert!(!b.window_capped);
    }
}
```

Add `pub mod briefing;` to `crates/aivyx-channel/src/lib.rs`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p aivyx-channel --lib briefing::`
Expected: FAIL to compile (`window` and `compose` are undefined).

- [ ] **Step 3: Implement `window` and `compose`**

Insert this above `#[cfg(test)]`:

```rust
/// Where the log starts: the end of the last visit, else the last 24 h,
/// never more than 7 days back. Returns `(start, capped)`.
pub fn window(last_here: Option<i64>, now: i64) -> (i64, bool) {
    match last_here {
        None => (now - DAY, false),
        Some(t) if t < now - WEEK => (now - WEEK, true),
        Some(t) => (t, false),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}

fn item(key: String, sentence: String, detail: Option<String>, action: NeedsYouAction, link: &str) -> NeedsYouItem {
    NeedsYouItem { key, sentence, detail, action, link: link.to_string() }
}

/// Pure: facts in, the briefing out.
pub fn compose(f: &Facts, now: i64) -> Briefing {
    let _ = now;
    let mut needs = Vec::new();

    // Approvals.
    for g in &f.mission_gates {
        needs.push(item(
            format!("gate:{}:{}", g.mission_id, g.gate_id),
            format!("A mission is waiting for your go-ahead: {}.", g.reason.trim_end_matches('.')),
            None,
            NeedsYouAction::MissionGate { mission_id: g.mission_id.clone(), gate_id: g.gate_id.clone() },
            "missions",
        ));
    }
    for g in &f.team_gates {
        needs.push(item(
            format!("team-gate:{}:{}", g.mission_id, g.step),
            format!("The team is waiting for your go-ahead on “{}” (step {}).", g.goal, g.step),
            None,
            NeedsYouAction::TeamGate { mission_id: g.mission_id.clone(), step: g.step.clone() },
            "mission-control",
        ));
    }
    // Proposals.
    for p in &f.proposals {
        let sentence = if p.is_skill {
            "I'd like to add or refine a skill.".to_string()
        } else {
            format!("I'd like to update my {}.", p.category)
        };
        needs.push(item(format!("proposal:{}", p.id), sentence, p.reason.clone(), NeedsYouAction::Review, "agents"));
    }
    // Went wrong.
    for r in f.routine_runs.iter().filter(|r| r.failed == Some(true)) {
        needs.push(item(
            format!("failed:{}:{}", r.what, r.at_unix),
            format!("The {} failed.", r.what),
            None,
            NeedsYouAction::Look,
            "missions",
        ));
    }
    // Failed notifications, grouped by (target, kind) in first-seen order.
    let mut groups: Vec<(&str, &str, usize)> = Vec::new();
    for n in &f.notifications {
        if let Some(kind) = &n.error_kind {
            match groups.iter_mut().find(|(t, k, _)| *t == n.target && *k == kind) {
                Some(g) => g.2 += 1,
                None => groups.push((&n.target, kind, 1)),
            }
        }
    }
    for (target, kind, count) in groups {
        let what = if count == 1 { "a notification".to_string() } else { format!("{count} notifications") };
        needs.push(item(
            format!("notify-failed:{target}:{kind}"),
            format!("I couldn't send {what} to {target} ({kind})."),
            None,
            NeedsYouAction::Look,
            "notifications",
        ));
    }
    for s in &f.source_errors {
        needs.push(item(
            format!("source:{s}"),
            format!("I couldn't read {s} just now."),
            None,
            NeedsYouAction::Look,
            "command",
        ));
    }
    // Due reminders.
    for r in &f.due_reminders {
        needs.push(item(
            format!("reminder:{}", r.id),
            format!("Reminder: {}", r.message),
            None,
            NeedsYouAction::Reminder { id: r.id.clone() },
            "reminders",
        ));
    }

    // The log.
    let mut log: Vec<LogEntry> = Vec::new();
    for r in &f.routine_runs {
        let (sentence, warn) = match r.failed {
            Some(true) => (format!("The {} failed.", r.what), true),
            _ => (format!("I ran the {}.", r.what), false),
        };
        log.push(LogEntry { at_unix: r.at_unix, sentence, warn, link: "schedules".into() });
    }
    for n in &f.notifications {
        let (sentence, warn) = match &n.error_kind {
            None => (format!("I sent a notification to {}.", n.target), false),
            Some(kind) => (format!("A notification to {} failed ({kind}).", n.target), true),
        };
        log.push(LogEntry { at_unix: n.at_unix, sentence, warn, link: "notifications".into() });
    }
    if let Some(last) = f.changes.iter().map(|(_, t)| *t).max() {
        let mut counts: Vec<(&str, usize)> = Vec::new();
        for (tool, _) in &f.changes {
            match counts.iter_mut().find(|(t, _)| *t == tool) {
                Some(c) => c.1 += 1,
                None => counts.push((tool, 1)),
            }
        }
        counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let mut named: Vec<String> = counts.iter().take(3).map(|(t, n)| format!("{t} ×{n}")).collect();
        if counts.len() > 3 {
            named.push("and others".into());
        }
        log.push(LogEntry {
            at_unix: last,
            sentence: format!("I made {}: {}.", plural(f.changes.len(), "change", "changes"), named.join(", ")),
            warn: false,
            link: "audit".into(),
        });
    }
    if let Some(last) = f.memories_saved.iter().copied().max() {
        log.push(LogEntry {
            at_unix: last,
            sentence: format!("I saved {}.", plural(f.memories_saved.len(), "memory", "memories")),
            warn: false,
            link: "memory".into(),
        });
    }
    log.sort_by_key(|l| l.at_unix);
    let log_more = log.len().saturating_sub(LOG_CAP);
    let log: Vec<LogEntry> = log.into_iter().skip(log_more).collect();

    // Coming up.
    let mut upcoming: Vec<&UpcomingFact> = f.upcoming.iter().collect();
    upcoming.sort_by_key(|u| u.at_unix);
    let mut coming_up: Vec<UpcomingItem> = upcoming
        .into_iter()
        .take(3)
        .map(|u| UpcomingItem { at_unix: Some(u.at_unix), sentence: u.name.clone(), link: "schedules".into() })
        .collect();
    coming_up.extend(f.in_progress.iter().map(|goal| UpcomingItem {
        at_unix: None,
        sentence: format!("Working on “{goal}”."),
        link: "mission-control".into(),
    }));

    Briefing {
        last_active_unix: f.last_here,
        window_start_unix: f.window_start,
        window_capped: f.window_capped,
        spend_24h_usd: f.spend_24h_usd,
        memory_topics: f.memory_topics,
        needs_you: needs,
        log,
        log_more: log_more as u32,
        coming_up,
    }
}
```

`compose` takes `now` so later wording can use relative time. If clippy
flags it as unused, remove the parameter **everywhere**, including tests and
Task 5's call site, rather than keeping `let _ = now;`. Keep the signature
`compose(f: &Facts, now: i64)` only if it is actually used.

- [ ] **Step 4: Run to verify it passes**

Run:

```bash
cargo test -p aivyx-channel --lib briefing:: && cargo clippy -p aivyx-channel --all-targets -- -D warnings
```

Expected: 8 tests PASS, with zero warnings. If an exact-string assertion
fails, fix `compose`; never edit the expected copy in the test.

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-channel/src/briefing.rs crates/aivyx-channel/src/lib.rs
git commit -s -m "feat(channel): briefing composer — fixed first-person templates

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: The gatherer

**Files:**
- Modify: `crates/aivyx-channel/src/briefing.rs`
- Test: `crates/aivyx-channel/src/briefing.rs` (a new `gather_tests` module)

**Interfaces:**
- Consumes:
  - `Facts` and its parts, plus `window` (Task 3)
  - Existing APIs:
    - `crate::mission::list_missions(&DomainHandle) -> Result<Vec<MissionRecord>, StorageError>`, using `MissionRecord.{mission_id, description, state, gates, updated_at}` (ms) and `GateRecord.{gate_id, reason, state}`
    - `crate::schedule::list_schedules(&DomainHandle) -> Result<Vec<ScheduleRecord>, _>`, using `ScheduleRecord.{schedule_id, enabled, wrap_mission, last_fired_at (ms), next_fire_time()}`
    - `PersistentAuditLog::{len, entries_range}`
    - `crate::persona_proposal::PersistentPersonaProposalLog::list(ProposalStatusFilter::Pending)`
    - `crate::team_mission_driver::TeamMissionService::list() -> Vec<TeamMissionRecord>`
    - `ReminderStore::list`
    - `Memory::list_topics`
    - `crate::prune_sink::is_internal_topic`
    - `aivyx_cost::Pricing::cost_of`
    - `aivyx_capability::is_read_only_base`
- Produces:
  - `pub struct BriefingSources<'a> { pub mission_store: Option<&'a DomainHandle>, pub schedule_store: Option<&'a DomainHandle>, pub audit_log: Option<&'a PersistentAuditLog>, pub persona_proposals: Option<&'a crate::persona_proposal::PersistentPersonaProposalLog>, pub team_missions: Option<&'a crate::team_mission_driver::TeamMissionService>, pub reminders: Option<&'a crate::reminder_tool::SharedReminderStore>, pub memory: Option<&'a Arc<dyn aivyx_memory::Memory>>, pub pricing: &'a aivyx_cost::Pricing }`
  - `pub fn audit_facts(entries: &[SignedEntry], window_start: i64, now: i64, pricing: &Pricing) -> AuditFacts`
  - `pub struct AuditFacts { pub notifications: Vec<NotifyFact>, pub changes: Vec<(String, i64)>, pub memories_saved: Vec<i64>, pub spend_24h_usd: f64 }`
  - `pub fn trigger_label(description: &str) -> Option<String>`
  - `pub async fn gather(src: &BriefingSources<'_>, last_here: Option<i64>, now: i64) -> Facts`

- [ ] **Step 1: Write the failing tests**

Append this module to `briefing.rs`:

```rust
#[cfg(test)]
mod gather_tests {
    use super::*;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use aivyx_audit::{AuditEvent, AutoNotifyOutcomeSummary, SignedEntry, TriggerKindSummary};

    const NOW: i64 = 2_000_000_000;

    fn at(unix: i64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(unix as u64)
    }

    fn entry(seq: u64, unix: i64, event: AuditEvent) -> SignedEntry {
        SignedEntry { seq, appended_at: at(unix), event, mac: [0u8; 32], prev_mac: [0u8; 32] }
    }

    fn tool(scope: &str, outcome: aivyx_core::ToolOutcomeSummary) -> AuditEvent {
        AuditEvent::ToolCall {
            turn_id: aivyx_core::TurnId::new(),
            tool_id: aivyx_core::ToolId::new(),
            scope_used: aivyx_capability::Scope::parse(scope).unwrap(),
            input_hash: [0u8; 32],
            outcome,
            duration: Duration::from_millis(1),
            auto_corrected_from: None,
            extracted_from_text: None,
        }
    }

    fn notify(target: &str, outcome: AutoNotifyOutcomeSummary, unix: i64) -> AuditEvent {
        AuditEvent::AutoNotifyDispatched {
            session_id: aivyx_core::SessionId::new(),
            trigger_kind: TriggerKindSummary::Cron,
            trigger_id: "cfg-digest".into(),
            target_name: target.into(),
            outcome,
            dispatched_at_unix_ms: (unix * 1000) as u64,
        }
    }

    #[test]
    fn trigger_missions_name_their_routine() {
        assert_eq!(trigger_label("cron trigger cfg-digest: summarise"), Some("routine digest".into()));
        assert_eq!(trigger_label("webhook trigger deploy: go"), Some("webhook trigger deploy".into()));
        assert_eq!(trigger_label("write the report"), None);
    }

    #[test]
    fn audit_facts_keep_the_window_and_skip_reads() {
        let completed = aivyx_core::ToolOutcomeSummary::Completed {
            verified: aivyx_core::VerificationSummary::Verified,
        };
        let start = NOW - 3_600;
        let entries = vec![
            // Before the window: ignored for the log, but inside 24 h → priced.
            entry(0, start - 10, tool("fs.write", completed)),
            entry(1, start - 10, AuditEvent::LlmCost {
                turn_id: aivyx_core::TurnId::new(),
                model: "claude-sonnet-5".into(),
                usage: aivyx_core::TokenUsage { input_tokens: 1_000_000, ..Default::default() },
            }),
            entry(2, start + 10, tool("fs.write", completed)),
            entry(3, start + 20, tool("fs.read", completed)),
            entry(4, start + 30, tool("fs.delete", aivyx_core::ToolOutcomeSummary::Denied)),
            entry(4, start + 35, tool("memory.write", completed)),
            entry(5, start + 40, AuditEvent::MemoryAccess {
                turn_id: aivyx_core::TurnId::new(),
                operation: aivyx_audit::MemoryOperation::Write,
                scope: aivyx_capability::Scope::parse("memory.write").unwrap(),
                query_or_key: "k".into(),
            }),
            entry(6, start + 50, notify("telegram", AutoNotifyOutcomeSummary::Delivered, start + 50)),
            entry(7, start + 60, notify(
                "email",
                AutoNotifyOutcomeSummary::Failed { error_kind: "auth".into(), error_message: "bad".into() },
                start + 60,
            )),
        ];
        let pricing = aivyx_cost::Pricing::new();
        let a = audit_facts(&entries, start, NOW, &pricing);
        assert_eq!(a.changes, vec![("fs.write".to_string(), start + 10)]);
        assert_eq!(a.memories_saved, vec![start + 40]);
        assert_eq!(
            a.notifications,
            vec![
                NotifyFact { target: "telegram".into(), at_unix: start + 50, error_kind: None },
                NotifyFact { target: "email".into(), at_unix: start + 60, error_kind: Some("auth".into()) },
            ]
        );
        let expected = pricing
            .cost_of("claude-sonnet-5", &aivyx_cost::TokenCounts { input: 1_000_000, ..Default::default() })
            .usd;
        assert!((a.spend_24h_usd - expected).abs() < 1e-9);
    }

    #[tokio::test]
    async fn gather_reads_missions_schedules_and_reminders() {
        use aivyx_crypto::MasterKey;
        use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};
        let dir = std::env::temp_dir().join(format!("aivyx-briefing-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let storage: Arc<dyn Storage> =
            RedbStorage::open(StorageConfig::new(dir.join("store.redb")), MasterKey::from_raw([3u8; 32]))
                .await
                .unwrap();
        let now = crate::activity::now_unix();
        let missions = storage.domain(KeyDomain::Missions);

        let mut failed = crate::mission::MissionRecord::new(
            "trg-1".into(),
            "default".into(),
            "cron trigger cfg-trend-scan: scan".into(),
        );
        failed.state = crate::mission::MissionState::Failed;
        crate::mission::create_mission(&missions, &failed).await.unwrap();

        let mut gated = crate::mission::MissionRecord::new("m-2".into(), "default".into(), "write the report".into());
        gated.state = crate::mission::MissionState::GatePending;
        gated.gates.push(crate::mission::GateRecord {
            gate_id: "g1".into(),
            reason: "send the weekly digest".into(),
            scope: None,
            state: crate::mission::GateState::Pending,
            created_at: 0,
            resolved_at: None,
        });
        crate::mission::create_mission(&missions, &gated).await.unwrap();

        let reminders: crate::reminder_tool::SharedReminderStore =
            Arc::new(crate::reminder_store::ReminderStore::new(storage.domain(KeyDomain::Reminders)));
        reminders
            .set(&crate::reminder_store::Reminder {
                id: "due".into(),
                due_unix: now - 10,
                message: "call mom".into(),
                notify_targets: vec![],
                created_unix: 0,
            })
            .await
            .unwrap();
        reminders
            .set(&crate::reminder_store::Reminder {
                id: "later".into(),
                due_unix: now + 9_999,
                message: "later".into(),
                notify_targets: vec![],
                created_unix: 0,
            })
            .await
            .unwrap();

        let pricing = aivyx_cost::Pricing::new();
        let src = BriefingSources {
            mission_store: Some(&missions),
            schedule_store: None,
            audit_log: None,
            persona_proposals: None,
            team_missions: None,
            reminders: Some(&reminders),
            memory: None,
            pricing: &pricing,
        };
        let f = gather(&src, Some(now - 3_600), now).await;
        assert_eq!(f.routine_runs.len(), 1);
        assert_eq!(f.routine_runs[0].what, "routine trend-scan");
        assert_eq!(f.routine_runs[0].failed, Some(true));
        assert_eq!(
            f.mission_gates,
            vec![GateFact { mission_id: "m-2".into(), gate_id: "g1".into(), reason: "send the weekly digest".into() }]
        );
        assert_eq!(f.in_progress, vec!["write the report".to_string()]);
        assert_eq!(
            f.due_reminders,
            vec![ReminderFact { id: "due".into(), message: "call mom".into(), due_unix: now - 10 }]
        );
        assert!(f.source_errors.is_empty());
        assert_eq!(f.spend_24h_usd, None);
        assert_eq!(f.memory_topics, None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
```

Before running, check every path these tests name and adjust only the
import paths, not the behaviour:

- `aivyx_core::VerificationSummary`, `aivyx_core::TokenUsage`, `aivyx_core::SessionId::new()`
- `aivyx_audit::MemoryOperation`, `aivyx_audit::TriggerKindSummary::Cron`
- `aivyx_cost::TokenCounts`
- `Default` on `TokenCounts`

Use these to check:

```bash
grep -n "pub enum TriggerKindSummary" -A10 crates/aivyx-audit/src/lib.rs
grep -n "pub use\|pub struct TokenCounts" -A6 crates/aivyx-cost/src/lib.rs | head -30
grep -n "pub struct TokenUsage" -B2 crates/aivyx-core/src/lib.rs
```

If `TokenCounts` has no `Default`, build it with all four fields, the way
`budget_gate.rs`'s `to_counts` does.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p aivyx-channel --lib briefing::gather_tests`
Expected: FAIL to compile (`audit_facts`, `trigger_label`, `gather` and
`BriefingSources` are undefined).

- [ ] **Step 3: Implement**

Add these imports at the top of `briefing.rs`:

```rust
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use aivyx_audit::{AuditEvent, AutoNotifyOutcomeSummary, PersistentAuditLog, SignedEntry};
use aivyx_storage::DomainHandle;
```

Then add this above `#[cfg(test)] mod tests`:

```rust
/// Read handles for [`gather`]. Every source is optional; a missing one is
/// simply absent from the briefing, an unreadable one is named in
/// `Facts::source_errors`.
pub struct BriefingSources<'a> {
    pub mission_store: Option<&'a DomainHandle>,
    pub schedule_store: Option<&'a DomainHandle>,
    pub audit_log: Option<&'a PersistentAuditLog>,
    pub persona_proposals: Option<&'a crate::persona_proposal::PersistentPersonaProposalLog>,
    pub team_missions: Option<&'a crate::team_mission_driver::TeamMissionService>,
    pub reminders: Option<&'a crate::reminder_tool::SharedReminderStore>,
    pub memory: Option<&'a Arc<dyn aivyx_memory::Memory>>,
    pub pricing: &'a aivyx_cost::Pricing,
}

/// What the audit chain contributes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuditFacts {
    pub notifications: Vec<NotifyFact>,
    pub changes: Vec<(String, i64)>,
    pub memories_saved: Vec<i64>,
    pub spend_24h_usd: f64,
}

fn unix_of(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Pure fold over recent chain entries: window events for the log, and the
/// last 24 h of `LlmCost` for the spend instrument.
pub fn audit_facts(
    entries: &[SignedEntry],
    window_start: i64,
    now: i64,
    pricing: &aivyx_cost::Pricing,
) -> AuditFacts {
    let mut a = AuditFacts::default();
    for e in entries {
        let t = unix_of(e.appended_at);
        if let AuditEvent::LlmCost { model, usage, .. } = &e.event
            && t >= now - DAY
        {
            let counts = aivyx_cost::TokenCounts {
                input: usage.input_tokens as u64,
                output: usage.output_tokens as u64,
                cache_read: usage.cache_read_input_tokens as u64,
                cache_write: usage.cache_creation_input_tokens as u64,
            };
            a.spend_24h_usd += pricing.cost_of(model, &counts).usd;
        }
        if t < window_start {
            continue;
        }
        match &e.event {
            AuditEvent::ToolCall { scope_used, outcome: aivyx_core::ToolOutcomeSummary::Completed { .. }, .. }
                // Memory writes get their own line ("I saved N memories").
                if !aivyx_capability::is_read_only_base(scope_used.base())
                    && !scope_used.base().starts_with("memory.") =>
            {
                a.changes.push((scope_used.base().to_string(), t));
            }
            AuditEvent::MemoryAccess { operation: aivyx_audit::MemoryOperation::Write, .. } => {
                a.memories_saved.push(t);
            }
            AuditEvent::AutoNotifyDispatched { target_name, outcome, .. } => match outcome {
                AutoNotifyOutcomeSummary::Delivered => a.notifications.push(NotifyFact {
                    target: target_name.clone(),
                    at_unix: t,
                    error_kind: None,
                }),
                AutoNotifyOutcomeSummary::Failed { error_kind, .. } => a.notifications.push(NotifyFact {
                    target: target_name.clone(),
                    at_unix: t,
                    error_kind: Some(error_kind.clone()),
                }),
                _ => {}
            },
            _ => {}
        }
    }
    a
}

/// `"cron trigger cfg-digest: …"` → `"routine digest"`; other trigger
/// sources keep their name (`"webhook trigger deploy"`). Not a trigger
/// mission → `None`.
pub fn trigger_label(description: &str) -> Option<String> {
    let (head, _) = description.split_once(": ")?;
    let (source, id) = head.split_once(" trigger ")?;
    let id = id.strip_prefix("cfg-").unwrap_or(id);
    Some(if source == "cron" { format!("routine {id}") } else { format!("{source} trigger {id}") })
}

/// The newest entries back to `since` (inclusive), oldest first. Pages
/// backwards so a long chain isn't read in full.
fn read_since(log: &PersistentAuditLog, since: i64) -> Result<Vec<SignedEntry>, String> {
    const PAGE: u64 = 512;
    let mut end = log.len() as u64;
    let mut out: Vec<SignedEntry> = Vec::new();
    while end > 0 {
        let from = end.saturating_sub(PAGE);
        let page = log.entries_range(from, (end - from) as usize).map_err(|e| e.to_string())?;
        let reached = page.first().is_some_and(|e| unix_of(e.appended_at) < since);
        out.splice(0..0, page);
        if reached {
            break;
        }
        end = from;
    }
    out.retain(|e| unix_of(e.appended_at) >= since);
    Ok(out)
}

/// Category in words: `CommunicationStyle` → `communication style`.
fn category_words(debug_name: &str) -> String {
    let mut out = String::new();
    for (i, ch) in debug_name.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            out.push(' ');
        }
        out.extend(ch.to_lowercase());
    }
    out
}

/// Read every source into [`Facts`].
pub async fn gather(src: &BriefingSources<'_>, last_here: Option<i64>, now: i64) -> Facts {
    let (window_start, window_capped) = window(last_here, now);
    let mut f = Facts { last_here, window_start, window_capped, ..Default::default() };

    if let Some(store) = src.mission_store {
        match crate::mission::list_missions(store).await {
            Ok(records) => {
                for m in records {
                    let updated = (m.updated_at / 1000) as i64;
                    let label = trigger_label(&m.description);
                    if let Some(g) = m.pending_gate() {
                        f.mission_gates.push(GateFact {
                            mission_id: m.mission_id.clone(),
                            gate_id: g.gate_id.clone(),
                            reason: g.reason.clone(),
                        });
                    }
                    match (&label, m.state) {
                        (Some(what), crate::mission::MissionState::Completed | crate::mission::MissionState::Failed)
                            if updated >= window_start =>
                        {
                            f.routine_runs.push(RoutineRun {
                                what: what.clone(),
                                at_unix: updated,
                                failed: Some(m.state == crate::mission::MissionState::Failed),
                            });
                        }
                        (None, s) if !m.is_terminal() && s != crate::mission::MissionState::Created => {
                            f.in_progress.push(m.description.clone());
                        }
                        _ => {}
                    }
                }
            }
            Err(_) => f.source_errors.push("missions".into()),
        }
    }

    if let Some(store) = src.schedule_store {
        match crate::schedule::list_schedules(store).await {
            Ok(records) => {
                for r in records.iter().filter(|r| r.enabled) {
                    let name = r.schedule_id.strip_prefix("cfg-").unwrap_or(&r.schedule_id).to_string();
                    if let Some(next) = r.next_fire_time() {
                        f.upcoming.push(UpcomingFact { name: name.clone(), at_unix: next.timestamp() });
                    }
                    // Wrapped runs are already counted from their mission.
                    if !r.wrap_mission
                        && let Some(ms) = r.last_fired_at
                        && (ms / 1000) as i64 >= window_start
                    {
                        f.routine_runs.push(RoutineRun {
                            what: format!("routine {name}"),
                            at_unix: (ms / 1000) as i64,
                            failed: None,
                        });
                    }
                }
            }
            Err(_) => f.source_errors.push("routines".into()),
        }
    }

    if let Some(log) = src.audit_log {
        match read_since(log, window_start.min(now - DAY)) {
            Ok(entries) => {
                let a = audit_facts(&entries, window_start, now, src.pricing);
                f.notifications = a.notifications;
                f.changes = a.changes;
                f.memories_saved = a.memories_saved;
                f.spend_24h_usd = Some(a.spend_24h_usd);
            }
            Err(_) => f.source_errors.push("the audit trail".into()),
        }
    }

    if let Some(log) = src.persona_proposals {
        for p in log.list(crate::persona_proposal::ProposalStatusFilter::Pending) {
            let is_skill = p.proposed_op.category == crate::persona::PersonaDeltaCategory::LearnedSkill;
            f.proposals.push(ProposalFact {
                id: p.id.clone(),
                category: category_words(&format!("{:?}", p.proposed_op.category)),
                is_skill,
                reason: p.proposed_op.reason.clone(),
            });
        }
    }

    if let Some(svc) = src.team_missions {
        for m in svc.list() {
            if m.phase == aivyx_ipc::team_mission::TeamMissionPhase::AwaitingApproval
                && let Some(step) = &m.pending_gate
            {
                f.team_gates.push(TeamGateFact { mission_id: m.id.clone(), step: step.clone(), goal: m.goal.clone() });
            } else if !m.phase.is_terminal() {
                f.in_progress.push(m.goal.clone());
            }
        }
    }

    if let Some(store) = src.reminders {
        match store.due(now).await {
            Ok(due) => {
                f.due_reminders = due
                    .into_iter()
                    .map(|r| ReminderFact { id: r.id, message: r.message, due_unix: r.due_unix })
                    .collect();
            }
            Err(_) => f.source_errors.push("reminders".into()),
        }
    }

    if let Some(mem) = src.memory {
        match mem.list_topics().await {
            Ok(topics) => {
                f.memory_topics =
                    Some(topics.iter().filter(|t| !crate::prune_sink::is_internal_topic(t)).count() as u64);
            }
            Err(_) => f.source_errors.push("memory".into()),
        }
    }

    f
}
```

Then fix up the paths. The compiler will flag any mismatch; correct the path,
not the logic.

- **Path checks:**
  - `crate::persona::PersonaDeltaCategory` is used this way in `daemon_server.rs` at ~line 5752.
  - `crate::prune_sink::is_internal_topic` is called that way at ~line 5539.
  - `MissionState` must derive `PartialEq` and `Copy` for `m.state ==` and the tuple `match`; it is `#[derive(.., Copy, PartialEq, ..)]` at `mission.rs:19`. If it isn't `Copy`, match on `&m.state` instead.
  - `ScheduleRecord::next_fire_time()` returns `Option<chrono::DateTime<_>>`, so `.timestamp()` gives seconds.
  - `TeamMissionPhase` must derive `PartialEq`. If it doesn't, use `matches!(m.phase, TeamMissionPhase::AwaitingApproval)`.
- **Clean up the composer.** Remove the now-redundant `let _ = now;` in
  `compose` only if `now` became used; otherwise follow Task 3, Step 3's note.
- **Interim mission states.** `MissionState::Created` work isn't listed as
  in progress: trigger missions pass through `Created` for an instant, and a
  user mission that never started isn't "working on".

- [ ] **Step 4: Run to verify it passes**

Run:

```bash
cargo test -p aivyx-channel --lib briefing:: && cargo clippy -p aivyx-channel --all-targets -- -D warnings
```

Expected: all briefing tests PASS (8 composer and 3 gatherer), with zero
warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-channel/src/briefing.rs
git commit -s -m "feat(channel): briefing gatherer — missions, routines, chain, proposals, reminders

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: Daemon wiring and the end-to-end test

**Files:**
- Modify:
  - `crates/aivyx-channel/src/daemon_server.rs`:
    - `DaemonConfig` (~line 131);
    - its destructure (~line 746);
    - the `ConnectionContext` struct (~line 2135) and where it is built (~line 1821);
    - the `handle_connection` destructure (~line 2197);
    - the reader task (~line 2234);
    - the `FrontendMessage::Query` arm (~line 3113);
    - `run_daemon_compat` (~line 4068) and the other in-file `DaemonConfig {` literal (~line 4149)
  - `crates/aivyx-channel/tests/daemon_roundtrip_e2e.rs` (7 `DaemonConfig {` literals)
  - `crates/aivyx-cli/src/bin/aivyx.rs` (the `run_daemon(DaemonConfig {` at ~line 10482)
- Create: `crates/aivyx-channel/tests/briefing_e2e.rs`

**Interfaces:**
- Consumes:
  - `ActivityClock`, `is_operator_action` and `now_unix` (Task 2)
  - `BriefingSources`, `gather` and `compose` (Tasks 3–4)
- Produces:
  - `DaemonConfig.activity_store: Option<DomainHandle>`
  - Over IPC, `Query { payload: GetBriefing }` → `QueryResponse { payload: Briefing { briefing } }`

- [ ] **Step 1: Write the failing end-to-end test**

Create `crates/aivyx-channel/tests/briefing_e2e.rs`:

```rust
//! The Command Center briefing over the real daemon IPC server.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use aivyx_capability::CapabilitySet;
use aivyx_channel::LocalChannel;
use aivyx_channel::daemon_ipc::{
    DaemonEnvelope, FrameError, FrontendMessage, QueryPayload, QueryResponsePayload, decode_frame, encode_frame,
};
use aivyx_channel::daemon_server::run_daemon_compat;
use aivyx_core::{Agent, AgentId, CancellationToken, ChannelContext, Message, TurnOutcome};

struct QuietAgent {
    id: AgentId,
    caps: CapabilitySet,
}

#[async_trait]
impl Agent for QuietAgent {
    fn id(&self) -> AgentId {
        self.id
    }
    fn capabilities(&self) -> &CapabilitySet {
        &self.caps
    }
    async fn turn(&self, _m: Message, _c: &dyn ChannelContext) -> TurnOutcome {
        TurnOutcome::Completed { final_message: "ok".into(), tool_calls_made: 0, duration: Duration::from_millis(1) }
    }
}

async fn query(stream: &mut UnixStream, buf: &mut Vec<u8>, id: &str, payload: QueryPayload) -> QueryResponsePayload {
    let msg = FrontendMessage::Query { id: id.into(), payload };
    stream.write_all(&encode_frame(&msg).unwrap()).await.unwrap();
    loop {
        match decode_frame::<DaemonEnvelope>(buf) {
            Ok((env, n)) => {
                buf.drain(..n);
                if let DaemonEnvelope::QueryResponse { id: got, payload } = env
                    && got == id
                {
                    return payload;
                }
            }
            Err(FrameError::IncompleteBuf) => {
                let mut tmp = [0u8; 4096];
                let n = stream.read(&mut tmp).await.unwrap();
                assert!(n > 0, "daemon closed the connection");
                buf.extend_from_slice(&tmp[..n]);
            }
            Err(e) => panic!("decode: {e}"),
        }
    }
}

#[tokio::test]
async fn get_briefing_answers_over_ipc() {
    let dir: PathBuf = std::env::temp_dir().join(format!("aivyx-briefing-e2e-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("daemon.sock");
    let shutdown = CancellationToken::new();
    let agent: Arc<dyn Agent> = Arc::new(QuietAgent { id: AgentId::new(), caps: CapabilitySet::empty() });
    let channel = Arc::new(LocalChannel::new("briefing-e2e", Vec::<u8>::new()));
    let (s, sd) = (socket.clone(), shutdown.clone());
    tokio::spawn(async move {
        let _ = run_daemon_compat(&s, agent, channel, sd).await;
    });
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut stream = UnixStream::connect(&socket).await.unwrap();
    let mut buf = Vec::new();

    let QueryResponsePayload::Briefing { briefing } = query(&mut stream, &mut buf, "b1", QueryPayload::GetBriefing).await
    else {
        panic!("expected a Briefing");
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    assert_eq!(briefing.last_active_unix, None);
    assert!((briefing.window_start_unix - (now - 24 * 3600)).abs() < 60);
    assert!(!briefing.window_capped);

    // No reminder store in the compat daemon → a clean "not ok", not an error.
    let resp = query(&mut stream, &mut buf, "r1", QueryPayload::CompleteReminder { id: "x".into() }).await;
    assert_eq!(resp, QueryResponsePayload::ReminderUpdated { id: "x".into(), ok: false, due_unix: None });

    shutdown.cancel();
    let _ = std::fs::remove_dir_all(&dir);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p aivyx-channel --test briefing_e2e`
Expected: FAIL. The assertion `expected a Briefing` panics, because
`handle_query` answers `QueryError { code: "briefing_unavailable" }`.

- [ ] **Step 3: Thread the clock through the daemon**

1. **`DaemonConfig`.** Add this after `reminder_store`:

   ```rust
       /// Command Center — where the operator's last activity is persisted
       /// (the `ChannelState` domain). `None` ⇒ kept in memory for this run.
       pub activity_store: Option<DomainHandle>,
   ```

2. **`run_daemon`.** Add `activity_store,` to the `let DaemonConfig { … } = cfg;`
   destructure (~line 746). Right after the destructure, add:

   ```rust
       let activity = Arc::new(crate::activity::ActivityClock::load(activity_store).await);
   ```

3. **`ConnectionContext`.** Add these fields after `reminder_store`:

   ```rust
       /// Command Center — the daemon's one operator-activity clock.
       activity: Arc<crate::activity::ActivityClock>,
       /// Command Center — prices the briefing's 24 h spend.
       pricing: aivyx_cost::Pricing,
   ```

   Where the context is built (~line 1821), add:

   ```rust
               activity: Arc::clone(&activity),
               pricing: pricing.clone(),
   ```

   If `pricing` was moved earlier, for example into `ld_pricing` at ~line 1045,
   clone it into a `let briefing_pricing = pricing.clone();` before that move
   and use `briefing_pricing.clone()` here.

4. **`handle_connection`.** Add `activity,` and `pricing,` to its destructure
   (~line 2197).

5. **Reader task (~line 2234).** Before `tokio::spawn`, add
   `let reader_activity = Arc::clone(&activity);`. Inside, change the
   `Ok((msg, consumed))` arm to:

   ```rust
                       Ok((msg, consumed)) => {
                           buf.drain(..consumed);
                           // Command Center — stamp what the operator *does*
                           // here, so it counts even mid-turn (an approval).
                           if crate::activity::is_operator_action(&msg) {
                               reader_activity.touch(crate::activity::now_unix()).await;
                           }
                           if frame_tx.send(Ok(msg)).is_err() {
                               return;
                           }
                       }
   ```

6. **Query arm (~line 3113).** Answer `GetBriefing` before `handle_query`:

   ```rust
                        FrontendMessage::Query { id, payload } => {
                            let response_payload = if matches!(payload, QueryPayload::GetBriefing) {
                                let now = crate::activity::now_unix();
                                let sources = crate::briefing::BriefingSources {
                                    mission_store: mission_store.as_deref(),
                                    schedule_store: schedule_store.as_deref(),
                                    audit_log: audit_log.as_deref(),
                                    persona_proposals: persona_proposal_log.as_deref(),
                                    team_missions: team_missions.as_ref(),
                                    reminders: reminder_store.as_ref(),
                                    memory: memory.as_ref(),
                                    pricing: &pricing,
                                };
                                let facts = crate::briefing::gather(
                                    &sources,
                                    activity.snapshot().last_here(now),
                                    now,
                                )
                                .await;
                                QueryResponsePayload::Briefing {
                                    briefing: crate::briefing::compose(&facts, now),
                                }
                            } else {
                                handle_query(
                                    payload,
                                    /* …the existing argument list, unchanged… */
                                )
                                .await
                            };
   ```

   Keep the existing argument list exactly as it is (do not elide it in the
   real code). The `.as_deref()` and `.as_ref()` forms must match how each
   binding is passed to `handle_query` today:
   - `mission_store.as_deref()`, `audit_log.as_deref()`,
     `persona_proposal_log.as_deref()`;
   - `team_missions.as_ref()`, `reminder_store.as_ref()`, `memory.as_ref()`.

   Copy each one from that call.

7. **`DaemonConfig` literals.** Add `activity_store: None,` to every other
   `DaemonConfig {` literal: `run_daemon_compat` (~4068), the literal at
   ~4149, and the 7 in `tests/daemon_roundtrip_e2e.rs`. Find them with:

   ```bash
   grep -rn "DaemonConfig {" crates | grep "\.rs:"
   ```

8. **CLI.** In `crates/aivyx-cli/src/bin/aivyx.rs`'s
   `run_daemon(DaemonConfig {` (~10482), add this next to `reminder_store`:

   ```rust
               activity_store: Some(storage.domain(KeyDomain::ChannelState)),
   ```

- [ ] **Step 4: Run the e2e test, the channel suite and clippy**

Run:

```bash
cargo test -p aivyx-channel --test briefing_e2e && cargo test -p aivyx-channel && cargo clippy --workspace --all-targets -- -D warnings
```

Expected: all PASS, with zero warnings.

- [ ] **Step 5: Commit**

```bash
git add -u crates/aivyx-channel crates/aivyx-cli
git add crates/aivyx-channel/tests/briefing_e2e.rs
git commit -s -m "feat(daemon): answer GetBriefing; stamp operator activity per frame

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: The Studio logbook

**Files:**
- Create: `crates/aivyx-web/src/command_center.rs`
- Modify:
  - `crates/aivyx-web/src/main.rs`:
    - `mod command_center;` next to `mod cron_text;` (~line 40);
    - a `BriefingState` signal, provided as context;
    - the `ws_task`/`read_task` params plus a response arm;
    - a briefing poll;
    - `CommandPanel` (~line 1779) rewritten;
    - `CommandSkeleton` simplified;
    - components that become unused, deleted.
  - `crates/aivyx-web/assets/stitch.css` (append `.cc-*` rules)
- Test: `crates/aivyx-web/src/command_center.rs` (pure helpers, run natively)

**Interfaces:**
- Consumes:
  - `aivyx_ipc::briefing::*`
  - the `QueryPayload::{GetBriefing, CompleteReminder, SnoozeReminder, ResolveTeamGate}` variants
  - `FrontendMessage::ResolveGate`
  - the existing `ApprovalCard`, `PendingApproval`, `Dashboard`, `View::from_slug` and `Sender`
- Produces:
  - `pub fn greeting(hour: u32) -> &'static str`
  - `pub fn ago(then: i64, now: i64) -> String`
  - `pub fn hhmm(hour: u32, minute: u32) -> String`
  - `pub fn when_label(day_offset: i64, hhmm: &str) -> String`
  - `pub fn log_heading(b: &Briefing) -> &'static str`
  - `pub fn spend_reading(spend: Option<f64>, per_day: Option<f64>) -> (String, bool)`
  - `pub fn more_line(n: u32) -> Option<String>`
  - `#[derive(Clone, Default, PartialEq)] pub struct BriefingState { pub briefing: Option<Briefing>, pub as_of_ms: f64, pub notice: Option<String> }`
  - `pub fn briefing_query() -> FrontendMessage`
  - `#[component] pub fn Logbook(dashboard: Dashboard, connected: bool, view: Signal<View>, first_visit: bool, name: String) -> Element`

`Dashboard` and `View` are private types in `main.rs`. `command_center` is a
child module, so it reaches them with `use crate::{Dashboard, View, …}`
(child modules may use private items of their parent).

- [ ] **Step 1: Write the pure helpers with failing tests**

Create `crates/aivyx-web/src/command_center.rs`:

```rust
//! The Command Center as a logbook: an instrument strip, a greeting, what
//! needs the operator, what the assistant did since they were last here, and
//! what's coming up — all from the daemon's `Briefing`. The helpers at the
//! top are pure (tested natively); the components below render them.

use aivyx_ipc::briefing::Briefing;

/// The headline for a local hour (0–23).
pub fn greeting(hour: u32) -> &'static str {
    match hour {
        5..=11 => "Good morning.",
        12..=17 => "Good afternoon.",
        _ => "Good evening.",
    }
}

/// "just now", "12 min ago", "9 h ago", "3 days ago".
pub fn ago(then: i64, now: i64) -> String {
    let d = (now - then).max(0);
    match d {
        0..=59 => "just now".to_string(),
        60..=3_599 => format!("{} min ago", d / 60),
        3_600..=86_399 => format!("{} h ago", d / 3_600),
        _ if d < 2 * 86_400 => "1 day ago".to_string(),
        _ => format!("{} days ago", d / 86_400),
    }
}

pub fn hhmm(hour: u32, minute: u32) -> String {
    format!("{hour:02}:{minute:02}")
}

/// A "Coming up" time: today → "19:00", tomorrow → "tmrw 07:00",
/// later → "in 3 d · 07:00".
pub fn when_label(day_offset: i64, hhmm: &str) -> String {
    match day_offset {
        i64::MIN..=0 => hhmm.to_string(),
        1 => format!("tmrw {hhmm}"),
        n => format!("in {n} d · {hhmm}"),
    }
}

pub fn log_heading(b: &Briefing) -> &'static str {
    if b.window_capped {
        "In the last 7 days"
    } else if b.last_active_unix.is_none() {
        "In the last 24 hours"
    } else {
        "Since you were last here"
    }
}

/// The spend instrument and whether it should warn (≥ 80 % of the day cap).
pub fn spend_reading(spend: Option<f64>, per_day: Option<f64>) -> (String, bool) {
    match (spend, per_day) {
        (None, _) => ("spend —".to_string(), false),
        (Some(s), Some(cap)) if cap > 0.0 => (format!("${s:.2} of ${cap:.2} · 24 h"), s >= 0.8 * cap),
        (Some(s), _) => (format!("${s:.2} · 24 h"), false),
    }
}

pub fn more_line(n: u32) -> Option<String> {
    (n > 0).then(|| format!("and {n} more → Audit"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greeting_turns_at_five_noon_and_six() {
        assert_eq!(greeting(4), "Good evening.");
        assert_eq!(greeting(5), "Good morning.");
        assert_eq!(greeting(11), "Good morning.");
        assert_eq!(greeting(12), "Good afternoon.");
        assert_eq!(greeting(17), "Good afternoon.");
        assert_eq!(greeting(18), "Good evening.");
        assert_eq!(greeting(23), "Good evening.");
    }

    #[test]
    fn ago_reads_in_one_unit() {
        assert_eq!(ago(100, 130), "just now");
        assert_eq!(ago(0, 720), "12 min ago");
        assert_eq!(ago(0, 9 * 3_600 + 5), "9 h ago");
        assert_eq!(ago(0, 86_400 + 5), "1 day ago");
        assert_eq!(ago(0, 3 * 86_400), "3 days ago");
        assert_eq!(ago(500, 100), "just now");
    }

    #[test]
    fn coming_up_times() {
        assert_eq!(hhmm(7, 0), "07:00");
        assert_eq!(when_label(0, "19:00"), "19:00");
        assert_eq!(when_label(1, "07:00"), "tmrw 07:00");
        assert_eq!(when_label(3, "07:00"), "in 3 d · 07:00");
    }

    #[test]
    fn the_log_heading_follows_the_window() {
        let mut b = Briefing { last_active_unix: Some(1), ..Default::default() };
        assert_eq!(log_heading(&b), "Since you were last here");
        b.window_capped = true;
        assert_eq!(log_heading(&b), "In the last 7 days");
        let b = Briefing::default();
        assert_eq!(log_heading(&b), "In the last 24 hours");
    }

    #[test]
    fn spend_warns_at_eighty_percent_of_the_cap() {
        assert_eq!(spend_reading(None, Some(2.0)), ("spend —".to_string(), false));
        assert_eq!(spend_reading(Some(0.42), None), ("$0.42 · 24 h".to_string(), false));
        assert_eq!(spend_reading(Some(0.42), Some(2.0)), ("$0.42 of $2.00 · 24 h".to_string(), false));
        assert_eq!(spend_reading(Some(1.6), Some(2.0)).1, true);
    }

    #[test]
    fn more_line_only_when_something_was_left_out() {
        assert_eq!(more_line(0), None);
        assert_eq!(more_line(3).as_deref(), Some("and 3 more → Audit"));
    }
}
```

Add `mod command_center;` to `main.rs` next to `mod cron_text;`.

- [ ] **Step 2: Run the helper tests**

Run (native):

```bash
PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$HOME/.cargo/bin:$PATH" cargo test -p aivyx-web command_center
```

Expected: 6 PASS. The helpers are written with their tests in the same step,
so these pass straight away; that is acceptable for pure copy helpers. To see
the red first, temporarily change one expected string, watch it fail, then
restore it.

- [ ] **Step 3: Add the state, the query and the components**

Append this to `command_center.rs`, below the helpers and above
`#[cfg(test)]`:

```rust
use aivyx_ipc::briefing::{NeedsYouAction, NeedsYouItem};
use aivyx_ipc::protocol::{FrontendMessage, QueryPayload};
use dioxus::prelude::*;

use crate::{ApprovalCard, Dashboard, PendingApproval, Sender, View};

/// The latest briefing, when it arrived, and the last failed action.
#[derive(Clone, Default, PartialEq)]
pub struct BriefingState {
    pub briefing: Option<Briefing>,
    /// `Date.now()` when `briefing` arrived — the "as of" line when offline.
    pub as_of_ms: f64,
    pub notice: Option<String>,
}

pub fn briefing_query() -> FrontendMessage {
    FrontendMessage::Query { id: "cc-briefing".to_string(), payload: QueryPayload::GetBriefing }
}

/// Local (hour, minute, day number) for a unix time — wasm only.
fn local_parts(unix: i64) -> (u32, u32, i64) {
    let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(unix as f64 * 1000.0));
    let offset_ms = d.get_timezone_offset() * 60_000.0;
    let day = ((unix as f64 * 1000.0 - offset_ms) / 86_400_000.0).floor() as i64;
    (d.get_hours(), d.get_minutes(), day)
}

fn now_unix() -> i64 {
    (js_sys::Date::now() / 1000.0) as i64
}

fn go(view: Signal<View>, slug: &str) {
    if let Some(v) = View::from_slug(slug) {
        let mut view = view;
        view.set(v);
    }
}

/// Send an action, then re-ask for the briefing on the same connection, so
/// the answer already reflects it (frames are handled in order).
fn act(ws: Sender, msg: FrontendMessage) {
    ws.send(msg);
    ws.send(briefing_query());
}

#[component]
fn InstrumentStrip(dashboard: Dashboard, connected: bool, briefing: Option<Briefing>, view: Signal<View>) -> Element {
    let (model, ctx) = match &dashboard.settings {
        Some(s) => (s.model.clone(), s.num_ctx.map(|n| format!("{}k ctx", n / 1024)).unwrap_or_else(|| "default ctx".into())),
        None => ("model —".to_string(), String::new()),
    };
    let per_day = dashboard.settings.as_ref().and_then(|s| s.budget.per_day_usd);
    let (spend, spend_warn) = spend_reading(briefing.as_ref().and_then(|b| b.spend_24h_usd), per_day);
    let memory = briefing
        .as_ref()
        .and_then(|b| b.memory_topics)
        .map(|n| format!("{n} topics"))
        .unwrap_or_else(|| "memory —".into());
    let (chain, chain_bad) = match dashboard.chain_ok {
        Some(true) => ("◆ chain sealed", false),
        Some(false) => ("◆ chain broken", true),
        None => ("◆ chain …", false),
    };
    rsx! {
        div { class: "cc-strip",
            button { class: if connected { "cc-reading" } else { "cc-reading bad" }, onclick: move |_| go(view, "sessions"),
                if connected { "● daemon" } else { "● offline" }
            }
            button { class: "cc-reading", onclick: move |_| go(view, "models"), "{model} · {ctx}" }
            button { class: if spend_warn { "cc-reading warn" } else { "cc-reading" }, onclick: move |_| go(view, "settings"), "{spend}" }
            button { class: "cc-reading", onclick: move |_| go(view, "memory"), "{memory}" }
            button { class: if chain_bad { "cc-reading bad" } else { "cc-reading" }, onclick: move |_| go(view, "audit"), "{chain}" }
        }
    }
}

#[component]
fn NeedsYouCard(item: NeedsYouItem, view: Signal<View>) -> Element {
    let ws = use_context::<Sender>();
    let link = item.link.clone();
    let buttons = match item.action.clone() {
        NeedsYouAction::MissionGate { mission_id, gate_id } => {
            let (m2, g2) = (mission_id.clone(), gate_id.clone());
            rsx! {
                button { class: "btn", onclick: move |_| act(ws, FrontendMessage::ResolveGate { mission_id: mission_id.clone(), gate_id: gate_id.clone(), approved: true }), "Approve" }
                button { class: "btn ghost", onclick: move |_| act(ws, FrontendMessage::ResolveGate { mission_id: m2.clone(), gate_id: g2.clone(), approved: false }), "Deny" }
            }
        }
        NeedsYouAction::TeamGate { mission_id, step } => {
            let (m2, s2) = (mission_id.clone(), step.clone());
            rsx! {
                button { class: "btn", onclick: move |_| act(ws, FrontendMessage::Query { id: "cc-action".into(), payload: QueryPayload::ResolveTeamGate { mission_id: mission_id.clone(), step: step.clone(), approve: true } }), "Approve" }
                button { class: "btn ghost", onclick: move |_| act(ws, FrontendMessage::Query { id: "cc-action".into(), payload: QueryPayload::ResolveTeamGate { mission_id: m2.clone(), step: s2.clone(), approve: false } }), "Deny" }
            }
        }
        NeedsYouAction::Reminder { id } => {
            let id2 = id.clone();
            rsx! {
                button { class: "btn", onclick: move |_| act(ws, FrontendMessage::Query { id: "cc-action".into(), payload: QueryPayload::CompleteReminder { id: id.clone() } }), "Done" }
                button { class: "btn ghost", onclick: move |_| act(ws, FrontendMessage::Query { id: "cc-action".into(), payload: QueryPayload::SnoozeReminder { id: id2.clone(), secs: 3_600 } }), "Snooze 1 h" }
            }
        }
        NeedsYouAction::Review => rsx! {
            button { class: "btn", onclick: move |_| go(view, &link), "Review" }
        },
        NeedsYouAction::Look => rsx! {
            button { class: "btn ghost", onclick: move |_| go(view, &link), "Open" }
        },
    };
    rsx! {
        div { class: "cc-card",
            p { class: "cc-sentence", "{item.sentence}" }
            if let Some(d) = &item.detail {
                p { class: "muted", "{d}" }
            }
            div { class: "cc-actions", {buttons} }
        }
    }
}

/// The whole home screen.
#[component]
pub fn Logbook(dashboard: Dashboard, connected: bool, view: Signal<View>, first_visit: bool, name: String) -> Element {
    let state = use_context::<Signal<BriefingState>>();
    let pending = use_context::<Signal<Option<PendingApproval>>>();
    let s = state();
    let now = now_unix();
    let (hour, _, today) = local_parts(now);
    let date_line = js_sys::Date::new_0().to_date_string().as_string().unwrap_or_default();

    rsx! {
        div { class: if connected { "cc-page" } else { "cc-page stale" },
            InstrumentStrip { dashboard: dashboard.clone(), connected, briefing: s.briefing.clone(), view }
            if first_visit {
                div { class: "glass-card welcome-card",
                    h3 { "Start here: say hi to {name}" }
                    p { class: "muted",
                        "Chat is where you ask for things — try “What can you help me with?” "
                        "This page fills in as it works: what needs you, what it did, and what's next."
                    }
                    div { class: "welcome-actions",
                        button { class: "btn", onclick: move |_| go(view, "chat"), "Open Chat" }
                        button { class: "btn ghost", onclick: move |_| go(view, "guide"), "Read the guide" }
                    }
                }
            }
            h1 { class: "cc-greeting", "{greeting(hour)}" }
            p { class: "label-tech cc-dateline",
                "{date_line}"
                if let Some(t) = s.briefing.as_ref().and_then(|b| b.last_active_unix) {
                    " · last here {ago(t, now)}"
                }
                if !connected && s.as_of_ms > 0.0 {
                    {
                        let (h, m, _) = local_parts((s.as_of_ms / 1000.0) as i64);
                        rsx! { " · as of {hhmm(h, m)}" }
                    }
                }
            }

            match &s.briefing {
                None => rsx! { p { class: "label-tech", "Reading the record…" } },
                Some(b) => {
                    let count = b.needs_you.len() + usize::from(pending().is_some());
                    rsx! {
                        h2 { class: "label-tech cc-needs", "Needs you · {count}" }
                        if let Some(n) = &s.notice {
                            p { class: "cc-notice", "{n}" }
                        }
                        if let Some(p) = pending() {
                            ApprovalCard { p }
                        }
                        if count == 0 {
                            p { class: "cc-quiet", "Nothing needs you." }
                        }
                        for item in b.needs_you.iter() {
                            NeedsYouCard { key: "{item.key}", item: item.clone(), view }
                        }

                        h2 { class: "label-tech cc-section", "{log_heading(b)}" }
                        if b.log.is_empty() {
                            p { class: "cc-quiet", "All quiet since you were last here." }
                        }
                        for (i, line) in b.log.iter().enumerate() {
                            {
                                let (h, m, _) = local_parts(line.at_unix);
                                let slug = line.link.clone();
                                rsx! {
                                    button { key: "{i}", class: "cc-entry", onclick: move |_| go(view, &slug),
                                        span { class: if line.warn { "cc-time warn" } else { "cc-time" }, "{hhmm(h, m)}" }
                                        span { "{line.sentence}" }
                                    }
                                }
                            }
                        }
                        if let Some(more) = more_line(b.log_more) {
                            button { class: "cc-entry cc-more", onclick: move |_| go(view, "audit"), "{more}" }
                        }

                        h2 { class: "label-tech cc-section", "Coming up" }
                        if b.coming_up.is_empty() {
                            button { class: "cc-entry cc-quiet", onclick: move |_| go(view, "schedules"), "Nothing scheduled." }
                        }
                        for (i, up) in b.coming_up.iter().enumerate() {
                            {
                                let label = match up.at_unix {
                                    Some(t) => {
                                        let (h, m, day) = local_parts(t);
                                        when_label(day - today, &hhmm(h, m))
                                    }
                                    None => "now".to_string(),
                                };
                                let slug = up.link.clone();
                                rsx! {
                                    button { key: "{i}", class: "cc-entry", onclick: move |_| go(view, &slug),
                                        span { class: "cc-time", "{label}" }
                                        span { "{up.sentence}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
```

Compile notes (Dioxus 0.6):

- **Clones in `onclick` closures.** Each closure is `FnMut`, so any captured
  `String` passed by value must be cloned inside it, as shown
  (`mission_id.clone()`). If the borrow checker complains about `link` being
  moved into two closures in `NeedsYouCard`, clone it per arm:
  `let link = item.link.clone();` inside each arm.
- **`View::from_slug`.** It may be a private associated fn. Child modules can
  call it, so `crate::View::from_slug` works.
- **Visibility.** `ApprovalCard`, `PendingApproval`, `Dashboard`, `Sender` and
  `View` are private items of `main.rs`. A child module can `use crate::…`
  them.

- [ ] **Step 4: Wire the state, routing and polling into `main.rs`**

1. **State and context.** Next to `let dashboard = use_signal(Dashboard::default);`
   (~line 1032), add:

   ```rust
       let briefing = use_signal(command_center::BriefingState::default);
       use_context_provider(|| briefing);
   ```

2. **Task parameters.** Add `briefing` as a new last argument to the
   `ws_task(...)` call (~line 1087). In both `ws_task` and `read_task`
   signatures, add a trailing parameter
   `mut briefing: Signal<command_center::BriefingState>`, and pass it from
   `ws_task` into `read_task`. Drop `mut` on `ws_task`'s copy if clippy says
   it's unused.

3. **Response arms.** In `read_task`'s response `match`, next to the
   `QueryResponsePayload::Reminders` arm (~line 10903), add:

   ```rust
                   DaemonEnvelope::QueryResponse {
                       payload: QueryResponsePayload::Briefing { briefing: b },
                       ..
                   } => {
                       let mut s = briefing.write();
                       s.briefing = Some(b);
                       s.as_of_ms = js_sys::Date::now();
                   }
                   DaemonEnvelope::QueryResponse {
                       payload: QueryResponsePayload::ReminderUpdated { ok, .. },
                       ..
                   } => {
                       briefing.write().notice = (!ok).then(|| "That reminder couldn't be updated — it may already be gone.".to_string());
                   }
                   DaemonEnvelope::QueryResponse { id, payload: QueryResponsePayload::QueryError { message, .. } }
                       if id == "cc-action" || id == "cc-briefing" =>
                   {
                       briefing.write().notice = Some(message);
                   }
   ```

   The `QueryError` arm must come **before** any existing catch-all
   `QueryError` arm. Check:

   ```bash
   grep -n "QueryResponsePayload::QueryError" crates/aivyx-web/src/main.rs | head
   ```

   Place it above the first match.

4. **Poll.** After the live-poll `use_future` (~line 1236), add:

   ```rust
       // Command Center — the briefing walks the recent audit chain, so it
       // refreshes every 10 s (and right after each card action), not on the
       // 1.5 s live poll.
       use_future(move || async move {
           loop {
               if view() == View::Command && connected() {
                   ws.send(command_center::briefing_query());
               }
               TimeoutFuture::new(10_000).await;
           }
       });
   ```

   `connected` is the existing `Signal<bool>` (~line 1097). If it is declared
   after this point, place the poll after its declaration.

5. **Immediate fetch on Command view.** So the page doesn't wait 10 s, add an
   effect next to the poll:

   ```rust
       use_effect(move || {
           if view() == View::Command && connected() {
               ws.send(command_center::briefing_query());
           }
       });
   ```

6. **`CommandPanel`.** Replace its whole body (~lines 1779–1900) with:

   ```rust
   #[component]
   fn CommandPanel(
       missions: Vec<TeamMissionView>,
       dashboard: Dashboard,
       connected: bool,
       view: Signal<View>,
   ) -> Element {
       if !dashboard.loaded {
           return rsx! { CommandSkeleton {} };
       }
       let first_visit = missions.is_empty() && nothing_has_happened_yet(&dashboard);
       let name = dashboard
           .assistant_name
           .clone()
           .unwrap_or_else(|| "your assistant".to_string());
       rsx! {
           command_center::Logbook { dashboard, connected, view, first_visit, name }
       }
   }
   ```

7. **`CommandSkeleton`.** Replace its body with the strip and three quiet
   bars, so swapping in live data doesn't shift the page:

   ```rust
   #[component]
   fn CommandSkeleton() -> Element {
       rsx! {
           div { class: "cc-page",
               div { class: "cc-strip skeleton", "\u{00a0}" }
               div { class: "skeleton cc-skel-line wide" }
               div { class: "skeleton cc-skel-line" }
               div { class: "skeleton cc-skel-line" }
           }
       }
   }
   ```

8. **Delete unused components.** Run `just check-web`, which uses
   `-D warnings` through clippy if configured, or watch for `dead_code`
   warnings. Delete each component or helper that is now unused, typically
   `StatCard`, `DashMissionRow`, `RoutineRow`, `AuditFeed`, `AgentStatus`,
   `chain_label` and `chain_tone`, **only if** nothing else references it:

   ```bash
   grep -n "StatCard\b\|DashMissionRow\|RoutineRow\|AuditFeed\|AgentStatus\|chain_label\|chain_tone" crates/aivyx-web/src/main.rs
   ```

   Keep any that other screens use.

- [ ] **Step 5: Styles**

Append this to `crates/aivyx-web/assets/stitch.css`. Use the file's existing
custom properties for colours; check their names with:

```bash
grep -n "^\s*--" crates/aivyx-web/assets/stitch.css | head -40
```

Substitute the real names for `--brass`, `--rust`, `--warn`, `--rule`,
`--ink-muted` and `--surface` below if they differ.

```css
/* Command Center — the logbook */
.cc-page { max-width: 760px; margin: 0 auto; display: flex; flex-direction: column; gap: 10px; }
.cc-page.stale > *:not(.cc-strip) { opacity: .55; }
.cc-strip { display: flex; flex-wrap: wrap; gap: 0; border: 1px solid var(--rule); border-radius: 4px; }
.cc-reading { font-family: 'IBM Plex Mono', ui-monospace, monospace; font-size: 11px; letter-spacing: .08em;
  text-transform: uppercase; color: var(--ink-muted); background: none; border: 0; border-right: 1px solid var(--rule);
  padding: 6px 10px; cursor: pointer; }
.cc-reading:last-child { border-right: 0; }
.cc-reading:hover { color: var(--brass); }
.cc-reading.warn { color: var(--warn); }
.cc-reading.bad { color: var(--rust); }
.cc-greeting { font-family: 'Fraunces', Georgia, serif; font-weight: 600; font-size: 34px; margin: 18px 0 0; }
.cc-dateline { margin: 0 0 8px; }
.cc-needs { color: var(--brass); margin-top: 10px; }
.cc-section { margin-top: 18px; padding-top: 10px; border-top: 1px solid var(--rule); }
.cc-card { background: var(--surface); border: 1px solid var(--rule); border-radius: 4px; padding: 10px 12px; }
.cc-sentence { margin: 0 0 6px; }
.cc-actions { display: flex; gap: 8px; }
.cc-entry { display: flex; gap: 14px; width: 100%; text-align: left; background: none; border: 0; color: inherit;
  padding: 4px 0; cursor: pointer; font: inherit; }
.cc-entry:hover span:last-child { color: var(--brass); }
.cc-time { flex: none; width: 92px; font-family: 'IBM Plex Mono', ui-monospace, monospace; font-size: 12px; color: var(--ink-muted); }
.cc-time.warn { color: var(--warn); }
.cc-quiet, .cc-more { color: var(--ink-muted); }
.cc-notice { color: var(--rust); margin: 0; }
.cc-skel-line { height: 14px; border-radius: 3px; margin: 8px 0; width: 60%; }
.cc-skel-line.wide { height: 34px; width: 45%; }
@media (max-width: 600px) { .cc-greeting { font-size: 26px; } .cc-time { width: 72px; } }
```

- [ ] **Step 6: Compile, test and lint**

Run:

```bash
export PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$HOME/.cargo/bin:$PATH"
just check-web && cargo test -p aivyx-web && cargo clippy --workspace --all-targets -- -D warnings
```

Expected: the wasm build is clean, all `aivyx-web` tests pass (including
`first_visit_tests` and the 6 new ones), and there are zero warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx-web/src crates/aivyx-web/assets/stitch.css
git commit -s -m "feat(studio): Command Center as a logbook

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: Docs, bundle and live check

**Files:**
- Modify:
  - `docs/DAEMON_IPC.md`
  - the guide page that describes the Command Center (find it with `grep -rln "Command Center" docs/guide`)
  - `CHANGELOG.md` (`## [Unreleased]`)
  - `crates/aivyx-web/dist/*` (rebuilt bundle)

**Interfaces:**
- Consumes: everything above.
- Produces: no code interfaces.

- [ ] **Step 1: DAEMON_IPC.md**

Next to the `GetReminders` entry (`grep -n "GetReminders" docs/DAEMON_IPC.md`),
document:

- `GetBriefing` → `Briefing { briefing }`, including the field meanings
  from `aivyx-ipc/src/briefing.rs`. Note that it is answered by the
  connection loop and is read-only, so it does not count as activity.
- `CompleteReminder { id }` and `SnoozeReminder { id, secs }` →
  `ReminderUpdated { id, ok, due_unix }`. Both count as operator activity.
- A short "Operator activity" paragraph covering:
  - which frames count (the `is_operator_action` list);
  - visits of 30 min;
  - the 24 h fallback and the 7-day cap;
  - persistence in the encrypted `ChannelState` domain;
  - the fact that in-daemon chat apps (Telegram, Discord, Slack) don't count
    yet.

- [ ] **Step 2: Guide**

In the guide page found above, replace the description of the old stat cards
and panels with the logbook:

- the instrument strip;
- "Needs you" and what each card does;
- "Since you were last here";
- "Coming up".

Say plainly that the log is written from the record with fixed wording, not
by a model, and that it can't name which files changed (the audit trail keeps
only a hash of each call's arguments).

- [ ] **Step 3: CHANGELOG**

Under `## [Unreleased]` → `### Changed` (create either heading if missing),
add:

```markdown
- **Studio Command Center is now a logbook.** One readable column instead of stat cards: a ruled instrument strip (daemon, model, 24 h spend against your day budget, memory, audit chain), what needs you (mission and team approvals, pending proposals, failed routines and notifications, due reminders — with Approve/Deny/Done/Snooze right there), a first-person log of what your assistant did since you were last here, and what's coming up. Written from the record with fixed wording — no model involved.
```

Under `### Added`, add:

```markdown
- **IPC:** `GetBriefing`, `CompleteReminder`, `SnoozeReminder` queries (see `docs/DAEMON_IPC.md`); the daemon remembers your last activity across restarts.
```

Then check the release secret-mask pitfall:

```bash
grep -niE "bearer|authorization|token=" CHANGELOG.md | head
```

Expected: no new hits.

- [ ] **Step 4: Rebuild the bundle**

```bash
export PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$HOME/.cargo/bin:$PATH"
just build-web && find crates/aivyx-web/dist -name '*.br' -delete
```

Expected: `crates/aivyx-web/dist/` is updated and holds no `.br` files.

- [ ] **Step 5: Full verification**

```bash
cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

Expected: zero warnings, and all tests pass. If anything fails, stop and fix
it before continuing.

- [ ] **Step 6: Live check**

1. Build the CLI and start it in the scratch home:

   ```bash
   cargo build -p aivyx-cli
   source /tmp/claude-1000/-home-julian-Projects-Rust/5d3d9165-cd90-484b-bf84-962d37590e4e/scratchpad/a3/env.sh
   ```

   Stop any running scratch daemon first: an old daemon locks the binary
   ("text file busy").

2. Start the daemon against Lemonade, which is already running on :13305;
   leave it running.

3. Seed one due reminder through chat ("remind me in 1 minute to stretch"),
   wait a minute, then load the Studio with
   `node .../scratchpad/a2/cdp.mjs`.

4. Screenshot three states:
   - **busy:** the reminder card, at least one log line, and "Coming up" if a
     routine is configured;
   - **after Done:** the card is gone and "Nothing needs you." shows;
   - **offline:** stop the daemon; the strip says "● offline" and the page
     dims with "as of HH:MM".

5. Read each screenshot and check it against the spec's visual rules:
   - flat ruled surfaces;
   - a Fraunces greeting;
   - mono times;
   - brass for "Needs you".

Fix anything that's off, rebuild (Step 4) and re-shoot.

- [ ] **Step 7: Commit**

```bash
git add docs CHANGELOG.md crates/aivyx-web/dist
git add -u
git commit -s -m "docs: Command Center logbook — IPC, guide, changelog; rebuild bundle

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

Then use superpowers:finishing-a-development-branch.
