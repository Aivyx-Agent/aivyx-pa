# Matrix Channel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the operator talk to Aivyx PA from any Matrix client, in an end-to-end-encrypted room, with only their own verified devices able to drive turns.

**Architecture:** A new `crates/aivyx-matrix` crate shaped like `aivyx-discord` (transport trait + scripted double, a pure trust policy, `MatrixChannel`, an in-process session loop), plus the real `matrix-sdk` transport, sealed credentials and SAS verification. `aivyx-channel` gains a daemon frontend; `aivyx-config` a `[matrix]` section; the CLI `--channel matrix`, `connect matrix` and `matrix verify`.

**Tech Stack:** Rust 2024, tokio, `matrix-sdk` 0.19.1 (`e2e-encryption`, `sqlite`, `rustls-aws-lc-rs`, no default features), `aivyx-crypto` `SubKey::seal/open`.

**Spec:** `docs/superpowers/specs/2026-10-11-matrix-channel-design.md`.

## Global Constraints

- `matrix-sdk = { version = "0.19", default-features = false, features = ["e2e-encryption", "sqlite", "rustls-aws-lc-rs"] }` — the API used here is compile-checked against 0.19.1.
- A message becomes a turn only when: room == `room_filter` AND sender ∈ `allowed_senders` AND it came from a verified device. Turns run at `TrustTier::SemiTrusted`. Nothing else is ever a turn.
- An `allowed_senders` ID in the `room_filter` room writing from an unverified device, or sending an undecryptable message, gets **one** reply per room per run; anyone else is ignored silently.
- The assistant refuses to start when `room_filter` names an unencrypted room.
- Credentials (access token, device id) live only in `<data_dir>/matrix/session.enc`, sealed with `MasterKey::derive_subkey(b"aivyx-pa matrix session")`. The `matrix-sdk` SQLite store passphrase is the hex of `MasterKey::derive_subkey(b"aivyx-pa matrix store")`. The password is never written anywhere.
- Every aivyx-pa path is built in `aivyx-instance` (the `no_stray_paths` guard test enforces it).
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` must pass after every task. Commits are signed off (`git commit -s`).
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## File Structure

| File | Responsibility |
|---|---|
| `crates/aivyx-instance/src/lib.rs` (modify) | `InstancePaths::matrix_dir()` |
| `crates/aivyx-matrix/Cargo.toml` (create) | crate manifest |
| `crates/aivyx-matrix/src/lib.rs` (create) | module list + re-exports |
| `crates/aivyx-matrix/src/transport.rs` (create) | `Inbound`/`IncomingMessage`/`OutgoingMessage`, `MatrixTransport` trait, `ScriptedTransport` |
| `crates/aivyx-matrix/src/policy.rs` (create) | `MatrixPolicy`, `Action`, `decide()`, `OncePerRoom` |
| `crates/aivyx-matrix/src/matrix_channel.rs` (create) | `MatrixChannel` (`ChannelContext`) |
| `crates/aivyx-matrix/src/session.rs` (create) | in-process multi-room session loop |
| `crates/aivyx-matrix/src/credentials.rs` (create) | `MatrixKeys`, sealed `session.enc` |
| `crates/aivyx-matrix/src/sdk.rs` (create) | `SdkTransport` over `matrix-sdk`, `login`, `restore` |
| `crates/aivyx-matrix/src/verify.rs` (create) | SAS emoji verification |
| `crates/aivyx-matrix/src/tests.rs` (create) | session-level tests |
| `crates/aivyx-matrix/tests/live_conduit.rs` (create) | `#[ignore]`d live e2e test |
| `crates/aivyx-config/src/lib.rs` (modify) | `[matrix]` → `MatrixConfig` |
| `crates/aivyx-ipc/src/protocol.rs` (modify) | `FrontendType::Matrix` |
| `crates/aivyx-channel/src/matrix_daemon_frontend.rs` (create) | daemon-mode pump |
| `crates/aivyx-channel/src/role_render.rs` (modify) | `ChannelKind::Matrix` |
| `crates/aivyx-cli/src/bin/aivyx.rs` (modify) | `--channel matrix`, modes, factory |
| `crates/aivyx-cli/src/bin/aivyx_modules/matrix_cli.rs` (create) | `connect matrix`, `matrix verify` |
| `docs/…` (modify) | guide, references, THREAT_MODEL |

---

### Task 1: The instance's Matrix folder

**Files:**
- Modify: `crates/aivyx-instance/src/lib.rs` (add a method next to `packs_dir`, add a test in the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `InstancePaths::matrix_dir(&self) -> Option<PathBuf>` = `data_dir()/matrix`.

- [ ] **Step 1: Write the failing test** — append inside the existing `mod tests` in `crates/aivyx-instance/src/lib.rs`:

```rust
    #[test]
    fn matrix_dir_is_under_the_instance_data_dir() {
        let dirs = BaseDirs {
            home: Some(PathBuf::from("/h")),
            xdg_config_home: None,
            xdg_data_home: None,
            xdg_runtime_dir: None,
        };
        let default = InstancePaths::new(InstanceName::default_instance(), dirs.clone());
        assert_eq!(
            default.matrix_dir(),
            Some(PathBuf::from("/h/.local/share/aivyx-pa/matrix"))
        );
        let work = InstancePaths::new(InstanceName::parse("work").unwrap(), dirs);
        assert_eq!(
            work.matrix_dir(),
            Some(PathBuf::from("/h/.local/share/aivyx-pa/instances/work/matrix"))
        );
    }
```

- [ ] **Step 2: Run it — expect a compile error** (`no method named matrix_dir`)

Run: `cargo test -p aivyx-instance matrix_dir`

- [ ] **Step 3: Implement** — next to `packs_dir`:

```rust
    /// Matrix channel state: `data_dir/matrix` — the `matrix-sdk` SQLite
    /// store and the sealed `session.enc`. Per instance, so each named
    /// instance is its own Matrix device.
    pub fn matrix_dir(&self) -> Option<PathBuf> {
        self.data_dir().map(|d| d.join("matrix"))
    }
```

- [ ] **Step 4: Run** `cargo test -p aivyx-instance` — PASS (including `no_stray_paths`).

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-instance/src/lib.rs
git commit -s -m "feat(instance): matrix_dir for the Matrix channel's state" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: `aivyx-matrix` crate — transport types and scripted double

**Files:**
- Create: `crates/aivyx-matrix/Cargo.toml`, `crates/aivyx-matrix/src/lib.rs`, `crates/aivyx-matrix/src/transport.rs`
- Modify: root `Cargo.toml` — add `"crates/aivyx-matrix"` to both `members` (after `"crates/aivyx-discord"`) and `default-members`

**Interfaces:**
- Produces (all `pub` in `aivyx_matrix::transport`):
  - `struct IncomingMessage { room_id: String, sender: String, text: String, verified: bool }`
  - `enum Inbound { Message(IncomingMessage), Undecryptable { room_id: String, sender: String }, Invite { room_id: String, inviter: String } }`
  - `struct OutgoingMessage { room_id: String, text: String }`
  - `enum TransportError { Platform(String), Unauthorized(String) }`
  - `#[async_trait] trait MatrixTransport: Send + Sync { async fn next_inbound(&self) -> Result<Inbound, TransportError>; async fn send_message(&self, msg: OutgoingMessage) -> Result<(), TransportError>; async fn join_room(&self, room_id: &str) -> Result<(), TransportError>; async fn decline_invite(&self, room_id: &str) -> Result<(), TransportError>; async fn room_is_encrypted(&self, room_id: &str) -> Result<bool, TransportError>; }`
  - `ScriptedTransport::with_queue(Vec<Inbound>)`, `.encrypted_rooms(Vec<&str>)`, `.sent()`, `.joined()`, `.declined()`, `.push_inbound(Inbound)`

- [ ] **Step 1: Create the manifest** `crates/aivyx-matrix/Cargo.toml`:

```toml
[package]
name = "aivyx-matrix"
description = "Matrix ChannelContext adapter — end-to-end-encrypted rooms, verified devices only"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
publish.workspace = true
authors.workspace = true
repository.workspace = true
readme.workspace = true

[dependencies]
aivyx-core = { path = "../aivyx-core" }
aivyx-capability = { path = "../aivyx-capability" }
aivyx-crypto = { path = "../aivyx-crypto" }
aivyx-llm = { path = "../aivyx-llm" }
aivyx-storage = { path = "../aivyx-storage" }
async-trait = { workspace = true }
futures-util = { workspace = true }
hex = { workspace = true }
rand = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
tokio = { workspace = true }
# Apache-2.0. No default features: we want E2EE + the encrypted SQLite
# store + rustls on aws-lc-rs (the TLS stack the rest of pa uses).
matrix-sdk = { version = "0.19", default-features = false, features = ["e2e-encryption", "sqlite", "rustls-aws-lc-rs"] }

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt", "rt-multi-thread", "time", "sync"] }
aivyx-audit = { path = "../aivyx-audit" }
```

- [ ] **Step 2: Create `src/lib.rs`**:

```rust
//! `aivyx-matrix` — the Matrix channel. Same shape as `aivyx-discord`
//! (transport trait + scripted double, channel, session loop), plus the
//! trust policy (`policy`), the real `matrix-sdk` transport (`sdk`),
//! sealed credentials (`credentials`) and SAS verification (`verify`).
//! See `docs/superpowers/specs/2026-10-11-matrix-channel-design.md`.

pub mod transport;

#[cfg(test)]
mod tests;
```

and an empty `src/tests.rs` (`//! Session-level tests (Task 5).`).

- [ ] **Step 3: Write `src/transport.rs` with failing tests first** — the tests at the bottom of the file below; run `cargo test -p aivyx-matrix` and see them fail to compile until the types exist. Then the full file:

```rust
//! The transport seam between the channel and Matrix. Production is
//! `sdk::SdkTransport`; tests use [`ScriptedTransport`]. No `matrix-sdk`
//! type crosses this boundary.

use std::collections::VecDeque;
use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;
use tokio::sync::Mutex;

/// A decrypted text message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingMessage {
    pub room_id: String,
    pub sender: String,
    pub text: String,
    /// Sent from a device the assistant trusts (`VerificationState::Verified`).
    pub verified: bool,
}

/// Everything the session loop reacts to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inbound {
    Message(IncomingMessage),
    /// An encrypted event the assistant couldn't decrypt.
    Undecryptable { room_id: String, sender: String },
    /// An invite to a room.
    Invite { room_id: String, inviter: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingMessage {
    pub room_id: String,
    pub text: String,
}

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("matrix transport error: {0}")]
    Platform(String),
    /// The homeserver rejected the access token — the device was logged
    /// out. The channel stops; the operator runs `connect matrix` again.
    #[error("matrix rejected the assistant's login ({0}) — run `aivyx-pa connect matrix` again")]
    Unauthorized(String),
}

#[async_trait]
pub trait MatrixTransport: Send + Sync {
    async fn next_inbound(&self) -> Result<Inbound, TransportError>;
    async fn send_message(&self, msg: OutgoingMessage) -> Result<(), TransportError>;
    async fn join_room(&self, room_id: &str) -> Result<(), TransportError>;
    async fn decline_invite(&self, room_id: &str) -> Result<(), TransportError>;
    async fn room_is_encrypted(&self, room_id: &str) -> Result<bool, TransportError>;
}

/// Test double: pops queued inbound items, records everything sent.
pub struct ScriptedTransport {
    queue: Mutex<VecDeque<Inbound>>,
    sent: Mutex<Vec<OutgoingMessage>>,
    joined: Mutex<Vec<String>>,
    declined: Mutex<Vec<String>>,
    encrypted: Vec<String>,
}

impl ScriptedTransport {
    pub fn with_queue(queue: Vec<Inbound>) -> Self {
        ScriptedTransport {
            queue: Mutex::new(queue.into()),
            sent: Mutex::new(Vec::new()),
            joined: Mutex::new(Vec::new()),
            declined: Mutex::new(Vec::new()),
            encrypted: Vec::new(),
        }
    }

    /// Rooms `room_is_encrypted` reports as encrypted (all others: not).
    pub fn encrypted_rooms(mut self, rooms: Vec<&str>) -> Self {
        self.encrypted = rooms.into_iter().map(str::to_string).collect();
        self
    }

    pub async fn sent(&self) -> Vec<OutgoingMessage> {
        self.sent.lock().await.clone()
    }

    pub async fn joined(&self) -> Vec<String> {
        self.joined.lock().await.clone()
    }

    pub async fn declined(&self) -> Vec<String> {
        self.declined.lock().await.clone()
    }

    pub async fn push_inbound(&self, item: Inbound) {
        self.queue.lock().await.push_back(item);
    }
}

#[async_trait]
impl MatrixTransport for ScriptedTransport {
    async fn next_inbound(&self) -> Result<Inbound, TransportError> {
        loop {
            if let Some(item) = self.queue.lock().await.pop_front() {
                return Ok(item);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn send_message(&self, msg: OutgoingMessage) -> Result<(), TransportError> {
        self.sent.lock().await.push(msg);
        Ok(())
    }

    async fn join_room(&self, room_id: &str) -> Result<(), TransportError> {
        self.joined.lock().await.push(room_id.to_string());
        Ok(())
    }

    async fn decline_invite(&self, room_id: &str) -> Result<(), TransportError> {
        self.declined.lock().await.push(room_id.to_string());
        Ok(())
    }

    async fn room_is_encrypted(&self, room_id: &str) -> Result<bool, TransportError> {
        Ok(self.encrypted.iter().any(|r| r == room_id))
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;

    fn msg(text: &str) -> Inbound {
        Inbound::Message(IncomingMessage {
            room_id: "!r:x".into(),
            sender: "@me:x".into(),
            text: text.into(),
            verified: true,
        })
    }

    #[tokio::test]
    async fn queue_pops_in_order_and_sends_are_recorded() {
        let t = ScriptedTransport::with_queue(vec![msg("a"), msg("b")]);
        assert_eq!(t.next_inbound().await.unwrap(), msg("a"));
        assert_eq!(t.next_inbound().await.unwrap(), msg("b"));
        t.send_message(OutgoingMessage { room_id: "!r:x".into(), text: "hi".into() })
            .await
            .unwrap();
        t.join_room("!r:x").await.unwrap();
        t.decline_invite("!s:x").await.unwrap();
        assert_eq!(t.sent().await.len(), 1);
        assert_eq!(t.joined().await, vec!["!r:x".to_string()]);
        assert_eq!(t.declined().await, vec!["!s:x".to_string()]);
    }

    #[tokio::test]
    async fn encrypted_rooms_are_reported() {
        let t = ScriptedTransport::with_queue(vec![]).encrypted_rooms(vec!["!e:x"]);
        assert!(t.room_is_encrypted("!e:x").await.unwrap());
        assert!(!t.room_is_encrypted("!plain:x").await.unwrap());
    }

    #[test]
    fn unauthorized_tells_the_operator_what_to_do() {
        let e = TransportError::Unauthorized("M_UNKNOWN_TOKEN".into());
        assert!(e.to_string().contains("aivyx-pa connect matrix"));
    }
}
```

- [ ] **Step 4: Run** `cargo test -p aivyx-matrix` — PASS (3 tests). `cargo clippy -p aivyx-matrix --all-targets -- -D warnings` — clean. (The first build compiles `matrix-sdk`; expect several minutes.)

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/aivyx-matrix
git commit -s -m "feat(matrix): aivyx-matrix crate — transport seam and scripted double" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: The trust policy

**Files:**
- Create: `crates/aivyx-matrix/src/policy.rs`
- Modify: `crates/aivyx-matrix/src/lib.rs` (add `pub mod policy;`)

**Interfaces:**
- Consumes: `transport::{Inbound, IncomingMessage}`
- Produces:
  - `struct MatrixPolicy { room_filter: String, allowed_senders: Vec<String> }`
  - `enum Action { Turn(IncomingMessage), AskToVerify { room_id: String }, CantDecrypt { room_id: String }, Join { room_id: String }, Decline { room_id: String }, Ignore }`
  - `fn decide(policy: &MatrixPolicy, inbound: Inbound) -> Action`
  - `struct OncePerRoom` with `fn new() -> Self`, `fn first(&mut self, room_id: &str) -> bool`
  - `const ASK_TO_VERIFY: &str`, `const CANT_DECRYPT: &str`

- [ ] **Step 1: Write the failing tests** (bottom of `policy.rs`; the module body is added in Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> MatrixPolicy {
        MatrixPolicy { room_filter: "!home:x".into(), allowed_senders: vec!["@me:x".into()] }
    }

    fn message(room: &str, sender: &str, verified: bool) -> Inbound {
        Inbound::Message(IncomingMessage {
            room_id: room.into(),
            sender: sender.into(),
            text: "hi".into(),
            verified,
        })
    }

    #[test]
    fn only_the_operator_from_a_verified_device_in_the_home_room_is_a_turn() {
        assert!(matches!(decide(&policy(), message("!home:x", "@me:x", true)), Action::Turn(_)));
        // Every other combination is not.
        assert_eq!(
            decide(&policy(), message("!home:x", "@me:x", false)),
            Action::AskToVerify { room_id: "!home:x".into() }
        );
        assert_eq!(decide(&policy(), message("!home:x", "@stranger:x", true)), Action::Ignore);
        assert_eq!(decide(&policy(), message("!other:x", "@me:x", true)), Action::Ignore);
        assert_eq!(decide(&policy(), message("!other:x", "@stranger:x", false)), Action::Ignore);
    }

    #[test]
    fn undecryptable_is_answered_only_for_the_operator_in_the_home_room() {
        let ud = |room: &str, sender: &str| Inbound::Undecryptable {
            room_id: room.into(),
            sender: sender.into(),
        };
        assert_eq!(
            decide(&policy(), ud("!home:x", "@me:x")),
            Action::CantDecrypt { room_id: "!home:x".into() }
        );
        assert_eq!(decide(&policy(), ud("!home:x", "@stranger:x")), Action::Ignore);
        assert_eq!(decide(&policy(), ud("!other:x", "@me:x")), Action::Ignore);
    }

    #[test]
    fn invites_are_accepted_only_from_allowed_senders() {
        let inv = |inviter: &str| Inbound::Invite { room_id: "!r:x".into(), inviter: inviter.into() };
        assert_eq!(decide(&policy(), inv("@me:x")), Action::Join { room_id: "!r:x".into() });
        assert_eq!(decide(&policy(), inv("@stranger:x")), Action::Decline { room_id: "!r:x".into() });
    }

    #[test]
    fn once_per_room_answers_each_room_once() {
        let mut once = OncePerRoom::new();
        assert!(once.first("!a:x"));
        assert!(!once.first("!a:x"));
        assert!(once.first("!b:x"));
    }
}
```

- [ ] **Step 2: Run** `cargo test -p aivyx-matrix policy` — FAIL (types undefined).

- [ ] **Step 3: Implement** — top of `policy.rs`:

```rust
//! Who may drive a turn. Pure, so every case is a unit test.
//!
//! A message is a turn only from an `allowed_senders` ID, in the
//! `room_filter` room, sent from a verified device. The operator writing
//! from an unverified device, or sending something the assistant can't
//! decrypt, gets one explanation per room; anyone else is ignored.

use std::collections::HashSet;

use crate::transport::{Inbound, IncomingMessage};

/// Told once per room when the operator writes from an unverified device.
pub const ASK_TO_VERIFY: &str = "I only answer your verified devices. Run \
    `aivyx-pa matrix verify` and verify this device from your Matrix app.";

/// Told once per room when a message from the operator can't be decrypted.
pub const CANT_DECRYPT: &str = "I couldn't read that message. Verify my device \
    with `aivyx-pa matrix verify`, then send it again.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatrixPolicy {
    pub room_filter: String,
    pub allowed_senders: Vec<String>,
}

