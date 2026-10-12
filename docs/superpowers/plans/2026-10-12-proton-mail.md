# Proton Mail Channel and Tools Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The operator emails their assistant from Proton Mail and gets the answer back in the same thread, and the agent can search, read, draft and send in the operator's Proton mailbox — all through Proton Mail Bridge's local IMAP/SMTP, so mail is decrypted only on the operator's machine.

**Architecture:** A new crate `crates/aivyx-proton` shaped like `aivyx-matrix`: a `MailTransport` trait with a scripted double, a pure trust policy, a `ChannelContext`, and one receive loop (`run_mail_loop`) that both modes share through a `TurnRunner` (the agent in-process; the daemon in daemon mode, one IPC session per email thread). The real transport speaks IMAP (`async-imap`, IDLE) and SMTP (a small client of our own) to Bridge on loopback, STARTTLS pinned to the SHA-256 of Bridge's certificate. The same crate ships the `aivyx-proton` tool process (`proton.search/read/draft/send`). `aivyx-config` gains `[proton]`, `aivyx-channel` a daemon frontend, the CLI `--channel proton` and `connect proton`.

**Tech Stack:** Rust 2024, tokio, `async-imap` 0.11 (`runtime-tokio`), `mail-parser` 0.11, `rustls` 0.23 / `tokio-rustls` 0.26 on aws-lc-rs, `lettre` 0.11 (message builder only).

**Spec:** `docs/superpowers/specs/2026-10-12-proton-mail-design.md`.

**How this plan was checked:** every code block below was built and run before the plan was written, in a scratch worktree of this repo at `947a1e93`: `cargo clippy --workspace --all-targets -- -D warnings` clean, `cargo test --workspace` passing (7,208 tests, 0 failed), and both `#[ignore]`d live tests (Task 16) passing against GreenMail 2.1.3. The IMAP calls, the IDLE wake-up and the folder move were exercised against a real server; the pinned TLS against a local rustls server using a Bridge-like self-signed CA certificate. Two things the research found that shape the code:

- Bridge's certificate is a self-signed **CA** certificate, which webpki refuses as a server certificate (`CaUsedAsEndEntity`), so it can't be a trust root. The plan pins its SHA-256 with a custom rustls verifier (handshake signatures still verified). `lettre`'s SMTP transport can't take a custom verifier, so `smtp.rs` is a small SMTP client of our own; `lettre` only builds the message.
- In-process, every turn's planner starts fresh (as on Matrix and Telegram): a thread keeps one session id and memory partition, and the reply is threaded, but the model doesn't see earlier emails. Daemon mode keeps one daemon session per thread, which carries the conversation.

## Where this plan differs from the spec's wording

Deliberate; reviewers should not flag these as gaps.

- **Module names:** the spec's `imap.rs` is `bridge.rs` here (it holds IMAP *and* the SMTP dial, behind one `Dial` trait). The shared receive loop is `session::run_mail_loop`, used by both modes.
- **A failed reply** is logged to stderr and the email stays unread in the inbox; the turn itself is in the audit chain as usual, but there is no separate audit event for the failed send (the loop has no audit hook in daemon mode).
- **History per thread:** daemon mode keeps one daemon session per thread (the conversation carries over); in-process mode gives each thread its own session id and memory partition, but each turn's planner starts fresh — the same as Matrix and Telegram in-process.
- **Tools send from** `credentials.toml`'s `from_address` (your first `own_addresses` entry): the sandboxed tool process can't read `aivyx-pa.toml`. There is no `from` input.
- **`[proton]` without `channel_address`** is valid (tools only); `--channel proton` requires it, and it must be one of `own_addresses`.

## Global Constraints