impl MatrixPolicy {
    fn is_operator(&self, sender: &str) -> bool {
        self.allowed_senders.iter().any(|s| s == sender)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Turn(IncomingMessage),
    AskToVerify { room_id: String },
    CantDecrypt { room_id: String },
    Join { room_id: String },
    Decline { room_id: String },
    Ignore,
}

pub fn decide(policy: &MatrixPolicy, inbound: Inbound) -> Action {
    match inbound {
        Inbound::Message(m) => {
            if m.room_id != policy.room_filter || !policy.is_operator(&m.sender) {
                Action::Ignore
            } else if m.verified {
                Action::Turn(m)
            } else {
                Action::AskToVerify { room_id: m.room_id }
            }
        }
        Inbound::Undecryptable { room_id, sender } => {
            if room_id == policy.room_filter && policy.is_operator(&sender) {
                Action::CantDecrypt { room_id }
            } else {
                Action::Ignore
            }
        }
        Inbound::Invite { room_id, inviter } => {
            if policy.is_operator(&inviter) {
                Action::Join { room_id }
            } else {
                Action::Decline { room_id }
            }
        }
    }
}

/// Remembers which rooms already got a given explanation this run.
#[derive(Debug, Default)]
pub struct OncePerRoom(HashSet<String>);

impl OncePerRoom {
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` the first time `room_id` is seen.
    pub fn first(&mut self, room_id: &str) -> bool {
        self.0.insert(room_id.to_string())
    }
}
```

- [ ] **Step 4: Run** `cargo test -p aivyx-matrix` — PASS. Clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-matrix/src/policy.rs crates/aivyx-matrix/src/lib.rs
git commit -s -m "feat(matrix): trust policy — verified operator devices in the home room only" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: `MatrixChannel`

**Files:**
- Create: `crates/aivyx-matrix/src/matrix_channel.rs`
- Modify: `crates/aivyx-matrix/src/lib.rs` (`pub mod matrix_channel;`)

**Interfaces:**
- Consumes: `transport::{MatrixTransport, OutgoingMessage, TransportError}`
- Produces: `MatrixChannel<T>` with `pub(crate) fn new(name: impl Into<String>, room_id: String, transport: Arc<T>) -> Self`, `pub(crate) fn room_id(&self) -> &str`, `pub(crate) fn reset_cancellation(&self)`; implements `ChannelContext` with `platform() == ChannelPlatform::Matrix`, `trust_tier() == TrustTier::SemiTrusted` (only policy-approved messages ever reach a channel), `session_partition() == Some(room_id)`.

- [ ] **Step 1: Write the failing tests** (bottom of the file):

```rust
#[cfg(test)]
mod channel_tests {
    use super::*;
    use crate::transport::ScriptedTransport;

    fn channel() -> (MatrixChannel<ScriptedTransport>, Arc<ScriptedTransport>) {
        let t = Arc::new(ScriptedTransport::with_queue(vec![]));
        (MatrixChannel::new("aivyx-matrix-test", "!home:x".into(), Arc::clone(&t)), t)
    }

    #[test]
    fn identity_is_matrix_semitrusted_partitioned_by_room() {
        let (c, _) = channel();
        assert_eq!(c.platform(), ChannelPlatform::Matrix);
        assert_eq!(c.trust_tier(), TrustTier::SemiTrusted);
        assert_eq!(c.session_partition().as_deref(), Some("!home:x"));
        assert_eq!(c.channel_name(), "aivyx-matrix-test");
    }

    #[tokio::test]
    async fn finalize_sends_one_message_with_the_streamed_text() {
        let (c, t) = channel();
        c.stream_event(StreamEvent::Text("the answer is ")).await.unwrap();
        c.stream_event(StreamEvent::Text("42")).await.unwrap();
        c.finalize(&TurnOutcome::Completed {
            final_message: "the answer is 42".into(),
            tool_calls_made: 0,
            duration: std::time::Duration::ZERO,
        })
        .await
        .unwrap();
        assert_eq!(
            t.sent().await,
            vec![OutgoingMessage { room_id: "!home:x".into(), text: "the answer is 42".into() }]
        );
    }

    #[tokio::test]
    async fn an_empty_turn_still_sends_something() {
        let (c, t) = channel();
        c.finalize(&TurnOutcome::Cancelled { tool_calls_made: 0 }).await.unwrap();
        assert_eq!(t.sent().await[0].text, "\n✕ cancelled");
    }

    #[test]
    fn reset_cancellation_yields_a_fresh_token() {
        let (c, _) = channel();
        c.cancellation_token().cancel();
        c.reset_cancellation();
        assert!(!c.cancellation_token().is_cancelled());
    }
}
```

- [ ] **Step 2: Run** `cargo test -p aivyx-matrix channel_tests` — FAIL (type undefined).

- [ ] **Step 3: Implement** — top of `matrix_channel.rs` (the rendering matches `aivyx-discord` so replies look the same on every chat app):

```rust
//! [`MatrixChannel`] — the `ChannelContext` for one Matrix room. Whole-
//! message delivery: streamed text is buffered and sent once at
//! `finalize`, as on Telegram and Discord.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use aivyx_capability::TrustTier;
use aivyx_core::{
    CancellationToken, ChannelContext, ChannelError, ChannelPlatform, SessionId, StreamEvent,
    TurnOutcome,
};

use crate::transport::{MatrixTransport, OutgoingMessage, TransportError};

pub struct MatrixChannel<T: MatrixTransport + 'static> {
    name: String,
    session: SessionId,
    room_id: String,
    token: Arc<Mutex<CancellationToken>>,
    buffer: Mutex<String>,
    transport: Arc<T>,
}

impl<T: MatrixTransport + 'static> MatrixChannel<T> {
    pub(crate) fn new(name: impl Into<String>, room_id: String, transport: Arc<T>) -> Self {
        MatrixChannel {
            name: name.into(),
            session: SessionId::new(),
            room_id,
            token: Arc::new(Mutex::new(CancellationToken::new())),
            buffer: Mutex::new(String::new()),
            transport,
        }
    }

    pub(crate) fn room_id(&self) -> &str {
        &self.room_id
    }

    pub(crate) fn reset_cancellation(&self) {
        *self.token.lock().expect("token mutex poisoned") = CancellationToken::new();
    }
}

fn append_event(buffer: &mut String, event: &StreamEvent<'_>) {
    let newline = |b: &mut String| {
        if !b.is_empty() && !b.ends_with('\n') {
            b.push('\n');
        }
    };
    match event {
        StreamEvent::Text(chunk) => buffer.push_str(chunk),
        StreamEvent::Status(s) => {
            newline(buffer);
            let _ = writeln!(buffer, "… {s}");
        }
        StreamEvent::ToolCallStarted { tool_name, .. } => {
            newline(buffer);
            let _ = writeln!(buffer, "→ {tool_name}");
        }
        StreamEvent::ToolCallFinished { tool_name, outcome_summary, .. } => {
            newline(buffer);
            let _ = writeln!(buffer, "← {tool_name} {outcome_summary}");
        }
        StreamEvent::Attachment { .. } | StreamEvent::ToolOutput { .. } => {}
    }
}

fn finalize_footer(outcome: &TurnOutcome) -> String {
    match outcome {
        TurnOutcome::Completed { .. } => String::new(),
        TurnOutcome::Escalated { reason, .. } => format!("\n⏸ escalation: {reason}"),
        TurnOutcome::TimedOut { elapsed, .. } => format!("\n⏱ timed out after {elapsed:?}"),
        TurnOutcome::Cancelled { .. } => "\n✕ cancelled".to_string(),
        TurnOutcome::MaxStepsExceeded { max_steps, .. } => {
            format!("\n✕ planner exceeded {max_steps} steps per turn")
        }
        TurnOutcome::Looping { repeat_limit, .. } => {
            format!("\n✕ stopped after {repeat_limit} repeated identical tool calls")
        }
        TurnOutcome::Failed(e) => format!("\n✕ failed: {e}"),
    }
}

impl From<TransportError> for ChannelError {
    fn from(e: TransportError) -> Self {
        ChannelError::Platform(e.to_string())
    }
}

#[async_trait]
impl<T: MatrixTransport + 'static> ChannelContext for MatrixChannel<T> {
    fn channel_name(&self) -> &str {
        &self.name
    }

    fn platform(&self) -> ChannelPlatform {
        ChannelPlatform::Matrix
    }

    fn trust_tier(&self) -> TrustTier {
        // Only messages `policy::decide` turned into turns ever reach a
        // channel: the operator, in the home room, from a verified device.
        TrustTier::SemiTrusted
    }

    fn session_id(&self) -> SessionId {
        self.session
    }

    fn session_partition(&self) -> Option<String> {
        Some(self.room_id.clone())
    }

    async fn stream_event(&self, event: StreamEvent<'_>) -> Result<(), ChannelError> {
        let mut buf = self
            .buffer
            .lock()
            .map_err(|e| ChannelError::Send(format!("MatrixChannel buffer poisoned: {e}")))?;
        append_event(&mut buf, &event);
        Ok(())
    }

    async fn finalize(&self, outcome: &TurnOutcome) -> Result<(), ChannelError> {
        let payload = {
            let mut buf = self
                .buffer
                .lock()
                .map_err(|e| ChannelError::Send(format!("MatrixChannel buffer poisoned: {e}")))?;
            let mut out = std::mem::take(&mut *buf);
            out.push_str(&finalize_footer(outcome));
            out
        };
        let text = if payload.trim().is_empty() { "(no reply)".to_string() } else { payload };
        self.transport
            .send_message(OutgoingMessage { room_id: self.room_id.clone(), text })
            .await?;
        Ok(())
    }

    fn cancellation_token(&self) -> CancellationToken {
        self.token.lock().expect("token mutex poisoned").clone()
    }
}
```

- [ ] **Step 4: Run** `cargo test -p aivyx-matrix` — PASS. Clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-matrix/src/matrix_channel.rs crates/aivyx-matrix/src/lib.rs
git commit -s -m "feat(matrix): MatrixChannel — one reply per turn, SemiTrusted, partitioned by room" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: The in-process session loop

**Files:**
- Create: `crates/aivyx-matrix/src/session.rs`; fill `crates/aivyx-matrix/src/tests.rs`
- Modify: `crates/aivyx-matrix/src/lib.rs` (`pub mod session;` and `pub use session::{run_matrix_session_with_transport, MatrixSessionConfig, MatrixSessionReport};`)

**Interfaces:**
- Consumes: `policy::{decide, Action, MatrixPolicy, OncePerRoom, ASK_TO_VERIFY, CANT_DECRYPT}`, `MatrixChannel`, `MatrixTransport`
- Produces:
  - `MatrixSessionConfig` — identical fields to `aivyx_discord::DiscordSessionConfig` (copy them verbatim: `model, system_prompt, max_tokens, capabilities, tools, storage, tool_allowlist, memory_topic_prefix, turn_timeout_secs, cycle_detection, injection_scan_enabled, confirm_integration_writes, budget_gate, rate_gate, injection_scan_exempt`).
  - `struct MatrixSessionReport { pub turns_run: usize }`
  - `pub async fn run_matrix_session_with_transport<T: MatrixTransport + 'static>(transport: Arc<T>, policy: MatrixPolicy, config: MatrixSessionConfig, provider: Arc<dyn LlmProvider>, audit: Arc<dyn AuditHook>, checkpointer: Option<Arc<aivyx_core::GitCheckpointer>>, shutdown: CancellationToken) -> Result<MatrixSessionReport, String>`

Unlike Discord (one inner task per channel), only one room can ever drive turns, so the loop is single-room: one `MatrixChannel` for `room_filter`, a pending queue, and `/cancel` during a turn.

- [ ] **Step 1: Write the failing tests** in `src/tests.rs`. Copy `ScriptedStep`, `ScriptedProvider`, `ScriptedStream`, `final_step` and `scratch_storage` verbatim from `crates/aivyx-discord/src/tests.rs` (rename the scratch prefix to `aivyx-matrix-`), then:

```rust
use crate::policy::{MatrixPolicy, ASK_TO_VERIFY, CANT_DECRYPT};
use crate::session::{run_matrix_session_with_transport, MatrixSessionConfig};
use crate::transport::{Inbound, IncomingMessage, ScriptedTransport};

fn config(storage: Arc<dyn Storage>) -> MatrixSessionConfig {
    MatrixSessionConfig {
        model: "test-model".into(),
        system_prompt: "matrix test".into(),
        max_tokens: 128,
        capabilities: CapabilitySet::from_scopes([Scope::parse("memory.read").unwrap()]),
        tools: Arc::new(ToolRegistry::new(Vec::new())),
        storage,
        tool_allowlist: None,
        memory_topic_prefix: None,
        turn_timeout_secs: None,
        cycle_detection: None,
        injection_scan_enabled: true,
        injection_scan_exempt: std::collections::BTreeSet::new(),
        confirm_integration_writes: false,
        budget_gate: None,
        rate_gate: None,
    }
}

fn policy() -> MatrixPolicy {
    MatrixPolicy { room_filter: "!home:x".into(), allowed_senders: vec!["@me:x".into()] }
}

fn msg(room: &str, sender: &str, text: &str, verified: bool) -> Inbound {
    Inbound::Message(IncomingMessage { room_id: room.into(), sender: sender.into(), text: text.into(), verified })
}

/// Runs the session until `transport` has sent `n` messages (or 5 s).
async fn run_until_sent(
    transport: Arc<ScriptedTransport>,
    provider: Vec<ScriptedStep>,
    n: usize,
) -> Result<usize, String> {
    let (storage, _dir) = scratch_storage("session").await;
    let provider: Arc<dyn LlmProvider> = Arc::new(ScriptedProvider { queue: StdMutex::new(provider.into()) });
    let audit: Arc<dyn AuditHook> = Arc::new(AuditBridge::new(HmacChainLog::new([42u8; 32].to_vec())));
    let shutdown = CancellationToken::new();
    let (watch_t, watch_s) = (Arc::clone(&transport), shutdown.clone());
    tokio::spawn(async move {
        while watch_t.sent().await.len() < n {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        watch_s.cancel();
    });
    tokio::time::timeout(
        Duration::from_secs(5),
        run_matrix_session_with_transport(transport, policy(), config(storage), provider, audit, None, shutdown),
    )
    .await
    .map_err(|_| "session did not finish".to_string())?
    .map(|r| r.turns_run)
}

#[tokio::test]
async fn the_verified_operator_gets_a_turn_and_strangers_get_nothing() {
    let transport = Arc::new(
        ScriptedTransport::with_queue(vec![
            msg("!home:x", "@stranger:x", "hello?", true),
            msg("!other:x", "@me:x", "wrong room", true),
            msg("!home:x", "@me:x", "hi", true),
        ])
        .encrypted_rooms(vec!["!home:x"]),
    );
    let turns = run_until_sent(Arc::clone(&transport), vec![final_step(&["hello!"], "hello!")], 1)
        .await
        .unwrap();
    assert_eq!(turns, 1);
    let sent = transport.sent().await;
    assert_eq!(sent.len(), 1);
    assert_eq!((sent[0].room_id.as_str(), sent[0].text.as_str()), ("!home:x", "hello!"));
}

#[tokio::test]
async fn an_unverified_device_is_asked_to_verify_once() {
    let transport = Arc::new(
        ScriptedTransport::with_queue(vec![
            msg("!home:x", "@me:x", "one", false),
            msg("!home:x", "@me:x", "two", false),
            Inbound::Undecryptable { room_id: "!home:x".into(), sender: "@me:x".into() },
            Inbound::Undecryptable { room_id: "!home:x".into(), sender: "@me:x".into() },
            msg("!home:x", "@me:x", "verified now", true),
        ])
        .encrypted_rooms(vec!["!home:x"]),
    );
    run_until_sent(Arc::clone(&transport), vec![final_step(&["ok"], "ok")], 3).await.unwrap();
    let texts: Vec<String> = transport.sent().await.into_iter().map(|m| m.text).collect();
    assert_eq!(texts, vec![ASK_TO_VERIFY.to_string(), CANT_DECRYPT.to_string(), "ok".to_string()]);
}

#[tokio::test]
async fn invites_are_joined_or_declined_by_inviter() {
    let transport = Arc::new(
        ScriptedTransport::with_queue(vec![
            Inbound::Invite { room_id: "!a:x".into(), inviter: "@me:x".into() },
            Inbound::Invite { room_id: "!b:x".into(), inviter: "@stranger:x".into() },
            msg("!home:x", "@me:x", "hi", true),
        ])
        .encrypted_rooms(vec!["!home:x"]),
    );
    run_until_sent(Arc::clone(&transport), vec![final_step(&["hi"], "hi")], 1).await.unwrap();
    assert_eq!(transport.joined().await, vec!["!a:x".to_string()]);
    assert_eq!(transport.declined().await, vec!["!b:x".to_string()]);
}

#[tokio::test]
async fn an_unencrypted_home_room_refuses_to_start() {
    let transport = Arc::new(ScriptedTransport::with_queue(vec![])); // nothing encrypted
    let err = run_until_sent(transport, vec![], 1).await.unwrap_err();
    assert!(err.contains("isn't encrypted"), "{err}");
}
```

(Imports at the top of `tests.rs`: the same ones `aivyx-discord/src/tests.rs` uses, minus `aivyx_checkpoint` and `uuid`.)

- [ ] **Step 2: Run** `cargo test -p aivyx-matrix --lib tests::` — FAIL (`session` module missing).

- [ ] **Step 3: Implement `src/session.rs`**:

```rust
//! The in-process Matrix session: one room may drive turns
//! (`MatrixPolicy::room_filter`), so one `MatrixChannel`, a pending queue
//! and `/cancel` during a turn. Everything else is decided by
//! `policy::decide`.

use std::collections::VecDeque;
use std::sync::Arc;

use aivyx_capability::CapabilitySet;
use aivyx_core::{
    agent::ConcreteAgent, llm_planner::LlmPlanner, planner::ToolRegistry, Agent, AgentId,
    AuditHook, CancellationToken, ChannelContext, LlmPlannerConfig, Message,
};
use aivyx_llm::LlmProvider;
use aivyx_storage::Storage;

use crate::matrix_channel::MatrixChannel;
use crate::policy::{decide, Action, MatrixPolicy, OncePerRoom, ASK_TO_VERIFY, CANT_DECRYPT};
use crate::transport::{IncomingMessage, MatrixTransport, OutgoingMessage, TransportError};

/// Same fields and meaning as `aivyx_discord::DiscordSessionConfig`.
#[derive(Clone)]
pub struct MatrixSessionConfig {
    pub model: String,
    pub system_prompt: String,
    pub max_tokens: u32,
    pub capabilities: CapabilitySet,
    pub tools: Arc<ToolRegistry>,
    pub storage: Arc<dyn Storage>,
    pub tool_allowlist: Option<std::collections::BTreeSet<String>>,
    pub memory_topic_prefix: Option<String>,
    pub turn_timeout_secs: Option<u64>,
    pub cycle_detection: Option<bool>,
    pub injection_scan_enabled: bool,
    pub confirm_integration_writes: bool,
    pub budget_gate: Option<Arc<dyn aivyx_core::BudgetGate>>,
    pub rate_gate: Option<Arc<dyn aivyx_core::RateGate>>,
    pub injection_scan_exempt: std::collections::BTreeSet<String>,
}

#[derive(Debug, Clone, Default)]
pub struct MatrixSessionReport {
    pub turns_run: usize,
}

fn build_agent(
    config: MatrixSessionConfig,
    provider: Arc<dyn LlmProvider>,
    audit: Arc<dyn AuditHook>,
    checkpointer: Option<Arc<aivyx_core::GitCheckpointer>>,
) -> impl Agent {
    let registry = config.tools;
    let registry_for_factory = Arc::clone(&registry);
    let planner_config = LlmPlannerConfig::new(config.model)
        .with_system_prompt(config.system_prompt)
        .with_max_tokens(config.max_tokens)
        .with_tool_allowlist(config.tool_allowlist.clone());
    let agent = ConcreteAgent::new(AgentId::new(), config.capabilities, registry, audit, move || {
        Box::new(LlmPlanner::new(
            Arc::clone(&provider),
            Arc::clone(&registry_for_factory),
            planner_config.clone(),
        ))
    })
    .with_tool_allowlist(config.tool_allowlist)
    .with_memory_topic_prefix(config.memory_topic_prefix)
    .with_checkpointer(checkpointer)
    .with_confirm_integration_writes(config.confirm_integration_writes)
    .with_budget_gate(config.budget_gate)
    .with_rate_gate(config.rate_gate);
    aivyx_core::TurnSafety::interactive(
        config.turn_timeout_secs,
        config.cycle_detection,
        config.injection_scan_enabled,
        config.injection_scan_exempt,
    )
    .apply(agent)
}

/// What the routing step did with one inbound item.
enum Routed {
    Turn(IncomingMessage),
    Handled,
}

/// Applies `decide` to one inbound item: turns are returned; joins,
/// declines and once-per-room explanations are carried out here.
async fn route<T: MatrixTransport>(
    transport: &T,
    policy: &MatrixPolicy,
    asked: &mut OncePerRoom,
    told_undecryptable: &mut OncePerRoom,
    inbound: crate::transport::Inbound,
) -> Result<Routed, TransportError> {
    match decide(policy, inbound) {
        Action::Turn(m) => return Ok(Routed::Turn(m)),
        Action::AskToVerify { room_id } if asked.first(&room_id) => {
            transport.send_message(OutgoingMessage { room_id, text: ASK_TO_VERIFY.into() }).await?;
        }
        Action::CantDecrypt { room_id } if told_undecryptable.first(&room_id) => {
            transport.send_message(OutgoingMessage { room_id, text: CANT_DECRYPT.into() }).await?;
        }
        Action::Join { room_id } => transport.join_room(&room_id).await?,
        Action::Decline { room_id } => transport.decline_invite(&room_id).await?,
        Action::AskToVerify { .. } | Action::CantDecrypt { .. } | Action::Ignore => {}
    }
    Ok(Routed::Handled)
}

pub async fn run_matrix_session_with_transport<T: MatrixTransport + 'static>(
    transport: Arc<T>,
    policy: MatrixPolicy,
    config: MatrixSessionConfig,
    provider: Arc<dyn LlmProvider>,
    audit: Arc<dyn AuditHook>,
    checkpointer: Option<Arc<aivyx_core::GitCheckpointer>>,
    shutdown: CancellationToken,
) -> Result<MatrixSessionReport, String> {
    if !transport.room_is_encrypted(&policy.room_filter).await.map_err(|e| e.to_string())? {
        return Err(format!(
            "the Matrix room {} isn't encrypted — the assistant only talks in \
             end-to-end-encrypted rooms. Turn on encryption for it, or use another room.",
            policy.room_filter
        ));
    }
    let agent = build_agent(config, provider, audit, checkpointer);
    let channel = Arc::new(MatrixChannel::new("aivyx-matrix", policy.room_filter.clone(), Arc::clone(&transport)));
    let (mut asked, mut told) = (OncePerRoom::new(), OncePerRoom::new());
    let mut pending: VecDeque<IncomingMessage> = VecDeque::new();
    let mut turns_run = 0usize;
    let mut backoff = std::time::Duration::from_secs(1);

    loop {
        if pending.is_empty() {
            let inbound = tokio::select! {
                biased;
                _ = shutdown.cancelled() => return Ok(MatrixSessionReport { turns_run }),
                res = transport.next_inbound() => res,
            };
            let inbound = match inbound {
                Ok(i) => {
                    backoff = std::time::Duration::from_secs(1);
                    i
                }
                Err(TransportError::Unauthorized(e)) => {
                    return Err(TransportError::Unauthorized(e).to_string());
                }
                Err(e) => {
                    eprintln!("aivyx-matrix: {e}; retrying in {}s", backoff.as_secs());
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(std::time::Duration::from_secs(60));
                    continue;
                }
            };
            match route(transport.as_ref(), &policy, &mut asked, &mut told, inbound).await {
                Ok(Routed::Turn(m)) if m.text.trim() == "/cancel" => continue,
                Ok(Routed::Turn(m)) => pending.push_back(m),
                Ok(Routed::Handled) => continue,
                Err(e) => {
                    eprintln!("aivyx-matrix: {e}");
                    continue;
                }
            }
        }

        let m = pending.pop_front().expect("pending is non-empty here");
        channel.reset_cancellation();
        let turn = agent.turn(Message::text(channel.session_id(), &m.text), channel.as_ref());
        tokio::pin!(turn);
        loop {
            tokio::select! {
                biased;
                _ = &mut turn => break,
                _ = shutdown.cancelled() => {
                    channel.cancellation_token().cancel();
                }
                res = transport.next_inbound() => {
                    let Ok(inbound) = res else { continue };
                    if let Ok(Routed::Turn(next)) =
                        route(transport.as_ref(), &policy, &mut asked, &mut told, inbound).await
                    {
                        if next.text.trim() == "/cancel" {
                            channel.cancellation_token().cancel();
                        } else {
                            pending.push_back(next);
                        }
                    }
                }
            }
        }
        turns_run += 1;
    }
}
```

- [ ] **Step 4: Run** `cargo test -p aivyx-matrix` — PASS (all session tests). Clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-matrix/src
git commit -s -m "feat(matrix): in-process session — policy-routed turns, /cancel, encrypted rooms only" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: Sealed credentials, the real transport, and verification

**Files:**
- Create: `crates/aivyx-matrix/src/credentials.rs`, `src/sdk.rs`, `src/verify.rs`
- Modify: `src/lib.rs` (`pub mod credentials; pub mod sdk; pub mod verify;` and `pub use session::run_matrix_session;` once added below)

**Interfaces:**
- Produces:
  - `credentials::MatrixKeys::derive(master: &aivyx_crypto::MasterKey) -> Result<MatrixKeys, String>`; `MatrixKeys::store_passphrase(&self) -> String`
  - `credentials::Session { homeserver: String, user_id: String, device_id: String, access_token: String }` (serde)
  - `credentials::save(keys: &MatrixKeys, dir: &Path, session: &Session) -> Result<(), String>`; `credentials::load(keys: &MatrixKeys, dir: &Path) -> Result<Option<Session>, String>`
  - `sdk::login(homeserver, store_dir, passphrase, user, password) -> Result<Session, String>`; `sdk::SdkTransport::connect(session: &Session, store_dir: &Path, passphrase: &str) -> Result<Arc<SdkTransport>, String>` (implements `MatrixTransport`)
  - `verify::Confirm = Arc<dyn Fn(String) -> bool + Send + Sync>`; `verify::run(session: &Session, store_dir: &Path, passphrase: &str, allowed_senders: Vec<String>, confirm: Confirm) -> Result<bool, String>` (`true` = verified)
  - `sdk::SdkTransport::sync_once(&self) -> Result<(), String>`
  - `session::run_matrix_session(session: &Session, store_dir: &Path, passphrase: &str, policy: MatrixPolicy, config: MatrixSessionConfig, provider: Arc<dyn LlmProvider>, audit: Arc<dyn AuditHook>, checkpointer: Option<Arc<aivyx_core::GitCheckpointer>>, shutdown: CancellationToken) -> Result<MatrixSessionReport, String>`

- [ ] **Step 1: Write the failing credentials tests** (bottom of `credentials.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> MatrixKeys {
        MatrixKeys::derive(&aivyx_crypto::MasterKey::from_raw([3u8; 32])).unwrap()
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("aivyx-matrix-cred-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn sample() -> Session {
        Session {
            homeserver: "https://matrix.example".into(),
            user_id: "@bot:example".into(),
            device_id: "DEVICE".into(),
            access_token: "syt_secret".into(),
        }
    }

    #[test]
    fn a_saved_session_loads_back_and_is_not_plaintext() {
        let dir = scratch("roundtrip");
        save(&keys(), &dir, &sample()).unwrap();
        assert_eq!(load(&keys(), &dir).unwrap(), Some(sample()));
        let raw = std::fs::read(dir.join(SESSION_FILE)).unwrap();
        assert!(!raw.windows(10).any(|w| w == b"syt_secret"), "token must not be stored in the clear");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(SESSION_FILE)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn no_session_is_none_and_another_key_cant_open_it() {
        let dir = scratch("wrongkey");
        assert_eq!(load(&keys(), &dir).unwrap(), None);
        save(&keys(), &dir, &sample()).unwrap();
        let other = MatrixKeys::derive(&aivyx_crypto::MasterKey::from_raw([4u8; 32])).unwrap();
        assert!(load(&other, &dir).is_err());
    }

    #[test]
    fn the_store_passphrase_is_stable_hex_and_differs_per_key() {
        assert_eq!(keys().store_passphrase(), keys().store_passphrase());
        assert_eq!(keys().store_passphrase().len(), 64);
        let other = MatrixKeys::derive(&aivyx_crypto::MasterKey::from_raw([4u8; 32])).unwrap();
        assert_ne!(keys().store_passphrase(), other.store_passphrase());
    }
}
```

- [ ] **Step 2: Run** `cargo test -p aivyx-matrix credentials` — FAIL.

- [ ] **Step 3: Implement `credentials.rs`**:

```rust
//! The assistant's Matrix login, sealed under the operator's master key:
//! `<matrix_dir>/session.enc` = 12-byte nonce || ChaCha20-Poly1305(JSON).
//! Not pa's redb store — the running daemon holds that locked, and
//! `connect matrix` must work either way.

use std::path::Path;

use aivyx_crypto::{MasterKey, SubKey};
use serde::{Deserialize, Serialize};

pub const SESSION_FILE: &str = "session.enc";
const AAD: &[u8] = b"aivyx-pa matrix session v1";

pub struct MatrixKeys {
    session: SubKey,
    store: SubKey,
}

impl MatrixKeys {
    pub fn derive(master: &MasterKey) -> Result<Self, String> {
        Ok(MatrixKeys {
            session: master.derive_subkey(b"aivyx-pa matrix session").map_err(|e| e.to_string())?,
            store: master.derive_subkey(b"aivyx-pa matrix store").map_err(|e| e.to_string())?,
        })
    }

    /// The `matrix-sdk` SQLite store passphrase. `SubKey::as_bytes` is the
    /// discouraged accessor; this is a justified caller: `matrix-sdk`
    /// takes its store key as a passphrase string, which it stretches
    /// itself, so the subkey's hex is the only way to hand it over.
    pub fn store_passphrase(&self) -> String {
        hex::encode(self.store.as_bytes())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub homeserver: String,
    pub user_id: String,
    pub device_id: String,
    pub access_token: String,
}

pub fn save(keys: &MatrixKeys, dir: &Path, session: &Session) -> Result<(), String> {
    use rand::RngCore;
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let mut nonce = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let plain = serde_json::to_vec(session).map_err(|e| e.to_string())?;
    let sealed = keys.session.seal(&nonce, AAD, &plain).map_err(|e| e.to_string())?;
    let mut out = nonce.to_vec();
    out.extend_from_slice(&sealed);
    let path = dir.join(SESSION_FILE);
    let tmp = dir.join(".session.enc.tmp");
    {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut f = options.open(&tmp).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        f.write_all(&out).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| format!("write {}: {e}", path.display()))
}

pub fn load(keys: &MatrixKeys, dir: &Path) -> Result<Option<Session>, String> {
    let path = dir.join(SESSION_FILE);
    let raw = match std::fs::read(&path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    if raw.len() < 12 {
        return Err(format!("{} is damaged — run `aivyx-pa connect matrix` again", path.display()));
    }
    let (nonce, sealed) = raw.split_at(12);
    let plain = keys.session.open(nonce, AAD, sealed).map_err(|_| {
        format!("{} can't be opened with this passphrase — run `aivyx-pa connect matrix` again", path.display())
    })?;
    serde_json::from_slice(&plain).map(Some).map_err(|e| e.to_string())
}
```

- [ ] **Step 4: Run** `cargo test -p aivyx-matrix credentials` — PASS.

- [ ] **Step 5: Implement `sdk.rs`** — compile-checked against `matrix-sdk` 0.19.1. The sync loop runs in a spawned task and feeds an mpsc channel that `next_inbound` drains; a sync error carrying `M_UNKNOWN_TOKEN` becomes `Unauthorized`:

```rust
//! The real transport over `matrix-sdk`: restore the sealed session,
//! open the encrypted SQLite store, sync in the background and turn
//! room events into `Inbound` items.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use matrix_sdk::{
    authentication::matrix::MatrixSession,
    config::SyncSettings,
    deserialized_responses::{EncryptionInfo, VerificationState},
    ruma::{
        events::room::{
            encrypted::OriginalSyncRoomEncryptedEvent,
            member::StrippedRoomMemberEvent,
            message::{MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent},
        },
        OwnedDeviceId, OwnedUserId, RoomId,
    },
    store::RoomLoadSettings,
    Client, Room, RoomState, SessionMeta, SessionTokens,
};
use tokio::sync::{mpsc, Mutex};

use crate::credentials::Session;
use crate::transport::{Inbound, IncomingMessage, MatrixTransport, OutgoingMessage, TransportError};

async fn client(homeserver: &str, store_dir: &Path, passphrase: &str) -> Result<Client, String> {
    Client::builder()
        .homeserver_url(homeserver)
        .sqlite_store(store_dir.join("store"), Some(passphrase))
        .build()
        .await
        .map_err(|e| format!("matrix client: {e}"))
}

/// Logs in once with a password and returns the session to seal. The
/// password is not kept.
pub async fn login(
    homeserver: &str,
    store_dir: &Path,
    passphrase: &str,
    user: &str,
    password: &str,
) -> Result<Session, String> {
    let client = client(homeserver, store_dir, passphrase).await?;
    let r = client
        .matrix_auth()
        .login_username(user, password)
        .initial_device_display_name("Aivyx PA")
        .await
        .map_err(|e| format!("matrix login failed: {e}"))?;
    Ok(Session {
        homeserver: homeserver.to_string(),
        user_id: r.user_id.to_string(),
        device_id: r.device_id.to_string(),
        access_token: r.access_token,
    })
}

pub(crate) async fn restore(session: &Session, store_dir: &Path, passphrase: &str) -> Result<Client, String> {
    let client = client(&session.homeserver, store_dir, passphrase).await?;
    let user_id: OwnedUserId = session.user_id.as_str().try_into().map_err(|e| format!("{e}"))?;
    let device_id: OwnedDeviceId = session.device_id.as_str().into();
    client
        .matrix_auth()
        .restore_session(
            MatrixSession {
                meta: SessionMeta { user_id, device_id },
                tokens: SessionTokens { access_token: session.access_token.clone(), refresh_token: None },
            },
            RoomLoadSettings::default(),
        )
        .await
        .map_err(|e| format!("matrix session restore failed: {e}"))?;
    Ok(client)
}

pub struct SdkTransport {
    client: Client,
    inbound: Mutex<mpsc::Receiver<Result<Inbound, TransportError>>>,
}

impl SdkTransport {
    pub async fn connect(session: &Session, store_dir: &Path, passphrase: &str) -> Result<Arc<Self>, String> {
        let client = restore(session, store_dir, passphrase).await?;
        let (tx, rx) = mpsc::channel(64);
        install_handlers(&client, tx.clone());
        let sync_client = client.clone();
        tokio::spawn(async move {
            if let Err(e) = sync_client.sync(SyncSettings::default()).await {
                let text = e.to_string();
                let err = if text.contains("M_UNKNOWN_TOKEN") {
                    TransportError::Unauthorized(text)
                } else {
                    TransportError::Platform(format!("sync stopped: {text}"))
                };
                let _ = tx.send(Err(err)).await;
            }
        });
        Ok(Arc::new(SdkTransport { client, inbound: Mutex::new(rx) }))
    }

    fn room(&self, room_id: &str) -> Result<Room, TransportError> {
        let id = <&RoomId>::try_from(room_id).map_err(|e| TransportError::Platform(e.to_string()))?;
        self.client
            .get_room(id)
            .ok_or_else(|| TransportError::Platform(format!("the assistant isn't in room {room_id}")))
    }
}

fn install_handlers(client: &Client, tx: mpsc::Sender<Result<Inbound, TransportError>>) {
    let own = client.user_id().map(|u| u.to_owned());
    let tx_msg = tx.clone();
    client.add_event_handler(move |ev: OriginalSyncRoomMessageEvent, room: Room, info: Option<EncryptionInfo>| {
        let (tx, own) = (tx_msg.clone(), own.clone());
        async move {
            if own.as_deref() == Some(ev.sender.as_ref()) {
                return;
            }
            let MessageType::Text(text) = ev.content.msgtype else { return };
            let verified = matches!(info.map(|i| i.verification_state), Some(VerificationState::Verified));
            let _ = tx
                .send(Ok(Inbound::Message(IncomingMessage {
                    room_id: room.room_id().to_string(),
                    sender: ev.sender.to_string(),
                    text: text.body,
                    verified,
                })))
                .await;
        }
    });
    let tx_enc = tx.clone();
    client.add_event_handler(move |ev: OriginalSyncRoomEncryptedEvent, room: Room| {
        let tx = tx_enc.clone();
        async move {
            let _ = tx
                .send(Ok(Inbound::Undecryptable { room_id: room.room_id().to_string(), sender: ev.sender.to_string() }))
                .await;
        }
    });
    client.add_event_handler(move |ev: StrippedRoomMemberEvent, room: Room, client: Client| {
        let tx = tx.clone();
        async move {
            if Some(ev.state_key.as_ref()) != client.user_id().map(|u| u.as_str()) || room.state() != RoomState::Invited {
                return;
            }
            let _ = tx
                .send(Ok(Inbound::Invite { room_id: room.room_id().to_string(), inviter: ev.sender.to_string() }))
                .await;
        }
    });
}

#[async_trait]
impl MatrixTransport for SdkTransport {
    async fn next_inbound(&self) -> Result<Inbound, TransportError> {
        match self.inbound.lock().await.recv().await {
            Some(item) => item,
            None => Err(TransportError::Platform("sync task ended".into())),
        }
    }

    async fn send_message(&self, msg: OutgoingMessage) -> Result<(), TransportError> {
        self.room(&msg.room_id)?
            .send(RoomMessageEventContent::text_plain(msg.text))
            .await
            .map(|_| ())
            .map_err(|e| TransportError::Platform(format!("send: {e}")))
    }

    async fn join_room(&self, room_id: &str) -> Result<(), TransportError> {
        self.room(room_id)?.join().await.map_err(|e| TransportError::Platform(format!("join: {e}")))
    }

    async fn decline_invite(&self, room_id: &str) -> Result<(), TransportError> {
        self.room(room_id)?.leave().await.map_err(|e| TransportError::Platform(format!("decline: {e}")))
    }

    async fn room_is_encrypted(&self, room_id: &str) -> Result<bool, TransportError> {
        Ok(self
            .room(room_id)?
            .latest_encryption_state()
            .await
            .map_err(|e| TransportError::Platform(format!("encryption state: {e}")))?
            .is_encrypted())
    }
}
```

Note: `room_is_encrypted` needs the room in the client's state, so `run_matrix_session` (below) does one `sync_once` before calling it. Add to `session.rs`:

```rust
/// Production entry point: restore the sealed session, sync once so room
/// state is known, then run the loop.
#[allow(clippy::too_many_arguments)]
pub async fn run_matrix_session(
    session: &crate::credentials::Session,
    store_dir: &std::path::Path,
    passphrase: &str,
    policy: MatrixPolicy,
    config: MatrixSessionConfig,
    provider: Arc<dyn LlmProvider>,
    audit: Arc<dyn AuditHook>,
    checkpointer: Option<Arc<aivyx_core::GitCheckpointer>>,
    shutdown: CancellationToken,
) -> Result<MatrixSessionReport, String> {
    let transport = crate::sdk::SdkTransport::connect(session, store_dir, passphrase).await?;
    transport.sync_once().await?;
    run_matrix_session_with_transport(transport, policy, config, provider, audit, checkpointer, shutdown).await
}
```

and to `SdkTransport`:

```rust
    /// One sync round so room state (membership, encryption) is known
    /// before the first `room_is_encrypted` call.
    pub async fn sync_once(&self) -> Result<(), String> {
        self.client
            .sync_once(SyncSettings::default())
            .await
            .map(|_| ())
            .map_err(|e| format!("matrix sync: {e}"))
    }
```

- [ ] **Step 6: Implement `verify.rs`** — handles both to-device requests (verifying a device) and in-room requests (Element's "verify user"), only from `allowed_senders`; returns once one verification finishes:

```rust
//! SAS (emoji) verification, run from the operator's terminal while the
//! channel is stopped: the operator starts verification from their Matrix
//! client, compares the emoji shown here, and confirms on both sides.

use std::path::Path;
use std::sync::Arc;

use futures_util::StreamExt;
use matrix_sdk::{
    config::SyncSettings,
    encryption::verification::{SasState, Verification, VerificationRequest, VerificationRequestState},
    ruma::events::{
        key::verification::request::ToDeviceKeyVerificationRequestEvent,
        room::message::{MessageType, OriginalSyncRoomMessageEvent},
    },
    Client,
};
use tokio::sync::mpsc;

use crate::credentials::Session;

pub type Confirm = Arc<dyn Fn(String) -> bool + Send + Sync>;

/// Waits for a verification request from one of `allowed_senders`, runs
/// it to the end, and returns whether it succeeded.
pub async fn run(
    session: &Session,
    store_dir: &Path,
    passphrase: &str,
    allowed_senders: Vec<String>,
    confirm: Confirm,
) -> Result<bool, String> {
    let client = crate::sdk::restore(session, store_dir, passphrase).await?;
    let (done_tx, mut done_rx) = mpsc::channel::<bool>(1);
    install(&client, allowed_senders, confirm, done_tx);
    let sync_client = client.clone();
    let sync = tokio::spawn(async move { sync_client.sync(SyncSettings::default()).await });
    let ok = done_rx.recv().await.unwrap_or(false);
    sync.abort();
    Ok(ok)
}

fn install(client: &Client, allowed: Vec<String>, confirm: Confirm, done: mpsc::Sender<bool>) {
    let (allowed_dev, confirm_dev, done_dev) = (allowed.clone(), confirm.clone(), done.clone());
    client.add_event_handler(move |ev: ToDeviceKeyVerificationRequestEvent, client: Client| {
        let (allowed, confirm, done) = (allowed_dev.clone(), confirm_dev.clone(), done_dev.clone());
        async move {
            if !allowed.iter().any(|a| a == ev.sender.as_str()) {
                return;
            }
            if let Some(req) = client.encryption().get_verification_request(&ev.sender, &ev.content.transaction_id).await {
                tokio::spawn(drive(req, confirm, done));
            }
        }
    });
    client.add_event_handler(move |ev: OriginalSyncRoomMessageEvent, client: Client| {
        let (allowed, confirm, done) = (allowed.clone(), confirm.clone(), done.clone());
        async move {
            let MessageType::VerificationRequest(_) = &ev.content.msgtype else { return };
            if !allowed.iter().any(|a| a == ev.sender.as_str()) {
                return;
            }
            if let Some(req) = client.encryption().get_verification_request(&ev.sender, &ev.event_id).await {
                tokio::spawn(drive(req, confirm, done));
            }
        }
    });
}

async fn drive(request: VerificationRequest, confirm: Confirm, done: mpsc::Sender<bool>) {
    if request.accept().await.is_err() {
        let _ = done.send(false).await;
        return;
    }
    let mut changes = request.changes();
    while let Some(state) = changes.next().await {
        match state {
            VerificationRequestState::Transitioned { verification: Verification::SasV1(sas) } => {
                if sas.accept().await.is_err() {
                    let _ = done.send(false).await;
                    return;
                }
                let mut sas_changes = sas.changes();
                while let Some(s) = sas_changes.next().await {
                    match s {
                        SasState::KeysExchanged { emojis: Some(emojis), .. } => {
                            let shown = emojis
                                .emojis
                                .iter()
                                .map(|e| format!("{} {}", e.symbol, e.description))
                                .collect::<Vec<_>>()
                                .join("   ");
                            if confirm(shown) {
                                let _ = sas.confirm().await;
                            } else {
                                let _ = sas.mismatch().await;
                            }
                        }
                        SasState::Done { .. } => {
                            let _ = done.send(true).await;
                            return;
                        }
                        SasState::Cancelled(_) => {
                            let _ = done.send(false).await;
                            return;
                        }
                        _ => {}
                    }
                }
                return;
            }
            VerificationRequestState::Cancelled(_) => {
                let _ = done.send(false).await;
                return;
            }
            _ => {}
        }
    }
}
```

- [ ] **Step 7: Run** `cargo build -p aivyx-matrix && cargo test -p aivyx-matrix && cargo clippy -p aivyx-matrix --all-targets -- -D warnings` — all pass. (`sdk.rs` and `verify.rs` are exercised by the live test in Task 11; the compile check is their unit gate, as for Discord's `TwilightTransport`.)

- [ ] **Step 8: Commit**

```bash
git add crates/aivyx-matrix
git commit -s -m "feat(matrix): sealed credentials, matrix-sdk transport and SAS verification" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: `[matrix]` config

**Files:**
- Modify: `crates/aivyx-config/src/lib.rs` — `RawMatrix` (next to `RawDiscord`), a `matrix: RawMatrix` field on the raw TOML struct (next to `discord: RawDiscord`), `pub struct MatrixConfig` (next to `DiscordConfig`), `pub matrix: Option<MatrixConfig>` on `AivyxConfig` (next to `pub discord`), its construction (next to the `let discord = …` block) and the struct literal (next to `discord,`).

**Interfaces:**
- Produces: `pub struct MatrixConfig { pub homeserver: String, pub user_id: String, pub room_filter: String, pub allowed_senders: Vec<String>, pub team_run_channel: bool, pub team_trigger_rate_limit: Option<u32>, pub team_command_allowed_senders: Vec<String> }`; `AivyxConfig::matrix: Option<MatrixConfig>` — `Some` only when `[matrix]` sets `homeserver`.

- [ ] **Step 1: Write the failing tests** — in `crates/aivyx-config/src/tests.rs`, next to `discord_and_slack_team_run_channel_round_trip_from_toml`, using that file's `EnvScope::new()`, `load_with_toml(body, tag)` and `load_with_toml_result(body, tag)` helpers:

```rust
    #[test]
    fn matrix_section_parses_and_absent_means_none() {
        let env = EnvScope::new();
        let toml = r#"
[matrix]
homeserver = "https://matrix.example"
user_id = "@pa:example"
room_filter = "!home:example"
allowed_senders = ["@me:example"]
team_command_allowed_senders = ["@me:example"]
"#;
        let cfg = load_with_toml(toml, "matrix-parse");
        let m = cfg.matrix.expect("[matrix] with a homeserver is Some");
        assert_eq!(m.homeserver, "https://matrix.example");
        assert_eq!(m.user_id, "@pa:example");
        assert_eq!(m.room_filter, "!home:example");
        assert_eq!(m.allowed_senders, vec!["@me:example".to_string()]);
        assert_eq!(m.team_command_allowed_senders, vec!["@me:example".to_string()]);
        assert!(!m.team_run_channel);
        assert!(load_with_toml("", "matrix-absent").matrix.is_none());
        drop(env);
    }

    #[test]
    fn matrix_requires_https_and_well_formed_ids() {
        let env = EnvScope::new();
        for bad in [
            "[matrix]\nhomeserver = \"http://matrix.example\"\nuser_id = \"@pa:x\"\nroom_filter = \"!r:x\"\nallowed_senders = [\"@me:x\"]\n",
            "[matrix]\nhomeserver = \"https://matrix.example\"\nuser_id = \"pa\"\nroom_filter = \"!r:x\"\nallowed_senders = [\"@me:x\"]\n",
            "[matrix]\nhomeserver = \"https://matrix.example\"\nuser_id = \"@pa:x\"\nroom_filter = \"#alias:x\"\nallowed_senders = [\"@me:x\"]\n",
            "[matrix]\nhomeserver = \"https://matrix.example\"\nuser_id = \"@pa:x\"\nroom_filter = \"!r:x\"\nallowed_senders = []\n",
        ] {
            assert!(load_with_toml_result(bad, "matrix-bad").is_err(), "should reject:\n{bad}");
        }
        // A homeserver on this machine may be plain http (a local test server).
        let local = "[matrix]\nhomeserver = \"http://127.0.0.1:6167\"\nuser_id = \"@pa:x\"\nroom_filter = \"!r:x\"\nallowed_senders = [\"@me:x\"]\n";
        assert!(load_with_toml_result(local, "matrix-local").is_ok());
        drop(env);
    }
```

- [ ] **Step 2: Run** `cargo test -p aivyx-config matrix` — FAIL.

- [ ] **Step 3: Implement.** `RawMatrix`:

```rust
/// `[matrix]` TOML section. Credentials are never here: they live in the
/// sealed `<data dir>/matrix/session.enc` written by `connect matrix`.
#[derive(Debug, Default, Deserialize)]
struct RawMatrix {
    #[serde(default)]
    homeserver: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    room_filter: Option<String>,
    #[serde(default)]
    allowed_senders: Vec<String>,
    #[serde(default)]
    team_run_channel: bool,
    #[serde(default)]
    team_trigger_rate_limit: Option<u32>,
    #[serde(default)]
    team_command_allowed_senders: Vec<String>,
}
```

`MatrixConfig` (with the doc comments the config-reference generator reads — one `///` per field):

```rust
/// The Matrix channel (`--channel matrix`). Credentials are not config:
/// `aivyx-pa connect matrix` seals them under the instance's data dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatrixConfig {
    /// Your homeserver's URL, e.g. `https://matrix.org`. Must be https (plain http only for a server on this machine).
    pub homeserver: String,
    /// The assistant's own Matrix ID, e.g. `@my-assistant:matrix.org`.
    pub user_id: String,
    /// The one room the assistant answers in (its room ID, `!…:server`). Must be end-to-end encrypted.
    pub room_filter: String,
    /// Your Matrix IDs. Only these, from a device the assistant has verified, in `room_filter`, can talk to it; invites are accepted only from them.
    pub allowed_senders: Vec<String>,
    /// Operator opt-in for `/team run <goal>` from this room.
    pub team_run_channel: bool,
    /// Max `/team run` confirmations accepted per rolling hour. Unset = unlimited.
    pub team_trigger_rate_limit: Option<u32>,
    /// The Matrix IDs allowed to issue `/team ...` commands. Empty: nobody.
    pub team_command_allowed_senders: Vec<String>,
}
```

Construction (next to the `let discord = …` block), returning `ConfigError::Invalid { field, reason }` as the `discord.application_id` parse does:

```rust
        let matrix = match toml.matrix.homeserver.clone() {
            None => None,
            Some(homeserver) => {
                let local = ["http://127.0.0.1", "http://localhost", "http://[::1]"]
                    .iter()
                    .any(|p| homeserver.starts_with(p));
                if !(homeserver.starts_with("https://") || local) {
                    return Err(ConfigError::Invalid {
                        field: "matrix.homeserver",
                        reason: "must be an https:// URL".into(),
                    });
                }
                let is_id = |s: &str, sigil: char| s.starts_with(sigil) && s[1..].contains(':');
                let user_id = toml.matrix.user_id.clone().unwrap_or_default();
                if !is_id(&user_id, '@') {
                    return Err(ConfigError::Invalid { field: "matrix.user_id", reason: "must look like @name:server".into() });
                }
                let room_filter = toml.matrix.room_filter.clone().unwrap_or_default();
                if !is_id(&room_filter, '!') {
                    return Err(ConfigError::Invalid { field: "matrix.room_filter", reason: "must be a room ID like !abc:server (not an #alias)".into() });
                }
                if toml.matrix.allowed_senders.is_empty() || !toml.matrix.allowed_senders.iter().all(|s| is_id(s, '@')) {
                    return Err(ConfigError::Invalid { field: "matrix.allowed_senders", reason: "list at least one Matrix ID like @you:server".into() });
                }
                Some(MatrixConfig {
                    homeserver,
                    user_id,
                    room_filter,
                    allowed_senders: toml.matrix.allowed_senders.clone(),
                    team_run_channel: toml.matrix.team_run_channel,
                    team_trigger_rate_limit: toml.matrix.team_trigger_rate_limit,
                    team_command_allowed_senders: toml.matrix.team_command_allowed_senders.clone(),
                })
            }
        };
```

- [ ] **Step 4: Run** `cargo test -p aivyx-config` — PASS. Then the whole workspace builds: `cargo build --workspace` (every `AivyxConfig { … }` literal in tests now needs `matrix: None` — add it wherever the compiler points).

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-config crates
git commit -s -m "feat(config): [matrix] section — homeserver, home room, allowed senders" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8: Daemon mode

**Files:**
- Modify: `crates/aivyx-ipc/src/protocol.rs` (add `Matrix` to `FrontendType` after `Slack`, with a doc comment)
- Create: `crates/aivyx-channel/src/matrix_daemon_frontend.rs`
- Modify: `crates/aivyx-channel/src/lib.rs` (`pub mod matrix_daemon_frontend;`), `crates/aivyx-channel/Cargo.toml` (`aivyx-matrix = { path = "../aivyx-matrix" }`)
- Modify: `crates/aivyx-cli/src/bin/aivyx.rs` — the daemon `ChannelFactory` match (next to `FrontendType::Discord =>`): `FrontendType::Matrix => Arc::new(MatrixDaemonChannel::new())`

**Interfaces:**
- Consumes: `aivyx_matrix::{policy::{decide, Action, MatrixPolicy, OncePerRoom, ASK_TO_VERIFY, CANT_DECRYPT}, transport::{MatrixTransport, OutgoingMessage}}`, `crate::discord_daemon_frontend::render_events_for_discord` (identical rendering), `crate::daemon_ipc::chat_reply`, `team_command`, `team_dispatch`, `team_trigger_state`, `gate_command`
- Produces: `pub struct MatrixDaemonChannel` (`ChannelContext`, `platform() == Matrix`, `trust_tier() == SemiTrusted`); `pub async fn run_matrix_daemon_session<T: MatrixTransport + 'static>(transport: Arc<T>, policy: MatrixPolicy, socket_path: PathBuf, role: Option<String>, shutdown: CancellationToken, team_run_channel: bool, team_trigger_rate_limit: Option<u32>, team_command_allowed_senders: Vec<String>) -> Result<(), DaemonError>`

The daemon tier is `SemiTrusted` unconditionally because only `Action::Turn` messages are ever submitted (the policy runs in the frontend before anything reaches the daemon).

- [ ] **Step 1: Write the failing tests** (bottom of the new file):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_channel_is_matrix_semitrusted() {
        let c = MatrixDaemonChannel::new();
        assert_eq!(c.platform(), ChannelPlatform::Matrix);
        assert_eq!(c.trust_tier(), aivyx_capability::TrustTier::SemiTrusted);
        assert_eq!(c.channel_name(), "aivyx-matrix-daemon");
    }

    #[tokio::test]
    async fn a_team_command_from_a_sender_not_allowed_is_refused() {
        let mut pending = None;
        let mut history = Vec::new();
        let out = handle_matrix_team_command(
            "/team status",
            std::path::Path::new("/nonexistent.sock"),
            &mut pending,
            &mut history,
            false,
            None,
            "@stranger:x".to_string(),
            &["@me:x".to_string()],
        )
        .await;
        assert_eq!(out, Some("✗ you are not authorized to issue /team commands.".to_string()));
    }

    #[tokio::test]
    async fn ordinary_text_is_not_a_team_command() {
        let mut pending = None;
        let mut history = Vec::new();
        let out = handle_matrix_team_command(
            "hello",
            std::path::Path::new("/nonexistent.sock"),
            &mut pending,
            &mut history,
            false,
            None,
            "@me:x".to_string(),
            &["@me:x".to_string()],
        )
        .await;
        assert_eq!(out, None);
    }
}
```

- [ ] **Step 2: Run** `cargo test -p aivyx-channel matrix_daemon` — FAIL.

- [ ] **Step 3: Implement** `crates/aivyx-channel/src/matrix_daemon_frontend.rs`. The `/team` precedence chain is the Discord one (`discord_daemon_frontend.rs` `handle_discord_team_run_message` + `handle_discord_incoming_command`) folded into one function over `String` sender IDs; keep its order exactly — sender allowlist, pending confirm, `/team run`, generic `/team`:

```rust
//! Daemon-mode Matrix frontend: this process holds the Matrix client and
//! applies `aivyx_matrix::policy`; only policy-approved turns are
//! submitted to the daemon over IPC. Mirrors `discord_daemon_frontend`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use aivyx_core::{CancellationToken, ChannelContext, ChannelError, ChannelPlatform, SessionId, StreamEvent, TurnOutcome};
use aivyx_matrix::policy::{decide, Action, MatrixPolicy, OncePerRoom, ASK_TO_VERIFY, CANT_DECRYPT};
use aivyx_matrix::transport::{MatrixTransport, OutgoingMessage};

use crate::daemon_client::{self, DaemonSession};
use crate::daemon_ipc::FrontendType;
use crate::daemon_server::DaemonError;
use crate::gate_command;
use crate::team_command::{self, sender_allowed, TeamCommand};
use crate::team_dispatch;
use crate::team_trigger_state::{check_and_record_trigger, parse_confirm_reply, ConfirmReply, PendingTrigger};

pub struct MatrixDaemonChannel {
    session: SessionId,
    token: Mutex<CancellationToken>,
}

impl MatrixDaemonChannel {
    pub fn new() -> Self {
        MatrixDaemonChannel { session: SessionId::new(), token: Mutex::new(CancellationToken::new()) }
    }
}

impl Default for MatrixDaemonChannel {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ChannelContext for MatrixDaemonChannel {
    fn channel_name(&self) -> &str {
        "aivyx-matrix-daemon"
    }
    fn platform(&self) -> ChannelPlatform {
        ChannelPlatform::Matrix
    }
    fn trust_tier(&self) -> aivyx_capability::TrustTier {
        // The frontend submits only `Action::Turn` messages: the operator,
        // in the home room, from a verified device.
        aivyx_capability::TrustTier::SemiTrusted
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
        self.token.lock().expect("token mutex poisoned").clone()
    }
    fn reset_cancellation(&self) {
        *self.token.lock().expect("token mutex poisoned") = CancellationToken::new();
    }
    fn cancel_inflight(&self) {
        self.token.lock().expect("token mutex poisoned").cancel();
    }
}

/// The `/team` precedence chain for one message. `Some(reply)` when it
/// was a team command or a pending confirmation; `None` to carry on.
#[allow(clippy::too_many_arguments)]
async fn handle_matrix_team_command(
    text: &str,
    socket_path: &Path,
    pending: &mut Option<PendingTrigger<String>>,
    history: &mut Vec<Instant>,
    team_run_channel: bool,
    team_trigger_rate_limit: Option<u32>,
    sender: String,
    allowed_senders: &[String],
) -> Option<String> {
    if team_command::parse(text).is_some() && !sender_allowed(allowed_senders, &sender) {
        return Some("✗ you are not authorized to issue /team commands.".to_string());
    }
    if let Some(p) = pending.take() {
        let now = Instant::now();
        match parse_confirm_reply(text) {
            Some(ConfirmReply::Yes) | Some(ConfirmReply::No) if p.sender_id != sender => {
                if !p.is_expired(now) {
                    *pending = Some(p);
                }
            }
            Some(ConfirmReply::Yes) if p.is_expired(now) => {
                return Some("✗ that request expired, ask again.".to_string());
            }
            Some(ConfirmReply::Yes) => {
                return Some(match daemon_client::run_team_mission_channel(socket_path, FrontendType::Matrix, p.goal.clone()).await {
                    Ok(id) => format!("✓ Started mission {id}."),
                    Err(e) => format!("✗ Could not start the mission: {e}"),
                });
            }
            Some(ConfirmReply::No) => return Some("Cancelled.".to_string()),
            None => {
                if !p.is_expired(now) {
                    *pending = Some(p);
                }
            }
        }
    }
    if let Some(TeamCommand::Run { goal }) = team_command::parse(text) {
        if !team_run_channel {
            return Some("✗ this channel is not authorized to start team missions.".to_string());
        }
        if let Some(limit) = team_trigger_rate_limit
            && !check_and_record_trigger(history, limit, Instant::now())
        {
            return Some(format!("✗ too many mission-start requests (max {limit} per hour), try again later."));
        }
        *pending = Some(PendingTrigger::new(goal.clone(), sender));
        return Some(format!("Start '{goal}' on the default team? Reply yes/no."));
    }
    if let Some(cmd) = team_command::parse(text) {
        return Some(team_dispatch::dispatch(socket_path, cmd).await);
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub async fn run_matrix_daemon_session<T: MatrixTransport + 'static>(
    transport: Arc<T>,
    policy: MatrixPolicy,
    socket_path: PathBuf,
    role: Option<String>,
    shutdown: CancellationToken,
    team_run_channel: bool,
    team_trigger_rate_limit: Option<u32>,
    team_command_allowed_senders: Vec<String>,
) -> Result<(), DaemonError> {
    if !transport
        .room_is_encrypted(&policy.room_filter)
        .await
        .map_err(|e| DaemonError::Internal(e.to_string()))?
    {
        return Err(DaemonError::Internal(format!(
            "the Matrix room {} isn't encrypted — the assistant only talks in end-to-end-encrypted rooms",
            policy.room_filter
        )));
    }
    let mut session = DaemonSession::connect(&socket_path, role, Some(FrontendType::Matrix)).await?;
    let (mut asked, mut told) = (OncePerRoom::new(), OncePerRoom::new());
    let mut pending: Option<PendingTrigger<String>> = None;
    let mut history = Vec::new();
    let mut backoff = std::time::Duration::from_secs(1);
    let send = |room_id: String, text: String| {
        let t = Arc::clone(&transport);
        async move {
            t.send_message(OutgoingMessage { room_id, text })
                .await
                .map_err(|e| DaemonError::Internal(format!("matrix send: {e}")))
        }
    };

    loop {
        let inbound = tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            res = transport.next_inbound() => match res {
                Ok(i) => {
                    backoff = std::time::Duration::from_secs(1);
                    i
                }
                Err(aivyx_matrix::transport::TransportError::Unauthorized(e)) => {
                    let _ = session.disconnect().await;
                    return Err(DaemonError::Internal(aivyx_matrix::transport::TransportError::Unauthorized(e).to_string()));
                }
                Err(e) => {
                    eprintln!("aivyx-matrix(daemon): {e}; retrying in {}s", backoff.as_secs());
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(std::time::Duration::from_secs(60));
                    continue;
                }
            }
        };
        let m = match decide(&policy, inbound) {
            Action::Turn(m) => m,
            Action::AskToVerify { room_id } => {
                if asked.first(&room_id) {
                    send(room_id, ASK_TO_VERIFY.into()).await?;
                }
                continue;
            }
            Action::CantDecrypt { room_id } => {
                if told.first(&room_id) {
                    send(room_id, CANT_DECRYPT.into()).await?;
                }
                continue;
            }
            Action::Join { room_id } => {
                let _ = transport.join_room(&room_id).await;
                continue;
            }
            Action::Decline { room_id } => {
                let _ = transport.decline_invite(&room_id).await;
                continue;
            }
            Action::Ignore => continue,
        };
        let text = m.text.trim().to_string();
        if text == "/cancel" {
            let _ = session.cancel_turn().await;
            continue;
        }
        if let Some(reply) = handle_matrix_team_command(
            &text,
            &socket_path,
            &mut pending,
            &mut history,
            team_run_channel,
            team_trigger_rate_limit,
            m.sender.clone(),
            &team_command_allowed_senders,
        )
        .await
        {
            send(m.room_id.clone(), reply).await?;
            continue;
        }
        if let Some(gate) = gate_command::parse(&text) {
            let reply = match session.resolve_gate(gate.mission_id, gate.gate_id, gate.approved).await {
                Ok(()) => format!("✓ Gate {}.", if gate.approved { "approved" } else { "rejected" }),
                Err(e) => format!("✗ Gate resolve failed: {e}"),
            };
            send(m.room_id.clone(), reply).await?;
            continue;
        }
        let (events, outcome) = session.submit_input(m.text).await?;
        let reply = crate::daemon_ipc::chat_reply(
            crate::discord_daemon_frontend::render_events_for_discord(&events),
            &events,
            &outcome,
        );
        send(m.room_id, reply).await?;
    }
    let _ = session.disconnect().await;
    Ok(())
}
```

- [ ] **Step 4: Fix every exhaustive `match` on `FrontendType`** the compiler reports (`cargo build --workspace`), mirroring each one's `Discord` arm; then add the factory arm in `aivyx.rs`:

```rust
                aivyx_channel::daemon_ipc::FrontendType::Matrix => Arc::new(
                    aivyx_channel::matrix_daemon_frontend::MatrixDaemonChannel::new(),
                ),
```

- [ ] **Step 5: Run** `cargo test -p aivyx-channel matrix_daemon && cargo clippy --workspace --all-targets -- -D warnings` — PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-ipc crates/aivyx-channel crates/aivyx-cli
git commit -s -m "feat(matrix): daemon mode — the frontend applies the policy, the daemon runs turns" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9: The CLI — `--channel matrix`, `connect matrix`, `matrix verify`

**Files:**
- Modify: `crates/aivyx-channel/src/role_render.rs` — `ChannelKind::Matrix` (after `Slack`, doc: "Matrix adapter — same SemiTrusted registration posture as Telegram/Discord/Slack"), and the `"matrix"` arm of the channel-name match.
- Modify: `crates/aivyx-cli/Cargo.toml` — `aivyx-matrix = { path = "../aivyx-matrix" }`
- Create: `crates/aivyx-cli/src/bin/aivyx_modules/matrix_cli.rs`
- Modify: `crates/aivyx-cli/src/bin/aivyx.rs` — every `ChannelKind::Slack` site (`grep -n "ChannelKind::Slack" crates/aivyx-cli/src/bin/aivyx.rs`: lines ~374, ~424, ~1228, ~5578, ~6725, ~11701); the `--channel` parser; `CliMode::MatrixConnect` / `CliMode::MatrixVerify`; their parse arms; their handling after the master key is derived; the `ChannelKind::Matrix` dispatch arm; the module declaration next to `connect`.

**Interfaces:**
- Consumes: `aivyx_matrix::{credentials::{self, MatrixKeys, Session}, sdk, verify, session::{run_matrix_session, MatrixSessionConfig}, policy::MatrixPolicy}`, `aivyx_config::MatrixConfig`, `aivyx_instance::InstancePaths::matrix_dir`
- Produces: `matrix_cli::run_connect(config: &AivyxConfig, keys: &MatrixKeys, dir: &Path) -> Result<(), String>`, `matrix_cli::run_verify(config: &AivyxConfig, keys: &MatrixKeys, dir: &Path) -> Result<(), String>`

- [ ] **Step 1: Write the failing parse tests** — next to the existing `connect` parse tests (`grep -n "connect gmail must parse" crates/aivyx-cli/src/bin/aivyx.rs`):

```rust
    #[test]
    fn matrix_commands_parse() {
        assert_eq!(parse_cli_args_from(&argv(&["connect", "matrix"])).unwrap().mode, CliMode::MatrixConnect);
        assert_eq!(parse_cli_args_from(&argv(&["matrix", "verify"])).unwrap().mode, CliMode::MatrixVerify);
        assert!(parse_cli_args_from(&argv(&["matrix"])).is_err());
        assert!(parse_cli_args_from(&argv(&["matrix", "verify", "x"])).is_err());
        assert_eq!(parse_cli_args_from(&argv(&["--channel", "matrix"])).unwrap().channel, ChannelKind::Matrix);
    }
```

- [ ] **Step 2: Run** `cargo test -p aivyx-cli matrix_commands_parse` — FAIL.

- [ ] **Step 3: Implement parsing.** Add `MatrixConnect` and `MatrixVerify` to `enum CliMode` (doc: "`aivyx-pa connect matrix` / `matrix verify` — need the master key, not the store"). In the `connect` block, before `return Ok(CliArgs { mode: CliMode::Connect(service), … })`:

```rust
        if service.as_deref() == Some("matrix") {
            return Ok(CliArgs {
                mode: CliMode::MatrixConnect,
                channel: ChannelKind::Local,
                role: None,
                no_daemon: false,
                mcp_servers: vec![],
                mcp_sse_servers: vec![],
                provider: None,
                web_ui_port: None,
            });
        }
```

and a new block right after the `connect` block:

```rust
    // `aivyx-pa matrix verify`: SAS-verify an operator device.
    if !args.is_empty() && args[0] == "matrix" {
        if args.get(1).map(String::as_str) != Some("verify") || args.len() > 2 {
            return Err("usage: `aivyx-pa matrix verify`".into());
        }
        return Ok(CliArgs {
            mode: CliMode::MatrixVerify,
            channel: ChannelKind::Local,
            role: None,
            no_daemon: false,
            mcp_servers: vec![],
            mcp_sse_servers: vec![],
            provider: None,
            web_ui_port: None,
        });
    }
```

In the `--channel` parser add `"matrix" => ChannelKind::Matrix,` and add `matrix` to both lists of supported channels in its error messages. Add `ChannelKind::Matrix` to every `ChannelKind::Slack` arm the compiler flags, as `ChannelKind::Telegram | ChannelKind::Discord | ChannelKind::Slack | ChannelKind::Matrix` (same SemiTrusted posture), and `ChannelKind::Matrix => "matrix"` in `role_render.rs`.

- [ ] **Step 4: Run** `cargo test -p aivyx-cli matrix_commands_parse` — PASS.

- [ ] **Step 5: Write `matrix_cli.rs`**:

```rust
//! `aivyx-pa connect matrix` and `aivyx-pa matrix verify`.

use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::Arc;

use aivyx_config::AivyxConfig;
use aivyx_matrix::credentials::{self, MatrixKeys};

fn matrix_config(config: &AivyxConfig) -> Result<&aivyx_config::MatrixConfig, String> {
    config.matrix.as_ref().ok_or_else(|| {
        "no [matrix] section in your config. Add homeserver, user_id, room_filter \
         and allowed_senders first — see `docs/guide/13-chat-apps-and-accounts.md`."
            .to_string()
    })
}

/// Logs the assistant in once and seals the session. The password is read
/// without echo and never written anywhere.
pub async fn run_connect(config: &AivyxConfig, keys: &MatrixKeys, dir: &Path) -> Result<(), String> {
    let m = matrix_config(config)?;
    eprintln!("Logging in to {} as {}.", m.homeserver, m.user_id);
    let password = rpassword::prompt_password("The assistant's Matrix password (not stored): ")
        .map_err(|e| format!("read password: {e}"))?;
    let session = aivyx_matrix::sdk::login(&m.homeserver, dir, &keys.store_passphrase(), &m.user_id, &password).await?;
    credentials::save(keys, dir, &session)?;
    eprintln!(
        "Connected: device {} for {}. Next: `aivyx-pa matrix verify`, then \
         `aivyx-pa --channel matrix`.",
        session.device_id, session.user_id
    );
    Ok(())
}

/// Waits for the operator to start verification from their Matrix app,
/// shows the emoji, and confirms only if they say they match.
pub async fn run_verify(config: &AivyxConfig, keys: &MatrixKeys, dir: &Path) -> Result<(), String> {
    let m = matrix_config(config)?;
    let session = credentials::load(keys, dir)?
        .ok_or("not connected yet — run `aivyx-pa connect matrix` first")?;
    eprintln!(
        "Waiting for a verification request. In your Matrix app, open the \
         assistant ({}) and choose \"Verify\". Ctrl-C to stop.",
        m.user_id
    );
    let confirm: aivyx_matrix::verify::Confirm = Arc::new(|emoji: String| {
        eprintln!("\nDo these match what your Matrix app shows?\n\n  {emoji}\n");
        eprint!("Type yes if they match: ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        let _ = std::io::stdin().lock().read_line(&mut line);
        line.trim().eq_ignore_ascii_case("yes")
    });
    let ok = aivyx_matrix::verify::run(&session, dir, &keys.store_passphrase(), m.allowed_senders.clone(), confirm).await?;
    if ok {
        eprintln!("Verified. The assistant now answers that device.");
        Ok(())
    } else {
        Err("verification didn't complete (cancelled or the emoji didn't match)".into())
    }
}
```

`rpassword = "7"` is already a dependency of `aivyx-cli`.

- [ ] **Step 6: Handle the two modes** in `run()`, immediately after `let master_key = derive_master_key(…)?;` and before the audit chain key derivation:

```rust
    if matches!(mode, CliMode::MatrixConnect | CliMode::MatrixVerify) {
        let keys = aivyx_matrix::credentials::MatrixKeys::derive(&master_key)?;
        let dir = aivyx_instance::InstancePaths::current()
            .map_err(|e| e.to_string())?
            .matrix_dir()
            .ok_or("can't resolve the instance's data folder (is HOME set?)")?;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("failed to build tokio runtime: {e}"))?;
        return rt.block_on(async {
            if mode == CliMode::MatrixVerify {
                if let Ok(sp) = default_socket_path()
                    && aivyx_channel::daemon_client::daemon_is_running(&sp).await
                {
                    return Err("stop the daemon first (`aivyx-pa daemon stop`) — \
                                verification opens the Matrix store the daemon uses"
                        .to_string());
                }
                matrix_cli::run_verify(&config, &keys, &dir).await
            } else {
                matrix_cli::run_connect(&config, &keys, &dir).await
            }
        });
    }
```

(Declare `mod matrix_cli;` where the other `aivyx_modules` are declared, and import it the way `connect` is imported.)

The dispatch arm needs the keys too: derive `let matrix_keys = aivyx_matrix::credentials::MatrixKeys::derive(&master_key)?;` next to `loop_backlog_chain_key`, add a `matrix_keys: aivyx_matrix::credentials::MatrixKeys` parameter to `run_async` (after `loop_backlog_chain_key`), and pass it at the call site.

- [ ] **Step 7: The `ChannelKind::Matrix` dispatch arm** — insert before `ChannelKind::Slack => {`:

```rust
        ChannelKind::Matrix => {
            let mc = matrix.ok_or("`--channel matrix` needs a [matrix] section in your config")?;
            let dir = aivyx_instance::InstancePaths::current()
                .map_err(|e| e.to_string())?
                .matrix_dir()
                .ok_or("can't resolve the instance's data folder")?;
            let session = aivyx_matrix::credentials::load(&matrix_keys, &dir)?
                .ok_or("not connected to Matrix yet — run `aivyx-pa connect matrix` first")?;
            let passphrase = matrix_keys.store_passphrase();
            let policy = aivyx_matrix::policy::MatrixPolicy {
                room_filter: mc.room_filter.clone(),
                allowed_senders: mc.allowed_senders.clone(),
            };
            let shutdown = CancellationToken::new();
            let shutdown_for_signal = shutdown.clone();
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_err() {
                    std::process::exit(130);
                }
                eprintln!("\naivyx: shutting down the Matrix channel.");
                shutdown_for_signal.cancel();
            });
            if !no_daemon && let Ok(sp) = default_socket_path() {
                let transport = aivyx_matrix::sdk::SdkTransport::connect(&session, &dir, &passphrase).await?;
                transport.sync_once().await?;
                eprintln!(
                    "aivyx-pa {} (daemon) — Matrix live in {} as {}",
                    env!("CARGO_PKG_VERSION"),
                    mc.room_filter,
                    session.user_id
                );
                match aivyx_channel::matrix_daemon_frontend::run_matrix_daemon_session(
                    transport,
                    policy.clone(),
                    sp.clone(),
                    Some(active_role_name.clone()),
                    shutdown.clone(),
                    mc.team_run_channel,
                    mc.team_trigger_rate_limit,
                    mc.team_command_allowed_senders.clone(),
                )
                .await
                {
                    Ok(()) => return Ok(()),
                    Err(e) => eprintln!("aivyx-pa: daemon Matrix session failed ({e}), falling back to in-process."),
                }
            }
            eprintln!(
                "aivyx-pa {} — Matrix live in {} (in-process)",
                env!("CARGO_PKG_VERSION"),
                mc.room_filter
            );
            let matrix_config = aivyx_matrix::MatrixSessionConfig {
                model,
                system_prompt,
                max_tokens: DEFAULT_MAX_TOKENS,
                capabilities,
                tools,
                storage,
                tool_allowlist,
                memory_topic_prefix,
                turn_timeout_secs,
                cycle_detection,
                injection_scan_enabled,
                injection_scan_exempt,
                confirm_integration_writes,
                budget_gate: aivyx_channel::budget_gate::ChannelBudgetGate::new_gate(
                    config_budget.clone(),
                    Arc::clone(&persistent_audit_for_query),
                    aivyx_cost::Pricing::with_overrides(config_pricing.clone()),
                    DEFAULT_MAX_TOKENS,
                ),
                rate_gate: aivyx_channel::rate_gate::ChannelRateGate::new_gate(config_rate_limit.clone()),
            };
            aivyx_matrix::session::run_matrix_session(
                &session, &dir, &passphrase, policy, matrix_config, provider, audit, checkpointer.clone(), shutdown,
            )
            .await
            .map(|_| ())
        }
```

(`matrix` comes from destructuring the config next to `discord,` / `slack,` — add `matrix,` there. If the daemon path consumed `session`/`dir` by reference only, they're still available for the fallback, as written.)

- [ ] **Step 8: Run** `cargo build --workspace && cargo test -p aivyx-cli && cargo clippy --workspace --all-targets -- -D warnings` — PASS. Smoke-check by hand: `cargo run --bin aivyx-pa -- matrix verify` with no `[matrix]` section prints the "no [matrix] section" message.

- [ ] **Step 9: Commit**

```bash
git add crates
git commit -s -m "feat(cli): --channel matrix, connect matrix and matrix verify" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 10: Docs

**Files:**
- Modify: `scripts/gen-config-reference.py` — put `matrix` in the same group as `telegram`/`discord`/`slack` (`grep -n "\"discord\"" scripts/gen-config-reference.py` shows the group list); then regenerate.
- Modify: `docs/manual/reference/02-configuration.md` (generated), `docs/manual/reference/01-cli.md` (`connect matrix`, `matrix verify`, `--channel matrix`), `docs/guide/13-chat-apps-and-accounts.md` (a `### Matrix` section after Telegram), `docs/THREAT_MODEL.md` (Matrix's tier rule), `crates/aivyx-cli/src/bin/aivyx_modules/help.rs` (the help text lists `matrix verify` and `--channel matrix`).

- [ ] **Step 1: Regenerate the config reference**

Run: `python3 scripts/gen-config-reference.py > /tmp/conf.md && cp /tmp/conf.md docs/manual/reference/02-configuration.md && git diff --stat docs/manual/reference/02-configuration.md`
Expected: a new `### \`[matrix]\`` section with the seven keys and their doc-comment text.

- [ ] **Step 2: Guide section** — add to `docs/guide/13-chat-apps-and-accounts.md` after the Telegram section:

````markdown
### Matrix

Matrix gives you an end-to-end-encrypted room with your assistant, on a
homeserver you choose (or run yourself).

1. Create a Matrix account for the assistant (e.g. `@my-assistant:matrix.org`),
   and from your own account create a room, **turn on encryption**, and invite it.
   Copy the room's ID (`!…:server`, in the room's advanced settings).
2. Add to your config:

   ```toml
   [matrix]
   homeserver = "https://matrix.org"
   user_id = "@my-assistant:matrix.org"
   room_filter = "!abcdef:matrix.org"
   allowed_senders = ["@you:matrix.org"]
   ```

3. `aivyx-pa connect matrix` — logs the assistant in once (the password isn't kept).
4. `aivyx-pa matrix verify` — then, in your Matrix app, open the assistant and
   choose **Verify**; compare the emoji and confirm on both sides. Repeat for each
   device you'll use.
5. `aivyx-pa --channel matrix`.

It answers only you (`allowed_senders`), only in that room, and only from devices
you've verified — a stolen password or the homeserver can't impersonate you. From an
unverified device it tells you once to verify; everyone else is ignored.
````

- [ ] **Step 3: CLI reference** — add `### aivyx-pa connect matrix` and `### aivyx-pa matrix verify` headings with two-line descriptions (from the guide), and `matrix` to the `--channel` list. The `every_command_has_a_cli_reference_heading` drift test (`cargo test -p aivyx-cli every_command`) must pass.

- [ ] **Step 4: THREAT_MODEL.md** — in the tier table's `SemiTrusted` row examples add "the Matrix home room, from a verified operator device"; add a paragraph under the channel section: "Matrix: a message is a turn only from `allowed_senders`, in `room_filter`, from a device the assistant verified by SAS; an unverified device or an undecryptable message from the operator gets one explanation per room; anything else is ignored. Credentials are sealed under the master key (`session.enc`); the `matrix-sdk` store is encrypted with a master-key subkey."

- [ ] **Step 5: Run** `cargo test -p aivyx-cli && cargo test -p aivyx-config` — PASS (drift tests).

- [ ] **Step 6: Commit**

```bash
git add docs scripts crates/aivyx-cli
git commit -s -m "docs: the Matrix channel — guide, references, threat model" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 11: Live end-to-end test (`#[ignore]`)

**Files:**
- Create: `crates/aivyx-matrix/tests/live_conduit.rs`

Runs against a throwaway Conduit homeserver in Docker; skipped by default. It proves login, the sealed session, an encrypted room, `room_is_encrypted`, and an encrypted message round trip through `SdkTransport` (verification itself needs a person, so this test checks the *unverified* path: the operator's message arrives with `verified == false`).

- [ ] **Step 1: Write the test**

```rust
//! Live test against a local Conduit homeserver. Run by hand:
//!
//!   docker run -d --rm --name aivyx-conduit -p 127.0.0.1:6167:6167 \
//!     -e CONDUIT_SERVER_NAME=localhost -e CONDUIT_ALLOW_REGISTRATION=true \
//!     -e CONDUIT_DATABASE_BACKEND=rocksdb -e CONDUIT_PORT=6167 \
//!     -e CONDUIT_ADDRESS=0.0.0.0 -e CONDUIT_DATABASE_PATH=/tmp \
//!     -e CONDUIT_YES_I_AM_VERY_VERY_SURE_I_WANT_AN_OPEN_REGISTRATION_SERVER_PRONE_TO_ABUSE=true \
//!     matrixconduit/matrix-conduit:latest
//!   cargo test -p aivyx-matrix --test live_conduit -- --ignored --nocapture
//!   docker stop aivyx-conduit

use std::time::Duration;

use aivyx_matrix::credentials::{self, MatrixKeys};
use aivyx_matrix::transport::{Inbound, MatrixTransport};
use matrix_sdk::{
    ruma::{api::client::room::create_room::v3::Request as CreateRoom, events::room::encryption::RoomEncryptionEventContent, events::InitialStateEvent},
    Client,
};

const HS: &str = "http://127.0.0.1:6167";

async fn register(user: &str) -> Client {
    let client = Client::builder().homeserver_url(HS).build().await.unwrap();
    let mut req = matrix_sdk::ruma::api::client::account::register::v3::Request::new();
    req.username = Some(user.to_string());
    req.password = Some("pw-live-test".to_string());
    req.auth = Some(matrix_sdk::ruma::api::client::uiaa::AuthData::Dummy(Default::default()));
    let _ = client.matrix_auth().register(req).await;
    client.matrix_auth().login_username(user, "pw-live-test").await.unwrap();
    client
}

#[tokio::test]
#[ignore = "needs a local Conduit homeserver in Docker — see the module doc"]
async fn an_encrypted_message_reaches_the_transport() {
    let stamp = std::process::id();
    let (bot, me) = (format!("pa{stamp}"), format!("me{stamp}"));
    register(&bot).await; // creates the account; logged out below by our own login
    let operator = register(&me).await;

    let dir = std::env::temp_dir().join(format!("aivyx-matrix-live-{stamp}"));
    let keys = MatrixKeys::derive(&aivyx_crypto::MasterKey::from_raw([5u8; 32])).unwrap();
    let session = aivyx_matrix::sdk::login(HS, &dir, &keys.store_passphrase(), &bot, "pw-live-test").await.unwrap();
    credentials::save(&keys, &dir, &session).unwrap();
    assert_eq!(credentials::load(&keys, &dir).unwrap(), Some(session.clone()));

    let mut create = CreateRoom::new();
    create.invite = vec![session.user_id.as_str().try_into().unwrap()];
    create.initial_state = vec![InitialStateEvent::with_empty_state_key(
        RoomEncryptionEventContent::with_recommended_defaults(),
    )
    .to_raw_any()];
    let room = operator.create_room(create).await.unwrap();

    let transport = aivyx_matrix::sdk::SdkTransport::connect(&session, &dir, &keys.store_passphrase()).await.unwrap();
    // The invite arrives; join it.
    let invite = tokio::time::timeout(Duration::from_secs(20), transport.next_inbound()).await.unwrap().unwrap();
    let Inbound::Invite { room_id, .. } = invite else { panic!("expected an invite, got {invite:?}") };
    transport.join_room(&room_id).await.unwrap();
    transport.sync_once().await.unwrap();
    assert!(transport.room_is_encrypted(&room_id).await.unwrap());

    operator.sync_once(Default::default()).await.unwrap();
    room.send(matrix_sdk::ruma::events::room::message::RoomMessageEventContent::text_plain("hello from live")).await.unwrap();
    loop {
        let item = tokio::time::timeout(Duration::from_secs(20), transport.next_inbound()).await.unwrap().unwrap();
        if let Inbound::Message(m) = item {
            assert_eq!(m.text, "hello from live");
            assert!(!m.verified, "an unverified operator device must not count as verified");
            break;
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
```

- [ ] **Step 2: Run** `cargo test -p aivyx-matrix --test live_conduit` — the test is listed as ignored (it compiles). Then, with Conduit running as in the doc comment, `cargo test -p aivyx-matrix --test live_conduit -- --ignored --nocapture` — PASS. If a `ruma` request field name differs in 0.19, fix it against the compiler; the assertions are what matter.

- [ ] **Step 3: Commit**

```bash
git add crates/aivyx-matrix/tests
git commit -s -m "test(matrix): live encrypted round trip against a local Conduit (ignored)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

## Final check

- [ ] `cargo clippy --workspace --all-targets -- -D warnings` — clean
- [ ] `cargo test --workspace` — all pass
- [ ] Live: with Conduit running, `connect matrix` → `matrix verify` (verify from Element against the local server) → `--channel matrix`; a message from the verified device gets a reply, an unverified one gets `ASK_TO_VERIFY` once.