- A message becomes a turn (at `TrustTier::SemiTrusted`) only when it is addressed to `channel_address` and is either **from you** — From ∈ `own_addresses` **and** the same Proton message (`X-Pm-Internal-Id`) is in the **Sent** folder — or **allowlisted** — From ∈ `allowed_external_senders` **and** the **first** `Authentication-Results` value is stamped `mail.protonmail.ch` and is `dmarc=pass` with `header.from` = the From domain. External mail must be addressed via `Delivered-To` / `X-Original-To`; mail from you may also be addressed via `To` / `Cc`.
- Never a turn for mail from `channel_address`, with `Auto-Submitted` other than `no`, or `Precedence: bulk/list/junk`. Everything not approved is ignored **silently** — no auto-reply, ever.
- Bridge only on loopback (`127.0.0.1`, `localhost`, `[::1]`). TLS always STARTTLS pinned to the SHA-256 captured by `connect proton`; never system trust, never "accept anything" (the capturing verifier is used only inside `connect proton`). A changed certificate or a rejected login stops the channel with "run `aivyx-pa connect proton` again".
- The Bridge credential and pins live only in `<tool_process_dir("proton")>/credentials.toml`, mode `0600` (folder `0700`); a file others can read is refused. The channel's cursor and lock live in `InstancePaths::proton_dir()` (`<data dir>/proton`). Every aivyx-pa path is built in `aivyx-instance` (the `no_stray_paths` guard test enforces it).
- `proton.search`/`proton.read`: `email.read`, `output_is_untrusted() == true`. `proton.draft`: `email.write`. `proton.send`: `email.send` (the turn loop's approval pause applies). All four are therefore `Trusted`-only, like Gmail.
- Turn input: text part (else HTML as text), quoted history dropped, at most 32 KiB; attachments never read. The reply says when it cut or ignored attachments.
- Dependencies exactly: `async-imap = { version = "0.11", default-features = false, features = ["runtime-tokio"] }`, `mail-parser = "0.11"`, `rustls = { version = "0.23", default-features = false, features = ["aws_lc_rs", "std", "tls12"] }`, `tokio-rustls = { version = "0.26", default-features = false, features = ["aws_lc_rs", "tls12"] }`.
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` must pass after every task. Don't run `cargo fmt` (pa doesn't enforce it; it would reformat unrelated files). Commits are signed off (`git commit -s`) and end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## File Structure

| File | Responsibility |
|---|---|
| `crates/aivyx-instance/src/lib.rs` (modify) | `InstancePaths::proton_dir()` |
| `crates/aivyx-proton/Cargo.toml` (create) | manifest: lib + `aivyx-proton` bin |
| `crates/aivyx-proton/src/lib.rs` (create) | module list + re-exports |
| `crates/aivyx-proton/src/message.rs` (create) | `MailMessage`, `parse`, quote stripping, caps |
| `crates/aivyx-proton/src/policy.rs` (create) | `ProtonPolicy`, `decide`, `proton_dmarc_passes` |
| `crates/aivyx-proton/src/outgoing.rs` (create) | `OutgoingMail` (replies, threading, RFC 5322) |
| `crates/aivyx-proton/src/transport.rs` (create) | `MailTransport`, `TransportError`, `SearchQuery`, `ScriptedTransport` |
| `crates/aivyx-proton/src/tls.rs` (create) | pinned / capturing rustls verifiers |
| `crates/aivyx-proton/src/smtp.rs` (create) | minimal SMTP submission client |
| `crates/aivyx-proton/src/bridge.rs` (create) | `Dial`, `BridgeDial`, `BridgeTransport` (IMAP+SMTP), `capture_pins` |
| `crates/aivyx-proton/src/lock.rs` (create) | one channel process per instance (flock) |
| `crates/aivyx-proton/src/state.rs` (create) | resume cursor |
| `crates/aivyx-proton/src/credentials.rs` (create) | `credentials.toml` |
| `crates/aivyx-proton/src/proton_channel.rs` (create) | `ProtonChannel` (`ChannelContext`) |
| `crates/aivyx-proton/src/session.rs` (create) | `screen`, `answer`, `TurnRunner`, `run_mail_loop`, in-process session |
| `crates/aivyx-proton/src/tests.rs` (create) | session-level tests |
| `crates/aivyx-proton/src/tools/*.rs` (create) | `proton.search/read/draft/send` |
| `crates/aivyx-proton/src/main.rs` (create) | the tool process |
| `crates/aivyx-proton/tests/fixtures/*` (create) | recorded Proton headers, Bridge-like test certificate |
| `crates/aivyx-proton/tests/live_greenmail.rs` (create) | `#[ignore]`d live tests |
| `crates/aivyx-config/src/{lib,tests}.rs` (modify) | `[proton]` → `ProtonConfig` |
| `crates/aivyx-ipc/src/protocol.rs` (modify) | `FrontendType::Proton` |
| `crates/aivyx-channel/src/channel_team_chain.rs` (create) | the `/team` chain, lifted from the Matrix frontend |
| `crates/aivyx-channel/src/proton_daemon_frontend.rs` (create) | daemon mode |
| `crates/aivyx-channel/src/{matrix_daemon_frontend,lib,role_render}.rs`, `Cargo.toml`, `tests/daemon_roundtrip_e2e.rs` (modify) | wiring |
| `crates/aivyx-cli/...` (modify/create `proton_cli.rs`) | `--channel proton`, `connect proton` |
| `docs/…`, `scripts/gen-*-reference.py` (modify) | guide, references, TOOLS.md, THREAT_MODEL |

---

### Task 1: The instance's Proton folder

**Files:**
- Modify: `crates/aivyx-instance/src/lib.rs` (a method after `matrix_dir`, a test in the existing `mod tests`)

**Interfaces:**
- Produces: `InstancePaths::proton_dir(&self) -> Option<PathBuf>` = `data_dir()/proton`.

- [ ] **Step 1: Write the failing test and the method** — apply this change (the test is the `proton_dir_is_under_the_instance_data_dir` hunk; add it first and run Step 2 before adding the method):

```diff
diff --git a/crates/aivyx-instance/src/lib.rs b/crates/aivyx-instance/src/lib.rs
index 373f6fd1..31337c5d 100644
--- a/crates/aivyx-instance/src/lib.rs
+++ b/crates/aivyx-instance/src/lib.rs
@@ -296,6 +296,14 @@ impl InstancePaths {
         self.data_dir().map(|d| d.join("matrix"))
     }
 
+    /// Proton Mail channel state: `data_dir/proton` — the resume cursor
+    /// (`state.json`) and the per-process lock. The Bridge credential is
+    /// not here: it lives in `tool_process_dir("proton")`, which the
+    /// sandboxed `proton.*` tools can reach.
+    pub fn proton_dir(&self) -> Option<PathBuf> {
+        self.data_dir().map(|d| d.join("proton"))
+    }
+
     /// Sandbox directory: `~/aivyx-pa-sandbox[-<n>]`
     pub fn sandbox_dir(&self) -> Option<PathBuf> {
         self.dirs.home.as_ref().map(|h| {
@@ -455,6 +463,26 @@ mod tests {
         );
     }
 
+    #[test]
+    fn proton_dir_is_under_the_instance_data_dir() {
+        let dirs = BaseDirs {
+            home: Some(PathBuf::from("/h")),
+            xdg_config_home: None,
+            xdg_data_home: None,
+            xdg_runtime_dir: None,
+        };
+        let default = InstancePaths::new(InstanceName::default_instance(), dirs.clone());
+        assert_eq!(
+            default.proton_dir(),
+            Some(PathBuf::from("/h/.local/share/aivyx-pa/proton"))
+        );
+        let work = InstancePaths::new(InstanceName::parse("work").unwrap(), dirs);
+        assert_eq!(
+            work.proton_dir(),
+            Some(PathBuf::from("/h/.local/share/aivyx-pa/instances/work/proton"))
+        );
+    }
+
     #[test]
     fn names_are_validated() {
         for ok in ["default", "research", "a", "home-2", "x1"] {
```

- [ ] **Step 2: Run it** — with only the test added: `cargo test -p aivyx-instance proton_dir` → FAIL to compile (`no method named proton_dir`). With the method: PASS. Also `cargo test -p aivyx-instance` (the `no_stray_paths` guard) → PASS.

- [ ] **Step: Commit**

```bash
git add crates/aivyx-instance/src/lib.rs
git commit -s -m "feat(instance): proton_dir for the Proton Mail channel's state" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: `aivyx-proton` crate and the parsed message

The crate, its manifest (all dependencies at once — later tasks only add modules), the recorded Proton header fixture, and the parsed-message type every other module reads.

- [ ] **Step 0: Scaffold the crate.**
  1. Create `crates/aivyx-proton/Cargo.toml`:

```toml
[package]
name = "aivyx-proton"
description = "Proton Mail channel and proton.* tools over Proton Mail Bridge (local IMAP/SMTP, pinned TLS)"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
publish.workspace = true
authors.workspace = true
repository.workspace = true
readme.workspace = true

[lib]
path = "src/lib.rs"

# The `proton.*` tool process, spawned by the daemon via `[[tool_process]]`.
[[bin]]
name = "aivyx-proton"
path = "src/main.rs"

[dependencies]
aivyx-core = { path = "../aivyx-core" }
aivyx-capability = { path = "../aivyx-capability" }
aivyx-instance = { path = "../aivyx-instance" }
aivyx-llm = { path = "../aivyx-llm" }
aivyx-storage = { path = "../aivyx-storage" }
aivyx-tool = { path = "../aivyx-tool" }
async-trait = { workspace = true }
base64 = { workspace = true }
futures-util = { workspace = true }
hex = { workspace = true }
# Message *building* only (RFC 5322, encoded headers); SMTP is our own
# `smtp.rs` because the TLS must be pinned, which lettre's transport
# can't express.
lettre = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
sha2 = { workspace = true }
thiserror = { workspace = true }
tokio = { workspace = true, features = ["net", "io-util", "fs"] }
toml = "0.8"
uuid = { workspace = true }
# MIT OR Apache-2.0. Async IMAP on tokio, with IDLE.
async-imap = { version = "0.11", default-features = false, features = ["runtime-tokio"] }
# Apache-2.0 OR MIT (Stalwart). MIME parsing, HTML→text fallback.
mail-parser = "0.11"
# Already in the tree (reqwest, matrix-sdk); named directly for the pinned
# certificate verifier. aws-lc-rs, like the rest of pa.
rustls = { version = "0.23", default-features = false, features = ["aws_lc_rs", "std", "tls12"] }
tokio-rustls = { version = "0.26", default-features = false, features = ["aws_lc_rs", "tls12"] }

# flock(2) for the channel lock (`lock.rs`).
[target.'cfg(unix)'.dependencies]
libc = { workspace = true }

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt", "rt-multi-thread", "time", "sync"] }
aivyx-audit = { path = "../aivyx-audit" }
aivyx-crypto = { path = "../aivyx-crypto" }
tempfile = "3"
```

  2. Add `"crates/aivyx-proton",` right after `"crates/aivyx-matrix",` in **both** `members` and `default-members` of the root `Cargo.toml`.
  3. Create `crates/aivyx-proton/src/lib.rs`:

```rust
//! `aivyx-proton` — Proton Mail for Aivyx PA, through Proton Mail Bridge's
//! local IMAP/SMTP: the email channel (`--channel proton`) and the
//! `proton.*` tools (the `aivyx-proton` tool process). Mail is decrypted
//! only by Bridge, on this machine.
//!
//! Shape: like `aivyx-matrix` — a transport trait + scripted double
//! (`transport`), the pure trust policy (`policy`), a channel
//! (`proton_channel`) and the receive loop (`session`), plus the real
//! Bridge transport (`bridge`, `smtp`, pinned `tls`). See
//! `docs/superpowers/specs/2026-10-12-proton-mail-design.md`.

pub mod message;
```

  4. Create `crates/aivyx-proton/src/main.rs` (a placeholder so the `[[bin]]` builds):

```rust
fn main() {
    // Replaced by the tool process in Task 11.
}
```

  5. `mkdir -p crates/aivyx-proton/tests/fixtures && cp docs/superpowers/specs/fixtures/proton-external-dmarc-pass.headers crates/aivyx-proton/tests/fixtures/`
  6. `cargo build -p aivyx-proton` → builds (this resolves and locks the new dependencies; `Cargo.lock` changes).

**Files:**
- Create: `crates/aivyx-proton/src/message.rs`
- Modify: `crates/aivyx-proton/src/lib.rs` (add `pub mod message;`, keeping the list alphabetical)
- Create: `crates/aivyx-proton/Cargo.toml`, `crates/aivyx-proton/src/main.rs`, `crates/aivyx-proton/tests/fixtures/proton-external-dmarc-pass.headers`
- Modify: `Cargo.toml` (workspace members), `Cargo.lock`

**Interfaces:**
- Consumes: nothing
- Produces: `struct MailMessage { uid, internal_id, message_id, in_reply_to, references, subject, from, from_name, to, cc, delivered_to, auth_results, auto_submitted, precedence, date, text, attachments }` (all `pub`, `Default`), `MailMessage::thread_root(&self) -> String`, `fn parse(uid: u32, raw: &[u8]) -> Option<MailMessage>`, `fn without_quoted_history(&str) -> String`, `fn cap(&str, usize) -> (String, bool)`, `const TURN_TEXT_CAP`, `const TOOL_TEXT_CAP` (both 32 KiB)

- [ ] **Step 1: Write the failing tests** — create `crates/aivyx-proton/src/message.rs` holding only this test module (the body goes above it in Step 3), and add `pub mod message;` to `lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/proton-external-dmarc-pass.headers");

    fn with_fixture(extra: &str) -> Vec<u8> {
        let mut raw = FIXTURE.to_vec();
        raw.extend_from_slice(extra.as_bytes());
        raw
    }

    #[test]
    fn proton_headers_are_read_in_order() {
        let raw = with_fixture(
            "From: Ann <Ann@NewsLetter.example>\r\nSubject: hi\r\n\r\nhello\r\n",
        );
        let m = parse(7, &raw).unwrap();
        assert_eq!(m.uid, 7);
        assert_eq!(m.from.as_deref(), Some("ann@newsletter.example"));
        assert_eq!(m.delivered_to, vec!["you@proton.me", "you@proton.me"]);
        assert_eq!(m.auth_results.len(), 4);
        assert_eq!(
            m.auth_results[0],
            "mail.protonmail.ch; dmarc=pass (p=reject dis=none) header.from=newsletter.example"
        );
        assert!(m.auth_results[1].contains("spf=pass"));
        assert_eq!(m.text.trim(), "hello");
    }

    #[test]
    fn threading_attachments_and_automatic_headers() {
        let raw = b"From: Me <me@proton.me>\r\nTo: a@x.com, B <B@y.com>\r\nCc: c@z.com\r\n\
            Message-ID: <abc@proton.me>\r\nIn-Reply-To: <root@proton.me>\r\n\
            References: <root@proton.me> <mid@proton.me>\r\nX-Pm-Internal-Id: AbC-_=\r\n\
            Auto-Submitted: auto-replied\r\nPrecedence: bulk\r\nSubject: hel\x01lo\r\n\
            Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\n\
            body here\r\n--b\r\nContent-Type: application/pdf\r\n\
            Content-Disposition: attachment; filename=x.pdf\r\n\r\nAAAA\r\n--b--\r\n";
        let m = parse(1, raw).unwrap();
        assert_eq!(m.to, vec!["a@x.com", "b@y.com"]);
        assert_eq!(m.cc, vec!["c@z.com"]);
        assert_eq!(m.message_id.as_deref(), Some("abc@proton.me"));
        assert_eq!(m.in_reply_to.as_deref(), Some("root@proton.me"));
        assert_eq!(m.references, vec!["root@proton.me", "mid@proton.me"]);
        assert_eq!(m.internal_id.as_deref(), Some("AbC-_="));
        assert_eq!(m.auto_submitted.as_deref(), Some("auto-replied"));
        assert_eq!(m.precedence.as_deref(), Some("bulk"));
        assert_eq!(m.subject, "hel lo");
        assert_eq!(m.text, "body here");
        assert_eq!(m.attachments, vec!["x.pdf"]);
        assert_eq!(m.thread_root(), "root@proton.me");
    }

    #[test]
    fn html_only_mail_becomes_text() {
        let raw = b"From: a@x.com\r\nContent-Type: text/html\r\n\r\n<p>Hello <b>you</b></p>";
        assert_eq!(parse(1, raw).unwrap().text.trim(), "Hello you");
    }

    #[test]
    fn thread_root_falls_back_to_own_id_then_uid() {
        let mut m = MailMessage { uid: 9, ..Default::default() };
        assert_eq!(m.thread_root(), "uid:9");
        m.message_id = Some("own@x".into());
        assert_eq!(m.thread_root(), "own@x");
        m.in_reply_to = Some("parent@x".into());
        assert_eq!(m.thread_root(), "parent@x");
    }

    #[test]
    fn quoted_history_is_dropped() {
        let t = "Thanks, and one more thing.\n\nOn Thu, Oct 1, 2026 at 12:30, Bot <b@x> wrote:\n> old\n> stuff";
        assert_eq!(without_quoted_history(t), "Thanks, and one more thing.");
        let t = "new\n> quoted inline\nmore\n------- Original Message -------\nold";
        assert_eq!(without_quoted_history(t), "new\nmore");
    }

    #[test]
    fn cap_cuts_on_a_char_boundary() {
        assert_eq!(cap("short", 10), ("short".to_string(), false));
        let (s, cut) = cap("ééé", 3);
        assert!(cut);
        assert_eq!(s, "é");
    }
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p aivyx-proton message::`
Expected: FAIL to compile — the items the tests use don't exist yet.

- [ ] **Step 3: Write the module** — put this above the test module in `crates/aivyx-proton/src/message.rs`:

```rust
//! One received email, parsed once (`mail-parser`) into the fields the
//! trust policy, the channel and the tools read. No IMAP type crosses
//! this boundary.

use mail_parser::{Address, MessageParser, MimeHeaders};

/// The most of an email's text a turn reads (the reply says when it cut).
pub const TURN_TEXT_CAP: usize = 32 * 1024;
/// The most of an email's text `proton.read` returns.
pub const TOOL_TEXT_CAP: usize = 32 * 1024;
/// The most text kept from any one message in memory.
const KEPT_TEXT_CAP: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MailMessage {
    /// The UID in the mailbox it was fetched from.
    pub uid: u32,
    /// `X-Pm-Internal-Id`: Proton's own message ID. Bridge always sets it,
    /// overwriting any copy the sender wrote.
    pub internal_id: Option<String>,
    /// `Message-ID`, without angle brackets.
    pub message_id: Option<String>,
    /// `In-Reply-To`, without angle brackets.
    pub in_reply_to: Option<String>,
    /// `References`, oldest first, without angle brackets.
    pub references: Vec<String>,
    /// Control characters replaced by spaces.
    pub subject: String,
    /// The From address, lowercased.
    pub from: Option<String>,
    pub from_name: Option<String>,
    /// Lowercased addresses.
    pub to: Vec<String>,
    pub cc: Vec<String>,
    /// Every `Delivered-To` and `X-Original-To` value, lowercased — set by
    /// Proton's receiving server for the actual delivery.
    pub delivered_to: Vec<String>,
    /// `Authentication-Results` values in header order, folding collapsed.
    pub auth_results: Vec<String>,
    pub auto_submitted: Option<String>,
    pub precedence: Option<String>,
    /// RFC 3339.
    pub date: Option<String>,
    /// The `text/plain` part, else the HTML converted to text (mail-parser
    /// does both). At most 256 KiB.
    pub text: String,
    /// Attachment file names; content is never read.
    pub attachments: Vec<String>,
}

impl MailMessage {
    /// The conversation this email belongs to: the thread's first
    /// `References` entry, else `In-Reply-To`, else its own `Message-ID`.
    pub fn thread_root(&self) -> String {
        self.references
            .first()
            .cloned()
            .or_else(|| self.in_reply_to.clone())
            .or_else(|| self.message_id.clone())
            .unwrap_or_else(|| format!("uid:{}", self.uid))
    }
}

/// Parses a full RFC 5322 message. `None` when it isn't one.
pub fn parse(uid: u32, raw: &[u8]) -> Option<MailMessage> {
    let m = MessageParser::default().parse(raw)?;
    let values = |name: &str| -> Vec<String> {
        m.header_values(name)
            .filter_map(|v| v.as_text())
            .map(collapse)
            .collect()
    };
    let first = |name: &str| values(name).into_iter().next();
    let addresses = |a: Option<&Address>| -> Vec<String> {
        a.map(|a| {
            a.iter()
                .filter_map(|x| x.address())
                .map(str::to_ascii_lowercase)
                .collect()
        })
        .unwrap_or_default()
    };
    let from = m.from().and_then(|a| a.first());
    let mut delivered_to: Vec<String> = values("Delivered-To");
    delivered_to.extend(values("X-Original-To"));
    let delivered_to = delivered_to
        .iter()
        .map(|a| a.trim_matches(|c| c == '<' || c == '>').to_ascii_lowercase())
        .collect();
    let (text, _) = cap(
        &m.body_text(0).map(|t| t.into_owned()).unwrap_or_default(),
        KEPT_TEXT_CAP,
    );
    Some(MailMessage {
        uid,
        internal_id: first("X-Pm-Internal-Id"),
        message_id: m.message_id().map(str::to_string),
        in_reply_to: m
            .in_reply_to()
            .as_text_list()
            .and_then(|l| l.first().map(|s| s.to_string())),
        references: m
            .references()
            .as_text_list()
            .map(|l| l.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default(),
        subject: m.subject().map(no_control).unwrap_or_default(),
        from: from.and_then(|a| a.address()).map(str::to_ascii_lowercase),
        from_name: from.and_then(|a| a.name()).map(no_control),
        to: addresses(m.to()),
        cc: addresses(m.cc()),
        delivered_to,
        auth_results: values("Authentication-Results"),
        auto_submitted: first("Auto-Submitted"),
        precedence: first("Precedence"),
        date: m.date().map(|d| d.to_rfc3339()),
        text,
        attachments: m
            .attachments()
            .map(|a| a.attachment_name().unwrap_or("(unnamed)").to_string())
            .collect(),
    })
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn no_control(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// The new part of a reply: everything before the first quote intro
/// ("On … wrote:", Proton's "------- Original Message -------"), with
/// `>`-quoted lines dropped.
pub fn without_quoted_history(text: &str) -> String {
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if (t.starts_with("On ") && t.ends_with("wrote:"))
            || t.starts_with("------- Original Message -------")
            || t.starts_with("-----Original Message-----")
        {
            break;
        }
        if t.starts_with('>') {
            continue;
        }
        out.push(line);
    }
    out.join("\n").trim().to_string()
}

/// At most `max` bytes of `text`, cut on a character boundary; `true` when
/// something was cut.
pub fn cap(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_string(), false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}
```

- [ ] **Step 4: Run the tests and clippy**

Run: `cargo test -p aivyx-proton message:: && cargo clippy -p aivyx-proton --all-targets -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step: Commit**

```bash
git add crates/aivyx-proton Cargo.toml Cargo.lock
git commit -s -m "feat(proton): aivyx-proton crate; parse received mail" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: The trust policy

**Files:**
- Create: `crates/aivyx-proton/src/policy.rs`
- Modify: `crates/aivyx-proton/src/lib.rs` (add `pub mod policy;`, keeping the list alphabetical)

**Interfaces:**
- Consumes: `message::{MailMessage, parse}`
- Produces: `struct ProtonPolicy { channel_address, own_addresses, allowed_external_senders }` with `ProtonPolicy::new(&str, &[String], &[String])` (lowercases) and `claims_operator(&self, &MailMessage) -> bool`; `enum Sender { Operator, External }`; `enum Decision { Turn(Sender), Ignore(&'static str) }`; `fn decide(&ProtonPolicy, &MailMessage, in_sent: bool) -> Decision`; `fn proton_dmarc_passes(&[String], &str) -> bool`; `const PROTON_AUTHSERV_ID`

- [ ] **Step 1: Write the failing tests** — create `crates/aivyx-proton/src/policy.rs` holding only this test module (the body goes above it in Step 3), and add `pub mod policy;` to `lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const PROTON_DMARC: &str =
        "mail.protonmail.ch; dmarc=pass (p=reject dis=none) header.from=example.com";

    fn policy() -> ProtonPolicy {
        ProtonPolicy::new(
            "Me_Aivyx@proton.me",
            &["me@proton.me".to_string(), "me_aivyx@proton.me".to_string()],
            &["Friend@Example.com".to_string()],
        )
    }

    fn mail(from: &str) -> MailMessage {
        MailMessage {
            uid: 1,
            from: Some(from.into()),
            delivered_to: vec!["me_aivyx@proton.me".into()],
            ..Default::default()
        }
    }

    fn external(auth_results: &[&str]) -> MailMessage {
        MailMessage {
            auth_results: auth_results.iter().map(|s| s.to_string()).collect(),
            ..mail("friend@example.com")
        }
    }

    #[test]
    fn mail_from_you_is_a_turn_only_when_this_account_sent_it() {
        let m = mail("me@proton.me");
        assert!(policy().claims_operator(&m));
        assert_eq!(decide(&policy(), &m, true), Decision::Turn(Sender::Operator));
        // A forged "From: you" from outside is never in your Sent folder.
        assert!(matches!(decide(&policy(), &m, false), Decision::Ignore(_)));
    }

    #[test]
    fn mail_from_you_may_be_addressed_by_to_or_cc() {
        let mut m = MailMessage {
            delivered_to: vec![],
            to: vec!["me_aivyx@proton.me".into()],
            ..mail("me@proton.me")
        };
        assert_eq!(decide(&policy(), &m, true), Decision::Turn(Sender::Operator));
        m.to = vec!["someone@else.com".into()];
        assert!(matches!(decide(&policy(), &m, true), Decision::Ignore(_)));
        m.cc = vec!["me_aivyx@proton.me".into()];
        assert_eq!(decide(&policy(), &m, true), Decision::Turn(Sender::Operator));
    }

    #[test]
    fn an_allowlisted_sender_needs_protons_own_dmarc_pass() {
        let p = policy();
        assert!(!p.claims_operator(&external(&[])));
        assert_eq!(
            decide(&p, &external(&[PROTON_DMARC, "mail.protonmail.ch; spf=pass"]), false),
            Decision::Turn(Sender::External)
        );
        // No Proton result at all.
        assert!(matches!(decide(&p, &external(&[]), false), Decision::Ignore(_)));
        // DMARC fail, or for another domain.
        assert!(matches!(
            decide(&p, &external(&["mail.protonmail.ch; dmarc=fail header.from=example.com"]), false),
            Decision::Ignore(_)
        ));
        assert!(matches!(
            decide(&p, &external(&["mail.protonmail.ch; dmarc=pass header.from=evil.com"]), false),
            Decision::Ignore(_)
        ));
        // A forged pass below Proton's own results is never first.
        assert!(matches!(
            decide(
                &p,
                &external(&[
                    "mail.protonmail.ch; spf=pass smtp.mailfrom=example.com",
                    "mail.protonmail.ch; dmarc=pass header.from=example.com",
                ]),
                false
            ),
            Decision::Ignore(_)
        ));
        // Another server's stamp.
        assert!(matches!(
            decide(&p, &external(&["mx.evil.com; dmarc=pass header.from=example.com"]), false),
            Decision::Ignore(_)
        ));
    }

    #[test]
    fn external_mail_must_be_delivered_to_the_channel_address() {
        let mut m = external(&[PROTON_DMARC]);
        m.delivered_to = vec!["me@proton.me".into()];
        m.to = vec!["me_aivyx@proton.me".into()];
        assert!(matches!(decide(&policy(), &m, false), Decision::Ignore(_)));
    }

    #[test]
    fn strangers_loops_and_automatic_mail_are_ignored() {
        let p = policy();
        assert!(matches!(decide(&p, &mail("stranger@x.com"), false), Decision::Ignore(_)));
        // The assistant's own replies (from the channel address).
        let own = mail("me_aivyx@proton.me");
        assert!(!p.claims_operator(&own));
        assert!(matches!(decide(&p, &own, true), Decision::Ignore(_)));
        let mut m = mail("me@proton.me");
        m.auto_submitted = Some("auto-replied".into());
        assert!(matches!(decide(&p, &m, true), Decision::Ignore(_)));
        m.auto_submitted = Some("no".into());
        assert_eq!(decide(&p, &m, true), Decision::Turn(Sender::Operator));
        m.precedence = Some("List".into());
        assert!(matches!(decide(&p, &m, true), Decision::Ignore(_)));
        let no_from = MailMessage { from: None, ..mail("x@y") };
        assert!(matches!(decide(&p, &no_from, true), Decision::Ignore(_)));
    }

    #[test]
    fn the_recorded_proton_headers_pass_for_their_domain() {
        let raw = include_bytes!("../tests/fixtures/proton-external-dmarc-pass.headers");
        let mut full = raw.to_vec();
        full.extend_from_slice(b"From: x@newsletter.example\r\n\r\nhi\r\n");
        let m = crate::message::parse(1, &full).unwrap();
        assert!(proton_dmarc_passes(&m.auth_results, "newsletter.example"));
        assert!(proton_dmarc_passes(&m.auth_results, "NewsLetter.example"));
        assert!(!proton_dmarc_passes(&m.auth_results, "bounce.newsletter.example"));
    }
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p aivyx-proton policy::`
Expected: FAIL to compile — the items the tests use don't exist yet.

- [ ] **Step 3: Write the module** — put this above the test module in `crates/aivyx-proton/src/policy.rs`:

```rust
//! Which emails become turns. Pure, so every case is a unit test.
//!
//! A message is a turn only when it is addressed to `channel_address` and
//! is either **from you** — From is one of `own_addresses` and the same
//! Proton message is in your Sent folder — or **allowlisted** — From is in
//! `allowed_external_senders` and Proton's own DMARC result passes for its
//! domain. Everything else is ignored silently (no auto-reply: that would
//! confirm the address and send backscatter to forged senders). See
//! `docs/superpowers/specs/2026-10-12-proton-mail-design.md` § Trust.

use crate::message::MailMessage;

/// The authserv-id Proton's receiving server stamps on its own
/// `Authentication-Results` values.
pub const PROTON_AUTHSERV_ID: &str = "mail.protonmail.ch";

/// All addresses lowercased (by [`ProtonPolicy::new`]); `MailMessage`
/// addresses are lowercased by the parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtonPolicy {
    pub channel_address: String,
    pub own_addresses: Vec<String>,
    pub allowed_external_senders: Vec<String>,
}

impl ProtonPolicy {
    pub fn new(channel_address: &str, own: &[String], external: &[String]) -> Self {
        let lower = |v: &[String]| v.iter().map(|a| a.trim().to_ascii_lowercase()).collect();
        ProtonPolicy {
            channel_address: channel_address.trim().to_ascii_lowercase(),
            own_addresses: lower(own),
            allowed_external_senders: lower(external),
        }
    }

    /// From is one of your addresses: the caller must look the message up
    /// in Sent (by `X-Pm-Internal-Id`) before calling [`decide`].
    pub fn claims_operator(&self, m: &MailMessage) -> bool {
        m.from
            .as_deref()
            .is_some_and(|f| f != self.channel_address && self.own_addresses.iter().any(|a| a == f))
    }
}

/// Who a turn came from. Both run at `SemiTrusted`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sender {
    Operator,
    External,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Turn(Sender),
    /// Why, for the debug log only — nothing is ever sent back.
    Ignore(&'static str),
}

/// `in_sent`: the message's `X-Pm-Internal-Id` is in the Sent folder
/// (only consulted when From is one of `own_addresses`).
pub fn decide(p: &ProtonPolicy, m: &MailMessage, in_sent: bool) -> Decision {
    let Some(from) = m.from.as_deref() else {
        return Decision::Ignore("no From address");
    };
    if from == p.channel_address {
        return Decision::Ignore("from the channel address");
    }
    if is_automatic(m) {
        return Decision::Ignore("automatic mail");
    }
    let delivered = m.delivered_to.iter().any(|a| a == &p.channel_address);
    if p.own_addresses.iter().any(|a| a == from) {
        if !in_sent {
            return Decision::Ignore("From is one of yours, but this account didn't send it");
        }
        // Proven yours by Sent membership, so the To/Cc you wrote count:
        // mail between two addresses of one account may carry no
        // Delivered-To.
        let addressed = delivered || m.to.iter().chain(&m.cc).any(|a| a == &p.channel_address);
        return if addressed {
            Decision::Turn(Sender::Operator)
        } else {
            Decision::Ignore("not addressed to the channel")
        };
    }
    if !delivered {
        return Decision::Ignore("not delivered to the channel address");
    }
    if !p.allowed_external_senders.iter().any(|a| a == from) {
        return Decision::Ignore("sender not allowed");
    }
    let domain = from.rsplit_once('@').map(|(_, d)| d).unwrap_or("");
    if proton_dmarc_passes(&m.auth_results, domain) {
        Decision::Turn(Sender::External)
    } else {
        Decision::Ignore("no Proton DMARC pass for the From domain")
    }
}

/// `Auto-Submitted` other than `no`, or `Precedence: bulk/list/junk`.
fn is_automatic(m: &MailMessage) -> bool {
    m.auto_submitted
        .as_deref()
        .is_some_and(|v| !v.trim().eq_ignore_ascii_case("no"))
        || m.precedence.as_deref().is_some_and(|v| {
            matches!(v.trim().to_ascii_lowercase().as_str(), "bulk" | "list" | "junk")
        })
}

/// The **first** `Authentication-Results` value must be stamped
/// `mail.protonmail.ch` and be `dmarc=pass` with `header.from` equal to
/// `from_domain`. Proton adds its own values, DMARC first, ahead of
/// anything the sender wrote, so a forged value is never first.
pub fn proton_dmarc_passes(auth_results: &[String], from_domain: &str) -> bool {
    let Some(first) = auth_results.first() else {
        return false;
    };
    let mut parts = first.split(';');
    let authserv = parts.next().unwrap_or("");
    // An authserv-id may be followed by a version number.
    if !authserv
        .split_whitespace()
        .next()
        .is_some_and(|id| id.eq_ignore_ascii_case(PROTON_AUTHSERV_ID))
    {
        return false;
    }
    let Some(result) = parts.next() else {
        return false;
    };
    let result = result.trim().to_ascii_lowercase();
    let Some(rest) = result.strip_prefix("dmarc=") else {
        return false;
    };
    let verdict = rest
        .split(|c: char| c.is_whitespace() || c == '(')
        .next()
        .unwrap_or("");
    verdict == "pass"
        && result
            .split_whitespace()
            .find_map(|t| t.strip_prefix("header.from="))
            .is_some_and(|d| !from_domain.is_empty() && d == from_domain.to_ascii_lowercase())
}
```

- [ ] **Step 4: Run the tests and clippy**

Run: `cargo test -p aivyx-proton policy:: && cargo clippy -p aivyx-proton --all-targets -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step: Commit**

```bash
git add crates/aivyx-proton/src/policy.rs crates/aivyx-proton/src/lib.rs
git commit -s -m "feat(proton): the email trust policy — Sent-folder proof for you, Proton's DMARC for allowlisted senders" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Outgoing mail

**Files:**
- Create: `crates/aivyx-proton/src/outgoing.rs`
- Modify: `crates/aivyx-proton/src/lib.rs` (add `pub mod outgoing;`, keeping the list alphabetical)

**Interfaces:**
- Consumes: `message::{MailMessage, parse}`
- Produces: `struct OutgoingMail { from, to, cc, subject, body, in_reply_to, references, message_id }`; `OutgoingMail::new(from: &str, to: Vec<String>, cc: Vec<String>, subject: &str, body: String)`, `OutgoingMail::reply(from: &str, to: Vec<String>, original: &MailMessage, body: String)`, `recipients(&self) -> Vec<String>`, `to_rfc5322(&self) -> Result<Vec<u8>, String>`; `fn re_subject(&str) -> String`

- [ ] **Step 1: Write the failing tests** — create `crates/aivyx-proton/src/outgoing.rs` holding only this test module (the body goes above it in Step 3), and add `pub mod outgoing;` to `lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn original() -> MailMessage {
        MailMessage {
            uid: 3,
            subject: "Lunch?".into(),
            from: Some("me@proton.me".into()),
            message_id: Some("m2@proton.me".into()),
            references: vec!["m1@proton.me".into()],
            ..Default::default()
        }
    }

    #[test]
    fn a_reply_is_threaded_and_prefixed_once() {
        let r = OutgoingMail::reply("bot@proton.me", vec!["me@proton.me".into()], &original(), "Sure.".into());
        assert_eq!(r.subject, "Re: Lunch?");
        assert_eq!(r.in_reply_to.as_deref(), Some("m2@proton.me"));
        assert_eq!(r.references, vec!["m1@proton.me", "m2@proton.me"]);
        assert!(r.message_id.ends_with("@proton.me"));
        assert_eq!(re_subject("RE: x"), "RE: x");
        assert_eq!(re_subject("é"), "Re: é");
    }

    #[test]
    fn rendering_carries_the_threading_headers() {
        let r = OutgoingMail::reply("bot@proton.me", vec!["me@proton.me".into()], &original(), "Sure.".into());
        let raw = String::from_utf8(r.to_rfc5322().unwrap()).unwrap();
        assert!(raw.contains("In-Reply-To: <m2@proton.me>\r\n"), "{raw}");
        assert!(raw.contains("References: <m1@proton.me> <m2@proton.me>\r\n"), "{raw}");
        assert!(raw.contains(&format!("Message-ID: <{}>\r\n", r.message_id)), "{raw}");
        assert!(raw.contains("Subject: Re: Lunch?\r\n"), "{raw}");
        assert!(raw.ends_with("Sure."), "{raw}");
        // And it parses back.
        let back = crate::message::parse(1, raw.as_bytes()).unwrap();
        assert_eq!(back.in_reply_to.as_deref(), Some("m2@proton.me"));
        assert_eq!(back.from.as_deref(), Some("bot@proton.me"));
    }

    #[test]
    fn a_subject_cannot_inject_headers_and_bad_addresses_fail() {
        let m = OutgoingMail::new("bot@proton.me", vec!["a@x.com".into()], vec![], "hi\r\nBcc: evil@x.com", "b".into());
        assert_eq!(m.subject, "hi  Bcc: evil@x.com");
        let raw = String::from_utf8(m.to_rfc5322().unwrap()).unwrap();
        assert!(!raw.contains("\r\nBcc:"), "{raw}");
        let bad = OutgoingMail::new("bot@proton.me", vec!["not an address".into()], vec![], "s", "b".into());
        assert!(bad.to_rfc5322().is_err());
        assert_eq!(m.recipients(), vec!["a@x.com"]);
    }
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p aivyx-proton outgoing::`
Expected: FAIL to compile — the items the tests use don't exist yet.

- [ ] **Step 3: Write the module** — put this above the test module in `crates/aivyx-proton/src/outgoing.rs`:

```rust
//! Mail the assistant writes: the channel's replies and the tools' drafts
//! and sends. Rendered to RFC 5322 bytes with `lettre`'s message builder
//! (CRLF, encoded headers); sent by our own SMTP client (`smtp`).

use lettre::message::header::ContentType;
use lettre::message::Mailbox;

use crate::message::MailMessage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingMail {
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    pub body: String,
    /// Without angle brackets.
    pub in_reply_to: Option<String>,
    /// Without angle brackets, oldest first.
    pub references: Vec<String>,
    /// Without angle brackets.
    pub message_id: String,
}

impl OutgoingMail {
    pub fn new(from: &str, to: Vec<String>, cc: Vec<String>, subject: &str, body: String) -> Self {
        OutgoingMail {
            from: from.to_string(),
            to,
            cc,
            subject: one_line(subject),
            body,
            in_reply_to: None,
            references: Vec::new(),
            message_id: new_message_id(from),
        }
    }

    /// A reply to `original`, threaded (`In-Reply-To` / `References`) so it
    /// lands in the same conversation in Proton Mail.
    pub fn reply(from: &str, to: Vec<String>, original: &MailMessage, body: String) -> Self {
        let mut mail = OutgoingMail::new(from, to, Vec::new(), &re_subject(&original.subject), body);
        mail.in_reply_to = original.message_id.clone();
        mail.references = original.references.clone();
        if let Some(id) = &original.message_id {
            mail.references.push(id.clone());
        }
        mail
    }

    /// Everyone the message goes to (SMTP `RCPT TO`).
    pub fn recipients(&self) -> Vec<String> {
        self.to.iter().chain(&self.cc).cloned().collect()
    }

    pub fn to_rfc5322(&self) -> Result<Vec<u8>, String> {
        let mailbox = |a: &str| a.parse::<Mailbox>().map_err(|e| format!("bad address {a:?}: {e}"));
        let mut b = lettre::Message::builder()
            .from(mailbox(&self.from)?)
            .subject(self.subject.clone())
            .message_id(Some(format!("<{}>", self.message_id)));
        for a in &self.to {
            b = b.to(mailbox(a)?);
        }
        for a in &self.cc {
            b = b.cc(mailbox(a)?);
        }
        if let Some(id) = &self.in_reply_to {
            b = b.in_reply_to(format!("<{id}>"));
        }
        if !self.references.is_empty() {
            let refs: Vec<String> = self.references.iter().map(|r| format!("<{r}>")).collect();
            b = b.references(refs.join(" "));
        }
        let message = b
            .header(ContentType::TEXT_PLAIN)
            .body(self.body.clone())
            .map_err(|e| format!("build message: {e}"))?;
        Ok(message.formatted())
    }
}

/// `Re: <subject>`, without stacking `Re: Re:`.
pub fn re_subject(subject: &str) -> String {
    let s = subject.trim();
    if s.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("re:")) {
        s.to_string()
    } else {
        format!("Re: {s}")
    }
}

fn one_line(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

fn new_message_id(from: &str) -> String {
    let domain = from.rsplit_once('@').map(|(_, d)| d).unwrap_or("localhost");
    format!("{}@{domain}", uuid::Uuid::new_v4().simple())
}
```

- [ ] **Step 4: Run the tests and clippy**

Run: `cargo test -p aivyx-proton outgoing:: && cargo clippy -p aivyx-proton --all-targets -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step: Commit**

```bash
git add crates/aivyx-proton/src/outgoing.rs crates/aivyx-proton/src/lib.rs
git commit -s -m "feat(proton): outgoing mail — threaded replies rendered with lettre's builder" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: The transport seam

**Files:**
- Create: `crates/aivyx-proton/src/transport.rs`
- Modify: `crates/aivyx-proton/src/lib.rs` (add `pub mod transport;`, keeping the list alphabetical)

**Interfaces:**
- Consumes: `message::MailMessage`, `outgoing::OutgoingMail`
- Produces: `enum TransportError { Platform(String), Unauthorized(String), CertChanged }` + `is_fatal()`; `struct InboxStatus { uid_validity: u32, uid_next: u32 }`; `struct SearchQuery { from, to, subject, text, since, before: Option<String>, max_results: usize }` + `to_imap()`; `fn quote`, `fn imap_date`; `trait MailTransport` (`inbox_status`, `wait_for_new(after_uid)`, `in_sent(internal_id)`, `send(&OutgoingMail)`, `file_handled(uid, &OutgoingMail)`, `search(&SearchQuery)`, `fetch(uid)`, `append_draft(&OutgoingMail)`); `ScriptedTransport` with `new(InboxStatus, Vec<Vec<MailMessage>>)`, `in_sent_folder`, `with_all_mail`, `failing_send`, `then_fail`, and recorders `sent()`, `filed()`, `drafts()`, `searches()`

- [ ] **Step 1: Write the failing tests** — create `crates/aivyx-proton/src/transport.rs` holding only this test module (the body goes above it in Step 3), and add `pub mod transport;` to `lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn m(uid: u32) -> MailMessage {
        MailMessage { uid, ..Default::default() }
    }

    #[tokio::test]
    async fn batches_are_filtered_by_uid_and_writes_recorded() {
        let t = ScriptedTransport::new(InboxStatus::default(), vec![vec![m(3), m(5)]]);
        assert_eq!(t.wait_for_new(3).await.unwrap(), vec![m(5)]);
        assert!(t.wait_for_new(0).await.unwrap().is_empty());
        let mail = OutgoingMail::new("a@x.com", vec!["b@y.com".into()], vec![], "s", "b".into());
        t.send(&mail).await.unwrap();
        t.file_handled(5, &mail).await.unwrap();
        assert_eq!(t.sent().await.len(), 1);
        assert_eq!(t.filed().await[0].0, 5);
    }

    #[test]
    fn only_a_bad_login_and_a_changed_certificate_are_fatal() {
        assert!(TransportError::Unauthorized("x".into()).is_fatal());
        assert!(TransportError::CertChanged.is_fatal());
        assert!(!TransportError::Platform("x".into()).is_fatal());
        assert!(TransportError::CertChanged.to_string().contains("connect proton"));
        assert!(TransportError::Unauthorized("x".into()).to_string().contains("connect proton"));
    }

    #[test]
    fn search_criteria_are_quoted_and_dated() {
        let q = SearchQuery {
            from: Some("ann@x.com".into()),
            subject: Some("say \"hi\" \\o/".into()),
            since: Some("2026-10-01".into()),
            before: Some("2026-12-31".into()),
            ..Default::default()
        };
        assert_eq!(
            q.to_imap().unwrap(),
            "FROM \"ann@x.com\" SUBJECT \"say \\\"hi\\\" \\\\o/\" SINCE 1-Oct-2026 BEFORE 31-Dec-2026"
        );
        assert_eq!(SearchQuery::default().to_imap().unwrap(), "ALL");
        let inject = SearchQuery { text: Some("x\r\nA DELETE INBOX".into()), ..Default::default() };
        assert!(inject.to_imap().is_err());
        assert!(imap_date("2026-13-01").is_err());
        assert!(imap_date("01/10/2026").is_err());
    }
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p aivyx-proton transport::`
Expected: FAIL to compile — the items the tests use don't exist yet.

- [ ] **Step 3: Write the module** — put this above the test module in `crates/aivyx-proton/src/transport.rs`:

```rust
//! The seam between the channel/tools and the mailbox. Production is
//! `bridge::BridgeTransport` (IMAP + SMTP to Proton Mail Bridge); tests
//! use [`ScriptedTransport`]. No IMAP or SMTP type crosses this boundary.

use std::collections::VecDeque;
use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;
use tokio::sync::Mutex;

use crate::message::MailMessage;
use crate::outgoing::OutgoingMail;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("Proton Mail Bridge: {0}")]
    Platform(String),
    /// Bridge rejected the stored login. Retrying can't help.
    #[error("Proton Mail Bridge rejected the login ({0}) — run `aivyx-pa connect proton` again")]
    Unauthorized(String),
    /// Bridge presented a certificate other than the pinned one.
    #[error(
        "Proton Mail Bridge's certificate changed — if you reinstalled or reset Bridge, \
         run `aivyx-pa connect proton` again"
    )]
    CertChanged,
}

impl TransportError {
    /// Ends the channel: no retry or backoff can recover from it.
    pub fn is_fatal(&self) -> bool {
        matches!(self, TransportError::Unauthorized(_) | TransportError::CertChanged)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InboxStatus {
    pub uid_validity: u32,
    /// The UID the next message will get.
    pub uid_next: u32,
}

/// `proton.search` criteria; all given ones must match.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchQuery {
    pub from: Option<String>,
    pub to: Option<String>,
    pub subject: Option<String>,
    pub text: Option<String>,
    /// `YYYY-MM-DD`, inclusive.
    pub since: Option<String>,
    /// `YYYY-MM-DD`, exclusive.
    pub before: Option<String>,
    pub max_results: usize,
}

impl SearchQuery {
    /// IMAP `SEARCH` criteria. Values are quoted; CR, LF and NUL refused.
    pub fn to_imap(&self) -> Result<String, String> {
        let mut parts = Vec::new();
        for (key, value) in [
            ("FROM", &self.from),
            ("TO", &self.to),
            ("SUBJECT", &self.subject),
            ("TEXT", &self.text),
        ] {
            if let Some(v) = value {
                parts.push(format!("{key} {}", quote(v)?));
            }
        }
        if let Some(d) = &self.since {
            parts.push(format!("SINCE {}", imap_date(d)?));
        }
        if let Some(d) = &self.before {
            parts.push(format!("BEFORE {}", imap_date(d)?));
        }
        Ok(if parts.is_empty() { "ALL".into() } else { parts.join(" ") })
    }
}

/// An IMAP quoted string.
pub fn quote(s: &str) -> Result<String, String> {
    if s.contains(['\r', '\n', '\0']) {
        return Err("search text can't contain line breaks".into());
    }
    Ok(format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")))
}

/// `2026-10-01` → `1-Oct-2026`.
pub fn imap_date(ymd: &str) -> Result<String, String> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let bad = || format!("dates are YYYY-MM-DD, got {ymd:?}");
    let mut it = ymd.trim().splitn(3, '-');
    let (y, m, d) = (it.next(), it.next(), it.next());
    let y: u32 = y.and_then(|v| v.parse().ok()).filter(|v| (1970..=9999).contains(v)).ok_or_else(bad)?;
    let m: usize = m.and_then(|v| v.parse().ok()).filter(|v| (1..=12).contains(v)).ok_or_else(bad)?;
    let d: u32 = d.and_then(|v| v.parse().ok()).filter(|v| (1..=31).contains(v)).ok_or_else(bad)?;
    Ok(format!("{d}-{}-{y}", MONTHS[m - 1]))
}

#[async_trait]
pub trait MailTransport: Send + Sync {
    /// INBOX's UIDVALIDITY and next UID.
    async fn inbox_status(&self) -> Result<InboxStatus, TransportError>;
    /// INBOX messages with a UID above `after_uid`, oldest first (at most
    /// 20). Waits (IDLE, or a 60 s poll) when there are none; may return
    /// an empty batch.
    async fn wait_for_new(&self, after_uid: u32) -> Result<Vec<MailMessage>, TransportError>;
    /// Whether the Proton message with this `X-Pm-Internal-Id` is also in
    /// the Sent folder — i.e. this account sent it.
    async fn in_sent(&self, internal_id: &str) -> Result<bool, TransportError>;
    async fn send(&self, mail: &OutgoingMail) -> Result<(), TransportError>;
    /// Marks INBOX message `uid` read and moves it, with a copy of
    /// `reply`, to the handled-mail folder.
    async fn file_handled(&self, uid: u32, reply: &OutgoingMail) -> Result<(), TransportError>;
    /// All Mail, newest first, at most `q.max_results`.
    async fn search(&self, q: &SearchQuery) -> Result<Vec<MailMessage>, TransportError>;
    /// One All Mail message by UID.
    async fn fetch(&self, uid: u32) -> Result<Option<MailMessage>, TransportError>;
    async fn append_draft(&self, mail: &OutgoingMail) -> Result<(), TransportError>;
}

/// Test double: pops queued INBOX batches, records everything written.
pub struct ScriptedTransport {
    status: InboxStatus,
    batches: Mutex<VecDeque<Vec<MailMessage>>>,
    sent_ids: Vec<String>,
    all_mail: Vec<MailMessage>,
    fail_send: bool,
    fail_when_empty: Option<fn() -> TransportError>,
    sent: Mutex<Vec<OutgoingMail>>,
    filed: Mutex<Vec<(u32, OutgoingMail)>>,
    drafts: Mutex<Vec<OutgoingMail>>,
    searches: Mutex<Vec<SearchQuery>>,
}

impl ScriptedTransport {
    pub fn new(status: InboxStatus, batches: Vec<Vec<MailMessage>>) -> Self {
        ScriptedTransport {
            status,
            batches: Mutex::new(batches.into()),
            sent_ids: Vec::new(),
            all_mail: Vec::new(),
            fail_send: false,
            fail_when_empty: None,
            sent: Mutex::new(Vec::new()),
            filed: Mutex::new(Vec::new()),
            drafts: Mutex::new(Vec::new()),
            searches: Mutex::new(Vec::new()),
        }
    }

    /// `X-Pm-Internal-Id`s `in_sent` reports as in Sent.
    pub fn in_sent_folder(mut self, ids: Vec<&str>) -> Self {
        self.sent_ids = ids.into_iter().map(str::to_string).collect();
        self
    }

    /// What `search` and `fetch` see.
    pub fn with_all_mail(mut self, mail: Vec<MailMessage>) -> Self {
        self.all_mail = mail;
        self
    }

    pub fn failing_send(mut self) -> Self {
        self.fail_send = true;
        self
    }

    /// Once the batches run out, `wait_for_new` returns `Err(err())`.
    pub fn then_fail(mut self, err: fn() -> TransportError) -> Self {
        self.fail_when_empty = Some(err);
        self
    }

    pub async fn sent(&self) -> Vec<OutgoingMail> {
        self.sent.lock().await.clone()
    }
    pub async fn filed(&self) -> Vec<(u32, OutgoingMail)> {
        self.filed.lock().await.clone()
    }
    pub async fn drafts(&self) -> Vec<OutgoingMail> {
        self.drafts.lock().await.clone()
    }
    pub async fn searches(&self) -> Vec<SearchQuery> {
        self.searches.lock().await.clone()
    }
}

#[async_trait]
impl MailTransport for ScriptedTransport {
    async fn inbox_status(&self) -> Result<InboxStatus, TransportError> {
        Ok(self.status)
    }

    async fn wait_for_new(&self, after_uid: u32) -> Result<Vec<MailMessage>, TransportError> {
        if let Some(batch) = self.batches.lock().await.pop_front() {
            return Ok(batch.into_iter().filter(|m| m.uid > after_uid).collect());
        }
        if let Some(err) = self.fail_when_empty {
            return Err(err());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
        Ok(Vec::new())
    }

    async fn in_sent(&self, internal_id: &str) -> Result<bool, TransportError> {
        Ok(self.sent_ids.iter().any(|id| id == internal_id))
    }

    async fn send(&self, mail: &OutgoingMail) -> Result<(), TransportError> {
        if self.fail_send {
            return Err(TransportError::Platform("smtp down".into()));
        }
        self.sent.lock().await.push(mail.clone());
        Ok(())
    }

    async fn file_handled(&self, uid: u32, reply: &OutgoingMail) -> Result<(), TransportError> {
        self.filed.lock().await.push((uid, reply.clone()));
        Ok(())
    }

    async fn search(&self, q: &SearchQuery) -> Result<Vec<MailMessage>, TransportError> {
        self.searches.lock().await.push(q.clone());
        let mut hits = self.all_mail.clone();
        hits.sort_by_key(|m| std::cmp::Reverse(m.uid));
        hits.truncate(q.max_results);
        Ok(hits)
    }

    async fn fetch(&self, uid: u32) -> Result<Option<MailMessage>, TransportError> {
        Ok(self.all_mail.iter().find(|m| m.uid == uid).cloned())
    }

    async fn append_draft(&self, mail: &OutgoingMail) -> Result<(), TransportError> {
        self.drafts.lock().await.push(mail.clone());
        Ok(())
    }
}
```

- [ ] **Step 4: Run the tests and clippy**

Run: `cargo test -p aivyx-proton transport:: && cargo clippy -p aivyx-proton --all-targets -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step: Commit**

```bash
git add crates/aivyx-proton/src/transport.rs crates/aivyx-proton/src/lib.rs
git commit -s -m "feat(proton): MailTransport seam and scripted double" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: Pinned TLS

- [ ] **Step 0: Make the test certificate** — a self-signed CA certificate for `127.0.0.1`, like Bridge's (this is what makes webpki refuse it as a server certificate, and why the pin exists):

```bash
cd crates/aivyx-proton/tests/fixtures
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 36500 \
  -subj "/CN=127.0.0.1" -addext "subjectAltName=IP:127.0.0.1" \
  -addext "basicConstraints=critical,CA:TRUE" -keyout key.pem -out cert.pem
openssl x509 -in cert.pem -outform DER -out bridge-test-cert.der
openssl pkcs8 -topk8 -nocrypt -in key.pem -outform DER -out bridge-test-key.der
rm key.pem cert.pem
cd -
```

**Files:**
- Create: `crates/aivyx-proton/src/tls.rs`
- Modify: `crates/aivyx-proton/src/lib.rs` (add `pub mod tls;`, keeping the list alphabetical)
- Create: `crates/aivyx-proton/tests/fixtures/bridge-test-cert.der`, `crates/aivyx-proton/tests/fixtures/bridge-test-key.der`

**Interfaces:**
- Consumes: nothing
- Produces: `type Pin = [u8; 32]`; `fn fingerprint(&[u8]) -> Pin`; `fn pin_to_hex`, `fn pin_from_hex`; `fn pinned(Pin) -> TlsConnector`; `fn capturing() -> (TlsConnector, Arc<Mutex<Option<Vec<u8>>>>)`; `fn server_name(&str) -> Result<ServerName<'static>, String>`; `fn is_pin_mismatch(&io::Error) -> bool`; `const PIN_MISMATCH`

- [ ] **Step 1: Write the failing tests** — create `crates/aivyx-proton/src/tls.rs` holding only this test module (the body goes above it in Step 3), and add `pub mod tls;` to `lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A self-signed CA certificate for `127.0.0.1`, like Bridge's.
    const CERT: &[u8] = include_bytes!("../tests/fixtures/bridge-test-cert.der");
    const KEY: &[u8] = include_bytes!("../tests/fixtures/bridge-test-key.der");

    /// A TLS server on one end of an in-memory pipe; returns the other end.
    fn serve() -> tokio::io::DuplexStream {
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KEY.to_vec()));
        let config = rustls::ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![CertificateDer::from(CERT.to_vec())], key)
            .unwrap();
        let (client, server) = tokio::io::duplex(64 * 1024);
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        tokio::spawn(async move {
            if let Ok(mut tls) = acceptor.accept(server).await {
                let _ = tls.write_all(b"ok").await;
                let _ = tls.flush().await;
                let mut b = [0u8; 1];
                let _ = tls.read(&mut b).await;
            }
        });
        client
    }

    #[tokio::test]
    async fn the_pinned_certificate_connects() {
        let name = server_name("127.0.0.1:1143").unwrap();
        let mut tls = pinned(fingerprint(CERT)).connect(name, serve()).await.unwrap();
        let mut buf = [0u8; 2];
        tls.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ok");
    }

    #[tokio::test]
    async fn any_other_certificate_is_refused() {
        let name = server_name("127.0.0.1:1143").unwrap();
        let err = pinned([0u8; 32]).connect(name, serve()).await.unwrap_err();
        assert!(is_pin_mismatch(&err), "{err}");
    }

    #[tokio::test]
    async fn capture_records_the_certificate() {
        let (c, seen) = capturing();
        c.connect(server_name("localhost:1025").unwrap(), serve()).await.unwrap();
        let der = seen.lock().unwrap().clone().unwrap();
        assert_eq!(fingerprint(&der), fingerprint(CERT));
    }

    #[test]
    fn pins_round_trip_as_hex_and_names_parse() {
        let pin = fingerprint(CERT);
        assert_eq!(pin_from_hex(&pin_to_hex(&pin)).unwrap(), pin);
        assert!(pin_from_hex("abcd").is_err());
        assert!(server_name("[::1]:1143").is_ok());
        assert!(server_name("localhost:1025").is_ok());
    }
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p aivyx-proton tls::`
Expected: FAIL to compile — the items the tests use don't exist yet.

- [ ] **Step 3: Write the module** — put this above the test module in `crates/aivyx-proton/src/tls.rs`:

```rust
//! TLS to Proton Mail Bridge, pinned to the certificate captured at
//! `aivyx-pa connect proton`.
//!
//! Bridge serves a self-signed certificate marked as a CA, which webpki
//! refuses as a server certificate (`CaUsedAsEndEntity`) — so it can't be
//! added as a trust root. Instead the SHA-256 of the exact certificate is
//! pinned: any other certificate fails the handshake. The handshake
//! signature is still verified, so the peer must hold the pinned key.
//! Never system trust, never "accept anything" — except [`capturing`],
//! used once by `connect proton` to read the certificate it then pins.

use std::sync::{Arc, Mutex};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};
use tokio_rustls::TlsConnector;

/// In the handshake error when Bridge presents another certificate.
pub const PIN_MISMATCH: &str = "Proton Mail Bridge presented a different certificate";

pub type Pin = [u8; 32];

pub fn fingerprint(der: &[u8]) -> Pin {
    let mut pin = [0u8; 32];
    pin.copy_from_slice(Sha256::digest(der).as_slice());
    pin
}

pub fn pin_to_hex(pin: &Pin) -> String {
    hex::encode(pin)
}

pub fn pin_from_hex(s: &str) -> Result<Pin, String> {
    let bytes = hex::decode(s.trim()).map_err(|e| format!("bad certificate pin: {e}"))?;
    bytes
        .try_into()
        .map_err(|_| "bad certificate pin: expected 32 bytes".to_string())
}

/// The crypto provider the rest of pa uses (both are compiled in, so
/// rustls can't pick a process default on its own).
fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

#[derive(Debug)]
struct Verifier {
    /// `Some`: only this certificate. `None`: capture mode.
    pin: Option<Pin>,
    seen: Arc<Mutex<Option<Vec<u8>>>>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        *self.seen.lock().expect("seen mutex poisoned") = Some(end_entity.as_ref().to_vec());
        match self.pin {
            Some(pin) if fingerprint(end_entity.as_ref()) != pin => {
                Err(rustls::Error::General(PIN_MISMATCH.to_string()))
            }
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

fn connector(pin: Option<Pin>) -> (TlsConnector, Arc<Mutex<Option<Vec<u8>>>>) {
    let provider = provider();
    let seen = Arc::new(Mutex::new(None));
    let verifier = Verifier {
        pin,
        seen: Arc::clone(&seen),
        provider: Arc::clone(&provider),
    };
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("aws-lc-rs supports the default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    (TlsConnector::from(Arc::new(config)), seen)
}

/// Accepts only the certificate whose SHA-256 is `pin`.
pub fn pinned(pin: Pin) -> TlsConnector {
    connector(Some(pin)).0
}

/// Accepts any certificate and records it — only for `connect proton`,
/// which shows the operator the fingerprint and then pins it.
pub fn capturing() -> (TlsConnector, Arc<Mutex<Option<Vec<u8>>>>) {
    connector(None)
}

/// The host part of `host:port` as a TLS server name. The pin is what's
/// checked; the name only has to be well-formed.
pub fn server_name(addr: &str) -> Result<ServerName<'static>, String> {
    let host = match addr.rsplit_once(':') {
        Some((h, _)) => h,
        None => addr,
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    ServerName::try_from(host.to_string()).map_err(|e| format!("bad Bridge address {addr:?}: {e}"))
}

/// True when a connect error is a pin mismatch.
pub fn is_pin_mismatch(e: &std::io::Error) -> bool {
    e.to_string().contains(PIN_MISMATCH)
}
```

- [ ] **Step 4: Run the tests and clippy**

Run: `cargo test -p aivyx-proton tls:: && cargo clippy -p aivyx-proton --all-targets -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step: Commit**

```bash
git add crates/aivyx-proton/src/tls.rs crates/aivyx-proton/src/lib.rs crates/aivyx-proton/tests/fixtures
git commit -s -m "feat(proton): TLS pinned to Bridge's certificate" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: SMTP submission client

**Files:**
- Create: `crates/aivyx-proton/src/smtp.rs`
- Modify: `crates/aivyx-proton/src/lib.rs` (add `pub mod smtp;`, keeping the list alphabetical)

**Interfaces:**
- Consumes: nothing
- Produces: `enum SmtpError { Io, Reply { code, text }, Auth(String), Protocol(String) }`; `struct Conn<S>` with `open(S)` (greeting + EHLO), `resume_after_tls(S)` (EHLO only), `starttls(self) -> Result<S, _>`, `auth_plain(&mut self, user, password)`, `send(&mut self, from, to: &[String], data: &[u8])`, `quit(self)`; `fn dot_stuff(&[u8]) -> Vec<u8>`

- [ ] **Step 1: Write the failing tests** — create `crates/aivyx-proton/src/smtp.rs` holding only this test module (the body goes above it in Step 3), and add `pub mod smtp;` to `lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::DuplexStream;

    /// A scripted server: for each `(line prefix, reply)` it reads client
    /// lines until one starts with the prefix (`"."` must match exactly —
    /// the DATA end marker), then sends the reply. Returns every line read.
    fn server(
        greeting: bool,
        script: Vec<(&'static str, &'static str)>,
    ) -> (DuplexStream, tokio::task::JoinHandle<Vec<String>>) {
        let (client, srv) = tokio::io::duplex(64 * 1024);
        let h = tokio::spawn(async move {
            let mut srv = BufReader::new(srv);
            if greeting {
                srv.get_mut().write_all(b"220 bridge ready\r\n").await.unwrap();
            }
            let mut seen = Vec::new();
            for (expect, reply) in script {
                loop {
                    let mut line = String::new();
                    if srv.read_line(&mut line).await.unwrap() == 0 {
                        return seen;
                    }
                    let line = line.trim_end_matches(['\r', '\n']).to_string();
                    let hit = if expect == "." { line == "." } else { line.starts_with(expect) };
                    seen.push(line);
                    if hit {
                        break;
                    }
                }
                srv.get_mut().write_all(reply.as_bytes()).await.unwrap();
            }
            seen
        });
        (client, h)
    }

    #[tokio::test]
    async fn a_message_is_submitted() {
        let (stream, srv) = server(
            true,
            vec![
                ("EHLO", "250-bridge\r\n250 AUTH PLAIN\r\n"),
                ("AUTH PLAIN", "235 ok\r\n"),
                ("MAIL FROM:<bot@proton.me>", "250 ok\r\n"),
                ("RCPT TO:<me@proton.me>", "250 ok\r\n"),
                ("DATA", "354 go\r\n"),
                (".", "250 queued\r\n"),
                ("QUIT", "221 bye\r\n"),
            ],
        );
        let mut c = Conn::open(stream).await.unwrap();
        c.auth_plain("user", "pw").await.unwrap();
        c.send("bot@proton.me", &["me@proton.me".to_string()], b"Subject: x\r\n\r\n.hidden\r\nhi")
            .await
            .unwrap();
        c.quit().await;
        let seen = srv.await.unwrap();
        let token = base64::engine::general_purpose::STANDARD.encode("\0user\0pw");
        assert!(seen.contains(&format!("AUTH PLAIN {token}")), "{seen:?}");
        let data = seen.iter().position(|l| l == "DATA").unwrap();
        assert_eq!(&seen[data + 1..data + 6], ["Subject: x", "", "..hidden", "hi", "."]);
    }

    #[tokio::test]
    async fn a_rejected_login_is_auth() {
        let (stream, _srv) = server(
            true,
            vec![
                ("EHLO", "250 bridge\r\n"),
                ("AUTH PLAIN", "535 5.7.8 bad credentials\r\n"),
            ],
        );
        let mut c = Conn::open(stream).await.unwrap();
        assert!(matches!(c.auth_plain("u", "p").await, Err(SmtpError::Auth(_))));
    }

    #[tokio::test]
    async fn starttls_refuses_injected_bytes() {
        let (stream, _srv) = server(
            true,
            vec![
                ("EHLO", "250 bridge\r\n"),
                ("STARTTLS", "220 go ahead\r\n250 injected\r\n"),
            ],
        );
        let c = Conn::open(stream).await.unwrap();
        assert!(matches!(c.starttls().await, Err(SmtpError::Protocol(_))));
    }

    #[tokio::test]
    async fn a_refused_recipient_is_an_error() {
        let (stream, _srv) = server(
            false,
            vec![
                ("EHLO", "250 bridge\r\n"),
                ("MAIL FROM", "250 ok\r\n"),
                ("RCPT TO", "550 no such user\r\n"),
            ],
        );
        let mut c = Conn::resume_after_tls(stream).await.unwrap();
        let e = c.send("a@x.com", &["b@y.com".to_string()], b"x").await.unwrap_err();
        assert!(matches!(e, SmtpError::Reply { code: 550, .. }), "{e}");
    }

    #[test]
    fn dot_stuffing_and_bad_paths() {
        assert_eq!(dot_stuff(b".a\r\nb\r\n.c"), b"..a\r\nb\r\n..c\r\n.\r\n".to_vec());
        assert!(path("a b@x").is_err());
        assert!(path("a@x>\r\nRCPT TO:<evil@x").is_err());
        assert!(path("nobody").is_err());
        assert_eq!(path(" a@x ").unwrap(), "a@x");
    }
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p aivyx-proton smtp::`
Expected: FAIL to compile — the items the tests use don't exist yet.

- [ ] **Step 3: Write the module** — put this above the test module in `crates/aivyx-proton/src/smtp.rs`:

```rust
//! A minimal SMTP submission client — just what Bridge needs: EHLO,
//! STARTTLS, AUTH PLAIN, MAIL/RCPT/DATA, QUIT. Our own rather than
//! `lettre`'s transport because the TLS must be pinned (`tls`), which
//! `lettre` can't express. Generic over the stream so the protocol is
//! unit-tested over an in-memory pipe.

use base64::Engine as _;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

#[derive(Debug, Error)]
pub enum SmtpError {
    #[error("smtp i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("smtp server said {code}: {text}")]
    Reply { code: u16, text: String },
    #[error("smtp login rejected: {0}")]
    Auth(String),
    #[error("smtp protocol: {0}")]
    Protocol(String),
}

pub struct Conn<S> {
    stream: BufReader<S>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Conn<S> {
    /// Reads the server greeting and says EHLO.
    pub async fn open(stream: S) -> Result<Self, SmtpError> {
        let mut c = Conn {
            stream: BufReader::new(stream),
        };
        c.expect(220).await?;
        c.ehlo().await?;
        Ok(c)
    }

    /// After STARTTLS there is no new greeting: just EHLO again.
    pub async fn resume_after_tls(stream: S) -> Result<Self, SmtpError> {
        let mut c = Conn {
            stream: BufReader::new(stream),
        };
        c.ehlo().await?;
        Ok(c)
    }

    async fn ehlo(&mut self) -> Result<(), SmtpError> {
        self.command("EHLO aivyx-pa").await?;
        self.expect(250).await.map(|_| ())
    }

    /// Asks to start TLS and hands back the raw stream for the handshake.
    /// Bytes the server sent after its `220` would be injected plaintext,
    /// so they are an error.
    pub async fn starttls(mut self) -> Result<S, SmtpError> {
        self.command("STARTTLS").await?;
        self.expect(220).await?;
        if !self.stream.buffer().is_empty() {
            return Err(SmtpError::Protocol("data after STARTTLS".into()));
        }
        Ok(self.stream.into_inner())
    }

    pub async fn auth_plain(&mut self, user: &str, password: &str) -> Result<(), SmtpError> {
        let token = base64::engine::general_purpose::STANDARD.encode(format!("\0{user}\0{password}"));
        self.command(&format!("AUTH PLAIN {token}")).await?;
        match self.expect(235).await {
            Err(SmtpError::Reply { code: 535, text }) => Err(SmtpError::Auth(text)),
            other => other.map(|_| ()),
        }
    }

    /// `data` is the RFC 5322 message (CRLF line endings).
    pub async fn send(&mut self, from: &str, to: &[String], data: &[u8]) -> Result<(), SmtpError> {
        if to.is_empty() {
            return Err(SmtpError::Protocol("no recipients".into()));
        }
        self.command(&format!("MAIL FROM:<{}>", path(from)?)).await?;
        self.expect(250).await?;
        for rcpt in to {
            self.command(&format!("RCPT TO:<{}>", path(rcpt)?)).await?;
            self.expect_one_of(&[250, 251]).await?;
        }
        self.command("DATA").await?;
        self.expect(354).await?;
        let w = self.stream.get_mut();
        w.write_all(&dot_stuff(data)).await?;
        w.flush().await?;
        self.expect(250).await.map(|_| ())
    }

    pub async fn quit(mut self) {
        if self.command("QUIT").await.is_ok() {
            let _ = self.expect(221).await;
        }
    }

    async fn command(&mut self, line: &str) -> Result<(), SmtpError> {
        if line.contains(['\r', '\n']) {
            return Err(SmtpError::Protocol("line break in a command".into()));
        }
        let w = self.stream.get_mut();
        w.write_all(line.as_bytes()).await?;
        w.write_all(b"\r\n").await?;
        w.flush().await?;
        Ok(())
    }

    async fn expect(&mut self, code: u16) -> Result<Vec<String>, SmtpError> {
        self.expect_one_of(&[code]).await
    }

    /// Reads one (possibly multi-line) reply.
    async fn expect_one_of(&mut self, codes: &[u16]) -> Result<Vec<String>, SmtpError> {
        let mut lines = Vec::new();
        loop {
            let mut line = String::new();
            if self.stream.read_line(&mut line).await? == 0 {
                return Err(SmtpError::Protocol("connection closed".into()));
            }
            let line = line.trim_end_matches(['\r', '\n']).to_string();
            let code: u16 = line
                .get(..3)
                .and_then(|c| c.parse().ok())
                .ok_or_else(|| SmtpError::Protocol(format!("bad reply {line:?}")))?;
            let more = line.as_bytes().get(3) == Some(&b'-');
            lines.push(line);
            if !more {
                if codes.contains(&code) {
                    return Ok(lines);
                }
                return Err(SmtpError::Reply {
                    code,
                    text: lines.join(" / "),
                });
            }
        }
    }
}

/// A bare address for `MAIL FROM` / `RCPT TO`.
fn path(addr: &str) -> Result<&str, SmtpError> {
    let a = addr.trim();
    if a.is_empty() || a.contains(['<', '>', ' ', '\r', '\n']) || !a.contains('@') {
        return Err(SmtpError::Protocol(format!("bad address {addr:?}")));
    }
    Ok(a)
}

/// The DATA payload: a leading `.` on any line doubled, CRLF-terminated,
/// then the `.` end marker.
pub fn dot_stuff(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 8);
    let mut line_start = true;
    for &b in data {
        if line_start && b == b'.' {
            out.push(b'.');
        }
        out.push(b);
        line_start = b == b'\n';
    }
    if !out.ends_with(b"\r\n") {
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b".\r\n");
    out
}
```

- [ ] **Step 4: Run the tests and clippy**

Run: `cargo test -p aivyx-proton smtp:: && cargo clippy -p aivyx-proton --all-targets -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step: Commit**

```bash
git add crates/aivyx-proton/src/smtp.rs crates/aivyx-proton/src/lib.rs
git commit -s -m "feat(proton): minimal SMTP client (EHLO, STARTTLS, AUTH PLAIN, DATA)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8: The Bridge transport

Everything that touches a real server is proven by the live tests in Task 16 (the code here was run against GreenMail 2.1.3: search, fetch, the Sent `HEADER` search, flag + `MOVE` + `APPEND` filing, All Mail search, drafts, IDLE waking 3.5 s after new mail). The unit tests below cover what needs no server. Bridge shows Proton folders as `Folders/<name>`; IMAP `UID N:*` also matches the last message when none is newer, hence the filter in `new_uids`.

**Files:**
- Create: `crates/aivyx-proton/src/bridge.rs`
- Modify: `crates/aivyx-proton/src/lib.rs` (add `pub mod bridge;`, keeping the list alphabetical)

**Interfaces:**
- Consumes: `message`, `outgoing`, `smtp`, `tls`, `transport`
- Produces: `const INBOX, SENT, DRAFTS, ALL_MAIL`; `trait Io`, `type Stream = Box<dyn Io>`, `type ImapClient`; `trait Dial { async fn imap(&self) -> Result<ImapClient, TransportError>; async fn smtp(&self) -> Result<smtp::Conn<Stream>, TransportError>; }`; `struct BridgeDial { imap_addr, smtp_addr, imap_pin, smtp_pin }`; `async fn capture_pins(imap_addr, smtp_addr) -> Result<(Pin, Pin), TransportError>`; `struct Login { username, password }` (redacted `Debug`); `struct BridgeTransport` with `new(Arc<dyn Dial>, Login, folder: &str)`, `check_login()`, and `impl MailTransport`

- [ ] **Step 1: Write the failing tests** — create `crates/aivyx-proton/src/bridge.rs` holding only this test module (the body goes above it in Step 3), and add `pub mod bridge;` to `lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_ids_are_checked_before_they_reach_a_search() {
        assert!(plausible_internal_id("AbC-_=09"));
        assert!(!plausible_internal_id(""));
        assert!(!plausible_internal_id("a\" OR ALL"));
        assert!(!plausible_internal_id("a b"));
    }

    #[test]
    fn the_password_never_prints() {
        let l = Login { username: "u".into(), password: "hunter2".into() };
        assert!(!format!("{l:?}").contains("hunter2"));
    }

    #[test]
    fn handled_mail_goes_under_folders() {
        struct Never;
        #[async_trait]
        impl Dial for Never {
            async fn imap(&self) -> Result<ImapClient, TransportError> {
                Err(platform("no"))
            }
            async fn smtp(&self) -> Result<smtp::Conn<Stream>, TransportError> {
                Err(platform("no"))
            }
        }
        let t = BridgeTransport::new(Arc::new(Never), Login { username: "u".into(), password: "p".into() }, "Aivyx");
        assert_eq!(t.handled_mailbox, "Folders/Aivyx");
    }
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p aivyx-proton bridge::`
Expected: FAIL to compile — the items the tests use don't exist yet.

- [ ] **Step 3: Write the module** — put this above the test module in `crates/aivyx-proton/src/bridge.rs`:

```rust
//! [`BridgeTransport`]: [`MailTransport`] over Proton Mail Bridge's local
//! IMAP (IDLE, 60 s poll fallback) and SMTP, both STARTTLS pinned to the
//! certificate captured at `connect proton` ([`BridgeDial`]).
//!
//! Connecting is behind [`Dial`] so the live test can drive the very same
//! transport over a plain local GreenMail (which has no STARTTLS) without
//! any plaintext path existing in this crate.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::TryStreamExt;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, MutexGuard};

use crate::message::{self, MailMessage};
use crate::outgoing::OutgoingMail;
use crate::smtp;
use crate::tls::{self, Pin};
use crate::transport::{InboxStatus, MailTransport, SearchQuery, TransportError};

/// Bridge's mailbox names.
pub const INBOX: &str = "INBOX";
pub const SENT: &str = "Sent";
pub const DRAFTS: &str = "Drafts";
pub const ALL_MAIL: &str = "All Mail";
/// Most messages one `wait_for_new` returns.
const BATCH: usize = 20;
/// How long one IDLE (or poll sleep) lasts before searching again.
const IDLE_FOR: Duration = Duration::from_secs(60);

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send + fmt::Debug {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + fmt::Debug> Io for T {}
pub type Stream = Box<dyn Io>;
pub type ImapClient = async_imap::Client<Stream>;
type ImapSession = async_imap::Session<Stream>;

/// Opens connections, ready to log in: IMAP after the greeting, SMTP
/// after EHLO.
#[async_trait]
pub trait Dial: Send + Sync {
    async fn imap(&self) -> Result<ImapClient, TransportError>;
    async fn smtp(&self) -> Result<smtp::Conn<Stream>, TransportError>;
}

/// STARTTLS to Bridge on loopback, each port pinned to its certificate.
pub struct BridgeDial {
    pub imap_addr: String,
    pub smtp_addr: String,
    pub imap_pin: Pin,
    pub smtp_pin: Pin,
}

fn platform(e: impl fmt::Display) -> TransportError {
    TransportError::Platform(e.to_string())
}

fn tls_error(e: std::io::Error) -> TransportError {
    if tls::is_pin_mismatch(&e) {
        TransportError::CertChanged
    } else {
        platform(e)
    }
}

/// Plain TCP → greeting → STARTTLS → the raw stream for the handshake.
async fn imap_starttls(addr: &str) -> Result<TcpStream, TransportError> {
    let tcp = TcpStream::connect(addr).await.map_err(platform)?;
    let mut client = async_imap::Client::new(tcp);
    client
        .read_response()
        .await
        .map_err(platform)?
        .ok_or_else(|| platform("Bridge closed the IMAP connection"))?;
    client
        .run_command_and_check_ok("STARTTLS", None)
        .await
        .map_err(platform)?;
    Ok(client.into_inner())
}

async fn smtp_starttls(addr: &str) -> Result<TcpStream, TransportError> {
    let tcp = TcpStream::connect(addr).await.map_err(platform)?;
    smtp::Conn::open(tcp)
        .await
        .map_err(platform)?
        .starttls()
        .await
        .map_err(platform)
}

#[async_trait]
impl Dial for BridgeDial {
    async fn imap(&self) -> Result<ImapClient, TransportError> {
        let tcp = imap_starttls(&self.imap_addr).await?;
        let name = tls::server_name(&self.imap_addr).map_err(platform)?;
        let stream = tls::pinned(self.imap_pin)
            .connect(name, tcp)
            .await
            .map_err(tls_error)?;
        Ok(async_imap::Client::new(Box::new(stream) as Stream))
    }

    async fn smtp(&self) -> Result<smtp::Conn<Stream>, TransportError> {
        let tcp = smtp_starttls(&self.smtp_addr).await?;
        let name = tls::server_name(&self.smtp_addr).map_err(platform)?;
        let stream = tls::pinned(self.smtp_pin)
            .connect(name, tcp)
            .await
            .map_err(tls_error)?;
        smtp::Conn::resume_after_tls(Box::new(stream) as Stream)
            .await
            .map_err(platform)
    }
}

/// For `connect proton`: STARTTLS to both ports accepting any certificate,
/// returning each one's pin for the operator to confirm.
pub async fn capture_pins(imap_addr: &str, smtp_addr: &str) -> Result<(Pin, Pin), TransportError> {
    async fn capture(addr: &str, tcp: TcpStream) -> Result<Pin, TransportError> {
        let (connector, seen) = tls::capturing();
        let name = tls::server_name(addr).map_err(platform)?;
        connector.connect(name, tcp).await.map_err(platform)?;
        let der = seen.lock().expect("seen mutex poisoned").clone();
        der.map(|d| tls::fingerprint(&d))
            .ok_or_else(|| platform("Bridge sent no certificate"))
    }
    let imap = capture(imap_addr, imap_starttls(imap_addr).await?).await?;
    let smtp = capture(smtp_addr, smtp_starttls(smtp_addr).await?).await?;
    Ok((imap, smtp))
}

pub struct Login {
    pub username: String,
    pub password: String,
}

impl fmt::Debug for Login {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Login")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

pub struct BridgeTransport {
    dial: Arc<dyn Dial>,
    login: Login,
    /// `Folders/<folder>`: Bridge shows Proton folders under `Folders/`.
    handled_mailbox: String,
    /// One IMAP session, reconnected after any error.
    imap: Mutex<Option<ImapSession>>,
}

impl BridgeTransport {
    pub fn new(dial: Arc<dyn Dial>, login: Login, folder: &str) -> Self {
        BridgeTransport {
            dial,
            login,
            handled_mailbox: format!("Folders/{folder}"),
            imap: Mutex::new(None),
        }
    }

    /// Logs in to IMAP and SMTP without changing anything — what
    /// `connect proton` runs before saving the credential.
    pub async fn check_login(&self) -> Result<(), TransportError> {
        self.inbox_status().await?;
        let mut conn = self.dial.smtp().await?;
        let r = conn
            .auth_plain(&self.login.username, &self.login.password)
            .await
            .map_err(|e| match e {
                smtp::SmtpError::Auth(m) => TransportError::Unauthorized(m),
                other => platform(other),
            });
        conn.quit().await;
        r
    }

    async fn session(&self) -> Result<MutexGuard<'_, Option<ImapSession>>, TransportError> {
        let mut guard = self.imap.lock().await;
        if guard.is_none() {
            let client = self.dial.imap().await?;
            let session = client
                .login(&self.login.username, &self.login.password)
                .await
                .map_err(|(e, _)| match e {
                    async_imap::error::Error::No(m) => TransportError::Unauthorized(m),
                    other => platform(other),
                })?;
            *guard = Some(session);
        }
        Ok(guard)
    }

    /// Runs `op` on the session; any error drops it so the next call
    /// reconnects.
    async fn with_session<T>(
        &self,
        op: impl for<'s> FnOnce(
            &'s mut ImapSession,
        ) -> futures_util::future::BoxFuture<'s, Result<T, TransportError>>,
    ) -> Result<T, TransportError> {
        let mut guard = self.session().await?;
        let result = op(guard.as_mut().expect("session() connected")).await;
        if result.is_err() {
            *guard = None;
        }
        result
    }
}

async fn new_uids(s: &mut ImapSession, after: u32) -> Result<Vec<u32>, TransportError> {
    s.select(INBOX).await.map_err(platform)?;
    // `N:*` also matches the last message when none is newer: filter.
    let mut uids: Vec<u32> = s
        .uid_search(format!("UID {}:*", after.saturating_add(1)))
        .await
        .map_err(platform)?
        .into_iter()
        .filter(|u| *u > after)
        .collect();
    uids.sort_unstable();
    uids.truncate(BATCH);
    Ok(uids)
}

async fn fetch_uids(s: &mut ImapSession, uids: &[u32]) -> Result<Vec<MailMessage>, TransportError> {
    if uids.is_empty() {
        return Ok(Vec::new());
    }
    let set: Vec<String> = uids.iter().map(u32::to_string).collect();
    let fetches: Vec<_> = s
        .uid_fetch(set.join(","), "(UID BODY.PEEK[])")
        .await
        .map_err(platform)?
        .try_collect()
        .await
        .map_err(platform)?;
    let mut out: Vec<MailMessage> = fetches
        .iter()
        .filter_map(|f| message::parse(f.uid?, f.body()?))
        .collect();
    out.sort_by_key(|m| m.uid);
    Ok(out)
}

/// Proton message IDs are base64url (plus `=`); anything else can't match.
fn plausible_internal_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'='))
}

#[async_trait]
impl MailTransport for BridgeTransport {
    async fn inbox_status(&self) -> Result<InboxStatus, TransportError> {
        self.with_session(|s| {
            Box::pin(async move {
                let mb = s.select(INBOX).await.map_err(platform)?;
                Ok(InboxStatus {
                    uid_validity: mb.uid_validity.unwrap_or(0),
                    uid_next: mb.uid_next.unwrap_or(1),
                })
            })
        })
        .await
    }

    async fn wait_for_new(&self, after_uid: u32) -> Result<Vec<MailMessage>, TransportError> {
        let found = self
            .with_session(|s| Box::pin(async move { new_uids(s, after_uid).await }))
            .await?;
        if !found.is_empty() {
            return self
                .with_session(|s| Box::pin(async move { fetch_uids(s, &found).await }))
                .await;
        }
        // Nothing new: wait for the server to say something (or 60 s).
        let mut guard = self.session().await?;
        let mut session = guard.take().expect("session() connected");
        let mut session = match session.capabilities().await {
            Ok(caps) if caps.has_str("IDLE") => {
                let mut idle = session.idle();
                idle.init().await.map_err(platform)?;
                {
                    let (wait, _stop) = idle.wait_with_timeout(IDLE_FOR);
                    wait.await.map_err(platform)?;
                }
                idle.done().await.map_err(platform)?
            }
            Ok(_) => {
                tokio::time::sleep(IDLE_FOR).await;
                session
            }
            Err(e) => return Err(platform(e)),
        };
        let found = new_uids(&mut session, after_uid).await;
        let result = match found {
            Ok(uids) => fetch_uids(&mut session, &uids).await,
            Err(e) => Err(e),
        };
        if result.is_ok() {
            *guard = Some(session);
        }
        result
    }

    async fn in_sent(&self, internal_id: &str) -> Result<bool, TransportError> {
        if !plausible_internal_id(internal_id) {
            return Ok(false);
        }
        let query = format!("HEADER X-Pm-Internal-Id {}", crate::transport::quote(internal_id).map_err(platform)?);
        self.with_session(|s| {
            Box::pin(async move {
                s.examine(SENT).await.map_err(platform)?;
                Ok(!s.uid_search(query).await.map_err(platform)?.is_empty())
            })
        })
        .await
    }

    async fn send(&self, mail: &OutgoingMail) -> Result<(), TransportError> {
        let data = mail.to_rfc5322().map_err(platform)?;
        let mut conn = self.dial.smtp().await?;
        conn.auth_plain(&self.login.username, &self.login.password)
            .await
            .map_err(|e| match e {
                smtp::SmtpError::Auth(m) => TransportError::Unauthorized(m),
                other => platform(other),
            })?;
        conn.send(&mail.from, &mail.recipients(), &data)
            .await
            .map_err(platform)?;
        conn.quit().await;
        Ok(())
    }

    async fn file_handled(&self, uid: u32, reply: &OutgoingMail) -> Result<(), TransportError> {
        let copy = reply.to_rfc5322().map_err(platform)?;
        let mailbox = self.handled_mailbox.clone();
        self.with_session(|s| {
            Box::pin(async move {
                s.select(INBOX).await.map_err(platform)?;
                let _: Vec<_> = s
                    .uid_store(uid.to_string(), "+FLAGS (\\Seen)")
                    .await
                    .map_err(platform)?
                    .try_collect()
                    .await
                    .map_err(platform)?;
                let exists = {
                    let names: Vec<_> = s
                        .list(Some(""), Some(&mailbox))
                        .await
                        .map_err(platform)?
                        .try_collect()
                        .await
                        .map_err(platform)?;
                    !names.is_empty()
                };
                if !exists {
                    s.create(&mailbox).await.map_err(platform)?;
                }
                s.uid_mv(uid.to_string(), &mailbox).await.map_err(platform)?;
                s.append(&mailbox, Some("(\\Seen)"), None, copy)
                    .await
                    .map_err(platform)?;
                Ok(())
            })
        })
        .await
    }

    async fn search(&self, q: &SearchQuery) -> Result<Vec<MailMessage>, TransportError> {
        let criteria = q.to_imap().map_err(platform)?;
        let max = q.max_results;
        self.with_session(|s| {
            Box::pin(async move {
                s.examine(ALL_MAIL).await.map_err(platform)?;
                let mut uids: Vec<u32> = s
                    .uid_search(criteria)
                    .await
                    .map_err(platform)?
                    .into_iter()
                    .collect();
                uids.sort_unstable_by_key(|u| std::cmp::Reverse(*u));
                uids.truncate(max);
                let mut found = fetch_uids(s, &uids).await?;
                found.sort_by_key(|m| std::cmp::Reverse(m.uid));
                Ok(found)
            })
        })
        .await
    }

    async fn fetch(&self, uid: u32) -> Result<Option<MailMessage>, TransportError> {
        self.with_session(|s| {
            Box::pin(async move {
                s.examine(ALL_MAIL).await.map_err(platform)?;
                Ok(fetch_uids(s, &[uid]).await?.into_iter().next())
            })
        })
        .await
    }

    async fn append_draft(&self, mail: &OutgoingMail) -> Result<(), TransportError> {
        let data = mail.to_rfc5322().map_err(platform)?;
        self.with_session(|s| {
            Box::pin(async move {
                s.append(DRAFTS, Some("(\\Draft \\Seen)"), None, data)
                    .await
                    .map_err(platform)
            })
        })
        .await
    }
}
```

- [ ] **Step 4: Run the tests and clippy**

Run: `cargo test -p aivyx-proton bridge:: && cargo clippy -p aivyx-proton --all-targets -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step: Commit**

```bash
git add crates/aivyx-proton/src/bridge.rs crates/aivyx-proton/src/lib.rs
git commit -s -m "feat(proton): Bridge transport — IMAP with IDLE, pinned STARTTLS, SMTP submission" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9: The lock, the resume cursor and the credential file

**Files:**
- Create: `crates/aivyx-proton/src/lock.rs`, `crates/aivyx-proton/src/state.rs`, `crates/aivyx-proton/src/credentials.rs`
- Modify: `crates/aivyx-proton/src/lib.rs` (add `pub mod credentials;`, `pub mod lock;`, `pub mod state;`)

**Interfaces:**
- Consumes: `bridge::{BridgeDial, BridgeTransport, Login}`, `tls::pin_from_hex`, `transport::InboxStatus`, `aivyx_instance::InstancePaths`
- Produces: `lock::{acquire(&Path) -> Result<ChannelLock, String>, BUSY, LOCK_FILE}`; `state::{Cursor { uid_validity, last_uid }, resume(Option<Cursor>, InboxStatus) -> Cursor, load(&Path), save(&Path, &Cursor), STATE_FILE}`; `credentials::{BridgeCredentials { username, password, imap_addr, smtp_addr, imap_pin, smtp_pin, from_address } (redacted Debug) with dial() and transport(folder), path_for(&InstancePaths) -> Option<PathBuf>, load(&Path) -> Result<Option<_>, String>, save(&Path, &_), CREDENTIALS_FILE}`

`lock.rs` is `aivyx-matrix`'s store lock with Proton wording (same flock mechanism). The credential is a `0600` file, not the keyring: the sandboxed tool process can only see its own tool folder (spec, Decisions).

- [ ] **Step 1: Write the failing tests** — create the three files holding only their test modules, and add the three `pub mod` lines:

`lock.rs`:
```rust
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("aivyx-proton-lock-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_second_holder_is_refused_with_a_clear_message() {
        let d = scratch("busy");
        let first = acquire(&d).unwrap();
        let err = acquire(&d).unwrap_err();
        assert_eq!(err, BUSY);
        assert!(err.contains("aivyx-pa --channel proton"));
        drop(first);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_lock_is_released_on_drop() {
        let d = scratch("release");
        drop(acquire(&d).unwrap());
        let again = acquire(&d);
        assert!(again.is_ok(), "{again:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_dir_is_created_private() {
        use std::os::unix::fs::PermissionsExt;
        let d = scratch("mode");
        let _l = acquire(&d).unwrap();
        assert_eq!(std::fs::metadata(&d).unwrap().permissions().mode() & 0o777, 0o700);
        let _ = std::fs::remove_dir_all(&d);
    }
}
```

`state.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn status(v: u32, next: u32) -> InboxStatus {
        InboxStatus { uid_validity: v, uid_next: next }
    }

    #[test]
    fn first_run_starts_at_now() {
        assert_eq!(resume(None, status(7, 42)), Cursor { uid_validity: 7, last_uid: 41 });
        assert_eq!(resume(None, status(7, 0)).last_uid, 0);
    }

    #[test]
    fn a_saved_cursor_resumes_unless_uidvalidity_changed() {
        let saved = Cursor { uid_validity: 7, last_uid: 12 };
        assert_eq!(resume(Some(saved), status(7, 42)), saved);
        assert_eq!(resume(Some(saved), status(8, 42)), Cursor { uid_validity: 8, last_uid: 41 });
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()).unwrap(), None);
        let c = Cursor { uid_validity: 3, last_uid: 9 };
        save(dir.path(), &c).unwrap();
        assert_eq!(load(dir.path()).unwrap(), Some(c));
    }
}
```

`credentials.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn creds() -> BridgeCredentials {
        BridgeCredentials {
            username: "me@proton.me".into(),
            password: "bridge-pw".into(),
            imap_addr: "127.0.0.1:1143".into(),
            smtp_addr: "127.0.0.1:1025".into(),
            imap_pin: "00".repeat(32),
            smtp_pin: "11".repeat(32),
            from_address: "me@proton.me".into(),
        }
    }

    #[test]
    fn save_then_load_round_trips_privately() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proton").join(CREDENTIALS_FILE);
        assert_eq!(load(&path).unwrap(), None);
        save(&path, &creds()).unwrap();
        assert_eq!(load(&path).unwrap(), Some(creds()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(
                std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_file_others_can_read_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CREDENTIALS_FILE);
        save(&path, &creds()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load(&path).unwrap_err().contains("chmod 600"));
    }

    #[test]
    fn the_file_lives_in_the_instances_proton_tool_folder() {
        let dirs = aivyx_instance::BaseDirs {
            home: Some(PathBuf::from("/h")),
            xdg_config_home: None,
            xdg_data_home: None,
            xdg_runtime_dir: None,
        };
        let work = InstancePaths::new(aivyx_instance::InstanceName::parse("work").unwrap(), dirs);
        assert_eq!(
            path_for(&work),
            Some(PathBuf::from("/h/.aivyx-pa/instances/work/tool-processes/proton/credentials.toml"))
        );
    }

    #[test]
    fn the_password_never_prints_and_pins_decode() {
        let c = creds();
        assert!(!format!("{c:?}").contains("bridge-pw"));
        let d = c.dial().unwrap();
        assert_eq!(d.imap_pin, [0u8; 32]);
        assert_eq!(d.smtp_pin, [0x11u8; 32]);
        let bad = BridgeCredentials { imap_pin: "zz".into(), ..creds() };
        assert!(bad.dial().is_err());
    }
}
```

- [ ] **Step 2: Run them to see them fail** — `cargo test -p aivyx-proton lock:: state:: credentials::` → FAIL to compile.

- [ ] **Step 3: Write the modules** — above each test module:

`lock.rs`:
```rust
//! One `--channel proton` process at a time per instance.
//!
//! Two channel processes on one mailbox would both answer every email.
//! The channel takes an exclusive advisory lock on `<proton_dir>/lock` and
//! holds it while it runs. Same mechanism as `aivyx-matrix`'s store lock.

use std::fs::File;
use std::path::Path;

pub const LOCK_FILE: &str = "lock";

/// Shown when another process holds the lock.
pub const BUSY: &str = "the Proton Mail channel is already running in another process \
    (`aivyx-pa --channel proton`) — stop it first";

/// Held while the channel runs; the lock is released on drop
/// (closing the file releases a `flock`).
#[derive(Debug)]
pub struct ChannelLock {
    _file: File,
}

/// Takes the exclusive lock on `<dir>/lock`, creating `dir` (mode 0700) and
/// the lock file if needed. Fails at once with [`BUSY`] if another holder
/// exists.
pub fn acquire(dir: &Path) -> Result<ChannelLock, String> {
    {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let path = dir.join(LOCK_FILE);
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path).map_err(|e| format!("open {}: {e}", path.display()))?;
    try_lock_exclusive(&file).map_err(|e| match e {
        LockError::Busy => BUSY.to_string(),
        LockError::Io(e) => format!("lock {}: {e}", path.display()),
    })?;
    Ok(ChannelLock { _file: file })
}

enum LockError {
    Busy,
    Io(std::io::Error),
}

#[cfg(unix)]
fn try_lock_exclusive(file: &File) -> Result<(), LockError> {
    use std::os::unix::io::AsRawFd;
    // SAFETY: `flock` on a file descriptor we own for the duration of the
    // call; no memory is passed.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.kind() == std::io::ErrorKind::WouldBlock {
        Err(LockError::Busy)
    } else {
        Err(LockError::Io(err))
    }
}

/// Non-Unix: no advisory lock (the workspace MSRV predates `File::try_lock`).
#[cfg(not(unix))]
fn try_lock_exclusive(_file: &File) -> Result<(), LockError> {
    Ok(())
}
```

`state.rs`:
```rust
//! Where the channel left off: the last INBOX UID it handled, so a
//! restart neither repeats nor misses mail. `<proton_dir>/state.json`.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::transport::InboxStatus;

pub const STATE_FILE: &str = "state.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub uid_validity: u32,
    pub last_uid: u32,
}

/// The saved cursor, if it is still valid for this INBOX; otherwise "now"
/// (the next UID), so existing mail is never processed. A changed
/// UIDVALIDITY means old UIDs mean nothing any more.
pub fn resume(saved: Option<Cursor>, status: InboxStatus) -> Cursor {
    match saved {
        Some(c) if c.uid_validity == status.uid_validity => c,
        _ => Cursor {
            uid_validity: status.uid_validity,
            last_uid: status.uid_next.saturating_sub(1),
        },
    }
}

pub fn load(dir: &Path) -> Result<Option<Cursor>, String> {
    let path = dir.join(STATE_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| format!("read {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

/// Written to a temporary file and renamed, so a crash never leaves half a
/// cursor. `dir` already exists (the channel lock creates it).
pub fn save(dir: &Path, cursor: &Cursor) -> Result<(), String> {
    let path = dir.join(STATE_FILE);
    let tmp = dir.join(format!("{STATE_FILE}.tmp"));
    let bytes = serde_json::to_vec(cursor).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("write {}: {e}", path.display()))
}
```

`credentials.rs`:
```rust
//! The Bridge login and certificate pins, in
//! `<tool_process_dir("proton")>/credentials.toml` (mode `0600`).
//!
//! Not the OS keyring: the `proton.*` tools run under the bundled
//! bubblewrap sandbox, which hides `$HOME` except this folder and has no
//! session bus — the same posture as Gmail's `tokens.json`. Written only by
//! `aivyx-pa connect proton`; read by the channel and the tool process.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aivyx_instance::InstancePaths;

use serde::{Deserialize, Serialize};

use crate::bridge::{BridgeDial, BridgeTransport, Login};
use crate::tls;

pub const CREDENTIALS_FILE: &str = "credentials.toml";

/// `<tool_process_dir("proton")>/credentials.toml` for `instance` — the
/// folder the bundled sandbox binds for the `proton` tool process.
pub fn path_for(instance: &InstancePaths) -> Option<PathBuf> {
    instance
        .tool_process_dir("proton")
        .map(|d| d.join(CREDENTIALS_FILE))
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeCredentials {
    /// Bridge's IMAP/SMTP username (Bridge → account → mailbox details).
    pub username: String,
    /// Bridge's own generated password — not your Proton password.
    pub password: String,
    pub imap_addr: String,
    pub smtp_addr: String,
    /// Hex SHA-256 of each port's certificate.
    pub imap_pin: String,
    pub smtp_pin: String,
    /// What `proton.draft` / `proton.send` send from: your first
    /// `own_addresses` entry.
    pub from_address: String,
}

impl fmt::Debug for BridgeCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BridgeCredentials")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("imap_addr", &self.imap_addr)
            .field("smtp_addr", &self.smtp_addr)
            .field("from_address", &self.from_address)
            .finish()
    }
}

impl BridgeCredentials {
    pub fn dial(&self) -> Result<BridgeDial, String> {
        Ok(BridgeDial {
            imap_addr: self.imap_addr.clone(),
            smtp_addr: self.smtp_addr.clone(),
            imap_pin: tls::pin_from_hex(&self.imap_pin)?,
            smtp_pin: tls::pin_from_hex(&self.smtp_pin)?,
        })
    }

    /// The real transport; `folder` is where handled mail goes.
    pub fn transport(&self, folder: &str) -> Result<BridgeTransport, String> {
        Ok(BridgeTransport::new(
            Arc::new(self.dial()?),
            Login {
                username: self.username.clone(),
                password: self.password.clone(),
            },
            folder,
        ))
    }
}

/// `None` when `connect proton` hasn't run. Refuses a file others can read.
pub fn load(path: &Path) -> Result<Option<BridgeCredentials>, String> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)
            .map_err(|e| format!("read {}: {e}", path.display()))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            return Err(format!(
                "{} is readable by other users — run `chmod 600 {}` (or `aivyx-pa connect proton` again)",
                path.display(),
                path.display()
            ));
        }
    }
    let text = String::from_utf8(bytes).map_err(|e| format!("read {}: {e}", path.display()))?;
    toml::from_str(&text)
        .map(Some)
        .map_err(|e| format!("read {}: {e}", path.display()))
}

/// Creates the folder (`0700`) and writes the file `0600` via a temporary
/// file and a rename.
pub fn save(path: &Path, creds: &BridgeCredentials) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no folder", path.display()))?;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .map_err(|e| format!("create {}: {e}", dir.display()))?;
    let text = toml::to_string(creds).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("toml.tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    {
        use std::io::Write;
        let mut f = options
            .open(&tmp)
            .map_err(|e| format!("write {}: {e}", tmp.display()))?;
        f.write_all(text.as_bytes())
            .map_err(|e| format!("write {}: {e}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("write {}: {e}", path.display()))
}
```

- [ ] **Step 4: Run** `cargo test -p aivyx-proton && cargo test -p aivyx-instance && cargo clippy -p aivyx-proton --all-targets -- -D warnings` → PASS (the instance guard test included).

- [ ] **Step: Commit**

```bash
git add crates/aivyx-proton/src
git commit -s -m "feat(proton): channel lock, resume cursor and the 0600 Bridge credential file" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 10: The channel and the mail loop

**Files:**
- Create: `crates/aivyx-proton/src/proton_channel.rs`, `crates/aivyx-proton/src/session.rs`, `crates/aivyx-proton/src/tests.rs`
- Modify: `crates/aivyx-proton/src/lib.rs` (add `pub mod proton_channel;`, `pub mod session;`, the `pub use session::{…}` re-export and `#[cfg(test)] mod tests;` — final `lib.rs` in Task 11)

**Interfaces:**
- Consumes: everything above; `aivyx_core::{Agent, ConcreteAgent, ChannelContext, Message, SessionId, …}` exactly as `aivyx-matrix`'s session uses them.
- Produces: `ProtonChannel` (crate-private `new(SessionId, partition)`, `take_reply()`); in `session`: `struct Turn { message, sender, text, first_line, notes }` + `thread()`, `async fn screen<T: MailTransport + ?Sized>(&T, &ProtonPolicy, MailMessage) -> Result<Option<Turn>, TransportError>`, `fn reply_body`, `async fn answer(&T, &ProtonPolicy, &Turn, &str) -> Result<(), TransportError>`, `trait TurnRunner { async fn run(&mut self, &Turn) -> Result<Option<String>, String>; }`, `struct ProtonSessionReport { turns_run, answered }`, `enum LoopError { Fatal(String), Runner(String) }`, `async fn run_mail_loop<T: MailTransport + ?Sized, R: TurnRunner>(&T, &ProtonPolicy, state_dir: &Path, &mut R, CancellationToken) -> Result<ProtonSessionReport, LoopError>`, `struct ProtonSessionConfig` (same fields as `MatrixSessionConfig`), `fn partition(&str) -> String`, `async fn run_proton_session_with_transport(&T, &ProtonPolicy, &Path, ProtonSessionConfig, provider, audit, checkpointer, CancellationToken)`, `const CUT_NOTE`, `fn attachments_note`.

`run_mail_loop` owns the lock, the cursor (first run starts at "now"; a changed UIDVALIDITY restarts at "now"), backoff (1 s doubling to 60 s), the trust check and the reply. A failed reply leaves the email unread in the inbox and moves on; a transient screening error retries the same email after the backoff; a fatal transport error stops. `/team` is daemon-only, as on Matrix.

- [ ] **Step 1: Write the failing tests.**
  1. `proton_channel.rs`, only its test module:

```rust
#[cfg(test)]
mod channel_tests {
    use super::*;

    fn channel() -> ProtonChannel {
        ProtonChannel::new(SessionId::new(), "proton-abc".into())
    }

    #[test]
    fn identity_is_email_semitrusted_partitioned_by_thread() {
        let c = channel();
        assert_eq!(c.platform(), ChannelPlatform::Email);
        assert_eq!(c.trust_tier(), TrustTier::SemiTrusted);
        assert_eq!(c.session_partition().as_deref(), Some("proton-abc"));
        assert_eq!(c.channel_name(), "aivyx-proton");
    }

    #[tokio::test]
    async fn finalize_keeps_the_whole_reply_once() {
        let c = channel();
        assert_eq!(c.take_reply(), None);
        c.stream_event(StreamEvent::Text("the answer is ")).await.unwrap();
        c.stream_event(StreamEvent::Text("42")).await.unwrap();
        c.finalize(&TurnOutcome::Completed {
            final_message: "the answer is 42".into(),
            tool_calls_made: 0,
            duration: std::time::Duration::ZERO,
        })
        .await
        .unwrap();
        assert_eq!(c.take_reply().as_deref(), Some("the answer is 42"));
        assert_eq!(c.take_reply(), None);
    }

    #[tokio::test]
    async fn an_empty_turn_still_says_something() {
        let c = channel();
        c.finalize(&TurnOutcome::Cancelled { tool_calls_made: 0 }).await.unwrap();
        assert_eq!(c.take_reply().as_deref(), Some("\n✕ cancelled"));
    }

    #[test]
    fn reset_cancellation_yields_a_fresh_token() {
        let c = channel();
        c.cancellation_token().cancel();
        c.reset_cancellation();
        assert!(!c.cancellation_token().is_cancelled());
    }
}
```

  2. `session.rs`, only its test module:

```rust
#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn partitions_are_stable_short_and_scope_safe() {
        let p = partition("<root@proton.me>");
        assert_eq!(p, partition("<root@proton.me>"));
        assert_ne!(p, partition("<other@proton.me>"));
        assert_eq!(p.len(), "proton-".len() + 16);
        assert!(p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
    }

    #[test]
    fn one_thread_one_session() {
        let mut t = Threads::default();
        let a = t.session_for("a");
        assert_eq!(t.session_for("a"), a);
        assert_ne!(t.session_for("b"), a);
    }

    #[test]
    fn notes_follow_the_answer() {
        assert_eq!(reply_body("hi", &[]), "hi");
        assert_eq!(
            reply_body("hi", &[CUT_NOTE.to_string(), attachments_note(&["a.pdf".to_string()])]),
            format!("hi\n\n{CUT_NOTE}\n(I can't open attachments, so I ignored: a.pdf.)")
        );
    }
}
```

  3. `tests.rs` — the session-level tests. The scripted provider and storage helpers are `aivyx-matrix`'s (its `src/tests.rs` lines 1–134), with the scratch prefix renamed:

```rust
//! Session-level tests: the scripted transport drives the real session
//! loop through a `ConcreteAgent` wired to a scripted `LlmProvider`.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

use aivyx_audit::{AuditBridge, HmacChainLog};
use aivyx_capability::{CapabilitySet, Scope};
use aivyx_core::{AuditHook, CancellationToken, ToolRegistry};
use aivyx_crypto::MasterKey;
use aivyx_llm::{
    LlmError, LlmMessage, LlmProvider, LlmRequest, LlmStepEnd, LlmStream, LlmStreamEvent, LlmUsage,
};
use aivyx_storage::{RedbStorage, Storage, StorageConfig};

// ---------------------------------------------------------------------------
// Scripted LLM provider — identical shape to the Telegram precedent.
// ---------------------------------------------------------------------------

struct ScriptedStep {
    events: Vec<LlmStreamEvent>,
    terminal: LlmStepEnd,
}

/// Lets a test hold a turn "in flight": each `chat_stream` call signals
/// `started`, then waits for `release` (or for the turn's cancellation).
#[derive(Default)]
struct Gate {
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

struct ScriptedProvider {
    queue: StdMutex<VecDeque<ScriptedStep>>,
    gate: Option<Arc<Gate>>,
    seen: Arc<StdMutex<Vec<String>>>,
}

#[async_trait]
impl LlmProvider for ScriptedProvider {
    async fn chat_stream(
        &self,
        request: LlmRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<Box<dyn LlmStream>, LlmError> {
        self.seen
            .lock()
            .unwrap()
            .push(format!("{:?}", request.messages));
        if let Some(gate) = &self.gate {
            gate.started.notify_one();
            tokio::select! {
                _ = gate.release.notified() => {}
                _ = cancellation.cancelled() => return Err(LlmError::Cancelled),
            }
        }
        assert!(
            !request.messages.is_empty(),
            "planner must always send non-empty history"
        );
        assert!(
            matches!(request.messages[0], LlmMessage::User { .. }),
            "history[0] should be a User message for a turn with no tools"
        );
        let step = self
            .queue
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| LlmError::Config("ScriptedProvider exhausted".into()))?;
        Ok(Box::new(ScriptedStream {
            events: step.events.into_iter(),
            terminal: Some(step.terminal),
        }))
    }
}

struct ScriptedStream {
    events: std::vec::IntoIter<LlmStreamEvent>,
    terminal: Option<LlmStepEnd>,
}

#[async_trait]
impl LlmStream for ScriptedStream {
    async fn next_event(&mut self) -> Result<Option<LlmStreamEvent>, LlmError> {
        Ok(self.events.next())
    }
    async fn finish(self: Box<Self>) -> Result<LlmStepEnd, LlmError> {
        self.terminal
            .ok_or_else(|| LlmError::StreamEnded("ScriptedStream::finish double-called".into()))
    }
}

fn final_step(chunks: &[&str], text: &str) -> ScriptedStep {
    ScriptedStep {
        events: chunks
            .iter()
            .map(|c| LlmStreamEvent::TextChunk((*c).to_string()))
            .collect(),
        terminal: LlmStepEnd::FinalMessage {
            text: text.to_string(),
            usage: LlmUsage::default(),
        },
    }
}
// ---------------------------------------------------------------------------
// Scratch storage helper — a fresh tmp dir per test so tests run in
// parallel without stepping on each other's redb files.
// ---------------------------------------------------------------------------

async fn scratch_storage(suffix: &str) -> (Arc<dyn Storage>, PathBuf) {
    let tmp = std::env::var("TMPDIR")
        .or_else(|_| std::env::var("TEMP"))
        .unwrap_or_else(|_| "/tmp".to_string());
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let parent = PathBuf::from(tmp).join(format!("aivyx-proton-{suffix}-{pid}-{nanos}"));
    std::fs::create_dir_all(&parent).expect("scratch store parent must be creatable");
    let store_path = parent.join("store.redb");
    let storage: Arc<dyn Storage> = RedbStorage::open(
        StorageConfig::new(store_path),
        MasterKey::from_raw([7u8; 32]),
    )
    .await
    .expect("scratch storage must open");
    (storage, parent)
}



use crate::lock;
use crate::message::MailMessage;
use crate::policy::ProtonPolicy;
use crate::session::{
    run_proton_session_with_transport, LoopError, ProtonSessionConfig, ProtonSessionReport,
    CUT_NOTE,
};
use crate::transport::{InboxStatus, ScriptedTransport, TransportError};

const PROTON_DMARC: &str = "mail.protonmail.ch; dmarc=pass (p=reject dis=none) header.from=example.com";

fn config(storage: Arc<dyn Storage>) -> ProtonSessionConfig {
    ProtonSessionConfig {
        model: "test-model".into(),
        system_prompt: "proton test".into(),
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

fn policy() -> ProtonPolicy {
    ProtonPolicy::new(
        "me_aivyx@proton.me",
        &["me@proton.me".to_string(), "me_aivyx@proton.me".to_string()],
        &["friend@example.com".to_string()],
    )
}

/// Status of an INBOX whose next UID is 10 (so the first run starts at 9).
fn status() -> InboxStatus {
    InboxStatus { uid_validity: 1, uid_next: 10 }
}

/// An email from you to the assistant's address, in Sent as `pm<uid>`.
fn from_me(uid: u32, subject: &str, text: &str) -> MailMessage {
    MailMessage {
        uid,
        internal_id: Some(format!("pm{uid}")),
        message_id: Some(format!("m{uid}@proton.me")),
        subject: subject.into(),
        from: Some("me@proton.me".into()),
        to: vec!["me_aivyx@proton.me".into()],
        text: text.into(),
        ..Default::default()
    }
}

fn from_friend(uid: u32, auth_results: &[&str]) -> MailMessage {
    MailMessage {
        uid,
        message_id: Some(format!("f{uid}@example.com")),
        subject: "Hello".into(),
        from: Some("friend@example.com".into()),
        to: vec!["me_aivyx@proton.me".into()],
        delivered_to: vec!["me_aivyx@proton.me".into()],
        auth_results: auth_results.iter().map(|s| s.to_string()).collect(),
        text: "Are you there?".into(),
        ..Default::default()
    }
}

struct Run {
    result: Result<ProtonSessionReport, LoopError>,
    seen: Vec<String>,
}

/// Runs the in-process session in `dir` until `transport` has sent `n`
/// replies (then 100 ms more), or for 400 ms when `n == 0`.
async fn run_in(dir: &std::path::Path, transport: Arc<ScriptedTransport>, steps: Vec<ScriptedStep>, n: usize) -> Run {
    let (storage, _scratch) = scratch_storage("session").await;
    let seen = Arc::new(StdMutex::new(Vec::new()));
    let provider: Arc<dyn LlmProvider> = Arc::new(ScriptedProvider {
        queue: StdMutex::new(steps.into()),
        gate: None,
        seen: Arc::clone(&seen),
    });
    let audit: Arc<dyn AuditHook> = Arc::new(AuditBridge::new(HmacChainLog::new([42u8; 32].to_vec())));
    let shutdown = CancellationToken::new();
    let (watch_t, watch_s) = (Arc::clone(&transport), shutdown.clone());
    tokio::spawn(async move {
        if n == 0 {
            tokio::time::sleep(Duration::from_millis(400)).await;
        } else {
            while watch_t.sent().await.len() < n {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        watch_s.cancel();
    });
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        run_proton_session_with_transport(
            transport.as_ref(),
            &policy(),
            dir,
            config(storage),
            provider,
            audit,
            None,
            shutdown,
        ),
    )
    .await
    .expect("session did not finish");
    let seen = seen.lock().unwrap().clone();
    Run { result, seen }
}

async fn run(transport: Arc<ScriptedTransport>, steps: Vec<ScriptedStep>, n: usize) -> Run {
    let dir = tempfile::tempdir().unwrap();
    run_in(dir.path(), transport, steps, n).await
}

#[tokio::test]
async fn an_email_from_you_is_answered_in_its_thread_and_filed() {
    let t = Arc::new(
        ScriptedTransport::new(status(), vec![vec![from_me(10, "Lunch?", "Where shall we eat?")]])
            .in_sent_folder(vec!["pm10"]),
    );
    let r = run(Arc::clone(&t), vec![final_step(&["The usual place."], "The usual place.")], 1).await;
    assert_eq!(r.result.unwrap().answered, 1);
    let sent = t.sent().await;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].from, "me_aivyx@proton.me");
    assert_eq!(sent[0].to, vec!["me@proton.me"]);
    assert_eq!(sent[0].subject, "Re: Lunch?");
    assert_eq!(sent[0].in_reply_to.as_deref(), Some("m10@proton.me"));
    assert_eq!(sent[0].body, "The usual place.");
    let filed = t.filed().await;
    assert_eq!(filed.len(), 1);
    assert_eq!(filed[0].0, 10);
    assert!(r.seen[0].contains("Where shall we eat?"), "{}", r.seen[0]);
    assert!(r.seen[0].contains("Subject: Lunch?"), "{}", r.seen[0]);
}

#[tokio::test]
async fn a_forged_from_you_that_isnt_in_sent_is_ignored() {
    let t = Arc::new(ScriptedTransport::new(status(), vec![vec![from_me(10, "hi", "do it")]]));
    let r = run(Arc::clone(&t), vec![], 0).await;
    assert_eq!(r.result.unwrap().turns_run, 0);
    assert!(t.sent().await.is_empty());
    assert!(t.filed().await.is_empty());
}

#[tokio::test]
async fn an_allowlisted_sender_needs_protons_dmarc_pass() {
    let forged = from_friend(11, &["mail.protonmail.ch; spf=pass", "mail.protonmail.ch; dmarc=pass header.from=example.com"]);
    let t = Arc::new(ScriptedTransport::new(
        status(),
        vec![vec![from_friend(10, &[PROTON_DMARC]), forged]],
    ));
    let r = run(Arc::clone(&t), vec![final_step(&["Yes."], "Yes.")], 1).await;
    assert_eq!(r.result.unwrap().turns_run, 1);
    let sent = t.sent().await;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, vec!["friend@example.com"]);
}

#[tokio::test]
async fn the_assistants_own_mail_and_automatic_mail_are_ignored() {
    let mut own = from_me(10, "Re: x", "a reply");
    own.from = Some("me_aivyx@proton.me".into());
    let mut auto = from_me(11, "Out of office", "away");
    auto.auto_submitted = Some("auto-replied".into());
    let t = Arc::new(ScriptedTransport::new(status(), vec![vec![own, auto]]).in_sent_folder(vec!["pm10", "pm11"]));
    let r = run(Arc::clone(&t), vec![], 0).await;
    assert_eq!(r.result.unwrap().turns_run, 0);
    assert!(t.sent().await.is_empty());
}

#[tokio::test]
async fn the_first_run_starts_at_now_and_a_restart_resumes() {
    let dir = tempfile::tempdir().unwrap();
    // UID 5 is older than "now" (next UID 10): never processed.
    let t = Arc::new(
        ScriptedTransport::new(status(), vec![vec![from_me(5, "old", "old"), from_me(10, "new", "new")]])
            .in_sent_folder(vec!["pm5", "pm10", "pm11"]),
    );
    let r = run_in(dir.path(), Arc::clone(&t), vec![final_step(&["ok"], "ok")], 1).await;
    assert_eq!(r.result.unwrap().turns_run, 1);
    assert_eq!(t.sent().await[0].subject, "Re: new");
    // Restart: UID 10 was handled, so only 11 runs.
    let t = Arc::new(
        ScriptedTransport::new(InboxStatus { uid_validity: 1, uid_next: 12 }, vec![vec![from_me(10, "new", "new"), from_me(11, "newer", "newer")]])
            .in_sent_folder(vec!["pm10", "pm11"]),
    );
    let r = run_in(dir.path(), Arc::clone(&t), vec![final_step(&["ok"], "ok")], 1).await;
    assert_eq!(r.result.unwrap().turns_run, 1);
    assert_eq!(t.sent().await[0].subject, "Re: newer");
}

#[tokio::test]
async fn a_failed_reply_leaves_the_email_unfiled_and_moves_on() {
    let t = Arc::new(
        ScriptedTransport::new(status(), vec![vec![from_me(10, "a", "a"), from_me(11, "b", "b")]])
            .in_sent_folder(vec!["pm10", "pm11"])
            .failing_send(),
    );
    let r = run(Arc::clone(&t), vec![final_step(&["x"], "x"), final_step(&["y"], "y")], 0).await;
    let report = r.result.unwrap();
    assert_eq!(report.turns_run, 2);
    assert_eq!(report.answered, 0);
    assert!(t.filed().await.is_empty());
}

/// In-process, like Matrix and Telegram, each turn's planner starts fresh
/// (the thread keeps one session id and memory partition); the daemon
/// mode carries model history per thread. What must hold here: a reply in
/// a thread is answered in that thread.
#[tokio::test]
async fn a_reply_in_a_thread_is_answered_in_that_thread() {
    let first = from_me(10, "Plan", "first question");
    let mut second = from_me(11, "Re: Plan", "second question");
    second.in_reply_to = Some("m10@proton.me".into());
    second.references = vec!["m10@proton.me".into()];
    let t = Arc::new(
        ScriptedTransport::new(status(), vec![vec![first], vec![second]]).in_sent_folder(vec!["pm10", "pm11"]),
    );
    let r = run(
        Arc::clone(&t),
        vec![final_step(&["one"], "one"), final_step(&["two"], "two")],
        2,
    )
    .await;
    assert_eq!(r.result.unwrap().answered, 2);
    let sent = t.sent().await;
    assert_eq!(sent[1].subject, "Re: Plan");
    assert_eq!(sent[1].in_reply_to.as_deref(), Some("m11@proton.me"));
    assert_eq!(sent[1].references, vec!["m10@proton.me", "m11@proton.me"]);
}

#[tokio::test]
async fn long_emails_and_attachments_are_noted_in_the_reply() {
    let mut m = from_me(10, "big", &"x".repeat(40 * 1024));
    m.attachments = vec!["a.pdf".into()];
    let t = Arc::new(ScriptedTransport::new(status(), vec![vec![m]]).in_sent_folder(vec!["pm10"]));
    let r = run(Arc::clone(&t), vec![final_step(&["done"], "done")], 1).await;
    r.result.unwrap();
    let body = &t.sent().await[0].body;
    assert!(body.starts_with("done\n\n"), "{body}");
    assert!(body.contains(CUT_NOTE), "{body}");
    assert!(body.contains("a.pdf"), "{body}");
}

#[tokio::test]
async fn a_rejected_login_ends_the_session() {
    let t = Arc::new(
        ScriptedTransport::new(status(), vec![]).then_fail(|| TransportError::Unauthorized("NO".into())),
    );
    let r = run(Arc::clone(&t), vec![], 0).await;
    match r.result {
        Err(LoopError::Fatal(e)) => assert!(e.contains("connect proton"), "{e}"),
        other => panic!("expected Fatal, got {other:?}"),
    }
}

#[tokio::test]
async fn a_second_channel_on_the_same_instance_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let _held = lock::acquire(dir.path()).unwrap();
    let t = Arc::new(ScriptedTransport::new(status(), vec![]));
    let r = run_in(dir.path(), t, vec![], 0).await;
    assert_eq!(r.result, Err(LoopError::Fatal(lock::BUSY.to_string())));
}
```

- [ ] **Step 2: Run them to see them fail** — `cargo test -p aivyx-proton` → FAIL to compile.

- [ ] **Step 3: Write the modules** — above each test module:

`proton_channel.rs`:
```rust
//! [`ProtonChannel`] — the `ChannelContext` for one email turn. The reply
//! is sent once complete: streamed text is buffered and, at `finalize`,
//! kept for the mail loop, which emails it (`session::answer`) and files
//! the original.

use std::fmt::Write as _;
use std::sync::Mutex;

use async_trait::async_trait;

use aivyx_capability::TrustTier;
use aivyx_core::{
    CancellationToken, ChannelContext, ChannelError, ChannelPlatform, SessionId, StreamEvent,
    TurnOutcome,
};

pub struct ProtonChannel {
    session: SessionId,
    partition: String,
    token: Mutex<CancellationToken>,
    buffer: Mutex<String>,
    reply: Mutex<Option<String>>,
}

impl ProtonChannel {
    /// `session` is the email thread's conversation; `partition` its
    /// memory namespace (`session::partition`).
    pub(crate) fn new(session: SessionId, partition: String) -> Self {
        ProtonChannel {
            session,
            partition,
            token: Mutex::new(CancellationToken::new()),
            buffer: Mutex::new(String::new()),
            reply: Mutex::new(None),
        }
    }

    /// The finished reply, once `finalize` ran.
    pub(crate) fn take_reply(&self) -> Option<String> {
        self.reply.lock().expect("reply mutex poisoned").take()
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
        StreamEvent::ToolCallFinished {
            tool_name,
            outcome_summary,
            ..
        } => {
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

#[async_trait]
impl ChannelContext for ProtonChannel {
    fn channel_name(&self) -> &str {
        "aivyx-proton"
    }

    fn platform(&self) -> ChannelPlatform {
        ChannelPlatform::Email
    }

    fn trust_tier(&self) -> TrustTier {
        // Only mail `policy::decide` approved reaches a channel: from you
        // (proven by Sent) or an allowlisted sender with Proton's DMARC pass.
        TrustTier::SemiTrusted
    }

    fn session_id(&self) -> SessionId {
        self.session
    }

    fn session_partition(&self) -> Option<String> {
        Some(self.partition.clone())
    }

    async fn stream_event(&self, event: StreamEvent<'_>) -> Result<(), ChannelError> {
        let mut buf = self
            .buffer
            .lock()
            .map_err(|e| ChannelError::Send(format!("ProtonChannel buffer poisoned: {e}")))?;
        append_event(&mut buf, &event);
        Ok(())
    }

    async fn finalize(&self, outcome: &TurnOutcome) -> Result<(), ChannelError> {
        let mut out = {
            let mut buf = self
                .buffer
                .lock()
                .map_err(|e| ChannelError::Send(format!("ProtonChannel buffer poisoned: {e}")))?;
            std::mem::take(&mut *buf)
        };
        out.push_str(&finalize_footer(outcome));
        let text = if out.trim().is_empty() {
            "(no reply)".to_string()
        } else {
            out
        };
        *self.reply.lock().expect("reply mutex poisoned") = Some(text);
        Ok(())
    }

    fn cancellation_token(&self) -> CancellationToken {
        self.token.lock().expect("token mutex poisoned").clone()
    }

    fn reset_cancellation(&self) {
        *self.token.lock().expect("token mutex poisoned") = CancellationToken::new();
    }
}
```

`session.rs`:
```rust
//! The mail loop both modes share, and the in-process session.
//!
//! [`run_mail_loop`] owns everything mode-independent: the per-instance
//! lock, the resume cursor, backoff, the trust check ([`screen`]) and the
//! reply ([`answer`]). A [`TurnRunner`] runs each approved email: the
//! agent here ([`run_proton_session_with_transport`]), or the daemon
//! (`aivyx_channel::proton_daemon_frontend`).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sha2::{Digest, Sha256};

use aivyx_capability::CapabilitySet;
use aivyx_core::{
    agent::ConcreteAgent, llm_planner::LlmPlanner, planner::ToolRegistry, Agent, AgentId,
    AuditHook, CancellationToken, ChannelContext, LlmPlannerConfig, Message, SessionId,
};
use aivyx_llm::LlmProvider;
use aivyx_storage::Storage;

use crate::message::{cap, without_quoted_history, MailMessage, TURN_TEXT_CAP};
use crate::outgoing::OutgoingMail;
use crate::policy::{decide, Decision, ProtonPolicy, Sender};
use crate::proton_channel::ProtonChannel;
use crate::transport::{MailTransport, TransportError};
use crate::{lock, state};

/// Added to the reply when the email was cut to [`TURN_TEXT_CAP`].
pub const CUT_NOTE: &str = "(Your email was longer than 32 KiB, so I read only the start.)";

/// Added to the reply when the email had attachments.
pub fn attachments_note(names: &[String]) -> String {
    format!("(I can't open attachments, so I ignored: {}.)", names.join(", "))
}

/// One approved email, ready to run.
#[derive(Debug, Clone)]
pub struct Turn {
    pub message: MailMessage,
    pub sender: Sender,
    /// What the agent reads: sender, subject and the new part of the body.
    pub text: String,
    /// The body's first non-empty line — where `/team …` is read.
    pub first_line: String,
    /// Said at the end of the reply (cut, attachments ignored).
    pub notes: Vec<String>,
}

impl Turn {
    pub fn thread(&self) -> String {
        self.message.thread_root()
    }
}

/// The trust check (`policy::decide`, consulting Sent for mail claiming to
/// be from you). `Ok(None)`: ignored silently.
pub async fn screen<T: MailTransport + ?Sized>(
    transport: &T,
    policy: &ProtonPolicy,
    m: MailMessage,
) -> Result<Option<Turn>, TransportError> {
    let in_sent = match (policy.claims_operator(&m), m.internal_id.as_deref()) {
        (true, Some(id)) => transport.in_sent(id).await?,
        _ => false,
    };
    let sender = match decide(policy, &m, in_sent) {
        Decision::Turn(s) => s,
        Decision::Ignore(_) => return Ok(None),
    };
    let body = without_quoted_history(&m.text);
    let (body, cut) = cap(&body, TURN_TEXT_CAP);
    let mut notes = Vec::new();
    if cut {
        notes.push(CUT_NOTE.to_string());
    }
    if !m.attachments.is_empty() {
        notes.push(attachments_note(&m.attachments));
    }
    let first_line = body
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string();
    let text = format!(
        "Email from {}\nSubject: {}\n\n{body}",
        m.from.as_deref().unwrap_or(""),
        m.subject
    );
    Ok(Some(Turn {
        message: m,
        sender,
        text,
        first_line,
        notes,
    }))
}

pub fn reply_body(answer: &str, notes: &[String]) -> String {
    if notes.is_empty() {
        answer.to_string()
    } else {
        format!("{answer}\n\n{}", notes.join("\n"))
    }
}

/// Emails `answer` to the sender from `channel_address`, in the thread,
/// then files the original with a copy of the reply. `Err` only when the
/// send failed (the original then stays unread in the inbox).
pub async fn answer<T: MailTransport + ?Sized>(
    transport: &T,
    policy: &ProtonPolicy,
    turn: &Turn,
    answer: &str,
) -> Result<(), TransportError> {
    let to = turn.message.from.clone().into_iter().collect();
    let reply = OutgoingMail::reply(
        &policy.channel_address,
        to,
        &turn.message,
        reply_body(answer, &turn.notes),
    );
    transport.send(&reply).await?;
    if let Err(e) = transport.file_handled(turn.message.uid, &reply).await {
        eprintln!("aivyx-proton: replied, but couldn't file the email: {e}");
    }
    Ok(())
}

/// Runs one approved email.
#[async_trait]
pub trait TurnRunner: Send {
    /// `Ok(Some(text))`: the reply. `Ok(None)`: no reply (the email stays
    /// in the inbox). `Err`: the runner can't go on; the loop stops.
    async fn run(&mut self, turn: &Turn) -> Result<Option<String>, String>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProtonSessionReport {
    pub turns_run: usize,
    pub answered: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopError {
    /// Bridge rejected the login or its certificate changed, or the
    /// channel can't start (another one is running): stop.
    Fatal(String),
    /// The runner failed (e.g. the daemon went away): the binary may fall
    /// back to in-process.
    Runner(String),
}

impl std::fmt::Display for LoopError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoopError::Fatal(e) | LoopError::Runner(e) => f.write_str(e),
        }
    }
}

/// The receive loop. `state_dir` is the instance's `proton_dir()`: it
/// holds the lock and the cursor.
pub async fn run_mail_loop<T: MailTransport + ?Sized, R: TurnRunner>(
    transport: &T,
    policy: &ProtonPolicy,
    state_dir: &Path,
    runner: &mut R,
    shutdown: CancellationToken,
) -> Result<ProtonSessionReport, LoopError> {
    let _lock = lock::acquire(state_dir).map_err(LoopError::Fatal)?;
    let status = transport
        .inbox_status()
        .await
        .map_err(|e| LoopError::Fatal(e.to_string()))?;
    let mut cursor = state::resume(state::load(state_dir).map_err(LoopError::Fatal)?, status);
    state::save(state_dir, &cursor).map_err(LoopError::Fatal)?;
    let mut report = ProtonSessionReport::default();
    let mut backoff = Duration::from_secs(1);

    loop {
        let batch = tokio::select! {
            biased;
            _ = shutdown.cancelled() => return Ok(report),
            r = transport.wait_for_new(cursor.last_uid) => r,
        };
        let mut transient: Option<TransportError> = None;
        match batch {
            Ok(batch) => {
                backoff = Duration::from_secs(1);
                let after = cursor.last_uid;
                for m in batch.into_iter().filter(|m| m.uid > after) {
                    let uid = m.uid;
                    match screen(transport, policy, m).await {
                        Ok(Some(turn)) => {
                            if let Some(text) = runner.run(&turn).await.map_err(LoopError::Runner)? {
                                match answer(transport, policy, &turn, &text).await {
                                    Ok(()) => report.answered += 1,
                                    Err(e) if e.is_fatal() => {
                                        cursor.last_uid = uid;
                                        let _ = state::save(state_dir, &cursor);
                                        return Err(LoopError::Fatal(e.to_string()));
                                    }
                                    Err(e) => eprintln!(
                                        "aivyx-proton: couldn't send the reply ({e}); the email \
                                         stays unread in your inbox"
                                    ),
                                }
                            }
                            report.turns_run += 1;
                        }
                        Ok(None) => {}
                        Err(e) if e.is_fatal() => return Err(LoopError::Fatal(e.to_string())),
                        Err(e) => {
                            // Retried after the backoff, not skipped.
                            transient = Some(e);
                            break;
                        }
                    }
                    cursor.last_uid = uid;
                    state::save(state_dir, &cursor).map_err(LoopError::Fatal)?;
                    if shutdown.is_cancelled() {
                        return Ok(report);
                    }
                }
            }
            Err(e) if e.is_fatal() => return Err(LoopError::Fatal(e.to_string())),
            Err(e) => transient = Some(e),
        }
        if let Some(e) = transient {
            eprintln!("aivyx-proton: {e}; retrying in {}s", backoff.as_secs());
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => return Ok(report),
                _ = tokio::time::sleep(backoff) => {}
            }
            backoff = (backoff * 2).min(Duration::from_secs(60));
        }
    }
}

/// Same fields and meaning as `aivyx_matrix::MatrixSessionConfig`.
#[derive(Clone)]
pub struct ProtonSessionConfig {
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

fn build_agent(
    config: ProtonSessionConfig,
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
    let agent = ConcreteAgent::new(
        AgentId::new(),
        config.capabilities,
        registry,
        audit,
        move || {
            Box::new(LlmPlanner::new(
                Arc::clone(&provider),
                Arc::clone(&registry_for_factory),
                planner_config.clone(),
            ))
        },
    )
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

/// A thread's memory namespace: a short hash of its root `Message-ID`
/// (raw IDs carry `@`, `<`, `>` and other characters scope strings
/// shouldn't).
pub fn partition(thread_root: &str) -> String {
    let digest = Sha256::digest(thread_root.as_bytes());
    format!("proton-{}", &hex::encode(digest.as_slice())[..16])
}

/// Threads remembered at once; past this the map starts over (a thread
/// then simply begins a fresh conversation).
const MAX_THREADS: usize = 256;

/// One conversation (`SessionId`) per email thread.
#[derive(Default)]
struct Threads(HashMap<String, SessionId>);

impl Threads {
    fn session_for(&mut self, root: &str) -> SessionId {
        if self.0.len() >= MAX_THREADS && !self.0.contains_key(root) {
            self.0.clear();
        }
        // `SessionId::default()` is a fresh random id.
        *self.0.entry(root.to_string()).or_default()
    }
}

struct AgentRunner<A> {
    agent: A,
    threads: Threads,
    shutdown: CancellationToken,
}

#[async_trait]
impl<A: Agent> TurnRunner for AgentRunner<A> {
    async fn run(&mut self, turn: &Turn) -> Result<Option<String>, String> {
        let root = turn.thread();
        let session = self.threads.session_for(&root);
        let channel = ProtonChannel::new(session, partition(&root));
        let fut = self.agent.turn(Message::text(session, &turn.text), &channel);
        tokio::pin!(fut);
        tokio::select! {
            biased;
            _ = &mut fut => Ok(channel.take_reply()),
            _ = self.shutdown.cancelled() => {
                // Stopping: let the turn wind down, send nothing.
                channel.cancellation_token().cancel();
                let _ = (&mut fut).await;
                Ok(None)
            }
        }
    }
}

/// The in-process session over `transport`. Production passes the
/// instance's `proton_dir()` as `state_dir` and the one
/// `BridgeTransport` built for the run.
#[allow(clippy::too_many_arguments)]
pub async fn run_proton_session_with_transport<T: MailTransport + ?Sized>(
    transport: &T,
    policy: &ProtonPolicy,
    state_dir: &Path,
    config: ProtonSessionConfig,
    provider: Arc<dyn LlmProvider>,
    audit: Arc<dyn AuditHook>,
    checkpointer: Option<Arc<aivyx_core::GitCheckpointer>>,
    shutdown: CancellationToken,
) -> Result<ProtonSessionReport, LoopError> {
    let mut runner = AgentRunner {
        agent: build_agent(config, provider, audit, checkpointer),
        threads: Threads::default(),
        shutdown: shutdown.clone(),
    };
    run_mail_loop(transport, policy, state_dir, &mut runner, shutdown).await
}
```

- [ ] **Step 4: Run** `cargo test -p aivyx-proton && cargo clippy -p aivyx-proton --all-targets -- -D warnings` → PASS.

- [ ] **Step: Commit**

```bash
git add crates/aivyx-proton/src
git commit -s -m "feat(proton): ProtonChannel and the shared mail loop (in-process session)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 11: The `proton.*` tools and the tool process

**Files:**
- Create: `crates/aivyx-proton/src/tools/{mod,search,read,draft,send}.rs`
- Replace: `crates/aivyx-proton/src/main.rs` (the placeholder from Task 2)
- Modify: `crates/aivyx-proton/src/lib.rs` (add `pub mod tools;` — the final file is below)

**Interfaces:**
- Consumes: `transport::{MailTransport, SearchQuery}`, `outgoing`, `message`, `credentials`, `aivyx_tool::multi_harness::run_multi_tool_subprocess`.
- Produces: `tools::{Mail = Arc<dyn MailTransport>, ProtonSearch::new(Mail), ProtonRead::new(Mail), ProtonDraft::new(Mail, from: String), ProtonSend::new(Mail, from: String)}`; the `aivyx-proton` binary.

Ids are All Mail UIDs. `proton.search` returns at most 25 (default 10). A reply defaults to the original sender and `Re:` subject and is threaded. Approval for `proton.send` needs no code here: the turn loop pauses every `email.send` call (as for `gmail.send`).

- [ ] **Step 1: Write the failing tests** — each file holding only its test module(s):

`tools/mod.rs`:
```rust
#[cfg(test)]
pub(crate) mod test_support {
    use aivyx_core::{
        AgentId, CancellationToken, ChannelContext, ChannelError, ChannelPlatform, MessageOrigin,
        NullAuditHook, SessionId, StreamEvent, ToolContext, TurnId, TurnOutcome,
    };

    struct NoopChannel;

    #[async_trait::async_trait]
    impl ChannelContext for NoopChannel {
        fn channel_name(&self) -> &str {
            "t"
        }
        fn platform(&self) -> ChannelPlatform {
            ChannelPlatform::Local
        }
        fn trust_tier(&self) -> aivyx_capability::TrustTier {
            aivyx_capability::TrustTier::Trusted
        }
        fn session_id(&self) -> SessionId {
            SessionId::new()
        }
        async fn stream_event(&self, _e: StreamEvent<'_>) -> Result<(), ChannelError> {
            Ok(())
        }
        async fn finalize(&self, _o: &TurnOutcome) -> Result<(), ChannelError> {
            Ok(())
        }
        fn cancellation_token(&self) -> CancellationToken {
            CancellationToken::new()
        }
    }

    pub fn ctx() -> ToolContext<'static> {
        ToolContext {
            agent_id: AgentId::new(),
            session_id: SessionId::new(),
            turn_id: TurnId::new(),
            channel: Box::leak(Box::new(NoopChannel)),
            audit: Box::leak(Box::new(NullAuditHook)),
            cancellation: Box::leak(Box::new(CancellationToken::new())),
            message_origin: MessageOrigin::Operator,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_needs_a_body_and_a_recipient_or_a_reply_id() {
        assert!(parse_compose(&json!({"to": ["a@x.com"], "subject": "s"})).is_err());
        assert!(parse_compose(&json!({"body": "b", "subject": "s"})).is_err());
        assert!(parse_compose(&json!({"to": ["a@x.com"], "body": "b"})).is_err());
        assert!(parse_compose(&json!({"body": "b", "reply_to_id": 4})).is_ok());
        assert!(parse_compose(&json!({"to": ["not-an-address"], "subject": "s", "body": "b"})).is_err());
        assert!(parse_compose(&json!({"to": ["a@x.com>\r\nBcc: e@x"], "subject": "s", "body": "b"})).is_err());
        assert!(parse_compose(&json!({"to": ["a@x.com"], "subject": "s\nBcc: e@x", "body": "b"})).is_err());
        let c = parse_compose(&json!({"to": ["a@x.com"], "cc": ["c@x.com"], "subject": "s", "body": "b"})).unwrap();
        assert_eq!(c.to, vec!["a@x.com"]);
        assert_eq!(c.cc, vec!["c@x.com"]);
    }

    #[test]
    fn summaries_are_short() {
        let m = MailMessage { uid: 3, text: "y".repeat(500), ..Default::default() };
        let s = summary(&m);
        assert_eq!(s["id"], 3);
        assert_eq!(s["snippet"].as_str().unwrap().len(), 200);
    }
}
```

`tools/search.rs`:
```rust
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::message::MailMessage;
    use crate::tools::test_support::ctx;
    use crate::transport::{InboxStatus, ScriptedTransport};

    fn mail(uid: u32) -> MailMessage {
        MailMessage { uid, subject: format!("s{uid}"), from: Some("a@x.com".into()), text: "hello".into(), ..Default::default() }
    }

    #[tokio::test]
    async fn results_are_newest_first_and_capped() {
        let t = Arc::new(ScriptedTransport::new(InboxStatus::default(), vec![]).with_all_mail((1..=30).map(mail).collect()));
        let tool = ProtonSearch::new(t.clone());
        assert!(tool.output_is_untrusted());
        assert_eq!(tool.required_scope(&json!({})).to_string(), "email.read");
        let out = tool.execute(json!({"subject": "s", "max_results": 99}), &ctx()).await;
        let ToolOutcome::Completed { output, .. } = out else { panic!("{out:?}") };
        let msgs = output["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), MAX_RESULTS);
        assert_eq!(msgs[0]["id"], 30);
        assert_eq!(t.searches().await[0].subject.as_deref(), Some("s"));
    }

    #[test]
    fn bad_input_is_refused_before_any_search() {
        assert!(parse(&json!({"since": "yesterday"})).is_err());
        assert!(parse(&json!({"text": "a\r\nb"})).is_err());
        assert!(parse(&json!({"max_results": 0})).is_err());
        assert_eq!(parse(&json!({})).unwrap().max_results, DEFAULT_RESULTS);
    }
}
```

`tools/read.rs`:
```rust
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::message::MailMessage;
    use crate::tools::test_support::ctx;
    use crate::transport::{InboxStatus, ScriptedTransport};

    #[tokio::test]
    async fn a_message_is_returned_capped_with_attachment_names() {
        let m = MailMessage {
            uid: 4,
            subject: "big".into(),
            text: "z".repeat(TOOL_TEXT_CAP + 10),
            attachments: vec!["a.pdf".into()],
            ..Default::default()
        };
        let t = Arc::new(ScriptedTransport::new(InboxStatus::default(), vec![]).with_all_mail(vec![m]));
        let tool = ProtonRead::new(t);
        assert!(tool.output_is_untrusted());
        let out = tool.execute(json!({"id": 4}), &ctx()).await;
        let ToolOutcome::Completed { output, .. } = out else { panic!("{out:?}") };
        assert_eq!(output["text"].as_str().unwrap().len(), TOOL_TEXT_CAP);
        assert_eq!(output["truncated"], true);
        assert_eq!(output["attachments"][0], "a.pdf");
        let missing = tool.execute(json!({"id": 5}), &ctx()).await;
        assert!(matches!(missing, ToolOutcome::Failed(_)));
        let bad = tool.execute(json!({"id": "x"}), &ctx()).await;
        assert!(matches!(bad, ToolOutcome::Failed(_)));
    }
}
```

`tools/draft.rs`:
```rust
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::message::MailMessage;
    use crate::tools::test_support::ctx;
    use crate::transport::{InboxStatus, ScriptedTransport};

    #[tokio::test]
    async fn a_reply_draft_is_threaded_to_the_original_sender() {
        let original = MailMessage {
            uid: 7,
            subject: "Invoice".into(),
            from: Some("ann@x.com".into()),
            message_id: Some("inv@x.com".into()),
            ..Default::default()
        };
        let t = Arc::new(ScriptedTransport::new(InboxStatus::default(), vec![]).with_all_mail(vec![original]));
        let tool = ProtonDraft::new(t.clone(), "me@proton.me".into());
        assert_eq!(tool.required_scope(&json!({})).to_string(), "email.write");
        let out = tool.execute(json!({"reply_to_id": 7, "body": "Paid."}), &ctx()).await;
        assert!(matches!(out, ToolOutcome::Completed { .. }), "{out:?}");
        let d = &t.drafts().await[0];
        assert_eq!(d.from, "me@proton.me");
        assert_eq!(d.to, vec!["ann@x.com"]);
        assert_eq!(d.subject, "Re: Invoice");
        assert_eq!(d.in_reply_to.as_deref(), Some("inv@x.com"));
        assert!(t.sent().await.is_empty(), "a draft is never sent");
    }

    #[tokio::test]
    async fn replying_to_a_missing_message_fails() {
        let t = Arc::new(ScriptedTransport::new(InboxStatus::default(), vec![]));
        let tool = ProtonDraft::new(t, "me@proton.me".into());
        let out = tool.execute(json!({"reply_to_id": 7, "body": "x"}), &ctx()).await;
        assert!(matches!(out, ToolOutcome::Failed(_)));
    }
}
```

`tools/send.rs`:
```rust
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::tools::test_support::ctx;
    use crate::transport::{InboxStatus, ScriptedTransport};

    #[tokio::test]
    async fn a_new_message_is_sent_from_the_configured_address() {
        let t = Arc::new(ScriptedTransport::new(InboxStatus::default(), vec![]));
        let tool = ProtonSend::new(t.clone(), "me@proton.me".into());
        assert_eq!(tool.required_scope(&json!({})).to_string(), "email.send");
        let out = tool
            .execute(json!({"to": ["ann@x.com"], "cc": ["bob@y.com"], "subject": "Hi", "body": "Hello"}), &ctx())
            .await;
        let ToolOutcome::Completed { output, .. } = out else { panic!("{out:?}") };
        assert_eq!(output["to"], json!(["ann@x.com", "bob@y.com"]));
        let s = &t.sent().await[0];
        assert_eq!(s.from, "me@proton.me");
        assert_eq!(s.subject, "Hi");
    }

    #[tokio::test]
    async fn a_failed_send_is_a_failure() {
        let t = Arc::new(ScriptedTransport::new(InboxStatus::default(), vec![]).failing_send());
        let tool = ProtonSend::new(t, "me@proton.me".into());
        let out = tool.execute(json!({"to": ["ann@x.com"], "subject": "Hi", "body": "x"}), &ctx()).await;
        assert!(matches!(out, ToolOutcome::Failed(_)));
    }
}
```

- [ ] **Step 2: Run them to see them fail** — `cargo test -p aivyx-proton tools::` → FAIL to compile.

- [ ] **Step 3: Write the tools** — above each test module:

`tools/mod.rs`:
```rust
//! The `proton.*` tools, served by the `aivyx-proton` tool process. Each
//! holds the shared [`MailTransport`] (Bridge in production).
//!
//! - `proton.search` / `proton.read` — `email.read`, output untrusted
//!   (Picket + Bulwark see mail content before the model does).
//! - `proton.draft` — `email.write`: saved to Drafts, never sent.
//! - `proton.send` — `email.send`: pauses for approval on every call, like
//!   `gmail.send` (the turn loop does this for the `email.send` scope).
//!
//! All four need `Trusted`, like Gmail's, so they are not available inside
//! an email turn (`SemiTrusted`).

pub mod draft;
pub mod read;
pub mod search;
pub mod send;

pub use draft::ProtonDraft;
pub use read::ProtonRead;
pub use search::ProtonSearch;
pub use send::ProtonSend;

use std::sync::Arc;

use serde_json::{json, Value};

use aivyx_core::{AivyxError, ToolId, ToolOutcome};

use crate::message::{cap, MailMessage};
use crate::outgoing::{re_subject, OutgoingMail};
use crate::transport::MailTransport;

pub type Mail = Arc<dyn MailTransport>;

pub(crate) fn fail(tool: ToolId, name: &str, detail: impl std::fmt::Display) -> ToolOutcome {
    ToolOutcome::Failed(AivyxError::Tool {
        tool,
        detail: format!("{name}: {detail}"),
    })
}

/// An optional string field; present but not a string is an error.
pub(crate) fn opt_str(input: &Value, key: &str) -> Result<Option<String>, String> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("`{key}` must be a string")),
    }
}

/// The search-result view of a message.
pub(crate) fn summary(m: &MailMessage) -> Value {
    let (snippet, _) = cap(m.text.trim(), 200);
    json!({
        "id": m.uid,
        "from": m.from,
        "subject": m.subject,
        "date": m.date,
        "snippet": snippet,
    })
}

/// What `proton.draft` and `proton.send` take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Compose {
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: Option<String>,
    pub body: String,
    /// A `proton.search` / `proton.read` id to reply to.
    pub reply_to_id: Option<u32>,
}

fn addresses(input: &Value, key: &str) -> Result<Vec<String>, String> {
    let list = match input.get(key) {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(a)) => a,
        Some(_) => return Err(format!("`{key}` must be a list of addresses")),
    };
    list.iter()
        .map(|v| {
            let a = v.as_str().ok_or_else(|| format!("`{key}` must be a list of addresses"))?.trim();
            if !a.contains('@') || a.contains(['<', '>', ' ', '\r', '\n', ',']) {
                return Err(format!("`{key}`: {a:?} isn't an email address"));
            }
            Ok(a.to_string())
        })
        .collect()
}

pub(crate) fn parse_compose(input: &Value) -> Result<Compose, String> {
    let body = opt_str(input, "body")?.ok_or("`body` is required")?;
    let reply_to_id = match input.get("reply_to_id") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or("`reply_to_id` must be an id from proton.search")?,
        ),
    };
    let subject = opt_str(input, "subject")?;
    if subject.as_deref().is_some_and(|s| s.contains(['\r', '\n'])) {
        return Err("`subject` must be one line".into());
    }
    let c = Compose {
        to: addresses(input, "to")?,
        cc: addresses(input, "cc")?,
        subject,
        body,
        reply_to_id,
    };
    if c.reply_to_id.is_none() && c.to.is_empty() {
        return Err("give `to`, or `reply_to_id` to answer a message".into());
    }
    if c.reply_to_id.is_none() && c.subject.is_none() {
        return Err("`subject` is required for a new message".into());
    }
    Ok(c)
}

/// Builds the message; a reply is threaded and defaults to the original's
/// sender and `Re:` subject.
pub(crate) async fn build(mail: &dyn MailTransport, from: &str, c: Compose) -> Result<OutgoingMail, String> {
    let Some(id) = c.reply_to_id else {
        return Ok(OutgoingMail::new(from, c.to, c.cc, c.subject.as_deref().unwrap_or(""), c.body));
    };
    let original = mail
        .fetch(id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no message with id {id}"))?;
    let to = if c.to.is_empty() {
        original.from.clone().into_iter().collect()
    } else {
        c.to
    };
    let mut out = OutgoingMail::reply(from, to, &original, c.body);
    out.cc = c.cc;
    if let Some(s) = c.subject {
        out.subject = re_subject(&s);
    }
    Ok(out)
}

pub(crate) fn compose_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "to": {"type": "array", "items": {"type": "string"}, "description": "Recipient addresses. Optional for a reply (defaults to the original sender)."},
            "cc": {"type": "array", "items": {"type": "string"}},
            "subject": {"type": "string", "description": "Required for a new message; a reply defaults to Re: <original subject>."},
            "body": {"type": "string", "description": "Plain text."},
            "reply_to_id": {"type": "integer", "description": "An id from proton.search or proton.read, to reply in that thread."}
        },
        "required": ["body"],
        "additionalProperties": false
    })
}
```

`tools/search.rs`:
```rust
//! `proton.search` — search All Mail.

use async_trait::async_trait;
use serde_json::{json, Value};

use aivyx_capability::Scope;
use aivyx_core::{Tool, ToolContext, ToolId, ToolOutcome, Verification};

use super::{fail, opt_str, summary, Mail};
use crate::transport::SearchQuery;

const DEFAULT_RESULTS: usize = 10;
const MAX_RESULTS: usize = 25;

pub struct ProtonSearch {
    id: ToolId,
    schema: Value,
    mail: Mail,
}

impl ProtonSearch {
    pub fn new(mail: Mail) -> Self {
        ProtonSearch {
            id: ToolId::new(),
            schema: json!({
                "type": "object",
                "properties": {
                    "from": {"type": "string"},
                    "to": {"type": "string"},
                    "subject": {"type": "string"},
                    "text": {"type": "string", "description": "Words anywhere in the message."},
                    "since": {"type": "string", "description": "YYYY-MM-DD, inclusive."},
                    "before": {"type": "string", "description": "YYYY-MM-DD, exclusive."},
                    "max_results": {"type": "integer", "minimum": 1, "maximum": MAX_RESULTS}
                },
                "additionalProperties": false
            }),
            mail,
        }
    }
}

fn parse(input: &Value) -> Result<SearchQuery, String> {
    let max_results = match input.get("max_results") {
        None | Some(Value::Null) => DEFAULT_RESULTS,
        Some(v) => v
            .as_u64()
            .filter(|n| *n >= 1)
            .ok_or("`max_results` must be a positive integer")?
            .min(MAX_RESULTS as u64) as usize,
    };
    let q = SearchQuery {
        from: opt_str(input, "from")?,
        to: opt_str(input, "to")?,
        subject: opt_str(input, "subject")?,
        text: opt_str(input, "text")?,
        since: opt_str(input, "since")?,
        before: opt_str(input, "before")?,
        max_results,
    };
    q.to_imap()?;
    Ok(q)
}

#[async_trait]
impl Tool for ProtonSearch {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "proton.search"
    }

    fn output_is_untrusted(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Search the operator's Proton Mail (All Mail) by `from`, `to`, \
         `subject`, `text` (anywhere in the message) and a `since` / \
         `before` date range (YYYY-MM-DD); every given field must match. \
         Returns up to `max_results` (default 10, at most 25) messages, \
         newest first, as `{id, from, subject, date, snippet}`. Pass an \
         `id` to `proton.read` for the whole message."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("email.read").expect("email.read is in KNOWN_BASES")
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let q = match parse(&input) {
            Ok(q) => q,
            Err(e) => return fail(self.id, "proton.search", e),
        };
        match self.mail.search(&q).await {
            Ok(found) => ToolOutcome::Completed {
                output: json!({"messages": found.iter().map(summary).collect::<Vec<_>>()}),
                verified: Verification::NotApplicable,
            },
            Err(e) => fail(self.id, "proton.search", e),
        }
    }
}
```

`tools/read.rs`:
```rust
//! `proton.read` — one message, headers and text.

use async_trait::async_trait;
use serde_json::{json, Value};

use aivyx_capability::Scope;
use aivyx_core::{Tool, ToolContext, ToolId, ToolOutcome, Verification};

use super::{fail, Mail};
use crate::message::{cap, TOOL_TEXT_CAP};

pub struct ProtonRead {
    id: ToolId,
    schema: Value,
    mail: Mail,
}

impl ProtonRead {
    pub fn new(mail: Mail) -> Self {
        ProtonRead {
            id: ToolId::new(),
            schema: json!({
                "type": "object",
                "properties": {"id": {"type": "integer", "description": "An id from proton.search."}},
                "required": ["id"],
                "additionalProperties": false
            }),
            mail,
        }
    }
}

#[async_trait]
impl Tool for ProtonRead {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "proton.read"
    }

    fn output_is_untrusted(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Read one Proton Mail message by the `id` from proton.search: \
         from, to, cc, subject, date, message_id, the text (at most 32 KiB; \
         `truncated` says if it was cut) and attachment names (their \
         content is not available)."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("email.read").expect("email.read is in KNOWN_BASES")
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let Some(id) = input.get("id").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()) else {
            return fail(self.id, "proton.read", "`id` must be an id from proton.search");
        };
        match self.mail.fetch(id).await {
            Ok(Some(m)) => {
                let (text, truncated) = cap(&m.text, TOOL_TEXT_CAP);
                ToolOutcome::Completed {
                    output: json!({
                        "id": m.uid,
                        "from": m.from,
                        "to": m.to,
                        "cc": m.cc,
                        "subject": m.subject,
                        "date": m.date,
                        "message_id": m.message_id,
                        "text": text,
                        "truncated": truncated,
                        "attachments": m.attachments,
                    }),
                    verified: Verification::NotApplicable,
                }
            }
            Ok(None) => fail(self.id, "proton.read", format!("no message with id {id}")),
            Err(e) => fail(self.id, "proton.read", e),
        }
    }
}
```

`tools/draft.rs`:
```rust
//! `proton.draft` — save a draft to Drafts. Never sends.

use async_trait::async_trait;
use serde_json::{json, Value};

use aivyx_capability::Scope;
use aivyx_core::{Tool, ToolContext, ToolId, ToolOutcome, Verification};

use super::{build, compose_schema, fail, parse_compose, Mail};

pub struct ProtonDraft {
    id: ToolId,
    schema: Value,
    mail: Mail,
    from: String,
}

impl ProtonDraft {
    /// `from`: the address drafts are written from (`credentials.toml`'s
    /// `from_address`).
    pub fn new(mail: Mail, from: String) -> Self {
        ProtonDraft {
            id: ToolId::new(),
            schema: compose_schema(),
            mail,
            from,
        }
    }
}

#[async_trait]
impl Tool for ProtonDraft {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "proton.draft"
    }

    fn description(&self) -> &str {
        "Save a plain-text draft in the operator's Proton Mail Drafts \
         folder — it is NOT sent; the operator sends it from Proton Mail. \
         A new message needs `to`, `subject` and `body`; a reply needs \
         `reply_to_id` (from proton.search) and `body`, and is threaded, \
         addressed to the original sender and titled Re: <subject> unless \
         you give `to` / `subject`. Optional `cc`."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("email.write").expect("email.write is in KNOWN_BASES")
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let compose = match parse_compose(&input) {
            Ok(c) => c,
            Err(e) => return fail(self.id, "proton.draft", e),
        };
        let mail = match build(self.mail.as_ref(), &self.from, compose).await {
            Ok(m) => m,
            Err(e) => return fail(self.id, "proton.draft", e),
        };
        match self.mail.append_draft(&mail).await {
            Ok(()) => ToolOutcome::Completed {
                output: json!({"saved": true, "to": mail.to, "subject": mail.subject}),
                verified: Verification::Unverified,
            },
            Err(e) => fail(self.id, "proton.draft", e),
        }
    }
}
```

`tools/send.rs`:
```rust
//! `proton.send` — send now, through Bridge. The `email.send` scope makes
//! the turn loop pause for the operator's approval on every call.

use async_trait::async_trait;
use serde_json::{json, Value};

use aivyx_capability::Scope;
use aivyx_core::{Tool, ToolContext, ToolId, ToolOutcome, Verification};

use super::{build, compose_schema, fail, parse_compose, Mail};

pub struct ProtonSend {
    id: ToolId,
    schema: Value,
    mail: Mail,
    from: String,
}

impl ProtonSend {
    /// `from`: the address mail is sent from (`credentials.toml`'s
    /// `from_address`).
    pub fn new(mail: Mail, from: String) -> Self {
        ProtonSend {
            id: ToolId::new(),
            schema: compose_schema(),
            mail,
            from,
        }
    }
}

#[async_trait]
impl Tool for ProtonSend {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "proton.send"
    }

    fn description(&self) -> &str {
        "Send a plain-text email from the operator's Proton Mail account. \
         **It goes out immediately — there is no undo.** A new message \
         needs `to`, `subject` and `body`; a reply needs `reply_to_id` \
         (from proton.search) and `body`, and is threaded, addressed to the \
         original sender and titled Re: <subject> unless you give `to` / \
         `subject`. Optional `cc`. Asks the operator to approve every send."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("email.send").expect("email.send is in KNOWN_BASES")
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let compose = match parse_compose(&input) {
            Ok(c) => c,
            Err(e) => return fail(self.id, "proton.send", e),
        };
        let mail = match build(self.mail.as_ref(), &self.from, compose).await {
            Ok(m) => m,
            Err(e) => return fail(self.id, "proton.send", e),
        };
        match self.mail.send(&mail).await {
            Ok(()) => ToolOutcome::Completed {
                output: json!({"sent": true, "to": mail.recipients(), "subject": mail.subject, "message_id": mail.message_id}),
                verified: Verification::Unverified,
            },
            Err(e) => fail(self.id, "proton.send", e),
        }
    }
}
```

- [ ] **Step 4: The tool process** — replace `crates/aivyx-proton/src/main.rs`:

```rust
//! `aivyx-proton` — the `proton.*` tool process. The daemon spawns it via
//! `[[tool_process]] name = "proton"` (sandboxed like Gmail); it reads the
//! Bridge credential `aivyx-pa connect proton` wrote and serves
//! `proton.search`, `proton.read`, `proton.draft` and `proton.send`.

use std::process::ExitCode;
use std::sync::Arc;

use aivyx_core::Tool;
use aivyx_instance::InstancePaths;
use aivyx_proton::credentials;
use aivyx_proton::tools::{Mail, ProtonDraft, ProtonRead, ProtonSearch, ProtonSend};
use aivyx_tool::multi_harness::run_multi_tool_subprocess;

#[tokio::main]
async fn main() -> ExitCode {
    if std::env::args().len() > 1 {
        eprintln!(
            "aivyx-proton takes no arguments: the aivyx-pa daemon runs it as a \
             [[tool_process]]. Set it up with `aivyx-pa connect proton`."
        );
        return ExitCode::from(2);
    }
    let Some(path) = InstancePaths::current().ok().and_then(|i| credentials::path_for(&i)) else {
        eprintln!("aivyx-proton: can't resolve the instance's folders (is HOME set?)");
        return ExitCode::from(2);
    };
    let creds = match credentials::load(&path) {
        Ok(Some(c)) => c,
        Ok(None) => {
            eprintln!("aivyx-proton: not connected — run `aivyx-pa connect proton` first");
            return ExitCode::from(2);
        }
        Err(e) => {
            eprintln!("aivyx-proton: {e}");
            return ExitCode::from(2);
        }
    };
    // The tools never file handled mail, so the folder is unused here.
    let mail: Mail = match creds.transport("Aivyx") {
        Ok(t) => Arc::new(t),
        Err(e) => {
            eprintln!("aivyx-proton: {e}");
            return ExitCode::from(2);
        }
    };
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(ProtonSearch::new(Arc::clone(&mail))),
        Arc::new(ProtonRead::new(Arc::clone(&mail))),
        Arc::new(ProtonDraft::new(Arc::clone(&mail), creds.from_address.clone())),
        Arc::new(ProtonSend::new(Arc::clone(&mail), creds.from_address.clone())),
    ];
    match run_multi_tool_subprocess(tools, "aivyx-proton", None).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aivyx-proton: harness exited with error: {e}");
            ExitCode::from(1)
        }
    }
}
```

and `crates/aivyx-proton/src/lib.rs` is now:

```rust
//! `aivyx-proton` — Proton Mail for Aivyx PA, through Proton Mail Bridge's
//! local IMAP/SMTP: the email channel (`--channel proton`) and the
//! `proton.*` tools (the `aivyx-proton` tool process). Mail is decrypted
//! only by Bridge, on this machine.
//!
//! Shape: like `aivyx-matrix` — a transport trait + scripted double
//! (`transport`), the pure trust policy (`policy`), a channel
//! (`proton_channel`) and the receive loop (`session`), plus the real
//! Bridge transport (`bridge`, `smtp`, pinned `tls`). See
//! `docs/superpowers/specs/2026-10-12-proton-mail-design.md`.

pub mod bridge;
pub mod credentials;
pub mod lock;
pub mod message;
pub mod outgoing;
pub mod policy;
pub mod proton_channel;
pub mod session;
pub mod smtp;
pub mod state;
pub mod tls;
pub mod tools;
pub mod transport;

pub use session::{
    run_mail_loop, run_proton_session_with_transport, LoopError, ProtonSessionConfig,
    ProtonSessionReport, TurnRunner,
};

#[cfg(test)]
mod tests;
```

- [ ] **Step 5: Run** `cargo test -p aivyx-proton && cargo build -p aivyx-proton --bin aivyx-proton && cargo clippy -p aivyx-proton --all-targets -- -D warnings` → PASS. `./target/debug/aivyx-proton extra-arg` → exits 2 with the "takes no arguments" message.

- [ ] **Step: Commit**

```bash
git add crates/aivyx-proton/src
git commit -s -m "feat(proton): proton.search/read/draft/send and the aivyx-proton tool process" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 12: `[proton]` config

**Files:**
- Modify: `crates/aivyx-config/src/lib.rs` (`AivyxConfig::proton`, `ProtonConfig` after `MatrixConfig`, `RawProton`, two validators, the parse block before `// --- slack (Phase 108)`, the struct literal)
- Modify: `crates/aivyx-config/src/tests.rs` (two tests before `matrix_accepts_loopback_http_homeservers`)
- Modify: `crates/aivyx-cli/src/bin/aivyx.rs` (one line: `run_async` destructures `AivyxConfig` exhaustively, so the new field must be bound there or `aivyx-cli` stops building)

**Interfaces:**
- Produces: `pub struct ProtonConfig { imap_addr: String, smtp_addr: String, channel_address: Option<String>, own_addresses: Vec<String>, allowed_external_senders: Vec<String>, folder: String, team_run_channel: bool, team_trigger_rate_limit: Option<u32>, team_command_allowed_senders: Vec<String> }`; `AivyxConfig::proton: Option<ProtonConfig>`.

`[proton]` is `Some` when it sets any key. `own_addresses` is required; `channel_address` is needed only for the channel (tools-only setups omit it) and must be one of `own_addresses`. Addresses are lowercased. Watch out: the first `/// Phase 109 — [git] configuration` comment in the file is a *field* doc inside `AivyxConfig`; `ProtonConfig` goes before the struct-level one (`/// Phase 109 — [git] configuration for the git.status / git.diff tools (Amendment A12).`), right after `MatrixConfig`.

- [ ] **Step 1: Write the failing tests** — the `tests.rs` hunk below; run `cargo test -p aivyx-config proton` → FAIL to compile.
- [ ] **Step 2: Implement** — the `lib.rs` hunks:

```diff
diff --git a/crates/aivyx-config/src/lib.rs b/crates/aivyx-config/src/lib.rs
index cfe1e2f2..d59f7771 100644
--- a/crates/aivyx-config/src/lib.rs
+++ b/crates/aivyx-config/src/lib.rs
@@ -1042,6 +1042,9 @@ pub struct AivyxConfig {
     /// `[matrix]` channel config. `None` unless `[matrix]` sets
     /// `homeserver`; see [`MatrixConfig`].
     pub matrix: Option<MatrixConfig>,
+    /// `[proton]` — Proton Mail through Bridge. `None` unless the section
+    /// sets a key; see [`ProtonConfig`].
+    pub proton: Option<ProtonConfig>,
     /// Phase 108 — Slack channel config. `None` when no
     /// Slack field is set. Filled in whenever any Slack
     /// field is set so the startup banner can warn about
@@ -1823,6 +1826,31 @@ pub struct MatrixConfig {
     pub team_command_allowed_senders: Vec<String>,
 }
 
+/// Proton Mail through Proton Mail Bridge: the email channel
+/// (`--channel proton`) and the `proton.*` tools. The Bridge login is not
+/// config: `aivyx-pa connect proton` stores it.
+#[derive(Debug, Clone, PartialEq, Eq)]
+pub struct ProtonConfig {
+    /// Bridge's IMAP address. On this machine only (`127.0.0.1`, `localhost` or `[::1]`). Default `127.0.0.1:1143`.
+    pub imap_addr: String,
+    /// Bridge's SMTP address. On this machine only. Default `127.0.0.1:1025`.
+    pub smtp_addr: String,
+    /// The address of yours the assistant answers on, e.g. `you_aivyx@proton.me` or `you+assistant@proton.me`. Must also be in `own_addresses`. Needed for `--channel proton`, not for the tools.
+    pub channel_address: Option<String>,
+    /// Every address of your Proton account. Mail "from you" counts only if it is from one of these and in your Sent folder. The first is what `proton.draft` / `proton.send` send from.
+    pub own_addresses: Vec<String>,
+    /// Outside addresses that may email the assistant; only when Proton's own DMARC check passes for the sender's domain.
+    pub allowed_external_senders: Vec<String>,
+    /// The Proton folder handled mail (and the assistant's reply) is moved to. Created if missing. Default `Aivyx`.
+    pub folder: String,
+    /// Operator opt-in for `/team run <goal>` by email.
+    pub team_run_channel: bool,
+    /// Max `/team run` confirmations accepted per rolling hour. Unset = unlimited.
+    pub team_trigger_rate_limit: Option<u32>,
+    /// The addresses allowed to issue `/team ...` commands. Empty: nobody.
+    pub team_command_allowed_senders: Vec<String>,
+}
+
 /// Phase 109 — `[git]` configuration for the `git.status` /
 /// `git.diff` tools (Amendment A12). The `repos` field is the
 /// allow-set the tools gate against; each entry is canonicalized
@@ -4108,6 +4136,8 @@ struct RawToml {
     #[serde(default)]
     matrix: RawMatrix,
     #[serde(default)]
+    proton: RawProton,
+    #[serde(default)]
     slack: RawSlack,
     #[serde(default)]
     git: RawGit,
@@ -5143,6 +5173,56 @@ struct RawMatrix {
     team_command_allowed_senders: Vec<String>,
 }
 
+/// `[proton]` TOML section. Present when any key is set.
+#[derive(Debug, Default, PartialEq, Eq, Deserialize)]
+struct RawProton {
+    #[serde(default)]
+    imap_addr: Option<String>,
+    #[serde(default)]
+    smtp_addr: Option<String>,
+    #[serde(default)]
+    channel_address: Option<String>,
+    #[serde(default)]
+    own_addresses: Vec<String>,
+    #[serde(default)]
+    allowed_external_senders: Vec<String>,
+    #[serde(default)]
+    folder: Option<String>,
+    #[serde(default)]
+    team_run_channel: bool,
+    #[serde(default)]
+    team_trigger_rate_limit: Option<u32>,
+    #[serde(default)]
+    team_command_allowed_senders: Vec<String>,
+}
+
+/// `host:port` with the host exactly `127.0.0.1`, `localhost` or `[::1]`.
+fn loopback_socket_addr(s: &str) -> bool {
+    let Some((host, port)) = s.rsplit_once(':') else {
+        return false;
+    };
+    !port.is_empty()
+        && port.bytes().all(|b| b.is_ascii_digit())
+        && port.parse::<u16>().is_ok_and(|p| p != 0)
+        && matches!(host, "127.0.0.1" | "localhost" | "[::1]")
+}
+
+/// `local@domain.tld`: one `@`, a dotted domain, no spaces, brackets,
+/// commas, semicolons or quotes.
+fn plausible_email(s: &str) -> bool {
+    let Some((local, domain)) = s.split_once('@') else {
+        return false;
+    };
+    !local.is_empty()
+        && !domain.contains('@')
+        && domain.contains('.')
+        && !domain.starts_with('.')
+        && !domain.ends_with('.')
+        && !s
+            .chars()
+            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | ',' | ';' | '"'))
+}
+
 /// Phase 108 — `[slack]` TOML section deserialize target.
 /// Three optional fields: bot token, app token (Socket Mode),
 /// optional team_id constraint.
@@ -6961,6 +7041,89 @@ impl AivyxConfig {
             }
         };
 
+        // --- proton ------------------------------------------------
+        // `[proton]` is `Some` when any key is set. The Bridge login and
+        // certificate pins are never here (`connect proton` stores them).
+        let proton = if toml.proton == RawProton::default() {
+            None
+        } else {
+            let raw = &toml.proton;
+            let invalid = |field: &'static str, reason: &str| ConfigError::Invalid {
+                field,
+                reason: reason.into(),
+            };
+            let emails = |list: &[String], field: &'static str| -> Result<Vec<String>, ConfigError> {
+                list.iter()
+                    .map(|a| {
+                        let a = a.trim().to_ascii_lowercase();
+                        if plausible_email(&a) {
+                            Ok(a)
+                        } else {
+                            Err(invalid(field, "each entry must be an email address like you@proton.me"))
+                        }
+                    })
+                    .collect()
+            };
+            let imap_addr = raw.imap_addr.clone().unwrap_or_else(|| "127.0.0.1:1143".into());
+            if !loopback_socket_addr(&imap_addr) {
+                return Err(invalid(
+                    "proton.imap_addr",
+                    "must be Bridge on this machine: 127.0.0.1, localhost or [::1], with a port",
+                ));
+            }
+            let smtp_addr = raw.smtp_addr.clone().unwrap_or_else(|| "127.0.0.1:1025".into());
+            if !loopback_socket_addr(&smtp_addr) {
+                return Err(invalid(
+                    "proton.smtp_addr",
+                    "must be Bridge on this machine: 127.0.0.1, localhost or [::1], with a port",
+                ));
+            }
+            let own_addresses = emails(&raw.own_addresses, "proton.own_addresses")?;
+            if own_addresses.is_empty() {
+                return Err(invalid(
+                    "proton.own_addresses",
+                    "list your Proton account's addresses, e.g. [\"you@proton.me\"]",
+                ));
+            }
+            let channel_address = match &raw.channel_address {
+                None => None,
+                Some(a) => {
+                    let a = a.trim().to_ascii_lowercase();
+                    if !plausible_email(&a) {
+                        return Err(invalid("proton.channel_address", "must be an email address"));
+                    }
+                    if !own_addresses.contains(&a) {
+                        return Err(invalid(
+                            "proton.channel_address",
+                            "must be one of your own_addresses (an address of your Proton account)",
+                        ));
+                    }
+                    Some(a)
+                }
+            };
+            let folder = raw.folder.clone().unwrap_or_else(|| "Aivyx".into());
+            if folder.trim().is_empty() || folder.contains('/') || folder.chars().any(char::is_control) {
+                return Err(invalid("proton.folder", "must be a folder name without `/`"));
+            }
+            Some(ProtonConfig {
+                imap_addr,
+                smtp_addr,
+                channel_address,
+                own_addresses,
+                allowed_external_senders: emails(
+                    &raw.allowed_external_senders,
+                    "proton.allowed_external_senders",
+                )?,
+                folder,
+                team_run_channel: raw.team_run_channel,
+                team_trigger_rate_limit: raw.team_trigger_rate_limit,
+                team_command_allowed_senders: emails(
+                    &raw.team_command_allowed_senders,
+                    "proton.team_command_allowed_senders",
+                )?,
+            })
+        };
+
         // --- slack (Phase 108) -------------------------------------
         // Same shape as Telegram + Discord: SlackConfig is
         // constructed whenever any Slack source fires. Both
@@ -8433,6 +8596,7 @@ impl AivyxConfig {
             telegram,
             discord,
             matrix,
+            proton,
             slack,
             git,
             email,
diff --git a/crates/aivyx-config/src/tests.rs b/crates/aivyx-config/src/tests.rs
index 147f5553..cd733862 100644
--- a/crates/aivyx-config/src/tests.rs
+++ b/crates/aivyx-config/src/tests.rs
@@ -618,6 +618,60 @@ fn matrix_rejects_malformed_team_command_senders() {
     drop(env);
 }
 
+#[test]
+fn proton_section_parses_with_defaults_and_absent_means_none() {
+    let env = EnvScope::new();
+    let toml = r#"
+[proton]
+channel_address = "Me_Aivyx@proton.me"
+own_addresses = ["me@proton.me", "me_aivyx@proton.me"]
+allowed_external_senders = ["Friend@Example.com"]
+team_command_allowed_senders = ["me@proton.me"]
+"#;
+    let p = load_with_toml(toml, "proton-parse").proton.expect("[proton] is Some");
+    assert_eq!(p.imap_addr, "127.0.0.1:1143");
+    assert_eq!(p.smtp_addr, "127.0.0.1:1025");
+    assert_eq!(p.channel_address.as_deref(), Some("me_aivyx@proton.me"));
+    assert_eq!(p.own_addresses, vec!["me@proton.me", "me_aivyx@proton.me"]);
+    assert_eq!(p.allowed_external_senders, vec!["friend@example.com"]);
+    assert_eq!(p.folder, "Aivyx");
+    assert_eq!(p.team_command_allowed_senders, vec!["me@proton.me"]);
+    assert!(!p.team_run_channel);
+    assert!(load_with_toml("", "proton-absent").proton.is_none());
+    // Tools only: no channel address needed.
+    let tools_only = load_with_toml("[proton]\nown_addresses = [\"me@proton.me\"]\n", "proton-tools");
+    assert_eq!(tools_only.proton.unwrap().channel_address, None);
+    drop(env);
+}
+
+#[test]
+fn proton_rejects_unsafe_or_malformed_settings() {
+    let env = EnvScope::new();
+    let own = "own_addresses = [\"me@proton.me\", \"bot@proton.me\"]\n";
+    let cases: &[(&str, &str)] = &[
+        ("imap_addr = \"192.168.1.5:1143\"\n", "proton.imap_addr"),
+        ("imap_addr = \"localhost.evil.com:1143\"\n", "proton.imap_addr"),
+        ("imap_addr = \"127.0.0.1\"\n", "proton.imap_addr"),
+        ("smtp_addr = \"mail.proton.me:25\"\n", "proton.smtp_addr"),
+        ("channel_address = \"stranger@x.com\"\n", "proton.channel_address"),
+        ("channel_address = \"not an address\"\n", "proton.channel_address"),
+        ("allowed_external_senders = [\"a@b\"]\n", "proton.allowed_external_senders"),
+        ("team_command_allowed_senders = [\"me\"]\n", "proton.team_command_allowed_senders"),
+        ("folder = \"a/b\"\n", "proton.folder"),
+    ];
+    for (line, field) in cases {
+        let body = format!("[proton]\n{own}{line}");
+        assert_eq!(matrix_invalid_field(&body, "proton-bad"), *field, "for:\n{body}");
+    }
+    let no_own = "[proton]\nchannel_address = \"bot@proton.me\"\n";
+    assert_eq!(matrix_invalid_field(no_own, "proton-no-own"), "proton.own_addresses");
+    for ok in ["localhost:1143", "[::1]:1143", "127.0.0.1:2143"] {
+        let body = format!("[proton]\n{own}imap_addr = \"{ok}\"\n");
+        assert!(load_with_toml_result(&body, "proton-ok").is_ok(), "should accept {ok}");
+    }
+    drop(env);
+}
+
 #[test]
 fn matrix_accepts_loopback_http_homeservers() {
     let env = EnvScope::new();
```

- [ ] **Step 3: Keep `aivyx-cli` building** — in `crates/aivyx-cli/src/bin/aivyx.rs`, inside `let AivyxConfig { … } = config;` in `run_async`, right after the `matrix,` line add:

```rust
        // `[proton]` — bound by the `ChannelKind::Proton` arm (next tasks).
        proton: _,
```

- [ ] **Step 4: Run** `cargo test -p aivyx-config && cargo clippy --workspace --all-targets -- -D warnings` → PASS.

- [ ] **Step: Commit**

```bash
git add crates/aivyx-config/src crates/aivyx-cli/src/bin/aivyx.rs
git commit -s -m "feat(config): [proton] — Bridge on loopback, your addresses, allowlisted senders" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 13: Daemon mode

**Files:**
- Modify: `crates/aivyx-ipc/src/protocol.rs` (`FrontendType::Proton`)
- Create: `crates/aivyx-channel/src/channel_team_chain.rs` (Matrix's `/team` chain, lifted, with a `frontend: FrontendType` parameter, plus its two tests)
- Modify: `crates/aivyx-channel/src/matrix_daemon_frontend.rs` (calls the shared chain; the moved function, tests and now-unused imports removed)
- Create: `crates/aivyx-channel/src/proton_daemon_frontend.rs`
- Modify: `crates/aivyx-channel/src/lib.rs`, `crates/aivyx-channel/Cargo.toml` (`aivyx-proton` dependency), `crates/aivyx-channel/tests/daemon_roundtrip_e2e.rs` (two exhaustive `FrontendType` matches)
- Modify: `crates/aivyx-cli/src/bin/aivyx.rs` (the daemon's channel factory matches `FrontendType` exhaustively: add the `Proton` arm here so the workspace keeps building)

**Interfaces:**
- Consumes: `aivyx_proton::{run_mail_loop, TurnRunner, LoopError, session::Turn, policy::ProtonPolicy, transport::MailTransport}`
- Produces: `channel_team_chain::handle_team_command(frontend: FrontendType, text: &str, socket_path: &Path, pending: &mut Option<PendingTrigger<String>>, history: &mut Vec<Instant>, team_run_channel: bool, team_trigger_rate_limit: Option<u32>, sender: String, allowed_senders: &[String]) -> Option<String>`; `ProtonDaemonChannel::new()` (platform `Email`, `SemiTrusted`); `run_proton_daemon_session<T: MailTransport + ?Sized>(&T, &ProtonPolicy, state_dir: &Path, socket_path: PathBuf, role: Option<String>, shutdown, team_run_channel, team_trigger_rate_limit, team_command_allowed_senders: Vec<String>) -> Result<(), DaemonError>` — `ChannelFatal` for a fatal loop error, `Internal` for a daemon failure (so the binary falls back).

One `DaemonSession` per email thread (at most 8 open; the oldest is disconnected), so each thread keeps its conversation. `/team …` and gate commands are read from the email's first line.

- [ ] **Step 1: The IPC variant, the lifted chain, the Matrix call site** — apply:

```diff
diff --git a/crates/aivyx-channel/Cargo.toml b/crates/aivyx-channel/Cargo.toml
index 11db1462..c0c19ce5 100644
--- a/crates/aivyx-channel/Cargo.toml
+++ b/crates/aivyx-channel/Cargo.toml
@@ -153,6 +153,7 @@ aivyx-telegram = { path = "../aivyx-telegram" }
 # `discord_daemon_frontend` consumes the IPC types directly.
 aivyx-discord = { path = "../aivyx-discord" }
 aivyx-matrix = { path = "../aivyx-matrix" }
+aivyx-proton = { path = "../aivyx-proton" }
 # Phase 108 — Slack channel adapter. Same binary-only-import
 # pattern as `aivyx-telegram` and `aivyx-discord`. The
 # production transport is currently stubbed (Phase-108-internal
diff --git a/crates/aivyx-channel/src/lib.rs b/crates/aivyx-channel/src/lib.rs
index b0782d7b..20b2e92b 100644
--- a/crates/aivyx-channel/src/lib.rs
+++ b/crates/aivyx-channel/src/lib.rs
@@ -188,6 +188,8 @@ pub mod telegram_daemon_frontend;
 // Discord + Slack) daemon-frontends.
 pub mod discord_daemon_frontend;
 pub mod matrix_daemon_frontend;
+pub mod proton_daemon_frontend;
+pub mod channel_team_chain;
 pub mod gate_command;
 pub mod team_command;
 pub mod team_dispatch;
diff --git a/crates/aivyx-channel/src/matrix_daemon_frontend.rs b/crates/aivyx-channel/src/matrix_daemon_frontend.rs
index 483798b5..86457ce0 100644
--- a/crates/aivyx-channel/src/matrix_daemon_frontend.rs
+++ b/crates/aivyx-channel/src/matrix_daemon_frontend.rs
@@ -2,9 +2,8 @@
 //! applies `aivyx_matrix::policy`; only policy-approved turns are
 //! submitted to the daemon over IPC. Mirrors `discord_daemon_frontend`.
 
-use std::path::{Path, PathBuf};
+use std::path::PathBuf;
 use std::sync::{Arc, Mutex};
-use std::time::Instant;
 
 use aivyx_core::{
     CancellationToken, ChannelContext, ChannelError, ChannelPlatform, SessionId, StreamEvent,
@@ -15,15 +14,11 @@ use aivyx_matrix::policy::{
 };
 use aivyx_matrix::transport::{MatrixTransport, OutgoingMessage, TransportError};
 
-use crate::daemon_client::{self, DaemonSession};
+use crate::daemon_client::DaemonSession;
 use crate::daemon_ipc::FrontendType;
 use crate::daemon_server::DaemonError;
 use crate::gate_command;
-use crate::team_command::{self, TeamCommand, sender_allowed};
-use crate::team_dispatch;
-use crate::team_trigger_state::{
-    ConfirmReply, PendingTrigger, check_and_record_trigger, parse_confirm_reply,
-};
+use crate::team_trigger_state::PendingTrigger;
 
 pub struct MatrixDaemonChannel {
     session: SessionId,
@@ -78,75 +73,6 @@ impl ChannelContext for MatrixDaemonChannel {
     }
 }
 
-/// The `/team` precedence chain for one message. `Some(reply)` when it
-/// was a team command or a pending confirmation; `None` to carry on.
-#[allow(clippy::too_many_arguments)]
-async fn handle_matrix_team_command(
-    text: &str,
-    socket_path: &Path,
-    pending: &mut Option<PendingTrigger<String>>,
-    history: &mut Vec<Instant>,
-    team_run_channel: bool,
-    team_trigger_rate_limit: Option<u32>,
-    sender: String,
-    allowed_senders: &[String],
-) -> Option<String> {
-    if team_command::parse(text).is_some() && !sender_allowed(allowed_senders, &sender) {
-        return Some("✗ you are not authorized to issue /team commands.".to_string());
-    }
-    if let Some(p) = pending.take() {
-        let now = Instant::now();
-        match parse_confirm_reply(text) {
-            Some(ConfirmReply::Yes) | Some(ConfirmReply::No) if p.sender_id != sender => {
-                if !p.is_expired(now) {
-                    *pending = Some(p);
-                }
-            }
-            Some(ConfirmReply::Yes) if p.is_expired(now) => {
-                return Some("✗ that request expired, ask again.".to_string());
-            }
-            Some(ConfirmReply::Yes) => {
-                return Some(
-                    match daemon_client::run_team_mission_channel(
-                        socket_path,
-                        FrontendType::Matrix,
-                        p.goal.clone(),
-                    )
-                    .await
-                    {
-                        Ok(id) => format!("✓ Started mission {id}."),
-                        Err(e) => format!("✗ Could not start the mission: {e}"),
-                    },
-                );
-            }
-            Some(ConfirmReply::No) => return Some("Cancelled.".to_string()),
-            None => {
-                if !p.is_expired(now) {
-                    *pending = Some(p);
-                }
-            }
-        }
-    }
-    if let Some(TeamCommand::Run { goal }) = team_command::parse(text) {
-        if !team_run_channel {
-            return Some("✗ this channel is not authorized to start team missions.".to_string());
-        }
-        if let Some(limit) = team_trigger_rate_limit
-            && !check_and_record_trigger(history, limit, Instant::now())
-        {
-            return Some(format!(
-                "✗ too many mission-start requests (max {limit} per hour), try again later."
-            ));
-        }
-        *pending = Some(PendingTrigger::new(goal.clone(), sender));
-        return Some(format!("Start '{goal}' on the default team? Reply yes/no."));
-    }
-    if let Some(cmd) = team_command::parse(text) {
-        return Some(team_dispatch::dispatch(socket_path, cmd).await);
-    }
-    None
-}
-
 /// Apply the failure rule to a Matrix call: Unauthorized ends the session
 /// (`DaemonError::ChannelFatal`, so the binary doesn't fall back
 /// in-process); anything else is logged and the turn's outcome stands.
@@ -258,7 +184,8 @@ pub async fn run_matrix_daemon_session<T: MatrixTransport + 'static>(
             let _ = session.cancel_turn().await;
             continue;
         }
-        if let Some(reply) = handle_matrix_team_command(
+        if let Some(reply) = crate::channel_team_chain::handle_team_command(
+            FrontendType::Matrix,
             &text,
             &socket_path,
             &mut pending,
@@ -318,45 +245,6 @@ mod tests {
         assert_eq!(c.channel_name(), "aivyx-matrix-daemon");
     }
 
-    #[tokio::test]
-    async fn a_team_command_from_a_sender_not_allowed_is_refused() {
-        let mut pending = None;
-        let mut history = Vec::new();
-        let out = handle_matrix_team_command(
-            "/team status",
-            std::path::Path::new("/nonexistent.sock"),
-            &mut pending,
-            &mut history,
-            false,
-            None,
-            "@stranger:x".to_string(),
-            &["@me:x".to_string()],
-        )
-        .await;
-        assert_eq!(
-            out,
-            Some("✗ you are not authorized to issue /team commands.".to_string())
-        );
-    }
-
-    #[tokio::test]
-    async fn ordinary_text_is_not_a_team_command() {
-        let mut pending = None;
-        let mut history = Vec::new();
-        let out = handle_matrix_team_command(
-            "hello",
-            std::path::Path::new("/nonexistent.sock"),
-            &mut pending,
-            &mut history,
-            false,
-            None,
-            "@me:x".to_string(),
-            &["@me:x".to_string()],
-        )
-        .await;
-        assert_eq!(out, None);
-    }
-
     use aivyx_matrix::transport::{Inbound, ScriptedTransport};
     use tokio::io::{AsyncReadExt, AsyncWriteExt};
     use tokio::net::UnixListener;
diff --git a/crates/aivyx-channel/tests/daemon_roundtrip_e2e.rs b/crates/aivyx-channel/tests/daemon_roundtrip_e2e.rs
index 37867b21..192e6fc5 100644
--- a/crates/aivyx-channel/tests/daemon_roundtrip_e2e.rs
+++ b/crates/aivyx-channel/tests/daemon_roundtrip_e2e.rs
@@ -1184,7 +1184,8 @@ async fn telegram_frontend_type_gets_telegram_channel() {
         | FrontendType::Web
         | FrontendType::Discord
         | FrontendType::Slack
-        | FrontendType::Matrix => {
+        | FrontendType::Matrix
+        | FrontendType::Proton => {
             Arc::new(LocalChannel::new("test-local", Vec::<u8>::new()))
         }
     });
@@ -1332,7 +1333,8 @@ async fn mixed_local_and_telegram_frontends_on_same_daemon() {
         | FrontendType::Web
         | FrontendType::Discord
         | FrontendType::Slack
-        | FrontendType::Matrix => {
+        | FrontendType::Matrix
+        | FrontendType::Proton => {
             Arc::new(LocalChannel::new("test-local", Vec::<u8>::new()))
         }
     });
diff --git a/crates/aivyx-ipc/src/protocol.rs b/crates/aivyx-ipc/src/protocol.rs
index 3bba809c..3c6a1803 100644
--- a/crates/aivyx-ipc/src/protocol.rs
+++ b/crates/aivyx-ipc/src/protocol.rs
@@ -91,6 +91,10 @@ pub enum FrontendType {
     /// Matrix client and applies the policy; only policy-approved turns
     /// reach the daemon over this connection.
     Matrix,
+    /// Proton Mail daemon-frontend. The frontend process holds the Bridge
+    /// connection and applies the email trust policy; only approved mail
+    /// reaches the daemon, one connection per email thread.
+    Proton,
 }
 
 // ---------------------------------------------------------------------------
```

and create `crates/aivyx-channel/src/channel_team_chain.rs`:

```rust
//! The `/team` command chain the Matrix and Proton daemon frontends share
//! (sender gate → pending yes/no confirmation → `/team run` opt-in and rate
//! limit → other `/team` subcommands). Lifted from
//! `matrix_daemon_frontend` so both use one copy.

use std::path::Path;
use std::time::Instant;

use crate::daemon_client;
use crate::daemon_ipc::FrontendType;
use crate::team_command::{self, TeamCommand, sender_allowed};
use crate::team_dispatch;
use crate::team_trigger_state::{
    ConfirmReply, PendingTrigger, check_and_record_trigger, parse_confirm_reply,
};

/// The `/team` precedence chain for one message, shared by the Matrix and
/// Proton daemon frontends. `Some(reply)` when it
/// was a team command or a pending confirmation; `None` to carry on.
#[allow(clippy::too_many_arguments)]
pub async fn handle_team_command(
    frontend: FrontendType,
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
                return Some(
                    match daemon_client::run_team_mission_channel(
                        socket_path,
                        frontend,
                        p.goal.clone(),
                    )
                    .await
                    {
                        Ok(id) => format!("✓ Started mission {id}."),
                        Err(e) => format!("✗ Could not start the mission: {e}"),
                    },
                );
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
            return Some(format!(
                "✗ too many mission-start requests (max {limit} per hour), try again later."
            ));
        }
        *pending = Some(PendingTrigger::new(goal.clone(), sender));
        return Some(format!("Start '{goal}' on the default team? Reply yes/no."));
    }
    if let Some(cmd) = team_command::parse(text) {
        return Some(team_dispatch::dispatch(socket_path, cmd).await);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_team_command_from_a_sender_not_allowed_is_refused() {
        let mut pending = None;
        let mut history = Vec::new();
        let out = handle_team_command(
            FrontendType::Matrix,
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
        assert_eq!(
            out,
            Some("✗ you are not authorized to issue /team commands.".to_string())
        );
    }

    #[tokio::test]
    async fn ordinary_text_is_not_a_team_command() {
        let mut pending = None;
        let mut history = Vec::new();
        let out = handle_team_command(
            FrontendType::Matrix,
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

Run `cargo test -p aivyx-channel --lib daemon_frontend` and `cargo test -p aivyx-channel --lib channel_team_chain` → PASS (the moved tests now live in the chain module). The build fails until Step 2 creates `proton_daemon_frontend.rs` (it's declared in `lib.rs`); do Step 2 before running.

- [ ] **Step 2: Write the Proton frontend's failing tests, then the frontend** — `crates/aivyx-channel/src/proton_daemon_frontend.rs` (tests at the bottom; write them first and watch them fail to compile, then the body):

```rust
//! Daemon-mode Proton Mail frontend: this process holds the Bridge
//! connection and runs `aivyx_proton::run_mail_loop` (lock, cursor, trust
//! policy, reply, filing); only approved mail is submitted to the daemon,
//! over one IPC session per email thread so each thread keeps its own
//! conversation. `/team …` and gate commands are read from the email's
//! first line, gated exactly as on Matrix.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use aivyx_core::{
    CancellationToken, ChannelContext, ChannelError, ChannelPlatform, SessionId, StreamEvent,
    TurnOutcome,
};
use aivyx_proton::policy::ProtonPolicy;
use aivyx_proton::session::Turn;
use aivyx_proton::transport::MailTransport;
use aivyx_proton::{LoopError, TurnRunner};

use crate::daemon_client::DaemonSession;
use crate::daemon_ipc::FrontendType;
use crate::daemon_server::DaemonError;
use crate::gate_command;
use crate::team_trigger_state::PendingTrigger;

/// The daemon-side channel for `FrontendType::Proton` connections.
pub struct ProtonDaemonChannel {
    session: SessionId,
    token: Mutex<CancellationToken>,
}

impl ProtonDaemonChannel {
    pub fn new() -> Self {
        ProtonDaemonChannel {
            session: SessionId::new(),
            token: Mutex::new(CancellationToken::new()),
        }
    }
}

impl Default for ProtonDaemonChannel {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ChannelContext for ProtonDaemonChannel {
    fn channel_name(&self) -> &str {
        "aivyx-proton-daemon"
    }
    fn platform(&self) -> ChannelPlatform {
        ChannelPlatform::Email
    }
    fn trust_tier(&self) -> aivyx_capability::TrustTier {
        // The frontend submits only mail `aivyx_proton::policy::decide`
        // approved: from you (in Sent) or allowlisted with a DMARC pass.
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

/// Daemon sessions kept open at once (one per recent thread); the oldest
/// is closed past this.
const MAX_THREAD_SESSIONS: usize = 8;

struct DaemonRunner {
    socket_path: PathBuf,
    role: Option<String>,
    threads: VecDeque<(String, DaemonSession)>,
    pending: Option<PendingTrigger<String>>,
    history: Vec<Instant>,
    team_run_channel: bool,
    team_trigger_rate_limit: Option<u32>,
    team_command_allowed_senders: Vec<String>,
}

impl DaemonRunner {
    /// The thread's daemon session, connecting (and evicting the oldest)
    /// when needed.
    async fn session_for(&mut self, root: &str) -> Result<&mut DaemonSession, DaemonError> {
        if let Some(i) = self.threads.iter().position(|(r, _)| r == root) {
            let entry = self.threads.remove(i).expect("index from position");
            self.threads.push_back(entry);
        } else {
            if self.threads.len() >= MAX_THREAD_SESSIONS
                && let Some((_, old)) = self.threads.pop_front()
            {
                let _ = old.disconnect().await;
            }
            let session = DaemonSession::connect(
                &self.socket_path,
                self.role.clone(),
                Some(FrontendType::Proton),
            )
            .await?;
            self.threads.push_back((root.to_string(), session));
        }
        Ok(&mut self.threads.back_mut().expect("just pushed").1)
    }

    async fn close_all(&mut self) {
        for (_, s) in self.threads.drain(..) {
            let _ = s.disconnect().await;
        }
    }
}

#[async_trait::async_trait]
impl TurnRunner for DaemonRunner {
    async fn run(&mut self, turn: &Turn) -> Result<Option<String>, String> {
        let sender = turn.message.from.clone().unwrap_or_default();
        if let Some(reply) = crate::channel_team_chain::handle_team_command(
            FrontendType::Proton,
            &turn.first_line,
            &self.socket_path,
            &mut self.pending,
            &mut self.history,
            self.team_run_channel,
            self.team_trigger_rate_limit,
            sender,
            &self.team_command_allowed_senders,
        )
        .await
        {
            return Ok(Some(reply));
        }
        let root = turn.thread();
        if let Some(gate) = gate_command::parse(&turn.first_line) {
            let session = self.session_for(&root).await.map_err(|e| e.to_string())?;
            let reply = match session
                .resolve_gate(gate.mission_id, gate.gate_id, gate.approved)
                .await
            {
                Ok(()) => format!(
                    "✓ Gate {}.",
                    if gate.approved { "approved" } else { "rejected" }
                ),
                Err(e) => format!("✗ Gate resolve failed: {e}"),
            };
            return Ok(Some(reply));
        }
        let session = self.session_for(&root).await.map_err(|e| e.to_string())?;
        let (events, outcome) = session
            .submit_input(turn.text.clone())
            .await
            .map_err(|e| e.to_string())?;
        Ok(Some(crate::daemon_ipc::chat_reply(
            crate::discord_daemon_frontend::render_events_for_discord(&events),
            &events,
            &outcome,
        )))
    }
}

/// Runs the Proton channel against a running daemon. `state_dir` is the
/// instance's `proton_dir()`. A rejected Bridge login, a changed
/// certificate or a second running channel is
/// `DaemonError::ChannelFatal` (no in-process fallback can help); a daemon
/// failure is `DaemonError::Internal`, so the binary falls back.
#[allow(clippy::too_many_arguments)]
pub async fn run_proton_daemon_session<T: MailTransport + ?Sized>(
    transport: &T,
    policy: &ProtonPolicy,
    state_dir: &Path,
    socket_path: PathBuf,
    role: Option<String>,
    shutdown: CancellationToken,
    team_run_channel: bool,
    team_trigger_rate_limit: Option<u32>,
    team_command_allowed_senders: Vec<String>,
) -> Result<(), DaemonError> {
    let mut runner = DaemonRunner {
        socket_path,
        role,
        threads: VecDeque::new(),
        pending: None,
        history: Vec::new(),
        team_run_channel,
        team_trigger_rate_limit,
        team_command_allowed_senders,
    };
    let result =
        aivyx_proton::run_mail_loop(transport, policy, state_dir, &mut runner, shutdown).await;
    runner.close_all().await;
    match result {
        Ok(_) => Ok(()),
        Err(LoopError::Fatal(e)) => Err(DaemonError::ChannelFatal(e)),
        Err(LoopError::Runner(e)) => Err(DaemonError::Internal(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_proton::transport::{InboxStatus, ScriptedTransport, TransportError};

    fn policy() -> ProtonPolicy {
        ProtonPolicy::new("bot@proton.me", &["me@proton.me".into(), "bot@proton.me".into()], &[])
    }

    #[test]
    fn daemon_channel_is_email_semitrusted() {
        let c = ProtonDaemonChannel::new();
        assert_eq!(c.platform(), ChannelPlatform::Email);
        assert_eq!(c.trust_tier(), aivyx_capability::TrustTier::SemiTrusted);
        assert_eq!(c.channel_name(), "aivyx-proton-daemon");
    }

    #[tokio::test]
    async fn a_rejected_bridge_login_is_channel_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let t = ScriptedTransport::new(InboxStatus { uid_validity: 1, uid_next: 1 }, vec![])
            .then_fail(|| TransportError::Unauthorized("NO".into()));
        let r = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_proton_daemon_session(
                &t,
                &policy(),
                dir.path(),
                PathBuf::from("/nonexistent.sock"),
                None,
                CancellationToken::new(),
                false,
                None,
                vec![],
            ),
        )
        .await
        .expect("returns");
        let e = r.expect_err("must be Err");
        assert!(matches!(e, DaemonError::ChannelFatal(_)), "{e:?}");
        assert!(e.to_string().contains("connect proton"), "{e}");
    }

    #[tokio::test]
    async fn a_missing_daemon_is_internal_so_the_binary_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let mail = aivyx_proton::message::MailMessage {
            uid: 5,
            internal_id: Some("pm5".into()),
            from: Some("me@proton.me".into()),
            to: vec!["bot@proton.me".into()],
            text: "hello".into(),
            ..Default::default()
        };
        let t = ScriptedTransport::new(InboxStatus { uid_validity: 1, uid_next: 5 }, vec![vec![mail]])
            .in_sent_folder(vec!["pm5"]);
        let r = run_proton_daemon_session(
            &t,
            &policy(),
            dir.path(),
            PathBuf::from("/nonexistent.sock"),
            None,
            CancellationToken::new(),
            false,
            None,
            vec![],
        )
        .await;
        assert!(matches!(r, Err(DaemonError::Internal(_))), "{r:?}");
        assert!(t.sent().await.is_empty());
    }
}
```

- [ ] **Step 3: The factory arm** — in `crates/aivyx-cli/src/bin/aivyx.rs`, after the `FrontendType::Matrix => Arc::new(…MatrixDaemonChannel::new(),),` arm of the channel factory:

```rust
                aivyx_channel::daemon_ipc::FrontendType::Proton => Arc::new(
                    aivyx_channel::proton_daemon_frontend::ProtonDaemonChannel::new(),
                ),
```

- [ ] **Step 4: Run** `cargo test -p aivyx-channel --lib daemon_frontend && cargo test -p aivyx-channel --lib channel_team_chain && cargo test -p aivyx-channel --test daemon_roundtrip_e2e && cargo clippy --workspace --all-targets -- -D warnings` → PASS.

- [ ] **Step: Commit**

```bash
git add crates/aivyx-ipc/src/protocol.rs crates/aivyx-channel crates/aivyx-cli/src/bin/aivyx.rs
git commit -s -m "feat(channel): Proton daemon frontend; /team chain shared with Matrix" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 14: The CLI — `--channel proton` and `connect proton`

**Files:**
- Modify: `crates/aivyx-channel/src/role_render.rs` (`ChannelKind::Proton`, label `"proton"`)
- Modify: `crates/aivyx-cli/Cargo.toml` (`aivyx-proton` dependency), `crates/aivyx-cli/src/bin/aivyx.rs`, `crates/aivyx-cli/src/bin/aivyx_modules/help.rs`
- Create: `crates/aivyx-cli/src/bin/aivyx_modules/proton_cli.rs`

**Interfaces:**
- Consumes: `aivyx_proton::{credentials, bridge::capture_pins, tls, policy::ProtonPolicy, ProtonSessionConfig, run_proton_session_with_transport}`, `BridgeTransport::check_login`, `aivyx_channel::proton_daemon_frontend::{ProtonDaemonChannel, run_proton_daemon_session}`, `AivyxConfig::proton`.
- Produces: `CliMode::ProtonConnect`, `ChannelKind::Proton`, `proton_cli::run_connect(&AivyxConfig) -> Result<(), String>`.

`connect proton` runs right after the config loads (after the `--print-role` exit): it needs no passphrase, store or daemon. It captures and shows both certificate fingerprints, asks for the Bridge username and password (no echo), tests IMAP and SMTP login, and only then writes `credentials.toml`. `--channel proton` refuses when `[proton]` lacks `channel_address`, when not connected, or when `imap_addr`/`smtp_addr` changed since `connect proton`; it uses the daemon when one is running (falling back in-process on a daemon failure, but not on a fatal channel error).

- [ ] **Step 1: The failing parse test** — add `proton_commands_parse` (in the `aivyx.rs` hunk below) and run `cargo test -p aivyx-cli proton_commands_parse` → FAIL to compile.
- [ ] **Step 2: Implement** — apply the hunks below. Two are already in place from earlier tasks: the `FrontendType::Proton` factory arm (Task 13), and the destructure, where Task 12's `proton: _,` (and its comment) becomes the `proton,` shown here.

```diff
diff --git a/crates/aivyx-channel/src/role_render.rs b/crates/aivyx-channel/src/role_render.rs
index 27863263..75f457e1 100644
--- a/crates/aivyx-channel/src/role_render.rs
+++ b/crates/aivyx-channel/src/role_render.rs
@@ -91,6 +91,8 @@ pub enum ChannelKind {
     Slack,
     /// Matrix adapter — same SemiTrusted registration posture as Telegram/Discord/Slack
     Matrix,
+    /// Proton Mail (email) channel — same SemiTrusted registration posture
+    Proton,
     /// Phase 135 — Voice I/O channel. Operator speaks
     /// through the microphone; agent speaks back through
     /// the speakers. Same `Trusted` tier posture as
@@ -212,6 +214,7 @@ pub fn render_role_envelope(
             ChannelKind::Discord => "discord",
             ChannelKind::Slack => "slack",
             ChannelKind::Matrix => "matrix",
+            ChannelKind::Proton => "proton",
             ChannelKind::Voice => "voice",
         }
     )
diff --git a/crates/aivyx-cli/Cargo.toml b/crates/aivyx-cli/Cargo.toml
index 42a6d007..2cd60f11 100644
--- a/crates/aivyx-cli/Cargo.toml
+++ b/crates/aivyx-cli/Cargo.toml
@@ -117,6 +117,7 @@ aivyx-telegram = { path = "../aivyx-telegram" }
 aivyx-discord = { path = "../aivyx-discord" }
 aivyx-slack = { path = "../aivyx-slack" }
 aivyx-matrix = { path = "../aivyx-matrix" }
+aivyx-proton = { path = "../aivyx-proton" }
 aivyx-voice = { path = "../aivyx-voice", optional = true }
 # Task 8 (Chapter Passport): `aivyx federation yubikey-init` drives
 # `aivyx-yubi`'s discovery/pin/provision flow directly (not re-exported by
diff --git a/crates/aivyx-cli/src/bin/aivyx.rs b/crates/aivyx-cli/src/bin/aivyx.rs
index 8d6f6ed4..f1c438f7 100644
--- a/crates/aivyx-cli/src/bin/aivyx.rs
+++ b/crates/aivyx-cli/src/bin/aivyx.rs
@@ -108,6 +108,8 @@ mod connect;
 mod connect_kitchen;
 #[path = "aivyx_modules/matrix_cli.rs"]
 mod matrix_cli;
+#[path = "aivyx_modules/proton_cli.rs"]
+mod proton_cli;
 #[path = "aivyx_modules/cost.rs"]
 mod cost;
 #[path = "aivyx_modules/daemon_service.rs"]
@@ -376,7 +378,8 @@ fn build_shell_exec_for_channel(
         ChannelKind::Telegram
         | ChannelKind::Discord
         | ChannelKind::Slack
-        | ChannelKind::Matrix => Ok(None),
+        | ChannelKind::Matrix
+        | ChannelKind::Proton => Ok(None),
     }
 }
 
@@ -429,7 +432,8 @@ fn build_fs_delete_for_channel(
         ChannelKind::Telegram
         | ChannelKind::Discord
         | ChannelKind::Slack
-        | ChannelKind::Matrix => Ok(None),
+        | ChannelKind::Matrix
+        | ChannelKind::Proton => Ok(None),
     }
 }
 
@@ -1270,6 +1274,17 @@ fn run() -> Result<(), String> {
         return Ok(());
     }
 
+    // `aivyx-pa connect proton`: needs the config ([proton]) but no
+    // passphrase, store or daemon — it talks only to Proton Mail Bridge and
+    // writes the instance's `tool-processes/proton/credentials.toml`.
+    if mode == CliMode::ProtonConnect {
+        let rt = tokio::runtime::Builder::new_current_thread()
+            .enable_all()
+            .build()
+            .map_err(|e| format!("failed to build tokio runtime: {e}"))?;
+        return rt.block_on(proton_cli::run_connect(&config));
+    }
+
     // Model routing Part 3a — `aivyx-pa routing status`. Like
     // `--print-role`, it needs no passphrase, store, or sandbox: it runs
     // the daemon's startup discovery + merge against `[routing]` and prints
@@ -2276,6 +2291,8 @@ enum CliMode {
     MatrixConnect,
     /// See [`CliMode::MatrixConnect`].
     MatrixVerify,
+    /// `aivyx-pa connect proton` — pins Bridge's certificate and stores its login.
+    ProtonConnect,
     /// `aivyx-pa instances list | create <name> | remove <name>`: named
     /// instances (several separate agents for one OS user).
     Instances(InstancesCmd),
@@ -3801,6 +3818,18 @@ fn parse_cli_args_from(args: &[String]) -> Result<CliArgs, String> {
         if args.len() > 2 {
             return Err("`aivyx-pa connect` takes at most one service name".into());
         }
+        if service.as_deref() == Some("proton") {
+            return Ok(CliArgs {
+                mode: CliMode::ProtonConnect,
+                channel: ChannelKind::Local,
+                role: None,
+                no_daemon: false,
+                mcp_servers: vec![],
+                mcp_sse_servers: vec![],
+                provider: None,
+                web_ui_port: None,
+            });
+        }
         if service.as_deref() == Some("matrix") {
             return Ok(CliArgs {
                 mode: CliMode::MatrixConnect,
@@ -5632,7 +5661,7 @@ fn parse_cli_args_from(args: &[String]) -> Result<CliArgs, String> {
             }
             "--channel" => {
                 let value = args.get(i + 1).ok_or_else(|| {
-                    "`--channel` requires a value: `local`, `telegram`, `discord`, `slack`, `matrix`, or `voice`"
+                    "`--channel` requires a value: `local`, `telegram`, `discord`, `slack`, `matrix`, `proton`, or `voice`"
                         .to_string()
                 })?;
                 channel = match value.as_str() {
@@ -5643,12 +5672,13 @@ fn parse_cli_args_from(args: &[String]) -> Result<CliArgs, String> {
                     // Phase 108 — Slack adapter parse arm.
                     "slack" => ChannelKind::Slack,
                     "matrix" => ChannelKind::Matrix,
+                    "proton" => ChannelKind::Proton,
                     // Phase 135 — Voice channel parse arm.
                     "voice" => ChannelKind::Voice,
                     other => {
                         return Err(format!(
                             "unrecognized channel `{other}`. \
-                             Supported: local, telegram, discord, slack, matrix, voice"
+                             Supported: local, telegram, discord, slack, matrix, proton, voice"
                         ));
                     }
                 };
@@ -6791,6 +6821,8 @@ async fn run_async(
         discord,
         // `[matrix]` config consumed by the `ChannelKind::Matrix` arm below.
         matrix,
+        // `[proton]` config consumed by the `ChannelKind::Proton` arm below.
+        proton,
         // Phase 108 — `slack` config consumed by the
         // `ChannelKind::Slack` dispatch arm below; carries
         // the bot + app tokens through to
@@ -10735,6 +10767,9 @@ async fn run_async(
                 aivyx_channel::daemon_ipc::FrontendType::Matrix => Arc::new(
                     aivyx_channel::matrix_daemon_frontend::MatrixDaemonChannel::new(),
                 ),
+                aivyx_channel::daemon_ipc::FrontendType::Proton => Arc::new(
+                    aivyx_channel::proton_daemon_frontend::ProtonDaemonChannel::new(),
+                ),
                 // Phase 111 — Slack daemon-mode channel stub.
                 // Mirrors TelegramDaemonChannel and
                 // DiscordDaemonChannel; the slack_daemon_frontend.rs
@@ -11846,6 +11881,101 @@ async fn run_async(
             .map(|_| ())
         }
 
+        ChannelKind::Proton => {
+            let pc = proton.ok_or("`--channel proton` needs a [proton] section in your config")?;
+            let channel_address = pc.channel_address.clone().ok_or(
+                "`--channel proton` needs `channel_address` in [proton] — the address the assistant answers on",
+            )?;
+            let instance = aivyx_instance::InstancePaths::current().map_err(|e| e.to_string())?;
+            let state_dir = instance
+                .proton_dir()
+                .ok_or("can't resolve the instance's data folder")?;
+            let creds_path = aivyx_proton::credentials::path_for(&instance)
+                .ok_or("can't resolve the instance's folders (is HOME set?)")?;
+            let creds = aivyx_proton::credentials::load(&creds_path)?.ok_or(
+                "not connected to Proton Mail Bridge yet — run `aivyx-pa connect proton` first",
+            )?;
+            if creds.imap_addr != pc.imap_addr || creds.smtp_addr != pc.smtp_addr {
+                return Err("[proton] imap_addr / smtp_addr changed since `aivyx-pa connect proton` — run it again".into());
+            }
+            let transport = creds.transport(&pc.folder)?;
+            let policy = aivyx_proton::policy::ProtonPolicy::new(
+                &channel_address,
+                &pc.own_addresses,
+                &pc.allowed_external_senders,
+            );
+            let shutdown = CancellationToken::new();
+            let shutdown_for_signal = shutdown.clone();
+            tokio::spawn(async move {
+                if tokio::signal::ctrl_c().await.is_err() {
+                    std::process::exit(130);
+                }
+                eprintln!("\naivyx: shutting down the Proton Mail channel.");
+                shutdown_for_signal.cancel();
+            });
+            let daemon_socket = match default_socket_path() {
+                Ok(sp) if !no_daemon && aivyx_channel::daemon_client::daemon_is_running(&sp).await => Some(sp),
+                _ => None,
+            };
+            if let Some(sp) = daemon_socket {
+                eprintln!(
+                    "aivyx-pa {} (daemon) — answering email to {channel_address}",
+                    env!("CARGO_PKG_VERSION")
+                );
+                match aivyx_channel::proton_daemon_frontend::run_proton_daemon_session(
+                    &transport,
+                    &policy,
+                    &state_dir,
+                    sp.clone(),
+                    Some(active_role_name.clone()),
+                    shutdown.clone(),
+                    pc.team_run_channel,
+                    pc.team_trigger_rate_limit,
+                    pc.team_command_allowed_senders.clone(),
+                )
+                .await
+                {
+                    Ok(()) => return Ok(()),
+                    // A rejected login, a changed certificate or a second
+                    // channel would fail the same way in-process.
+                    Err(e @ aivyx_channel::daemon_server::DaemonError::ChannelFatal(_)) => return Err(e.to_string()),
+                    Err(e) => eprintln!("aivyx-pa: daemon Proton session failed ({e}), falling back to in-process."),
+                }
+            }
+            eprintln!(
+                "aivyx-pa {} — answering email to {channel_address} (in-process)",
+                env!("CARGO_PKG_VERSION")
+            );
+            let proton_config = aivyx_proton::ProtonSessionConfig {
+                model,
+                system_prompt,
+                max_tokens: DEFAULT_MAX_TOKENS,
+                capabilities,
+                tools,
+                storage,
+                tool_allowlist,
+                memory_topic_prefix,
+                turn_timeout_secs,
+                cycle_detection,
+                injection_scan_enabled,
+                injection_scan_exempt,
+                confirm_integration_writes,
+                budget_gate: aivyx_channel::budget_gate::ChannelBudgetGate::new_gate(
+                    config_budget.clone(),
+                    Arc::clone(&persistent_audit_for_query),
+                    aivyx_cost::Pricing::with_overrides(config_pricing.clone()),
+                    DEFAULT_MAX_TOKENS,
+                ),
+                rate_gate: aivyx_channel::rate_gate::ChannelRateGate::new_gate(config_rate_limit.clone()),
+            };
+            aivyx_proton::run_proton_session_with_transport(
+                &transport, &policy, &state_dir, proton_config, provider, audit, checkpointer.clone(), shutdown,
+            )
+            .await
+            .map(|_| ())
+            .map_err(|e| e.to_string())
+        }
+
         // Phase 108 Task 5 — Slack adapter dispatch. Mirrors
         // the Discord arm above. Socket Mode requires two
         // tokens (bot for REST, app for the WebSocket); both
@@ -14676,6 +14806,12 @@ mod tests {
         assert!(local_config_notice(&default, false, true).is_none());
     }
 
+    #[test]
+    fn proton_commands_parse() {
+        assert_eq!(parse_cli_args_from(&argv(&["connect", "proton"])).unwrap().mode, CliMode::ProtonConnect);
+        assert_eq!(parse_cli_args_from(&argv(&["--channel", "proton"])).unwrap().channel, ChannelKind::Proton);
+    }
+
     #[test]
     fn matrix_commands_parse() {
         assert_eq!(parse_cli_args_from(&argv(&["connect", "matrix"])).unwrap().mode, CliMode::MatrixConnect);
diff --git a/crates/aivyx-cli/src/bin/aivyx_modules/help.rs b/crates/aivyx-cli/src/bin/aivyx_modules/help.rs
index 4866d4c2..5104282d 100644
--- a/crates/aivyx-cli/src/bin/aivyx_modules/help.rs
+++ b/crates/aivyx-cli/src/bin/aivyx_modules/help.rs
@@ -466,7 +466,8 @@ pub fn render_top_level_help() -> String {
     out.push_str("Global flags:\n");
     out.push_str("  --instance <name>      Use a named instance (multi-instance support).\n");
     out.push_str("  --verify-only          Forensic: verify the audit chain and exit.\n");
-    out.push_str("  --channel <kind>       local | telegram | discord | slack | matrix | voice.\n");
+    out.push_str("  --channel <kind>       local | telegram | discord | slack | matrix |\n");
+    out.push_str("                         proton | voice.\n");
     out.push_str("  --role <name>          Use a named role for this session.\n");
     out.push_str("  --print-role <name>    Print a role's rendered envelope and exit.\n");
     out.push_str("  --no-daemon            Chat in-process: don't connect to a running\n");
```

and create `crates/aivyx-cli/src/bin/aivyx_modules/proton_cli.rs`:

```rust
//! `aivyx-pa connect proton`: pin Proton Mail Bridge's certificate, test
//! its login, and store both in the instance's
//! `tool-processes/proton/credentials.toml` (`0600`).

use std::io::{BufRead, Write};

use aivyx_config::AivyxConfig;
use aivyx_proton::credentials::{self, BridgeCredentials};
use aivyx_proton::tls;

pub async fn run_connect(config: &AivyxConfig) -> Result<(), String> {
    let p = config.proton.as_ref().ok_or(
        "no [proton] section in your config. Add own_addresses (and channel_address for \
         the email channel) first — see `docs/guide/13-chat-apps-and-accounts.md`.",
    )?;
    let instance = aivyx_instance::InstancePaths::current().map_err(|e| e.to_string())?;
    let path = credentials::path_for(&instance)
        .ok_or("can't resolve the instance's folders (is HOME set?)")?;

    eprintln!(
        "Connecting to Proton Mail Bridge at {} (IMAP) and {} (SMTP).",
        p.imap_addr, p.smtp_addr
    );
    let (imap_pin, smtp_pin) = aivyx_proton::bridge::capture_pins(&p.imap_addr, &p.smtp_addr)
        .await
        .map_err(|e| format!("{e} — is Proton Mail Bridge running and signed in?"))?;
    eprintln!(
        "Bridge's certificates (SHA-256):\n  IMAP {}\n  SMTP {}\nThese are pinned: from now \
         on only this Bridge is trusted. If you reinstall Bridge, run this again.",
        tls::pin_to_hex(&imap_pin),
        tls::pin_to_hex(&smtp_pin)
    );

    eprint!("Bridge username (Bridge → your account → mailbox details): ");
    let _ = std::io::stderr().flush();
    let mut username = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut username)
        .map_err(|e| format!("read username: {e}"))?;
    let username = username.trim().to_string();
    if username.is_empty() {
        return Err("no username given".into());
    }
    let password = rpassword::prompt_password(
        "Bridge password (the one Bridge shows you, not your Proton password): ",
    )
    .map_err(|e| format!("read password: {e}"))?;

    let creds = BridgeCredentials {
        username,
        password,
        imap_addr: p.imap_addr.clone(),
        smtp_addr: p.smtp_addr.clone(),
        imap_pin: tls::pin_to_hex(&imap_pin),
        smtp_pin: tls::pin_to_hex(&smtp_pin),
        from_address: p.own_addresses[0].clone(),
    };
    creds
        .transport(&p.folder)?
        .check_login()
        .await
        .map_err(|e| format!("Bridge didn't accept that login: {e}"))?;
    credentials::save(&path, &creds)?;
    eprintln!("Connected. Saved to {} (readable only by you).", path.display());
    eprintln!(
        "Next: `aivyx-pa --channel proton` for the email channel. For the proton.* \
         tools, add to aivyx-pa.toml and restart the daemon:\n\n[[tool_process]]\n\
         name = \"proton\"\ncommand = \"aivyx-proton\"\n"
    );
    Ok(())
}
```

- [ ] **Step 3: Run** `cargo test -p aivyx-cli && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` → PASS. Then `./target/debug/aivyx-pa connect proton` with no `[proton]` section → the "no [proton] section" message; `./target/debug/aivyx-pa --channel proton` → "needs a [proton] section".

- [ ] **Step: Commit**

```bash
git add crates/aivyx-channel/src/role_render.rs crates/aivyx-cli
git commit -s -m "feat(cli): --channel proton and connect proton" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 15: Docs

**Files:**
- Modify: `scripts/gen-config-reference.py` — add `"proton"` after `"matrix"` in the `("Chat apps and voice", [...])` group, and to the intro dict after the `"matrix"` entry: `"proton": f"Proton Mail through Proton Mail Bridge: the email channel and the `proton.*` tools. See [Chat apps and accounts]({GUIDE}/13-chat-apps-and-accounts.md).",`
- Modify: `scripts/gen-tools-reference.py` — add to `GROUPS` after `"aivyx-gmail"`: `"aivyx-proton": ("Proton Mail", "Needs Proton Mail Bridge and `aivyx-pa connect proton`, then the `aivyx-proton` `[[tool_process]]`. See [Chat apps and accounts]({GUIDE}/13-chat-apps-and-accounts.md)."),`
- Regenerate: `docs/manual/reference/02-configuration.md`, `docs/manual/reference/03-tools.md`
- Modify: `docs/manual/reference/01-cli.md`, `docs/guide/13-chat-apps-and-accounts.md`, `docs/TOOLS.md`, `docs/THREAT_MODEL.md`

- [ ] **Step 1: Regenerate the references**

Run: `python3 scripts/gen-config-reference.py > /tmp/conf.md && cp /tmp/conf.md docs/manual/reference/02-configuration.md && python3 scripts/gen-tools-reference.py > /tmp/tools.md && cp /tmp/tools.md docs/manual/reference/03-tools.md && git diff --stat docs/manual/reference`
Expected: a `[proton]` section with its nine keys, and a "Proton Mail" tools section with the four `proton.*` tools.

- [ ] **Step 2: Guide** — in `docs/guide/13-chat-apps-and-accounts.md`, add after the Matrix section (before `### Discord and Slack`):

````markdown
### Proton Mail

Email your assistant from Proton Mail and get the answer back in the same
thread — and let it search, read, draft and send in your mailbox. It all goes
through [Proton Mail Bridge](https://proton.me/mail/bridge) on this machine
(a paid Proton plan), so your mail is decrypted only here.

1. Install Proton Mail Bridge and sign in. Add an address to your account for
   the assistant (Proton → Settings → Identity and addresses), e.g.
   `you_aivyx@proton.me`; a `+` alias (`you+assistant@proton.me`) also works.
2. Add to your config:

   ```toml
   [proton]
   channel_address = "you_aivyx@proton.me"
   own_addresses = ["you@proton.me", "you_aivyx@proton.me"]
   # allowed_external_senders = ["you@work.example"]
   ```

3. `aivyx-pa connect proton` — shows Bridge's certificate fingerprints and
   pins them, asks for the username and password **Bridge** shows you
   (Bridge → your account → mailbox details; not your Proton password),
   checks them, and saves them where only you can read them.
4. `aivyx-pa --channel proton`, then email `you_aivyx@proton.me`.
5. For the tools (`proton.search`, `proton.read`, `proton.draft`,
   `proton.send`), add this and restart the daemon:

   ```toml
   [[tool_process]]
   name = "proton"
   command = "aivyx-proton"
   ```

It answers mail sent to `channel_address` from **you** — one of
`own_addresses`, and only if the message really is in your Sent folder, so a
forged "From: you" from outside is ignored — or from an address in
`allowed_external_senders` that passes Proton's own DMARC check. Everything
else is ignored without a reply.

Good to know:

- **Handled mail is filed**: marked read and moved, with a copy of the reply,
  to the `Aivyx` folder (`folder` changes it). If a reply can't be sent, the
  email stays unread in your inbox.
- **It starts from now.** Mail already in your inbox when the channel first
  runs is never answered; after a restart it carries on where it stopped.
- **One email, one turn.** The new part of the message is read (quoted
  history is dropped), up to 32 KiB; attachments aren't opened. The reply
  says when it cut or ignored something.
- **`/team` and gate commands** go on the first line of the email, and work
  only through the daemon. There's no `/cancel`.
- **The mail tools aren't available inside an email turn** — like Gmail's,
  they need the trusted terminal or Studio. `proton.send` asks you to approve
  every send.
- **External senders**: DMARC proves the sender's domain sent the message,
  not who typed it, and replies to them leave Proton as ordinary email. Mail
  from another Proton account may carry no DMARC result and then isn't
  answered.
- **If you reinstall or reset Bridge**, its certificate changes and the
  channel stops: run `aivyx-pa connect proton` again. Same if you change
  `imap_addr` or `smtp_addr`.
- **One channel process at a time** per instance.
````

- [ ] **Step 3: CLI reference** — in `docs/manual/reference/01-cli.md`: add `proton` to the `--channel` list (`` `matrix`, `proton` or `voice` ``), and after the `### aivyx-pa matrix` section:

````markdown
### `aivyx-pa connect proton`

Connects to Proton Mail Bridge using the `[proton]` section of your config:
shows and pins Bridge's IMAP and SMTP certificates, asks for the username and
password Bridge shows you (not your Proton password), checks they work, and
saves them to the instance's `tool-processes/proton/credentials.toml`
(readable only by you). Needs no passphrase and no daemon. Run it again after
reinstalling Bridge or changing `imap_addr` / `smtp_addr`.

```
aivyx-pa connect proton
```

Then run the channel with `aivyx-pa --channel proton`.
````

The `every_command_has_a_cli_reference_heading` drift test must still pass (`proton` isn't a new top-level command; `connect` already has a heading).

- [ ] **Step 4: TOOLS.md** — after the `## Email — Gmail` table:

```markdown
## Email — Proton Mail (tool process `aivyx-proton`)

Through Proton Mail Bridge on this machine; set up with `aivyx-pa connect proton`.

| Tool | Scope | Min tier | Notes |
|---|---|---|---|
| `proton.search` / `proton.read` | `email.read` | Trusted | search All Mail / read one message; output untrusted |
| `proton.draft` | `email.write` | Trusted | save a draft (never sent) |
| `proton.send` | `email.send` | Trusted | send now; approval on every call |
```

and in the provider-neutral table near the end (the row `| gmail.* | email.* | … |`) add the row `` | `proton.*` | `email.*` | the same scopes as Gmail | ``.

- [ ] **Step 5: THREAT_MODEL.md** — in the tier table's `SemiTrusted` row examples add "an email to the Proton channel address, from you (proven by Sent) or an allowlisted sender with Proton's DMARC pass"; and after the **Matrix.** paragraph in §5.8:

```markdown
**Proton Mail.** An email is a turn only when it is addressed to
`channel_address` and is either from one of `own_addresses` **and** the same
Proton message (`X-Pm-Internal-Id`, which Bridge always sets) is in the
account's Sent folder, or from `allowed_external_senders` **and** the first
`Authentication-Results` value is Proton's own (`mail.protonmail.ch`)
`dmarc=pass` for the From domain. Sent-folder membership is account metadata,
not a header, so a forged "From: you" from outside never qualifies; Proton's
results come before any the sender wrote, so a forged pass is never first.
External mail must also be delivered to the channel address (`Delivered-To` /
`X-Original-To`, set by Proton's receiving server). Everything else is
ignored without a reply (no backscatter). DMARC proves the sender's *domain*
authorised the message, not the person, and replies to external addresses
leave Proton as ordinary email. Bridge is reached only on loopback, over
STARTTLS pinned to the SHA-256 of its certificate (captured by `connect
proton`); a changed certificate stops the channel. The Bridge login is in a
`0600` file in the instance's Proton tool folder, the same posture as Gmail's
OAuth tokens: the sandboxed tool process can't reach a keyring. The
`proton.*` tools need `Trusted` and are not available inside an email turn.
```

- [ ] **Step 6: Run** `cargo test -p aivyx-cli && cargo test -p aivyx-config` → PASS (drift tests).

- [ ] **Step: Commit**

```bash
git add docs scripts
git commit -s -m "docs: the Proton Mail channel and tools — guide, references, TOOLS, threat model" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 16: Live tests against GreenMail (`#[ignore]`)

**Files:**
- Create: `crates/aivyx-proton/tests/live_greenmail.rs`

Two tests, skipped by default: the transport against a real IMAP/SMTP server, and the whole mail loop (with a stub runner) answering an email from you and ignoring a forged one. GreenMail has no STARTTLS, so the test dials plain TCP through its own `Dial`; the crate has no plaintext path.

- [ ] **Step 1: Write the tests**

```rust
//! Live test against a local GreenMail (IMAP + SMTP) in Docker. Run by hand:
//!
//!   docker run -d --rm --name aivyx-greenmail \
//!     -p 127.0.0.1:3025:3025 -p 127.0.0.1:3143:3143 greenmail/standalone:2.1.3
//!   cargo test -p aivyx-proton --test live_greenmail -- --ignored --nocapture
//!   docker stop aivyx-greenmail
//!
//! GreenMail has no STARTTLS, so the test dials it in plain TCP through its
//! own `Dial` — the crate itself has no plaintext path. Everything above
//! the dial (IMAP search/fetch/IDLE/move/append, SMTP, the mail loop) is
//! the production code. The pinned TLS is covered by `tls.rs`'s tests.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use aivyx_core::CancellationToken;
use aivyx_proton::bridge::{BridgeTransport, Dial, ImapClient, Login, Stream};
use aivyx_proton::outgoing::OutgoingMail;
use aivyx_proton::policy::ProtonPolicy;
use aivyx_proton::session::Turn;
use aivyx_proton::smtp;
use aivyx_proton::transport::{MailTransport, SearchQuery, TransportError};
use aivyx_proton::{run_mail_loop, TurnRunner};

const IMAP: &str = "127.0.0.1:3143";
const SMTP: &str = "127.0.0.1:3025";

struct PlainDial;

fn platform(e: impl std::fmt::Display) -> TransportError {
    TransportError::Platform(e.to_string())
}

#[async_trait]
impl Dial for PlainDial {
    async fn imap(&self) -> Result<ImapClient, TransportError> {
        let tcp = tokio::net::TcpStream::connect(IMAP).await.map_err(platform)?;
        let mut c = async_imap::Client::new(Box::new(tcp) as Stream);
        c.read_response().await.map_err(platform)?;
        Ok(c)
    }
    async fn smtp(&self) -> Result<smtp::Conn<Stream>, TransportError> {
        let tcp = tokio::net::TcpStream::connect(SMTP).await.map_err(platform)?;
        smtp::Conn::open(Box::new(tcp) as Stream).await.map_err(platform)
    }
}

fn transport(user: &str) -> BridgeTransport {
    BridgeTransport::new(
        Arc::new(PlainDial),
        Login { username: user.into(), password: "pw".into() },
        "Aivyx",
    )
}

/// A raw IMAP session for setting up and inspecting the test mailbox.
async fn raw(user: &str) -> async_imap::Session<Stream> {
    PlainDial.imap().await.unwrap().login(user, "pw").await.map_err(|(e, _)| e).unwrap()
}

async fn wait_for_mail(t: &BridgeTransport, after: u32) -> Vec<aivyx_proton::message::MailMessage> {
    tokio::time::timeout(Duration::from_secs(70), async {
        loop {
            let b = t.wait_for_new(after).await.unwrap();
            if !b.is_empty() {
                return b;
            }
        }
    })
    .await
    .expect("mail arrives")
}

#[tokio::test]
#[ignore = "needs a local GreenMail in Docker — see the module doc"]
async fn the_transport_works_against_a_real_mailbox() {
    let stamp = std::process::id();
    let me = format!("me{stamp}@proton.test");
    let t = transport(&me);
    let status = t.inbox_status().await.unwrap();
    let mut r = raw(&me).await;
    for mb in ["Sent", "All Mail", "Drafts"] {
        let _ = r.create(mb).await;
    }

    // SMTP in, IMAP fetch out.
    let mail = OutgoingMail::new(&me, vec![me.clone()], vec![], "Lunch?", "Where shall we eat?".into());
    t.send(&mail).await.unwrap();
    let got = wait_for_mail(&t, status.uid_next.saturating_sub(1)).await;
    assert_eq!(got[0].subject, "Lunch?");
    assert_eq!(got[0].message_id.as_deref(), Some(mail.message_id.as_str()));

    // The Sent lookup by X-Pm-Internal-Id.
    r.append("Sent", None, None, b"X-Pm-Internal-Id: abc123==\r\nFrom: x@y.z\r\nSubject: s\r\n\r\nb\r\n")
        .await
        .unwrap();
    assert!(t.in_sent("abc123==").await.unwrap());
    assert!(!t.in_sent("zzz").await.unwrap());

    // Filing: read, moved with a copy of the reply to Folders/Aivyx.
    let reply = OutgoingMail::reply(&me, vec![me.clone()], &got[0], "The usual place.".into());
    t.file_handled(got[0].uid, &reply).await.unwrap();
    r.select("Folders/Aivyx").await.unwrap();
    assert_eq!(r.uid_search("ALL").await.unwrap().len(), 2);
    assert_eq!(r.select("INBOX").await.unwrap().exists, 0);

    // All Mail search + fetch, and a draft.
    r.append("All Mail", None, None, b"From: ann@x.com\r\nSubject: invoice 42\r\nMessage-ID: <inv@x.com>\r\n\r\nplease pay\r\n")
        .await
        .unwrap();
    let hits = t
        .search(&SearchQuery { subject: Some("invoice".into()), max_results: 5, ..Default::default() })
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].from.as_deref(), Some("ann@x.com"));
    assert!(t.fetch(hits[0].uid).await.unwrap().unwrap().text.contains("please pay"));
    t.append_draft(&OutgoingMail::new(&me, vec!["ann@x.com".into()], vec![], "Re: invoice", "paid".into()))
        .await
        .unwrap();
    r.select("Drafts").await.unwrap();
    assert_eq!(r.uid_search("ALL").await.unwrap().len(), 1);

    // IDLE wakes for new mail well before its 60 s timeout.
    let last = r.select("INBOX").await.unwrap().uid_next.unwrap_or(1) - 1;
    let me2 = me.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let m = OutgoingMail::new(&me2, vec![me2.clone()], vec![], "second", "x".into());
        transport(&me2).send(&m).await.unwrap();
    });
    let start = std::time::Instant::now();
    let b = wait_for_mail(&transport(&me), last).await;
    assert_eq!(b[0].subject, "second");
    assert!(start.elapsed() < Duration::from_secs(30), "IDLE took {:?}", start.elapsed());
    r.logout().await.unwrap();
}

/// Answers every turn with its subject, uppercased.
struct Echo;

#[async_trait]
impl TurnRunner for Echo {
    async fn run(&mut self, turn: &Turn) -> Result<Option<String>, String> {
        Ok(Some(turn.message.subject.to_uppercase()))
    }
}

#[tokio::test]
#[ignore = "needs a local GreenMail in Docker — see the module doc"]
async fn the_mail_loop_answers_mail_from_you_and_files_it() {
    let stamp = std::process::id();
    let me = format!("me{stamp}l@proton.test");
    let bot = format!("bot{stamp}l@proton.test");
    let mut r = raw(&me).await;
    let _ = r.create("Sent").await;
    let dir = tempfile::tempdir().unwrap();
    let policy = ProtonPolicy::new(&bot, &[me.clone(), bot.clone()], &[]);
    let shutdown = CancellationToken::new();
    let loop_shutdown = shutdown.clone();
    let loop_dir = dir.path().to_path_buf();
    let loop_user = me.clone();
    let looper = tokio::spawn(async move {
        let t = transport(&loop_user);
        let mut echo = Echo;
        run_mail_loop(&t, &policy, &loop_dir, &mut echo, loop_shutdown).await
    });
    // Let the loop take its "now" cursor before the mail arrives.
    tokio::time::sleep(Duration::from_secs(2)).await;

    // You → the bot address, delivered to your own inbox (one account), and
    // a copy in Sent carrying the same X-Pm-Internal-Id — as Proton does.
    let raw_mail = format!(
        "X-Pm-Internal-Id: pm{stamp}==\r\nFrom: {me}\r\nTo: {bot}\r\nMessage-ID: <q{stamp}@proton.test>\r\n\
         Subject: hello bot\r\n\r\nare you there?\r\n"
    );
    r.append("Sent", None, None, raw_mail.as_bytes()).await.unwrap();
    r.append("INBOX", None, None, raw_mail.as_bytes()).await.unwrap();
    // A forgery: same From, not in Sent.
    let forged = format!(
        "X-Pm-Internal-Id: forged{stamp}==\r\nFrom: {me}\r\nTo: {bot}\r\nSubject: forged\r\n\r\nx\r\n"
    );
    r.append("INBOX", None, None, forged.as_bytes()).await.unwrap();

    // The reply (from the bot address) lands back in this same inbox.
    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    let reply = loop {
        assert!(std::time::Instant::now() < deadline, "no reply arrived");
        r.select("INBOX").await.unwrap();
        let uids = r.uid_search(format!("FROM \"{bot}\"")).await.unwrap();
        if let Some(uid) = uids.into_iter().next() {
            use futures_util::TryStreamExt;
            let f: Vec<_> = r.uid_fetch(uid.to_string(), "BODY.PEEK[]").await.unwrap().try_collect().await.unwrap();
            break aivyx_proton::message::parse(uid, f[0].body().unwrap()).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    assert_eq!(reply.subject, "Re: hello bot");
    assert_eq!(reply.in_reply_to.as_deref(), Some(format!("q{stamp}@proton.test").as_str()));
    assert!(reply.text.contains("HELLO BOT"));
    // The forgery got nothing and stays unread in the inbox; the real one was filed.
    r.select("Folders/Aivyx").await.unwrap();
    assert_eq!(r.uid_search("ALL").await.unwrap().len(), 2, "original + reply copy");
    r.select("INBOX").await.unwrap();
    assert_eq!(r.uid_search("SUBJECT \"forged\" UNSEEN").await.unwrap().len(), 1);

    shutdown.cancel();
    let report = looper.await.unwrap().unwrap();
    assert_eq!(report.answered, 1);
    r.logout().await.unwrap();
}
```

- [ ] **Step 2: Run** `cargo test -p aivyx-proton --test live_greenmail` → both listed as ignored. Then with GreenMail running as in the doc comment: `cargo test -p aivyx-proton --test live_greenmail -- --ignored --nocapture` → 2 passed.

- [ ] **Step: Commit**

```bash
git add crates/aivyx-proton/tests/live_greenmail.rs
git commit -s -m "test(proton): live transport and mail-loop tests against GreenMail (ignored)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

## Final check

- [ ] `cargo clippy --workspace --all-targets -- -D warnings` — clean
- [ ] `cargo test --workspace` — all pass
- [ ] Live: both GreenMail tests pass (Task 16).
- [ ] **Manual, against the operator's real Bridge** (needs the paid account; can't run in CI). Record what you see in the progress ledger:
  1. `aivyx-pa connect proton` shows two fingerprints, accepts the Bridge login, and writes `credentials.toml` with mode `600`.
  2. Email from your main address to the channel address → a threaded reply arrives; the original and a copy of the reply are in `Folders/Aivyx`. Save that email's headers (address redacted) as `crates/aivyx-proton/tests/fixtures/proton-internal-from-you.headers` and note whether it carries `Delivered-To` (the To/Cc rule exists in case it doesn't).
  3. From an allowlisted external address (e.g. a Gmail account) → answered.
  4. A forged "From: <your Proton address>" sent from outside Proton → ignored, left unread.
  5. In the terminal: `proton.search` finds a message; `proton.send` asks for approval before sending.
  6. Stop and restart `--channel proton`: nothing is answered twice.
