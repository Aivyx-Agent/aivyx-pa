# Signal Channel and `signal.send` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The operator messages their assistant on Signal (the assistant has its own number) and gets the answer back there; and the agent can send a Signal message to a contact the operator listed, with approval each time.

**Architecture:** A new crate `crates/aivyx-signal` talks JSON-RPC 2.0 to the operator's own signal-cli daemon over a Unix socket in the instance's signal tool folder (signal-cli holds the account; libsignal is AGPL so it is never linked). It is shaped like `aivyx-matrix`/`aivyx-proton`: a JSON-RPC client, envelope parsing, a `SignalTransport` trait with a scripted double, a pure verified-safety-number policy, a `ChannelContext` and an in-process session loop, plus a daemon frontend and the `aivyx-signal` tool process serving `signal.send` (new scope `message.send`). `aivyx-core` gains `ChannelPlatform::Signal`, `aivyx-config` a `[signal]` section, the CLI `--channel signal`, `connect signal` and `signal verify`.

**Tech Stack:** Rust 2024, tokio (Unix sockets), serde_json; no new external crates.

**Spec:** `docs/superpowers/specs/2026-10-12-signal-channel-design.md`.

**How this plan was checked:** every code block below was built and run before the plan was written, on a scratch branch of this repo (based on `main` at `7cb6a184`, deleted afterwards): `cargo clippy --workspace --all-targets -- -D warnings` clean and `cargo test --workspace` passing (7,272 tests, 0 failed). The JSON-RPC client and `CliTransport` are tested against a fake signal-cli on a real Unix socket (including reconnect and resubscribe). The JSON field and method names come from signal-cli's source (`JsonMessageEnvelope`, `JsonDataMessage`, `ListIdentitiesCommand`, `TrustCommand`, `SendTypingCommand`, `SendReceiptCommand`, `SignalJsonRpcDispatcherHandler`, `TrustLevel`); the envelope fixtures are built from those field names, not recorded — the manual checklist (Final check) records real ones.

## Where this plan differs from the spec's wording

- **No identity cache:** the sender's trust level is read from signal-cli on every message (signal-cli doesn't report key changes to clients, so a cache could serve a stale "verified").
- **`connect signal`** checks the socket answers (`version`), not which account it serves (single-account signal-cli doesn't expose it); the socket lives in the instance's own folder and the operator starts signal-cli with `-a <account>`.
- **Disappearing timer:** signal-cli applies the chat's timer to outgoing messages itself; pa adds no code for it (it's on the manual checklist).
- **`signal verify` covers `send_to` too:** with `--trust-new-identities never`, signal-cli refuses to send to a contact whose key isn't verified.
- **The tool's `send_to`** reaches the sandboxed tool process through `tool-processes/signal/signal-tool.toml`, written by `connect signal` (the tool can't read `aivyx-pa.toml`).
- **Allowed senders** may be E.164 numbers or ACIs (UUIDs), in case Signal hides a number.

## Global Constraints

- A Signal event becomes a turn (at `TrustTier::SemiTrusted`) only when it is a 1:1 `dataMessage` with text (no `groupInfo`, `reaction`, `remoteDelete`, `sticker`, `payment`; not a sync/receipt/typing event), from a sender in `allowed_senders` (by number or ACI), whose identity key signal-cli reports as `TRUSTED_VERIFIED`. An allowed sender with any other trust level (or no key) gets `ASK_TO_VERIFY` once per run; everyone else is ignored silently.
- signal-cli runs as the operator's daemon: `signal-cli -a <account> --trust-new-identities never daemon --socket <tool_process_dir("signal")>/signal-cli.sock --receive-mode manual --ignore-attachments`. Only the channel calls `subscribeReceive`; the tool never does.
- signal-cli unreachable → retried with backoff (1 s doubling to 60 s), never fatal; an unregistered account (`Authorization failed` / `[401]` / `[403]`) → fatal with "re-register … then run `aivyx-pa connect signal`".
- `signal.send`: scope `message.send` (new base: `KNOWN_BASES`, `CEILING_TRUSTED` only, `WITHHELD_INTEGRATION_BASES`), only to `send_to` numbers, text ≤ 8 KiB, split into parts of ≤ 2000 characters.
- Turn input ≤ 4 KiB; replies split into parts of ≤ 2000 characters; attachments never downloaded.
- One `--channel signal` per instance (flock in `InstancePaths::signal_dir()` = `<data dir>/signal`). Every aivyx-pa path is built in `aivyx-instance`.
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` must pass after every task (use `-j 8`; pa's `target/` is large). Don't run `cargo fmt`. Commits are signed off (`git commit -s`) and end with a `Co-Authored-By:` trailer naming the implementer's model.

## File Structure

| File | Responsibility |
|---|---|
| `crates/aivyx-capability/src/lib.rs`, `docs/TOOLS.md`, `docs/amendments/2026-04-17-capability-taxonomy-growth.md` (modify) | `message.send` |
| `crates/aivyx-core/src/lib.rs`, `crates/aivyx-ipc/src/protocol.rs`, `crates/aivyx-channel/src/daemon_server.rs` (modify) | `ChannelPlatform::Signal` + wire mirror |
| `crates/aivyx-instance/src/lib.rs` (modify) | `InstancePaths::signal_dir()` |
| `crates/aivyx-signal/Cargo.toml`, `src/lib.rs`, `src/main.rs` (create) | crate, tool-process binary |
| `crates/aivyx-signal/src/rpc.rs` | JSON-RPC client over the socket |
| `crates/aivyx-signal/src/envelope.rs`, `tests/fixtures/*.json` | `receive` → `Inbound` |
| `crates/aivyx-signal/src/transport.rs` | `SignalTransport`, `CliTransport`, `ScriptedTransport` |
| `crates/aivyx-signal/src/policy.rs`, `src/lock.rs` | trust policy; channel lock |
| `crates/aivyx-signal/src/signal_channel.rs`, `src/session.rs`, `src/tests.rs` | channel, session loop, session tests |
| `crates/aivyx-signal/src/toolconf.rs`, `src/tools/*.rs` | socket/tool-config paths; `signal.send` |
| `crates/aivyx-config/src/{lib,tests}.rs` (modify) | `[signal]` |
| `crates/aivyx-channel/src/signal_daemon_frontend.rs` (create) + wiring | daemon mode |
| `crates/aivyx-cli/...` (modify; create `signal_cli.rs`) | `--channel signal`, `connect signal`, `signal verify` |
| `docs/…`, `scripts/gen-*-reference.py` (modify) | guide, references, threat model |

---

### Task 1: The `message.send` capability

**Files:**
- Modify: `crates/aivyx-capability/src/lib.rs` (`KNOWN_BASES`, `CEILING_TRUSTED`, `WITHHELD_INTEGRATION_BASES`, three tests)
- Modify: `docs/TOOLS.md` (a Signal section — the `tools_catalog_documents_every_known_base` test requires every base there)
- Modify: `docs/amendments/2026-04-17-capability-taxonomy-growth.md` (an addendum; the base-count test names it)

**Interfaces:**
- Produces: the scope base `message.send` — Trusted-only, withheld (approval on every call).

- [ ] **Step 1: The failing tests** — in `crates/aivyx-capability/src/lib.rs`'s test module: add `"message.send"` after `"email.send"` in `withheld_integration_bases_classify_correctly`'s list, bump `known_bases_count_matches_phase_143_a3_addendum` to `101`, and add `message_send_is_trusted_only_and_withheld` (all in the diff below). Run `cargo test -j 8 -p aivyx-capability` → FAIL.
- [ ] **Step 2: Implement** — apply the rest of this diff:

```diff
diff --git a/crates/aivyx-capability/src/lib.rs b/crates/aivyx-capability/src/lib.rs
index 54679ca3..5c85deca 100644
--- a/crates/aivyx-capability/src/lib.rs
+++ b/crates/aivyx-capability/src/lib.rs
@@ -103,6 +103,13 @@ const KNOWN_BASES: &[&str] = &[
     "email.read",
     "email.write",
     "email.send",
+    // Signal (aivyx-signal tool process). `message.send` gates
+    // `signal.send`: a text message from the assistant's own Signal
+    // number to a contact the operator listed in `[signal] send_to`.
+    // Trusted-tier only and withheld (approval on every call), like
+    // email.send. Not `channel.send`, which remote channels' ceilings
+    // carry.
+    "message.send",
     // Personal assistant tool bundle (Phase 125 — Chapter G #1,
     // aivyx-toolkit third-party tool process). Five bases for
     // the bundled tool surface:
@@ -656,6 +663,7 @@ pub fn is_read_only_base(base: &str) -> bool {
 const WITHHELD_INTEGRATION_BASES: &[&str] = &[
     "email.write",
     "email.send",
+    "message.send",
     "drive.write",
     "notion.write",
     "obsidian.write",
@@ -1265,6 +1273,8 @@ static CEILING_TRUSTED: LazyLock<CapabilitySet> = LazyLock::new(|| {
         "email.read",
         "email.write",
         "email.send",
+        // Signal `signal.send` — Trusted-tier only, like email.send.
+        "message.send",
         // Phase 125 — Personal assistant tool bundle (Chapter
         // G #1). Five bases for the aivyx-toolkit tool process
         // surface; same Trusted-only default as email.* and
@@ -1515,6 +1525,7 @@ mod tests {
         for base in [
             "email.write",
             "email.send",
+            "message.send",
             "drive.write",
             "notion.write",
             "obsidian.write",
@@ -2539,16 +2550,27 @@ mod tests {
         // Model routing Part 3a adds routing.status + routing.read — the
         // router's candidate list and per-session decision read gates
         // (infrastructure, no P10 amendment).
+        // Signal adds message.send — the signal.send tool's base
+        // (Trusted-only, withheld like email.send).
         // Any change here means updating the addendum's
         // "Current full enumeration" section in the same PR.
         assert_eq!(
             KNOWN_BASES.len(),
-            100,
+            101,
             "If KNOWN_BASES grew, also update the A3 addendum's \
              latest count + per-base list."
         );
     }
 
+    #[test]
+    fn message_send_is_trusted_only_and_withheld() {
+        let s = |x: &str| Scope::parse(x).expect("message.send parses");
+        assert!(TrustTier::Trusted.default_ceiling().grants(&s("message.send")));
+        assert!(!TrustTier::SemiTrusted.default_ceiling().grants(&s("message.send")));
+        assert!(!TrustTier::Untrusted.default_ceiling().grants(&s("message.send")));
+        assert!(is_withheld_integration_base("message.send"));
+    }
+
     #[test]
     fn vision_generate_is_a_known_base_reachable_at_semitrusted() {
         // Aivyx-Vision Milestone 1 — vision.generate must parse (it's a
diff --git a/docs/TOOLS.md b/docs/TOOLS.md
index ecec2b0b..d89d1ba6 100644
--- a/docs/TOOLS.md
+++ b/docs/TOOLS.md
@@ -196,6 +196,14 @@ Through Proton Mail Bridge on this machine; set up with `aivyx-pa connect proton
 | `proton.draft` | `email.write` | Trusted | save a draft (never sent) |
 | `proton.send` | `email.send` | Trusted | send now; approval on every call |
 
+## Signal (tool process `aivyx-signal`)
+
+Through the operator's own signal-cli daemon; set up with `aivyx-pa connect signal`.
+
+| Tool | Scope | Min tier | Notes |
+|---|---|---|---|
+| `signal.send` | `message.send` | Trusted | send a text from the assistant's Signal number to a `[signal] send_to` contact; approval on every call |
+
 ## Calendar (tool process `aivyx-calendar`)
 
 | Tool | Scope | Min tier | Notes |
diff --git a/docs/amendments/2026-04-17-capability-taxonomy-growth.md b/docs/amendments/2026-04-17-capability-taxonomy-growth.md
index 2b9ffe91..ff6d7ac9 100644
--- a/docs/amendments/2026-04-17-capability-taxonomy-growth.md
+++ b/docs/amendments/2026-04-17-capability-taxonomy-growth.md
@@ -873,6 +873,27 @@ infrastructure bases). The
 `known_bases_count_matches_phase_143_a3_addendum` test pins the new
 total at **100**, so this addendum and the runtime stay in sync.
 
+## Signal addendum — `message.send` (2026-10-12)
+
+> *Added for the Signal channel's tool process (`aivyx-signal`).
+> `message.send` gates `signal.send`: a text message from the
+> assistant's own Signal number to a contact the operator listed in
+> `[signal] send_to`. A third-party integration send, the same category
+> as `email.send` — not substrate, so no P10 amendment. Bare base,
+> `CEILING_TRUSTED` only, and in the withheld-integration list, so the
+> turn loop pauses for approval on every call. Deliberately not
+> `channel.send`, which remote channels' ceilings carry.*
+
+| Chapter | Bases added | Provenance |
+|---|---|---|
+| Signal | `message.send` | `signal.send` (Trusted-tier only, withheld like `email.send`) |
+
+### Running count
+
+`KNOWN_BASES.len()` moves **100 → 101**. The
+`known_bases_count_matches_phase_143_a3_addendum` test pins the new
+total at **101**.
+
 ## Phase 129 addendum — Chapter F #3 Google Drive (2026-06-01)
 
 > *Added at Phase 129 exit. Chapter F's third integration —
```

- [ ] **Step 3: Run** `cargo test -j 8 -p aivyx-capability` → PASS (85 tests).

- [ ] **Step: Commit**

```bash
git add crates/aivyx-capability/src/lib.rs docs/TOOLS.md docs/amendments/2026-04-17-capability-taxonomy-growth.md
git commit -s -m "feat(capability): message.send — Trusted-only, withheld, for signal.send" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: `ChannelPlatform::Signal` and the instance's Signal folder

**Files:**
- Modify: `crates/aivyx-core/src/lib.rs` (`ChannelPlatform::Signal`, `config_name` → `"signal"`, the round-trip test list)
- Modify: `crates/aivyx-ipc/src/protocol.rs` (`WireChannelPlatform::Signal`)
- Modify: `crates/aivyx-channel/src/daemon_server.rs` (the core → wire mapping)
- Modify: `crates/aivyx-instance/src/lib.rs` (`signal_dir()` + a test)

**Interfaces:**
- Produces: `ChannelPlatform::Signal` (`config_name() == "signal"`, so `[routing.sensitive] channels = ["signal"]` works); `InstancePaths::signal_dir(&self) -> Option<PathBuf>` = `data_dir()/signal`.

- [ ] **Step 1: Failing tests** — add `ChannelPlatform::Signal` to `channel_config_names_are_the_lowercase_serde_names`'s list and the `signal_dir_is_under_the_instance_data_dir` test (both in the diff). `cargo test -j 8 -p aivyx-core channel_config_names -p aivyx-instance` → FAIL to compile.
- [ ] **Step 2: Implement:**

```diff
diff --git a/crates/aivyx-channel/src/daemon_server.rs b/crates/aivyx-channel/src/daemon_server.rs
index 6afbddc8..12baefaf 100644
--- a/crates/aivyx-channel/src/daemon_server.rs
+++ b/crates/aivyx-channel/src/daemon_server.rs
@@ -4475,6 +4475,7 @@ pub fn to_wire_channel_platform(platform: aivyx_core::ChannelPlatform) -> WireCh
         aivyx_core::ChannelPlatform::Slack => WireChannelPlatform::Slack,
         aivyx_core::ChannelPlatform::Matrix => WireChannelPlatform::Matrix,
         aivyx_core::ChannelPlatform::Email => WireChannelPlatform::Email,
+        aivyx_core::ChannelPlatform::Signal => WireChannelPlatform::Signal,
         aivyx_core::ChannelPlatform::Rest => WireChannelPlatform::Rest,
         aivyx_core::ChannelPlatform::Voice => WireChannelPlatform::Voice,
     }
diff --git a/crates/aivyx-core/src/lib.rs b/crates/aivyx-core/src/lib.rs
index 0d1871d1..6bdc26cc 100644
--- a/crates/aivyx-core/src/lib.rs
+++ b/crates/aivyx-core/src/lib.rs
@@ -296,6 +296,7 @@ pub enum ChannelPlatform {
     Slack,
     Matrix,
     Email,
+    Signal,
     /// HTTP API, not bound locally
     Rest,
     /// Phase 135 — Voice I/O channel. Operator speaks
@@ -1367,6 +1368,7 @@ impl ChannelPlatform {
             ChannelPlatform::Slack => "slack",
             ChannelPlatform::Matrix => "matrix",
             ChannelPlatform::Email => "email",
+            ChannelPlatform::Signal => "signal",
             ChannelPlatform::Rest => "rest",
             ChannelPlatform::Voice => "voice",
         }
@@ -1925,6 +1927,7 @@ mod tests {
             ChannelPlatform::Slack,
             ChannelPlatform::Matrix,
             ChannelPlatform::Email,
+            ChannelPlatform::Signal,
             ChannelPlatform::Rest,
             ChannelPlatform::Voice,
         ] {
diff --git a/crates/aivyx-instance/src/lib.rs b/crates/aivyx-instance/src/lib.rs
index 31337c5d..63fac166 100644
--- a/crates/aivyx-instance/src/lib.rs
+++ b/crates/aivyx-instance/src/lib.rs
@@ -304,6 +304,14 @@ impl InstancePaths {
         self.data_dir().map(|d| d.join("proton"))
     }
 
+    /// Signal channel state: `data_dir/signal` — the per-process lock.
+    /// signal-cli's own socket is not here: it lives in
+    /// `tool_process_dir("signal")`, which the sandboxed `signal.send`
+    /// tool can reach.
+    pub fn signal_dir(&self) -> Option<PathBuf> {
+        self.data_dir().map(|d| d.join("signal"))
+    }
+
     /// Sandbox directory: `~/aivyx-pa-sandbox[-<n>]`
     pub fn sandbox_dir(&self) -> Option<PathBuf> {
         self.dirs.home.as_ref().map(|h| {
@@ -483,6 +491,26 @@ mod tests {
         );
     }
 
+    #[test]
+    fn signal_dir_is_under_the_instance_data_dir() {
+        let dirs = BaseDirs {
+            home: Some(PathBuf::from("/h")),
+            xdg_config_home: None,
+            xdg_data_home: None,
+            xdg_runtime_dir: None,
+        };
+        let default = InstancePaths::new(InstanceName::default_instance(), dirs.clone());
+        assert_eq!(
+            default.signal_dir(),
+            Some(PathBuf::from("/h/.local/share/aivyx-pa/signal"))
+        );
+        let work = InstancePaths::new(InstanceName::parse("work").unwrap(), dirs);
+        assert_eq!(
+            work.signal_dir(),
+            Some(PathBuf::from("/h/.local/share/aivyx-pa/instances/work/signal"))
+        );
+    }
+
     #[test]
     fn names_are_validated() {
         for ok in ["default", "research", "a", "home-2", "x1"] {
diff --git a/crates/aivyx-ipc/src/protocol.rs b/crates/aivyx-ipc/src/protocol.rs
index 3c6a1803..de991e75 100644
--- a/crates/aivyx-ipc/src/protocol.rs
+++ b/crates/aivyx-ipc/src/protocol.rs
@@ -95,6 +95,10 @@ pub enum FrontendType {
     /// connection and applies the email trust policy; only approved mail
     /// reaches the daemon, one connection per email thread.
     Proton,
+    /// Signal daemon-frontend. The frontend process talks to signal-cli and
+    /// applies the verified-safety-number policy; only approved messages
+    /// reach the daemon, one connection per sender.
+    Signal,
 }
 
 // ---------------------------------------------------------------------------
@@ -2017,6 +2021,7 @@ pub enum WireChannelPlatform {
     Slack,
     Matrix,
     Email,
+    Signal,
     Rest,
     Voice,
 }
```

(The `protocol.rs` hunk shown also contains `FrontendType::Signal`; leave that variant for Task 10 — add only the `WireChannelPlatform::Signal` line here.)

- [ ] **Step 3: Run** `cargo test -j 8 -p aivyx-core channel_config_names && cargo test -j 8 -p aivyx-instance && cargo clippy -j 8 --workspace --all-targets -- -D warnings` → PASS (no other match on `ChannelPlatform`/`WireChannelPlatform` needs an arm; the compiler confirms).

- [ ] **Step: Commit**

```bash
git add crates/aivyx-core/src/lib.rs crates/aivyx-ipc/src/protocol.rs crates/aivyx-channel/src/daemon_server.rs crates/aivyx-instance/src/lib.rs
git commit -s -m "feat(core): ChannelPlatform::Signal; InstancePaths::signal_dir" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: `aivyx-signal` crate and the JSON-RPC client

- [ ] **Step 0: Scaffold the crate.**
  1. `crates/aivyx-signal/Cargo.toml`:

```toml
[package]
name = "aivyx-signal"
description = "Signal channel and signal.send over the operator's signal-cli daemon (JSON-RPC on a Unix socket)"
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

# The `signal.send` tool process, spawned by the daemon via `[[tool_process]]`.
[[bin]]
name = "aivyx-signal"
path = "src/main.rs"

[dependencies]
aivyx-core = { path = "../aivyx-core" }
aivyx-capability = { path = "../aivyx-capability" }
aivyx-instance = { path = "../aivyx-instance" }
aivyx-llm = { path = "../aivyx-llm" }
aivyx-storage = { path = "../aivyx-storage" }
aivyx-tool = { path = "../aivyx-tool" }
async-trait = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
tokio = { workspace = true, features = ["net", "io-util", "fs"] }
toml = "0.8"

# flock(2) for the channel lock (`lock.rs`).
[target.'cfg(unix)'.dependencies]
libc = { workspace = true }

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt", "rt-multi-thread", "time", "sync"] }
aivyx-audit = { path = "../aivyx-audit" }
aivyx-crypto = { path = "../aivyx-crypto" }
tempfile = "3"
```

  2. Add `"crates/aivyx-signal",` after `"crates/aivyx-proton",` in **both** `members` and `default-members` of the root `Cargo.toml`.
  3. `crates/aivyx-signal/src/lib.rs`:

```rust
//! `aivyx-signal` — Signal for Aivyx PA, through the operator's own
//! signal-cli daemon (JSON-RPC on a Unix socket): the channel
//! (`--channel signal`) and `signal.send` (the `aivyx-signal` tool
//! process). signal-cli holds the account; pa holds no Signal credential.
//!
//! Shape: like `aivyx-proton` — a JSON-RPC client (`rpc`), envelope
//! parsing (`envelope`), a transport trait + scripted double
//! (`transport`), the pure trust policy (`policy`), a channel
//! (`signal_channel`) and the receive loop (`session`). See
//! `docs/superpowers/specs/2026-10-12-signal-channel-design.md`.

pub mod rpc;
```

  4. `crates/aivyx-signal/src/main.rs` (placeholder, replaced in Task 8): `fn main() {}`

**Files:**
- Create: `crates/aivyx-signal/src/rpc.rs`
- Modify: `crates/aivyx-signal/src/lib.rs` (add `pub mod rpc;`, alphabetical)

**Interfaces:**
- Consumes: nothing
- Produces: `enum RpcError { Io(String), Remote { code: i64, message: String }, Closed }`; `struct RpcClient` with `async fn connect(&Path) -> Result<Self, RpcError>`, `fn from_stream<S: AsyncRead + AsyncWrite + Send + 'static>(S) -> Self`, `async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError>` (60 s timeout), `async fn next_notification(&self) -> Result<Value, RpcError>` (the `params` of each `receive` notification)

- [ ] **Step 1: Write the failing tests** — create `crates/aivyx-signal/src/rpc.rs` holding only this test module (the body goes above it in Step 3), and add `pub mod rpc;` to `lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::DuplexStream;

    /// A scripted signal-cli: answers each request line with `respond`,
    /// and first writes `before` (e.g. notifications).
    fn server(
        before: Vec<Value>,
        respond: fn(&Value) -> Option<Value>,
    ) -> (DuplexStream, tokio::task::JoinHandle<Vec<Value>>) {
        let (client, srv) = tokio::io::duplex(64 * 1024);
        let h = tokio::spawn(async move {
            let (r, mut w) = tokio::io::split(srv);
            for m in before {
                w.write_all(format!("{m}\n").as_bytes()).await.unwrap();
            }
            let mut lines = BufReader::new(r).lines();
            let mut seen = Vec::new();
            while let Ok(Some(line)) = lines.next_line().await {
                let req: Value = serde_json::from_str(&line).unwrap();
                if let Some(resp) = respond(&req) {
                    w.write_all(format!("{resp}\n").as_bytes()).await.unwrap();
                }
                seen.push(req);
            }
            seen
        });
        (client, h)
    }

    #[tokio::test]
    async fn calls_are_matched_by_id_and_errors_surface() {
        let (stream, _srv) = server(vec![], |req| {
            let id = req["id"].clone();
            Some(match req["method"].as_str() {
                Some("version") => json!({"jsonrpc":"2.0","id":id,"result":{"version":"0.14.6"}}),
                _ => json!({"jsonrpc":"2.0","id":id,"error":{"code":-1,"message":"Unknown method"}}),
            })
        });
        let c = RpcClient::from_stream(stream);
        assert_eq!(c.call("version", json!({})).await.unwrap()["version"], "0.14.6");
        assert_eq!(
            c.call("nope", json!({})).await.unwrap_err(),
            RpcError::Remote { code: -1, message: "Unknown method".into() }
        );
    }

    #[tokio::test]
    async fn requests_are_well_formed_json_rpc() {
        let (stream, srv) = server(vec![], |req| {
            Some(json!({"jsonrpc":"2.0","id":req["id"],"result":null}))
        });
        let c = RpcClient::from_stream(stream);
        c.call("send", json!({"recipient":["+15550001"],"message":"hi"})).await.unwrap();
        drop(c);
        let seen = srv.await.unwrap();
        assert_eq!(seen[0]["jsonrpc"], "2.0");
        assert_eq!(seen[0]["method"], "send");
        assert_eq!(seen[0]["params"]["recipient"][0], "+15550001");
    }

    #[tokio::test]
    async fn receive_notifications_arrive_in_order_and_others_are_ignored() {
        let (stream, _srv) = server(
            vec![
                json!({"jsonrpc":"2.0","method":"receive","params":{"n":1}}),
                json!({"jsonrpc":"2.0","method":"callEvent","params":{"n":99}}),
                json!({"jsonrpc":"2.0","method":"receive","params":{"n":2}}),
            ],
            |_| None,
        );
        let c = RpcClient::from_stream(stream);
        assert_eq!(c.next_notification().await.unwrap()["n"], 1);
        assert_eq!(c.next_notification().await.unwrap()["n"], 2);
    }

    #[tokio::test]
    async fn a_closed_connection_fails_waiting_calls_and_the_stream() {
        let (client, srv) = tokio::io::duplex(1024);
        let c = RpcClient::from_stream(client);
        drop(srv);
        // Writing to a closed pipe fails (Io) or the reader saw EOF first
        // (Closed); the transport treats both as "unreachable".
        assert!(matches!(
            c.call("version", json!({})).await,
            Err(RpcError::Closed | RpcError::Io(_))
        ));
        assert_eq!(c.next_notification().await.unwrap_err(), RpcError::Closed);
    }
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -j 8 -p aivyx-signal rpc::`
Expected: FAIL to compile — the items the tests use don't exist yet.

- [ ] **Step 3: Write the module** — above the test module in `crates/aivyx-signal/src/rpc.rs`:

```rust
//! JSON-RPC 2.0 to signal-cli's daemon over its Unix socket: one JSON
//! object per line each way, responses matched to requests by `id`, and
//! `receive` notifications handed out in arrival order.
//!
//! One background task reads the socket. When the connection ends, every
//! waiting call and the notification stream fail with [`RpcError::Closed`];
//! the transport (`transport::CliTransport`) then reconnects.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde_json::{json, Value};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, Mutex};

/// The longest line accepted from signal-cli (a `receive` notification with
/// a long message is a few KiB; anything near this is not signal-cli).
const MAX_LINE: u64 = 4 * 1024 * 1024;
/// How long one request may wait for its response.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RpcError {
    #[error("can't reach signal-cli: {0}")]
    Io(String),
    /// signal-cli answered with a JSON-RPC error.
    #[error("signal-cli: {message}")]
    Remote { code: i64, message: String },
    #[error("signal-cli closed the connection")]
    Closed,
}

type Pending = Arc<StdMutex<HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>>>;

pub struct RpcClient {
    writer: Mutex<Box<dyn AsyncWrite + Send + Unpin>>,
    pending: Pending,
    next_id: AtomicU64,
    notifications: Mutex<mpsc::UnboundedReceiver<Value>>,
    reader: tokio::task::JoinHandle<()>,
}

impl Drop for RpcClient {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl RpcClient {
    /// Connects to signal-cli's socket at `path`.
    pub async fn connect(path: &Path) -> Result<Self, RpcError> {
        let stream = tokio::net::UnixStream::connect(path)
            .await
            .map_err(|e| RpcError::Io(format!("{}: {e}", path.display())))?;
        Ok(Self::from_stream(stream))
    }

    /// Wraps any byte stream (tests use an in-memory pipe).
    pub fn from_stream<S: AsyncRead + AsyncWrite + Send + 'static>(stream: S) -> Self {
        let (read, write) = tokio::io::split(stream);
        let pending: Pending = Arc::default();
        let (tx, rx) = mpsc::unbounded_channel();
        let reader = tokio::spawn(read_loop(read, Arc::clone(&pending), tx));
        RpcClient {
            writer: Mutex::new(Box::new(write)),
            pending,
            next_id: AtomicU64::new(1),
            notifications: Mutex::new(rx),
            reader,
        }
    }

    /// Calls `method` and waits for its result.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("pending mutex poisoned")
            .insert(id, tx);
        let mut line = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .map_err(|e| RpcError::Io(e.to_string()))?;
        line.push(b'\n');
        let written = {
            let mut w = self.writer.lock().await;
            match w.write_all(&line).await {
                Ok(()) => w.flush().await,
                Err(e) => Err(e),
            }
        };
        if let Err(e) = written {
            self.pending.lock().expect("pending mutex poisoned").remove(&id);
            return Err(RpcError::Io(e.to_string()));
        }
        match tokio::time::timeout(CALL_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(RpcError::Closed),
            Err(_) => {
                self.pending.lock().expect("pending mutex poisoned").remove(&id);
                Err(RpcError::Io(format!("signal-cli didn't answer `{method}` in time")))
            }
        }
    }

    /// The next `receive` notification's `params`. `Closed` once the
    /// connection has ended and every earlier notification was taken.
    pub async fn next_notification(&self) -> Result<Value, RpcError> {
        self.notifications
            .lock()
            .await
            .recv()
            .await
            .ok_or(RpcError::Closed)
    }
}

async fn read_loop<R: AsyncRead + Unpin>(
    read: R,
    pending: Pending,
    notifications: mpsc::UnboundedSender<Value>,
) {
    let mut reader = BufReader::new(read);
    loop {
        let mut line = Vec::new();
        let n = match (&mut reader).take(MAX_LINE).read_until(b'\n', &mut line).await {
            Ok(n) => n,
            Err(_) => break,
        };
        if n == 0 || !line.ends_with(b"\n") {
            // EOF, or a line longer than MAX_LINE: the stream is unusable.
            break;
        }
        let Ok(msg) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        dispatch(msg, &pending, &notifications);
    }
    for (_, tx) in pending.lock().expect("pending mutex poisoned").drain() {
        let _ = tx.send(Err(RpcError::Closed));
    }
}

fn dispatch(msg: Value, pending: &Pending, notifications: &mpsc::UnboundedSender<Value>) {
    if let Some(id) = msg.get("id").and_then(Value::as_u64) {
        let Some(tx) = pending.lock().expect("pending mutex poisoned").remove(&id) else {
            return;
        };
        let result = match msg.get("error") {
            Some(err) => Err(RpcError::Remote {
                code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                message: err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_string(),
            }),
            None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
        };
        let _ = tx.send(result);
    } else if msg.get("method").and_then(Value::as_str) == Some("receive") {
        let _ = notifications.send(msg.get("params").cloned().unwrap_or(Value::Null));
    }
}
```

- [ ] **Step 4: Run** `cargo test -j 8 -p aivyx-signal rpc:: && cargo clippy -j 8 -p aivyx-signal --all-targets -- -D warnings` → PASS, no warnings.

- [ ] **Step: Commit**

```bash
git add crates/aivyx-signal/src/rpc.rs crates/aivyx-signal/src/lib.rs crates/aivyx-signal/Cargo.toml crates/aivyx-signal/src/main.rs Cargo.toml Cargo.lock
git commit -s -m "feat(signal): aivyx-signal crate; JSON-RPC client for signal-cli's socket" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Envelope parsing

Fixtures — built from signal-cli's `JsonMessageEnvelope` / `JsonDataMessage` field names (the manual checklist records real ones to replace them). Create them first:

`tests/fixtures/receive-direct.json`:
```json
{"envelope":{"source":"+15550001111","sourceNumber":"+15550001111","sourceUuid":"8F3C7A1E-0B5D-4C2A-9E61-3D7F2B9A4C10","sourceName":"Julian","sourceDevice":1,"timestamp":1791763166000,"serverReceivedTimestamp":1791763166120,"serverDeliveredTimestamp":1791763166200,"dataMessage":{"timestamp":1791763166000,"message":"What's on tomorrow?","expiresInSeconds":0,"viewOnce":false}},"account":"+15559990000"}
```

`tests/fixtures/receive-group.json`:
```json
{"envelope":{"source":"+15550001111","sourceNumber":"+15550001111","sourceUuid":"8f3c7a1e-0b5d-4c2a-9e61-3d7f2b9a4c10","sourceName":"Julian","sourceDevice":1,"timestamp":1791763167000,"serverReceivedTimestamp":1791763167100,"serverDeliveredTimestamp":1791763167200,"dataMessage":{"timestamp":1791763167000,"message":"hello group","expiresInSeconds":0,"viewOnce":false,"groupInfo":{"groupId":"aGVsbG8=","type":"DELIVER"}}},"account":"+15559990000"}
```

`tests/fixtures/receive-reaction.json`:
```json
{"envelope":{"source":"+15550001111","sourceNumber":"+15550001111","sourceUuid":"8f3c7a1e-0b5d-4c2a-9e61-3d7f2b9a4c10","sourceName":"Julian","sourceDevice":1,"timestamp":1791763170000,"serverReceivedTimestamp":1791763170100,"serverDeliveredTimestamp":1791763170200,"dataMessage":{"timestamp":1791763170000,"message":null,"expiresInSeconds":0,"viewOnce":false,"reaction":{"emoji":"👍","targetAuthor":"+15559990000","targetAuthorNumber":"+15559990000","targetAuthorUuid":"11111111-2222-3333-4444-555555555555","targetSentTimestamp":1791763166500,"isRemove":false}}},"account":"+15559990000"}
```

`tests/fixtures/receive-receipt.json`:
```json
{"envelope":{"source":"+15550001111","sourceNumber":"+15550001111","sourceUuid":"8f3c7a1e-0b5d-4c2a-9e61-3d7f2b9a4c10","sourceName":"Julian","sourceDevice":1,"timestamp":1791763169000,"serverReceivedTimestamp":1791763169100,"serverDeliveredTimestamp":1791763169200,"receiptMessage":{"when":1791763169000,"isDelivery":false,"isRead":true,"isViewed":false,"timestamps":[1791763166500]}},"account":"+15559990000"}
```

`tests/fixtures/receive-sync.json`:
```json
{"envelope":{"source":"+15559990000","sourceNumber":"+15559990000","sourceUuid":"11111111-2222-3333-4444-555555555555","sourceName":null,"sourceDevice":2,"timestamp":1791763168000,"serverReceivedTimestamp":1791763168100,"serverDeliveredTimestamp":1791763168200,"syncMessage":{"sentMessage":{"destination":"+15550001111","destinationNumber":"+15550001111","destinationUuid":"8f3c7a1e-0b5d-4c2a-9e61-3d7f2b9a4c10","timestamp":1791763168000,"message":"sent from another device","expiresInSeconds":0,"viewOnce":false}}},"account":"+15559990000"}
```

**Files:**
- Create: `crates/aivyx-signal/src/envelope.rs`
- Modify: `crates/aivyx-signal/src/lib.rs` (add `pub mod envelope;`, alphabetical)

**Interfaces:**
- Consumes: nothing
- Produces: `struct TextMessage { sender_number: Option<String>, sender_uuid: Option<String> /* lowercased */, timestamp: u64, text: String, attachments: usize }` (`Default`) + `fn sender(&self) -> &str` (number else ACI); `enum Inbound { Text(TextMessage), Other }`; `fn parse(params: &Value) -> Inbound`

- [ ] **Step 1: Write the failing tests** — create `crates/aivyx-signal/src/envelope.rs` holding only this test module (the body goes above it in Step 3), and add `pub mod envelope;` to `lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture(name: &str) -> Value {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn a_direct_text_message_is_parsed() {
        let Inbound::Text(m) = parse(&fixture("receive-direct.json")) else {
            panic!("expected text");
        };
        assert_eq!(m.sender_number.as_deref(), Some("+15550001111"));
        assert_eq!(m.sender_uuid.as_deref(), Some("8f3c7a1e-0b5d-4c2a-9e61-3d7f2b9a4c10"));
        assert_eq!(m.timestamp, 1_791_763_166_000);
        assert_eq!(m.text, "What's on tomorrow?");
        assert_eq!(m.attachments, 0);
        assert_eq!(m.sender(), "+15550001111");
    }

    #[test]
    fn the_subscription_wrapper_is_unwrapped() {
        let wrapped = json!({"subscription": 0, "result": fixture("receive-direct.json")});
        assert!(matches!(parse(&wrapped), Inbound::Text(_)));
    }

    #[test]
    fn groups_sync_receipts_and_reactions_are_not_turns() {
        for name in ["receive-group.json", "receive-sync.json", "receive-receipt.json", "receive-reaction.json"] {
            assert_eq!(parse(&fixture(name)), Inbound::Other, "{name}");
        }
    }

    #[test]
    fn attachments_are_counted_and_empty_text_ignored() {
        let mut v = fixture("receive-direct.json");
        v["envelope"]["dataMessage"]["attachments"] = json!([{"id": "a"}, {"id": "b"}]);
        let Inbound::Text(m) = parse(&v) else { panic!() };
        assert_eq!(m.attachments, 2);
        v["envelope"]["dataMessage"]["message"] = json!("   ");
        assert_eq!(parse(&v), Inbound::Other);
        v["envelope"]["dataMessage"]["message"] = Value::Null;
        assert_eq!(parse(&v), Inbound::Other);
    }

    #[test]
    fn a_hidden_number_falls_back_to_the_aci() {
        let mut v = fixture("receive-direct.json");
        v["envelope"]["sourceNumber"] = Value::Null;
        let Inbound::Text(m) = parse(&v) else { panic!() };
        assert_eq!(m.sender(), "8f3c7a1e-0b5d-4c2a-9e61-3d7f2b9a4c10");
        v["envelope"]["sourceUuid"] = Value::Null;
        assert_eq!(parse(&v), Inbound::Other);
    }
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -j 8 -p aivyx-signal envelope::`
Expected: FAIL to compile — the items the tests use don't exist yet.

- [ ] **Step 3: Write the module** — above the test module in `crates/aivyx-signal/src/envelope.rs`:

```rust
//! signal-cli `receive` notifications → [`Inbound`]. Only a 1:1 message
//! with text is interesting; everything else (groups, sync messages from
//! the assistant's own devices, receipts, typing, reactions, edits,
//! deletes, stories, calls) is [`Inbound::Other`].

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TextMessage {
    /// E.164, when Signal shares it.
    pub sender_number: Option<String>,
    /// The sender's ACI (UUID), lowercased.
    pub sender_uuid: Option<String>,
    /// The message's own timestamp — what a read receipt names.
    pub timestamp: u64,
    pub text: String,
    /// How many attachments came with it (never downloaded).
    pub attachments: usize,
}

impl TextMessage {
    /// How to address the sender back: the number if known, else the ACI.
    pub fn sender(&self) -> &str {
        self.sender_number
            .as_deref()
            .or(self.sender_uuid.as_deref())
            .unwrap_or("")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inbound {
    Text(TextMessage),
    Other,
}

/// `params` of a `receive` notification — either `{"envelope": …}` or,
/// for an explicit `subscribeReceive`, `{"subscription": n, "result":
/// {"envelope": …}}`.
pub fn parse(params: &Value) -> Inbound {
    let body = params.get("result").unwrap_or(params);
    let Some(env) = body.get("envelope") else {
        return Inbound::Other;
    };
    let Some(data) = env.get("dataMessage").filter(|d| d.is_object()) else {
        return Inbound::Other;
    };
    // Groups and anything that isn't a plain message are not turns.
    for key in ["groupInfo", "reaction", "remoteDelete", "sticker", "payment"] {
        if data.get(key).is_some_and(|v| !v.is_null()) {
            return Inbound::Other;
        }
    }
    let text = data
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if text.trim().is_empty() {
        return Inbound::Other;
    }
    let str_field = |k: &str| {
        env.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let sender_number = str_field("sourceNumber");
    let sender_uuid = str_field("sourceUuid").map(|u| u.to_ascii_lowercase());
    if sender_number.is_none() && sender_uuid.is_none() {
        return Inbound::Other;
    }
    let timestamp = data
        .get("timestamp")
        .and_then(Value::as_u64)
        .or_else(|| env.get("timestamp").and_then(Value::as_u64))
        .unwrap_or(0);
    Inbound::Text(TextMessage {
        sender_number,
        sender_uuid,
        timestamp,
        text,
        attachments: data
            .get("attachments")
            .and_then(Value::as_array)
            .map_or(0, Vec::len),
    })
}
```

- [ ] **Step 4: Run** `cargo test -j 8 -p aivyx-signal envelope:: && cargo clippy -j 8 -p aivyx-signal --all-targets -- -D warnings` → PASS, no warnings.

- [ ] **Step: Commit**

```bash
git add crates/aivyx-signal/src/envelope.rs crates/aivyx-signal/src/lib.rs crates/aivyx-signal/tests/fixtures
git commit -s -m "feat(signal): parse signal-cli receive envelopes" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: The transport

signal-cli method names and params (from its source): `subscribeReceive`, `send {recipient: [..], message}` → `{timestamp, results}`, `sendTyping {recipient: [..], stop}`, `sendReceipt {recipient, targetTimestamp: [..], type: "read"}`, `listIdentities {number}` → `[{number, uuid, fingerprint, safetyNumber, scannableSafetyNumber, trustLevel, addedTimestamp}]`, `trust {recipient, verifiedSafetyNumber}`, `version` → `{version}`. The tests run a fake signal-cli on a real Unix socket (`tempfile`), including a dropped connection that must reconnect and resubscribe.

**Files:**
- Create: `crates/aivyx-signal/src/transport.rs`
- Modify: `crates/aivyx-signal/src/lib.rs` (add `pub mod transport;`, alphabetical)

**Interfaces:**
- Consumes: `rpc::{RpcClient, RpcError}`, `envelope::{parse, Inbound}`
- Produces: `enum TrustLevel { Untrusted, TrustedUnverified, TrustedVerified }`; `struct Identity { trust, safety_number: String }`; `enum TransportError { Unreachable(String), Unregistered(String), Refused(String) }` + `is_fatal()` (Unregistered only), `is_untrusted_recipient()`, `From<RpcError>`; `trait SignalTransport { next_event, send_text(to, text) -> u64, typing(to, stop), read_receipt(to, ts), identity(who) -> Option<Identity>, trust_verified(who, safety_number), version() -> String }`; `CliTransport::new(PathBuf)`; `ScriptedTransport::new(Vec<Inbound>)` + `with_identity`, `failing_send`, `then_fail`, `push`, recorders `sent()`, `typing_events()`, `receipts()`, `trusted()`

- [ ] **Step 1: Write the failing tests** — create `crates/aivyx-signal/src/transport.rs` holding only this test module (the body goes above it in Step 3), and add `pub mod transport;` to `lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    #[test]
    fn errors_classify() {
        assert!(TransportError::Unregistered("x".into()).is_fatal());
        assert!(!TransportError::Unreachable("x".into()).is_fatal());
        assert!(!TransportError::Refused("x".into()).is_fatal());
        let e: TransportError = RpcError::Remote { code: -1, message: "Authorization failed!".into() }.into();
        assert!(e.is_fatal());
        assert!(e.to_string().contains("connect signal"));
        let e: TransportError = RpcError::Remote {
            code: -1,
            message: "Failed to send message due to untrusted identities".into(),
        }
        .into();
        assert!(e.is_untrusted_recipient());
        assert!(!e.is_fatal());
        assert!(matches!(TransportError::from(RpcError::Closed), TransportError::Unreachable(_)));
    }

    /// A fake signal-cli daemon on a real Unix socket: answers `version`,
    /// `subscribeReceive`, `listIdentities`, `send`; after a subscription
    /// it pushes one `receive` notification. `connections` counts accepts.
    async fn fake_daemon(path: PathBuf, drop_first_after_notify: bool) -> tokio::task::JoinHandle<()> {
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let mut n = 0;
            loop {
                let Ok((stream, _)) = listener.accept().await else { return };
                n += 1;
                let drop_after = drop_first_after_notify && n == 1;
                tokio::spawn(async move {
                    let (r, mut w) = stream.into_split();
                    let mut lines = BufReader::new(r).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let req: Value = serde_json::from_str(&line).unwrap();
                        let id = req["id"].clone();
                        let result = match req["method"].as_str().unwrap() {
                            "version" => json!({"version": "0.14.6"}),
                            "subscribeReceive" => json!(0),
                            "listIdentities" => json!([{
                                "number": req["params"]["number"], "uuid": "u",
                                "fingerprint": "ab", "safetyNumber": "12345 67890",
                                "scannableSafetyNumber": null,
                                "trustLevel": "TRUSTED_VERIFIED", "addedTimestamp": 1
                            }]),
                            "send" => json!({"timestamp": 42, "results": []}),
                            _ => json!(null),
                        };
                        let resp = json!({"jsonrpc": "2.0", "id": id, "result": result});
                        w.write_all(format!("{resp}\n").as_bytes()).await.unwrap();
                        if req["method"] == "subscribeReceive" {
                            let note = json!({"jsonrpc": "2.0", "method": "receive", "params": {
                                "subscription": 0,
                                "result": {"envelope": {
                                    "sourceNumber": "+15550001111", "sourceUuid": "u",
                                    "timestamp": 7, "dataMessage": {"timestamp": 7, "message": "hi"}
                                }}
                            }});
                            w.write_all(format!("{note}\n").as_bytes()).await.unwrap();
                            if drop_after {
                                return;
                            }
                        }
                    }
                });
            }
        })
    }

    #[tokio::test]
    async fn the_cli_transport_talks_json_rpc_over_the_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("signal-cli.sock");
        let _srv = fake_daemon(path.clone(), false).await;
        let t = CliTransport::new(path);
        assert_eq!(t.version().await.unwrap(), "0.14.6");
        let Inbound::Text(m) = t.next_event().await.unwrap() else { panic!() };
        assert_eq!(m.text, "hi");
        let id = t.identity("+15550001111").await.unwrap().unwrap();
        assert_eq!(id.trust, TrustLevel::TrustedVerified);
        assert_eq!(id.safety_number, "12345 67890");
        assert_eq!(t.send_text("+15550001111", "yo").await.unwrap(), 42);
    }

    #[tokio::test]
    async fn a_dropped_connection_reconnects_and_resubscribes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("signal-cli.sock");
        let _srv = fake_daemon(path.clone(), true).await;
        let t = CliTransport::new(path);
        assert!(matches!(t.next_event().await.unwrap(), Inbound::Text(_)));
        // The first connection closed after its notification.
        assert!(matches!(t.next_event().await, Err(TransportError::Unreachable(_))));
        // The next call reconnects and subscribes again.
        assert!(matches!(t.next_event().await.unwrap(), Inbound::Text(_)));
    }

    #[tokio::test]
    async fn no_daemon_is_unreachable_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let t = CliTransport::new(dir.path().join("missing.sock"));
        let e = t.version().await.unwrap_err();
        assert!(matches!(e, TransportError::Unreachable(_)), "{e}");
        assert!(!e.is_fatal());
    }
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -j 8 -p aivyx-signal transport::`
Expected: FAIL to compile — the items the tests use don't exist yet.

- [ ] **Step 3: Write the module** — above the test module in `crates/aivyx-signal/src/transport.rs`:

```rust
//! The seam between the channel/tool and signal-cli. Production is
//! [`CliTransport`] (JSON-RPC over signal-cli's socket); tests use
//! [`ScriptedTransport`]. No JSON crosses this boundary.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use thiserror::Error;
use tokio::sync::Mutex;

use crate::envelope::{self, Inbound};
use crate::rpc::{RpcClient, RpcError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustLevel {
    Untrusted,
    TrustedUnverified,
    TrustedVerified,
}

impl TrustLevel {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "UNTRUSTED" => Some(TrustLevel::Untrusted),
            "TRUSTED_UNVERIFIED" => Some(TrustLevel::TrustedUnverified),
            "TRUSTED_VERIFIED" => Some(TrustLevel::TrustedVerified),
            _ => None,
        }
    }
}

/// A contact's identity key as signal-cli knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub trust: TrustLevel,
    /// The safety number as the Signal app shows it (digits, may contain spaces).
    pub safety_number: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TransportError {
    /// signal-cli isn't reachable (not running, socket missing, connection
    /// dropped). Retried.
    #[error("signal-cli isn't reachable ({0}) — is the signal-cli daemon running?")]
    Unreachable(String),
    /// Signal's server no longer accepts the assistant's number. Retrying
    /// can't help.
    #[error(
        "Signal no longer accepts the assistant's number ({0}) — re-register it with \
         signal-cli, then run `aivyx-pa connect signal`"
    )]
    Unregistered(String),
    /// signal-cli refused this one request (e.g. an untrusted recipient key).
    #[error("signal-cli refused: {0}")]
    Refused(String),
}

impl TransportError {
    /// Ends the channel: no retry or backoff can recover from it.
    pub fn is_fatal(&self) -> bool {
        matches!(self, TransportError::Unregistered(_))
    }

    /// signal-cli refused to send because the recipient's identity key
    /// isn't trusted.
    pub fn is_untrusted_recipient(&self) -> bool {
        matches!(self, TransportError::Refused(m) if m.to_ascii_lowercase().contains("untrusted"))
    }
}

impl From<RpcError> for TransportError {
    fn from(e: RpcError) -> Self {
        match e {
            RpcError::Io(m) => TransportError::Unreachable(m),
            RpcError::Closed => TransportError::Unreachable("connection closed".into()),
            RpcError::Remote { message, .. } => {
                // Signal's server answers 401/403 for a number that was
                // unregistered or taken over elsewhere.
                let lower = message.to_ascii_lowercase();
                if lower.contains("authorization failed") || lower.contains("[401]") || lower.contains("[403]") {
                    TransportError::Unregistered(message)
                } else {
                    TransportError::Refused(message)
                }
            }
        }
    }
}

#[async_trait]
pub trait SignalTransport: Send + Sync {
    /// The next incoming event (subscribing to messages if needed).
    async fn next_event(&self) -> Result<Inbound, TransportError>;
    /// Sends `text` to `to` (E.164 or ACI); returns the message timestamp.
    async fn send_text(&self, to: &str, text: &str) -> Result<u64, TransportError>;
    /// Starts (`stop == false`) or stops the typing indicator for `to`.
    async fn typing(&self, to: &str, stop: bool) -> Result<(), TransportError>;
    /// A read receipt for `to`'s message sent at `timestamp`.
    async fn read_receipt(&self, to: &str, timestamp: u64) -> Result<(), TransportError>;
    /// `who`'s identity key, if signal-cli has seen one.
    async fn identity(&self, who: &str) -> Result<Option<Identity>, TransportError>;
    /// Marks `who`'s key verified, given the safety number the operator compared.
    async fn trust_verified(&self, who: &str, safety_number: &str) -> Result<(), TransportError>;
    /// signal-cli's version — proves the daemon answers.
    async fn version(&self) -> Result<String, TransportError>;
}

/// JSON-RPC to the signal-cli daemon at `socket`. Connects lazily and
/// reconnects after any connection error; subscribes to incoming messages
/// the first time [`SignalTransport::next_event`] is called on a
/// connection (only the channel does; the tool never subscribes).
pub struct CliTransport {
    socket: PathBuf,
    conn: Mutex<Option<Conn>>,
}

struct Conn {
    client: Arc<RpcClient>,
    subscribed: bool,
}

impl CliTransport {
    pub fn new(socket: PathBuf) -> Self {
        CliTransport {
            socket,
            conn: Mutex::new(None),
        }
    }

    async fn client(&self) -> Result<Arc<RpcClient>, TransportError> {
        let mut guard = self.conn.lock().await;
        if let Some(c) = guard.as_ref() {
            return Ok(Arc::clone(&c.client));
        }
        let client = Arc::new(RpcClient::connect(&self.socket).await?);
        *guard = Some(Conn {
            client: Arc::clone(&client),
            subscribed: false,
        });
        Ok(client)
    }

    /// Calls `method`; a connection error drops the connection so the next
    /// call reconnects.
    async fn call(&self, method: &str, params: Value) -> Result<Value, TransportError> {
        let client = self.client().await?;
        match client.call(method, params).await {
            Ok(v) => Ok(v),
            Err(e) => {
                if !matches!(e, RpcError::Remote { .. }) {
                    *self.conn.lock().await = None;
                }
                Err(e.into())
            }
        }
    }
}

#[async_trait]
impl SignalTransport for CliTransport {
    async fn next_event(&self) -> Result<Inbound, TransportError> {
        let client = self.client().await?;
        let needs_subscribe = {
            let guard = self.conn.lock().await;
            guard.as_ref().is_some_and(|c| !c.subscribed)
        };
        if needs_subscribe {
            self.call("subscribeReceive", json!({})).await?;
            if let Some(c) = self.conn.lock().await.as_mut() {
                c.subscribed = true;
            }
        }
        match client.next_notification().await {
            Ok(params) => Ok(envelope::parse(&params)),
            Err(e) => {
                *self.conn.lock().await = None;
                Err(e.into())
            }
        }
    }

    async fn send_text(&self, to: &str, text: &str) -> Result<u64, TransportError> {
        let r = self
            .call("send", json!({"recipient": [to], "message": text}))
            .await?;
        Ok(r.get("timestamp").and_then(Value::as_u64).unwrap_or(0))
    }

    async fn typing(&self, to: &str, stop: bool) -> Result<(), TransportError> {
        self.call("sendTyping", json!({"recipient": [to], "stop": stop}))
            .await
            .map(|_| ())
    }

    async fn read_receipt(&self, to: &str, timestamp: u64) -> Result<(), TransportError> {
        self.call(
            "sendReceipt",
            json!({"recipient": to, "targetTimestamp": [timestamp], "type": "read"}),
        )
        .await
        .map(|_| ())
    }

    async fn identity(&self, who: &str) -> Result<Option<Identity>, TransportError> {
        let r = self.call("listIdentities", json!({"number": who})).await?;
        Ok(r.as_array().and_then(|ids| {
            ids.iter().find_map(|id| {
                Some(Identity {
                    trust: TrustLevel::parse(id.get("trustLevel")?.as_str()?)?,
                    safety_number: id.get("safetyNumber")?.as_str()?.to_string(),
                })
            })
        }))
    }

    async fn trust_verified(&self, who: &str, safety_number: &str) -> Result<(), TransportError> {
        self.call(
            "trust",
            json!({"recipient": who, "verifiedSafetyNumber": safety_number}),
        )
        .await
        .map(|_| ())
    }

    async fn version(&self) -> Result<String, TransportError> {
        let r = self.call("version", json!({})).await?;
        Ok(r.get("version")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string())
    }
}

/// Test double: pops queued events, records everything sent.
#[derive(Default)]
pub struct ScriptedTransport {
    events: Mutex<VecDeque<Inbound>>,
    identities: HashMap<String, Identity>,
    fail_send: Option<TransportError>,
    fail_when_empty: Option<fn() -> TransportError>,
    sent: Mutex<Vec<(String, String)>>,
    typing: Mutex<Vec<(String, bool)>>,
    receipts: Mutex<Vec<(String, u64)>>,
    trusted: Mutex<Vec<(String, String)>>,
}

impl ScriptedTransport {
    pub fn new(events: Vec<Inbound>) -> Self {
        ScriptedTransport {
            events: Mutex::new(events.into()),
            ..Default::default()
        }
    }

    /// `who`'s identity as `identity()` reports it.
    pub fn with_identity(mut self, who: &str, trust: TrustLevel, safety_number: &str) -> Self {
        self.identities.insert(
            who.to_string(),
            Identity {
                trust,
                safety_number: safety_number.to_string(),
            },
        );
        self
    }

    /// Every `send_text` fails with `err`.
    pub fn failing_send(mut self, err: TransportError) -> Self {
        self.fail_send = Some(err);
        self
    }

    /// Once the events run out, `next_event` returns `Err(err())`.
    pub fn then_fail(mut self, err: fn() -> TransportError) -> Self {
        self.fail_when_empty = Some(err);
        self
    }

    pub async fn push(&self, event: Inbound) {
        self.events.lock().await.push_back(event);
    }
    pub async fn sent(&self) -> Vec<(String, String)> {
        self.sent.lock().await.clone()
    }
    pub async fn typing_events(&self) -> Vec<(String, bool)> {
        self.typing.lock().await.clone()
    }
    pub async fn receipts(&self) -> Vec<(String, u64)> {
        self.receipts.lock().await.clone()
    }
    pub async fn trusted(&self) -> Vec<(String, String)> {
        self.trusted.lock().await.clone()
    }
}

#[async_trait]
impl SignalTransport for ScriptedTransport {
    async fn next_event(&self) -> Result<Inbound, TransportError> {
        loop {
            if let Some(e) = self.events.lock().await.pop_front() {
                return Ok(e);
            }
            if let Some(err) = self.fail_when_empty {
                return Err(err());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn send_text(&self, to: &str, text: &str) -> Result<u64, TransportError> {
        if let Some(e) = &self.fail_send {
            return Err(e.clone());
        }
        let mut sent = self.sent.lock().await;
        sent.push((to.to_string(), text.to_string()));
        Ok(sent.len() as u64)
    }

    async fn typing(&self, to: &str, stop: bool) -> Result<(), TransportError> {
        self.typing.lock().await.push((to.to_string(), stop));
        Ok(())
    }

    async fn read_receipt(&self, to: &str, timestamp: u64) -> Result<(), TransportError> {
        self.receipts.lock().await.push((to.to_string(), timestamp));
        Ok(())
    }

    async fn identity(&self, who: &str) -> Result<Option<Identity>, TransportError> {
        Ok(self.identities.get(who).cloned())
    }

    async fn trust_verified(&self, who: &str, safety_number: &str) -> Result<(), TransportError> {
        self.trusted
            .lock()
            .await
            .push((who.to_string(), safety_number.to_string()));
        Ok(())
    }

    async fn version(&self) -> Result<String, TransportError> {
        Ok("scripted".into())
    }
}
```

- [ ] **Step 4: Run** `cargo test -j 8 -p aivyx-signal transport:: && cargo clippy -j 8 -p aivyx-signal --all-targets -- -D warnings` → PASS, no warnings.

- [ ] **Step: Commit**

```bash
git add crates/aivyx-signal/src/transport.rs crates/aivyx-signal/src/lib.rs
git commit -s -m "feat(signal): SignalTransport over signal-cli JSON-RPC, and a scripted double" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: The trust policy and the channel lock

**Files:**
- Create: `crates/aivyx-signal/src/policy.rs`, `crates/aivyx-signal/src/lock.rs`
- Modify: `crates/aivyx-signal/src/lib.rs` (add `pub mod lock;`, `pub mod policy;`)

**Interfaces:**
- Consumes: `envelope::{Inbound, TextMessage}`, `transport::TrustLevel`
- Produces: `policy::{SignalPolicy::new(&[String]), allowed_senders(), is_allowed(&TextMessage), enum Action { Turn(TextMessage), AskToVerify { to }, Ignore }, fn decide(&SignalPolicy, Inbound, Option<TrustLevel>) -> Action, OncePerSender (Default) + first(&str) -> bool, const ASK_TO_VERIFY}`; `lock::{acquire(&Path) -> Result<ChannelLock, String>, BUSY, LOCK_FILE}`

`lock.rs` is `aivyx-proton`'s lock with Signal wording (same flock mechanism).

- [ ] **Step 1: Failing tests** — create both files with only their test modules, add the `pub mod` lines:

`policy.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> SignalPolicy {
        SignalPolicy::new(&["+15550001111".to_string(), "AAAA-UUID".to_string()])
    }

    fn msg(number: Option<&str>, uuid: Option<&str>) -> Inbound {
        Inbound::Text(TextMessage {
            sender_number: number.map(str::to_string),
            sender_uuid: uuid.map(str::to_string),
            timestamp: 1,
            text: "hi".into(),
            attachments: 0,
        })
    }

    #[test]
    fn only_a_verified_allowed_sender_is_a_turn() {
        let p = policy();
        let me = || msg(Some("+15550001111"), Some("x"));
        assert!(matches!(decide(&p, me(), Some(TrustLevel::TrustedVerified)), Action::Turn(_)));
        for t in [None, Some(TrustLevel::TrustedUnverified), Some(TrustLevel::Untrusted)] {
            assert_eq!(
                decide(&p, me(), t),
                Action::AskToVerify { to: "+15550001111".into() },
                "{t:?}"
            );
        }
    }

    #[test]
    fn strangers_and_non_messages_are_ignored_even_when_verified() {
        let p = policy();
        let v = Some(TrustLevel::TrustedVerified);
        assert_eq!(decide(&p, msg(Some("+15559999999"), Some("y")), v), Action::Ignore);
        assert_eq!(decide(&p, Inbound::Other, v), Action::Ignore);
    }

    #[test]
    fn a_sender_may_be_listed_by_aci() {
        let p = policy();
        let m = msg(None, Some("aaaa-uuid"));
        assert_eq!(
            decide(&p, m, Some(TrustLevel::TrustedUnverified)),
            Action::AskToVerify { to: "aaaa-uuid".into() }
        );
    }

    #[test]
    fn the_verify_reply_goes_once_per_sender() {
        let mut once = OncePerSender::default();
        assert!(once.first("+1"));
        assert!(!once.first("+1"));
        assert!(once.first("+2"));
    }
}
```

`lock.rs`:
```rust
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("aivyx-signal-lock-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_second_holder_is_refused_with_a_clear_message() {
        let d = scratch("busy");
        let first = acquire(&d).unwrap();
        let err = acquire(&d).unwrap_err();
        assert_eq!(err, BUSY);
        assert!(err.contains("aivyx-pa --channel signal"));
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

- [ ] **Step 2:** `cargo test -j 8 -p aivyx-signal policy:: lock::` → FAIL to compile.
- [ ] **Step 3: Write the modules** (above the tests):

`policy.rs`:
```rust
//! Who may drive a turn. Pure, so every case is a unit test.
//!
//! A message is a turn only from an `allowed_senders` entry whose identity
//! key the operator has verified (`TRUSTED_VERIFIED`). An allowed sender
//! with an unverified or changed key gets one explanation per run;
//! everyone else is ignored silently.

use std::collections::HashSet;

use crate::envelope::{Inbound, TextMessage};
use crate::transport::TrustLevel;

/// Told once per sender per run when their key isn't verified.
pub const ASK_TO_VERIFY: &str = "I only answer a verified safety number. Run \
    `aivyx-pa signal verify` and compare the number it shows with your phone \
    (Signal → this chat → View safety number).";

/// Entries are E.164 numbers or ACIs (UUIDs, lowercased).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalPolicy {
    allowed_senders: Vec<String>,
}

impl SignalPolicy {
    pub fn new(allowed_senders: &[String]) -> Self {
        SignalPolicy {
            allowed_senders: allowed_senders
                .iter()
                .map(|s| s.trim().to_ascii_lowercase())
                .collect(),
        }
    }

    pub fn allowed_senders(&self) -> &[String] {
        &self.allowed_senders
    }

    /// The sender is listed, by number or by ACI.
    pub fn is_allowed(&self, m: &TextMessage) -> bool {
        [m.sender_number.as_deref(), m.sender_uuid.as_deref()]
            .into_iter()
            .flatten()
            .any(|id| self.allowed_senders.iter().any(|a| a == &id.to_ascii_lowercase()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Turn(TextMessage),
    /// Reply [`ASK_TO_VERIFY`] to `to` (once per run).
    AskToVerify { to: String },
    Ignore,
}

/// `trust`: signal-cli's trust level for the sender's key (looked up only
/// for allowed senders; `None` when it knows no key).
pub fn decide(p: &SignalPolicy, inbound: Inbound, trust: Option<TrustLevel>) -> Action {
    let Inbound::Text(m) = inbound else {
        return Action::Ignore;
    };
    if !p.is_allowed(&m) {
        return Action::Ignore;
    }
    if trust == Some(TrustLevel::TrustedVerified) {
        Action::Turn(m)
    } else {
        Action::AskToVerify {
            to: m.sender().to_string(),
        }
    }
}

/// Remembers who already got [`ASK_TO_VERIFY`] this run.
#[derive(Debug, Default)]
pub struct OncePerSender(HashSet<String>);

impl OncePerSender {
    /// `true` the first time `who` is seen.
    pub fn first(&mut self, who: &str) -> bool {
        self.0.insert(who.to_string())
    }
}
```

`lock.rs`:
```rust
//! One `--channel signal` process at a time per instance.
//!
//! Two channel processes on one Signal account would both answer every message.
//! The channel takes an exclusive advisory lock on `<signal_dir>/lock` and
//! holds it while it runs. Same mechanism as `aivyx-matrix`'s store lock.

use std::fs::File;
use std::path::Path;

pub const LOCK_FILE: &str = "lock";

/// Shown when another process holds the lock.
pub const BUSY: &str = "the Signal channel is already running in another process \
    (`aivyx-pa --channel signal`) — stop it first";

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

- [ ] **Step 4: Run** `cargo test -j 8 -p aivyx-signal && cargo clippy -j 8 -p aivyx-signal --all-targets -- -D warnings` → PASS.

- [ ] **Step: Commit**

```bash
git add crates/aivyx-signal/src
git commit -s -m "feat(signal): verified-safety-number trust policy; one channel per instance" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: The channel and the in-process session

**Files:**
- Create: `crates/aivyx-signal/src/signal_channel.rs`, `crates/aivyx-signal/src/session.rs`, `crates/aivyx-signal/src/tests.rs`
- Modify: `crates/aivyx-signal/src/lib.rs` (add `pub mod session;`, `pub mod signal_channel;`, the `pub use session::{run_signal_session_with_transport, SignalSessionConfig, SignalSessionReport};` re-export, `#[cfg(test)] mod tests;`)

**Interfaces:**
- Consumes: everything above; `aivyx_core` agent APIs exactly as `aivyx-matrix`'s session uses them.
- Produces: `SignalChannel` (crate-private `new(SessionId, partition)`, `take_reply()`; platform `Signal`, `SemiTrusted`); in `session`: `const MAX_INPUT` (4 KiB), `MAX_PART` (2000), `CUT_NOTE`, `ATTACHMENTS_NOTE`; `enum Routed { Turn(TextMessage), Handled }`; `async fn route<T: SignalTransport + ?Sized>(&T, &SignalPolicy, &mut OncePerSender, Inbound) -> Result<Routed, TransportError>` (identity lookup for allowed senders, read receipt for turns, the once-per-run verify reply); `fn turn_text(&TextMessage) -> (String, Vec<String>)`; `fn split_reply(&str) -> Vec<String>`; `async fn send_reply(&T, to, answer, notes)`; `fn partition(&str) -> String`; `struct SignalSessionConfig` (same fields as `MatrixSessionConfig`); `struct SignalSessionReport { turns_run }`; `async fn run_signal_session_with_transport(&T, &SignalPolicy, state_dir: &Path, SignalSessionConfig, provider, audit, checkpointer, CancellationToken) -> Result<SignalSessionReport, String>`.

Behaviour: one turn at a time, later messages queued, `/cancel` cancels the running turn; typing indicator on while a turn runs; the reply is sent once complete (split); stopping mid-turn sends nothing; a fatal transport error stops; a transient one backs off.

- [ ] **Step 1: Failing tests.**
  1. `signal_channel.rs`, only its tests:

```rust
#[cfg(test)]
mod channel_tests {
    use super::*;

    fn channel() -> SignalChannel {
        SignalChannel::new(SessionId::new(), "signal-abc".into())
    }

    #[test]
    fn identity_is_signal_semitrusted_partitioned_by_sender() {
        let c = channel();
        assert_eq!(c.platform(), ChannelPlatform::Signal);
        assert_eq!(c.trust_tier(), TrustTier::SemiTrusted);
        assert_eq!(c.session_partition().as_deref(), Some("signal-abc"));
        assert_eq!(c.channel_name(), "aivyx-signal");
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

  2. `session.rs`, only its tests:

```rust
#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn long_replies_split_at_line_breaks_within_the_limit() {
        assert_eq!(split_reply("short"), vec!["short"]);
        assert!(split_reply("  ").is_empty());
        let para = "x".repeat(1500);
        let text = format!("{para}\n{para}\n{para}");
        let parts = split_reply(&text);
        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|p| p.chars().count() <= MAX_PART && *p == para));
        // No break at all: hard cut at the limit, multibyte-safe.
        let parts = split_reply(&"é".repeat(4500));
        assert_eq!(parts.iter().map(|p| p.chars().count()).collect::<Vec<_>>(), vec![2000, 2000, 500]);
    }

    #[test]
    fn long_or_attached_input_is_noted() {
        let m = TextMessage {
            text: "é".repeat(3000),
            attachments: 1,
            ..Default::default()
        };
        let (text, notes) = turn_text(&m);
        assert!(text.len() <= MAX_INPUT);
        assert_eq!(notes, vec![CUT_NOTE.to_string(), ATTACHMENTS_NOTE.to_string()]);
        let short = TextMessage { text: "hi".into(), ..Default::default() };
        assert_eq!(turn_text(&short), ("hi".to_string(), vec![]));
    }

    #[test]
    fn partitions_are_scope_safe() {
        assert_eq!(partition("+15550001111"), "signal-15550001111");
        assert_eq!(partition("8f3c-AB"), "signal-8f3c-AB");
    }
}
```

  3. `tests.rs` — the scripted LLM provider and storage helpers are `aivyx-proton`'s `src/tests.rs` lines 1–134 (scratch prefix renamed); the full file:

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
    let parent = PathBuf::from(tmp).join(format!("aivyx-signal-{suffix}-{pid}-{nanos}"));
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

use crate::envelope::{Inbound, TextMessage};
use crate::lock;
use crate::policy::{SignalPolicy, ASK_TO_VERIFY};
use crate::session::{
    run_signal_session_with_transport, SignalSessionConfig, SignalSessionReport, ATTACHMENTS_NOTE,
};
use crate::transport::{ScriptedTransport, TransportError, TrustLevel};

const ME: &str = "+15550001111";

fn config(storage: Arc<dyn Storage>) -> SignalSessionConfig {
    SignalSessionConfig {
        model: "test-model".into(),
        system_prompt: "signal test".into(),
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

fn policy() -> SignalPolicy {
    SignalPolicy::new(&[ME.to_string()])
}

fn text(from: &str, body: &str, ts: u64) -> Inbound {
    Inbound::Text(TextMessage {
        sender_number: Some(from.into()),
        sender_uuid: Some(format!("uuid-{from}")),
        timestamp: ts,
        text: body.into(),
        attachments: 0,
    })
}

/// The operator, with a verified key.
fn verified(events: Vec<Inbound>) -> ScriptedTransport {
    ScriptedTransport::new(events).with_identity(ME, TrustLevel::TrustedVerified, "11111 22222")
}

struct Run {
    result: Result<SignalSessionReport, String>,
    seen: Vec<String>,
}

/// Runs the session until `transport` has sent `n` messages (then 100 ms
/// more), or for 400 ms when `n == 0`. `gate` holds turns in flight.
async fn run_with(
    dir: &std::path::Path,
    transport: Arc<ScriptedTransport>,
    steps: Vec<ScriptedStep>,
    gate: Option<Arc<Gate>>,
    n: usize,
    shutdown: CancellationToken,
) -> Run {
    let (storage, _scratch) = scratch_storage("session").await;
    let seen = Arc::new(StdMutex::new(Vec::new()));
    let provider: Arc<dyn LlmProvider> = Arc::new(ScriptedProvider {
        queue: StdMutex::new(steps.into()),
        gate,
        seen: Arc::clone(&seen),
    });
    let audit: Arc<dyn AuditHook> = Arc::new(AuditBridge::new(HmacChainLog::new([42u8; 32].to_vec())));
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
        run_signal_session_with_transport(
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
    run_with(dir.path(), transport, steps, None, n, CancellationToken::new()).await
}

#[tokio::test]
async fn the_verified_operator_gets_an_answer_with_typing_and_a_read_receipt() {
    let t = Arc::new(verified(vec![text(ME, "What's on tomorrow?", 7)]));
    let r = run(Arc::clone(&t), vec![final_step(&["Dentist at 9."], "Dentist at 9.")], 1).await;
    assert_eq!(r.result.unwrap().turns_run, 1);
    assert_eq!(t.sent().await, vec![(ME.to_string(), "Dentist at 9.".to_string())]);
    assert_eq!(t.receipts().await, vec![(ME.to_string(), 7)]);
    assert_eq!(
        t.typing_events().await,
        vec![(ME.to_string(), false), (ME.to_string(), true)]
    );
    assert!(r.seen[0].contains("What's on tomorrow?"), "{}", r.seen[0]);
}

#[tokio::test]
async fn an_unverified_key_gets_one_explanation_and_no_turn() {
    let t = Arc::new(
        ScriptedTransport::new(vec![text(ME, "hi", 1), text(ME, "hello?", 2)])
            .with_identity(ME, TrustLevel::TrustedUnverified, "11111 22222"),
    );
    let r = run(Arc::clone(&t), vec![], 0).await;
    assert_eq!(r.result.unwrap().turns_run, 0);
    assert_eq!(t.sent().await, vec![(ME.to_string(), ASK_TO_VERIFY.to_string())]);
    assert!(t.receipts().await.is_empty());
}

#[tokio::test]
async fn a_changed_key_is_refused_like_an_unverified_one() {
    let t = Arc::new(
        ScriptedTransport::new(vec![text(ME, "it's me, new phone", 1)])
            .with_identity(ME, TrustLevel::Untrusted, "99999 00000"),
    );
    let r = run(Arc::clone(&t), vec![], 0).await;
    assert_eq!(r.result.unwrap().turns_run, 0);
    assert_eq!(t.sent().await, vec![(ME.to_string(), ASK_TO_VERIFY.to_string())]);
}

#[tokio::test]
async fn strangers_and_non_messages_get_nothing() {
    let stranger = "+15559999999";
    let t = Arc::new(
        verified(vec![text(stranger, "hello", 1), Inbound::Other])
            .with_identity(stranger, TrustLevel::TrustedVerified, "x"),
    );
    let r = run(Arc::clone(&t), vec![], 0).await;
    assert_eq!(r.result.unwrap().turns_run, 0);
    assert!(t.sent().await.is_empty());
}

#[tokio::test]
async fn a_long_reply_is_split_and_attachments_are_noted() {
    let long = format!("{}\n{}", "a".repeat(1500), "b".repeat(1500));
    let mut m = text(ME, "write a lot", 1);
    if let Inbound::Text(t) = &mut m {
        t.attachments = 1;
    }
    let t = Arc::new(verified(vec![m]));
    let r = run(Arc::clone(&t), vec![final_step(&[&long], &long)], 2).await;
    r.result.unwrap();
    let sent = t.sent().await;
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].1, "a".repeat(1500));
    assert!(sent[1].1.starts_with(&"b".repeat(1500)));
    assert!(sent[1].1.ends_with(ATTACHMENTS_NOTE), "{}", sent[1].1);
}

#[tokio::test]
async fn cancel_stops_the_running_turn_and_a_queued_message_runs_after() {
    let gate = Arc::new(Gate::default());
    let t = Arc::new(verified(vec![text(ME, "slow question", 1)]));
    let dir = tempfile::tempdir().unwrap();
    let (t2, g2) = (Arc::clone(&t), Arc::clone(&gate));
    tokio::spawn(async move {
        g2.started.notified().await;
        t2.push(text(ME, "and another thing", 2)).await;
        t2.push(text(ME, "/cancel", 3)).await;
        // The queued message's turn waits on the gate too: let it through.
        g2.started.notified().await;
        g2.release.notify_one();
    });
    let r = run_with(
        dir.path(),
        Arc::clone(&t),
        vec![final_step(&["second answer"], "second answer")],
        Some(Arc::clone(&gate)),
        2,
        CancellationToken::new(),
    )
    .await;
    let report = r.result.unwrap();
    assert_eq!(report.turns_run, 2);
    let sent = t.sent().await;
    assert!(sent[0].1.contains("cancelled"), "{sent:?}");
    assert_eq!(sent[1].1, "second answer");
}

#[tokio::test]
async fn stopping_mid_turn_sends_nothing() {
    let gate = Arc::new(Gate::default());
    let t = Arc::new(verified(vec![text(ME, "slow", 1)]));
    let dir = tempfile::tempdir().unwrap();
    let shutdown = CancellationToken::new();
    let (g2, s2) = (Arc::clone(&gate), shutdown.clone());
    tokio::spawn(async move {
        g2.started.notified().await;
        s2.cancel();
    });
    let r = run_with(dir.path(), Arc::clone(&t), vec![], Some(gate), 99, shutdown).await;
    assert_eq!(r.result.unwrap().turns_run, 1);
    assert!(t.sent().await.is_empty());
}

#[tokio::test]
async fn an_unregistered_account_ends_the_session() {
    let t = Arc::new(
        ScriptedTransport::new(vec![]).then_fail(|| TransportError::Unregistered("Authorization failed".into())),
    );
    let r = run(Arc::clone(&t), vec![], 0).await;
    assert!(r.result.unwrap_err().contains("connect signal"));
}

#[tokio::test]
async fn signal_cli_being_down_is_retried_not_fatal() {
    let t = Arc::new(ScriptedTransport::new(vec![]).then_fail(|| TransportError::Unreachable("no socket".into())));
    let r = run(Arc::clone(&t), vec![], 0).await;
    assert_eq!(r.result.unwrap().turns_run, 0);
}

#[tokio::test]
async fn a_second_channel_on_the_same_instance_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let _held = lock::acquire(dir.path()).unwrap();
    let t = Arc::new(ScriptedTransport::new(vec![]));
    let r = run_with(dir.path(), t, vec![], None, 0, CancellationToken::new()).await;
    assert_eq!(r.result.unwrap_err(), lock::BUSY);
}
```

- [ ] **Step 2:** `cargo test -j 8 -p aivyx-signal` → FAIL to compile.
- [ ] **Step 3: Write the modules** (above the tests):

`signal_channel.rs`:
```rust
//! [`SignalChannel`] — the `ChannelContext` for one Signal turn. The
//! reply is sent once complete: streamed text is buffered and, at
//! `finalize`, kept for the session loop, which sends it (split into
//! Signal-sized parts).

use std::fmt::Write as _;
use std::sync::Mutex;

use async_trait::async_trait;

use aivyx_capability::TrustTier;
use aivyx_core::{
    CancellationToken, ChannelContext, ChannelError, ChannelPlatform, SessionId, StreamEvent,
    TurnOutcome,
};

pub struct SignalChannel {
    session: SessionId,
    partition: String,
    token: Mutex<CancellationToken>,
    buffer: Mutex<String>,
    reply: Mutex<Option<String>>,
}

impl SignalChannel {
    /// `session` is the sender's conversation; `partition` its memory
    /// namespace (`session::partition`).
    pub(crate) fn new(session: SessionId, partition: String) -> Self {
        SignalChannel {
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
impl ChannelContext for SignalChannel {
    fn channel_name(&self) -> &str {
        "aivyx-signal"
    }

    fn platform(&self) -> ChannelPlatform {
        ChannelPlatform::Signal
    }

    fn trust_tier(&self) -> TrustTier {
        // Only messages `policy::decide` approved reach a channel: an
        // allowed sender whose safety number the operator verified.
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
            .map_err(|e| ChannelError::Send(format!("SignalChannel buffer poisoned: {e}")))?;
        append_event(&mut buf, &event);
        Ok(())
    }

    async fn finalize(&self, outcome: &TurnOutcome) -> Result<(), ChannelError> {
        let mut out = {
            let mut buf = self
                .buffer
                .lock()
                .map_err(|e| ChannelError::Send(format!("SignalChannel buffer poisoned: {e}")))?;
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
//! The in-process Signal session, and the pieces daemon mode shares
//! (`route`, `turn_text`, `send_reply`).
//!
//! One turn at a time; messages that arrive during a turn wait in a queue,
//! except `/cancel`, which cancels it. A typing indicator shows while a
//! turn runs; the reply goes out once complete, split into Signal-sized
//! parts.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use aivyx_capability::CapabilitySet;
use aivyx_core::{
    agent::ConcreteAgent, llm_planner::LlmPlanner, planner::ToolRegistry, Agent, AgentId,
    AuditHook, CancellationToken, ChannelContext, LlmPlannerConfig, Message, SessionId,
};
use aivyx_llm::LlmProvider;
use aivyx_storage::Storage;

use crate::envelope::{Inbound, TextMessage};
use crate::lock;
use crate::policy::{decide, Action, OncePerSender, SignalPolicy, ASK_TO_VERIFY};
use crate::signal_channel::SignalChannel;
use crate::transport::{SignalTransport, TransportError};

/// The most of a message a turn reads.
pub const MAX_INPUT: usize = 4 * 1024;
/// The longest single Signal message the assistant sends.
pub const MAX_PART: usize = 2000;
/// Added to the reply when the message was cut to [`MAX_INPUT`].
pub const CUT_NOTE: &str = "(Your message was longer than 4 KiB, so I read only the start.)";
/// Added to the reply when the message had attachments.
pub const ATTACHMENTS_NOTE: &str = "(I can't open attachments, so I ignored them.)";

/// What [`route`] did with one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Routed {
    Turn(TextMessage),
    Handled,
}

/// Applies the trust policy to one event: a turn is returned (after a
/// read receipt); the once-per-run "please verify" reply is sent here.
pub async fn route<T: SignalTransport + ?Sized>(
    transport: &T,
    policy: &SignalPolicy,
    asked: &mut OncePerSender,
    inbound: Inbound,
) -> Result<Routed, TransportError> {
    let trust = match &inbound {
        Inbound::Text(m) if policy.is_allowed(m) => {
            transport.identity(m.sender()).await?.map(|i| i.trust)
        }
        _ => None,
    };
    match decide(policy, inbound, trust) {
        Action::Turn(m) => {
            if let Err(e) = transport.read_receipt(m.sender(), m.timestamp).await {
                eprintln!("aivyx-signal: read receipt failed: {e}");
            }
            Ok(Routed::Turn(m))
        }
        Action::AskToVerify { to } => {
            if asked.first(&to) {
                transport.send_text(&to, ASK_TO_VERIFY).await?;
            }
            Ok(Routed::Handled)
        }
        Action::Ignore => Ok(Routed::Handled),
    }
}

/// What the agent reads (the text, cut to [`MAX_INPUT`]) and the notes to
/// add to the reply.
pub fn turn_text(m: &TextMessage) -> (String, Vec<String>) {
    let mut notes = Vec::new();
    let text = if m.text.len() > MAX_INPUT {
        notes.push(CUT_NOTE.to_string());
        let mut end = MAX_INPUT;
        while !m.text.is_char_boundary(end) {
            end -= 1;
        }
        m.text[..end].to_string()
    } else {
        m.text.clone()
    };
    if m.attachments > 0 {
        notes.push(ATTACHMENTS_NOTE.to_string());
    }
    (text, notes)
}

/// `text` in parts of at most [`MAX_PART`] characters, cut at a line break
/// (else a space) where one is close to the limit.
pub fn split_reply(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut rest = text.trim();
    while rest.chars().count() > MAX_PART {
        let limit = rest
            .char_indices()
            .nth(MAX_PART)
            .map_or(rest.len(), |(i, _)| i);
        let window = &rest[..limit];
        let cut = window
            .rfind('\n')
            .filter(|&i| i > limit / 2)
            .or_else(|| window.rfind(' ').filter(|&i| i > limit / 2))
            .unwrap_or(limit);
        parts.push(rest[..cut].trim_end().to_string());
        rest = rest[cut..].trim_start();
    }
    if !rest.is_empty() {
        parts.push(rest.to_string());
    }
    parts
}

/// Sends `answer` (plus `notes`) to `to`, in parts.
pub async fn send_reply<T: SignalTransport + ?Sized>(
    transport: &T,
    to: &str,
    answer: &str,
    notes: &[String],
) -> Result<(), TransportError> {
    let body = if notes.is_empty() {
        answer.to_string()
    } else {
        format!("{answer}\n\n{}", notes.join("\n"))
    };
    for part in split_reply(&body) {
        transport.send_text(to, &part).await?;
    }
    Ok(())
}

/// A sender's memory namespace: `signal-` plus the sender with anything
/// but letters, digits and `-` dropped.
pub fn partition(sender: &str) -> String {
    let id: String = sender
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    format!("signal-{id}")
}

/// Same fields and meaning as `aivyx_matrix::MatrixSessionConfig`.
#[derive(Clone)]
pub struct SignalSessionConfig {
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SignalSessionReport {
    pub turns_run: usize,
}

fn build_agent(
    config: SignalSessionConfig,
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

/// Runs the in-process session. `state_dir` is the instance's
/// `signal_dir()` (it holds the lock).
#[allow(clippy::too_many_arguments)]
pub async fn run_signal_session_with_transport<T: SignalTransport + ?Sized>(
    transport: &T,
    policy: &SignalPolicy,
    state_dir: &Path,
    config: SignalSessionConfig,
    provider: Arc<dyn LlmProvider>,
    audit: Arc<dyn AuditHook>,
    checkpointer: Option<Arc<aivyx_core::GitCheckpointer>>,
    shutdown: CancellationToken,
) -> Result<SignalSessionReport, String> {
    let _lock = lock::acquire(state_dir)?;
    let agent = build_agent(config, provider, audit, checkpointer);
    let mut sessions: HashMap<String, SessionId> = HashMap::new();
    let mut asked = OncePerSender::default();
    let mut pending: VecDeque<TextMessage> = VecDeque::new();
    let mut report = SignalSessionReport::default();
    let mut backoff = Duration::from_secs(1);

    loop {
        if pending.is_empty() {
            let event = tokio::select! {
                biased;
                _ = shutdown.cancelled() => return Ok(report),
                r = transport.next_event() => r,
            };
            let event = match event {
                Ok(e) => {
                    backoff = Duration::from_secs(1);
                    e
                }
                Err(e) if e.is_fatal() => return Err(e.to_string()),
                Err(e) => {
                    eprintln!("aivyx-signal: {e}; retrying in {}s", backoff.as_secs());
                    tokio::select! {
                        biased;
                        _ = shutdown.cancelled() => return Ok(report),
                        _ = tokio::time::sleep(backoff) => {}
                    }
                    backoff = (backoff * 2).min(Duration::from_secs(60));
                    continue;
                }
            };
            match route(transport, policy, &mut asked, event).await {
                Ok(Routed::Turn(m)) if m.text.trim() == "/cancel" => {}
                Ok(Routed::Turn(m)) => pending.push_back(m),
                Ok(Routed::Handled) => {}
                Err(e) if e.is_fatal() => return Err(e.to_string()),
                Err(e) => eprintln!("aivyx-signal: {e}"),
            }
            continue;
        }

        let m = pending.pop_front().expect("pending is non-empty here");
        let sender = m.sender().to_string();
        let session = *sessions.entry(sender.clone()).or_default();
        let channel = SignalChannel::new(session, partition(&sender));
        let (text, notes) = turn_text(&m);
        let _ = transport.typing(&sender, false).await;
        let turn = agent.turn(Message::text(session, text), &channel);
        tokio::pin!(turn);
        let mut fatal: Option<String> = None;
        let mut stopped = false;
        loop {
            tokio::select! {
                biased;
                _ = &mut turn => break,
                _ = shutdown.cancelled() => {
                    channel.cancellation_token().cancel();
                    let _ = (&mut turn).await;
                    stopped = true;
                    break;
                }
                r = transport.next_event() => {
                    let event = match r {
                        Ok(e) => e,
                        Err(e) if e.is_fatal() => {
                            fatal = Some(e.to_string());
                            let _ = (&mut turn).await;
                            break;
                        }
                        // A transient error could return at once on every
                        // poll: stop polling for this turn; the outer loop
                        // backs off.
                        Err(_) => {
                            let _ = (&mut turn).await;
                            break;
                        }
                    };
                    match route(transport, policy, &mut asked, event).await {
                        Ok(Routed::Turn(next)) if next.text.trim() == "/cancel" => {
                            channel.cancellation_token().cancel();
                        }
                        Ok(Routed::Turn(next)) => pending.push_back(next),
                        Ok(Routed::Handled) => {}
                        Err(e) if e.is_fatal() => {
                            fatal = Some(e.to_string());
                            let _ = (&mut turn).await;
                            break;
                        }
                        Err(e) => eprintln!("aivyx-signal: {e}"),
                    }
                }
            }
        }
        let _ = transport.typing(&sender, true).await;
        report.turns_run += 1;
        if let Some(e) = fatal {
            return Err(e);
        }
        if stopped {
            return Ok(report);
        }
        if let Some(answer) = channel.take_reply() {
            match send_reply(transport, &sender, &answer, &notes).await {
                Ok(()) => {}
                Err(e) if e.is_fatal() => return Err(e.to_string()),
                Err(e) => eprintln!("aivyx-signal: couldn't send the reply: {e}"),
            }
        }
    }
}
```

- [ ] **Step 4: Run** `cargo test -j 8 -p aivyx-signal && cargo clippy -j 8 -p aivyx-signal --all-targets -- -D warnings` → PASS.

- [ ] **Step: Commit**

```bash
git add crates/aivyx-signal/src
git commit -s -m "feat(signal): SignalChannel and the in-process session" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8: `signal.send` and the tool process

**Files:**
- Create: `crates/aivyx-signal/src/toolconf.rs`, `crates/aivyx-signal/src/tools/mod.rs`, `crates/aivyx-signal/src/tools/send.rs`
- Replace: `crates/aivyx-signal/src/main.rs`
- Modify: `crates/aivyx-signal/src/lib.rs` (add `pub mod toolconf;`, `pub mod tools;` — final file below)

**Interfaces:**
- Consumes: `transport::{SignalTransport, CliTransport}`, `session::split_reply`, `aivyx_tool::multi_harness::run_multi_tool_subprocess`.
- Produces: `toolconf::{SOCKET_FILE, TOOL_CONFIG_FILE, socket_path(&InstancePaths), tool_config_path(&InstancePaths), struct ToolConfig { send_to }, load(&Path), save(&Path, &ToolConfig)}`; `tools::SignalSend::new(Arc<dyn SignalTransport>, send_to: Vec<String>)`; the `aivyx-signal` binary.

- [ ] **Step 1: Failing tests** — `toolconf.rs` and `tools/send.rs` holding only their tests; `tools/mod.rs` in full:

`toolconf.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_live_in_the_instances_signal_tool_folder() {
        let dirs = aivyx_instance::BaseDirs {
            home: Some(PathBuf::from("/h")),
            xdg_config_home: None,
            xdg_data_home: None,
            xdg_runtime_dir: None,
        };
        let work = InstancePaths::new(aivyx_instance::InstanceName::parse("work").unwrap(), dirs);
        assert_eq!(
            socket_path(&work),
            Some(PathBuf::from("/h/.aivyx-pa/instances/work/tool-processes/signal/signal-cli.sock"))
        );
        assert_eq!(
            tool_config_path(&work),
            Some(PathBuf::from("/h/.aivyx-pa/instances/work/tool-processes/signal/signal-tool.toml"))
        );
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("signal").join(TOOL_CONFIG_FILE);
        assert_eq!(load(&path).unwrap(), None);
        let cfg = ToolConfig { send_to: vec!["+15550002222".into()] };
        save(&path, &cfg).unwrap();
        assert_eq!(load(&path).unwrap(), Some(cfg.clone()));
        // A leftover temporary file doesn't block or leak into the next save.
        std::fs::write(path.with_extension("toml.tmp"), "junk").unwrap();
        save(&path, &cfg).unwrap();
        assert_eq!(load(&path).unwrap(), Some(cfg));
    }
}
```

`tools/send.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{ScriptedTransport, TransportError};
    use aivyx_core::{
        AgentId, CancellationToken, ChannelContext, ChannelError, ChannelPlatform, MessageOrigin,
        NullAuditHook, SessionId, StreamEvent, TurnId, TurnOutcome,
    };

    struct NoopChannel;

    #[async_trait]
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

    fn ctx() -> ToolContext<'static> {
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

    const SAM: &str = "+15550002222";

    #[tokio::test]
    async fn a_listed_contact_gets_the_message() {
        let t = Arc::new(ScriptedTransport::new(vec![]));
        let tool = SignalSend::new(t.clone(), vec![SAM.into()]);
        assert_eq!(tool.required_scope(&json!({})).to_string(), "message.send");
        assert!(!tool.output_is_untrusted());
        let out = tool.execute(json!({"to": SAM, "text": "Running late."}), &ctx()).await;
        let ToolOutcome::Completed { output, .. } = out else { panic!("{out:?}") };
        assert_eq!(output["parts"], 1);
        assert_eq!(t.sent().await, vec![(SAM.to_string(), "Running late.".to_string())]);
    }

    #[tokio::test]
    async fn an_unlisted_number_is_refused_before_anything_is_sent() {
        let t = Arc::new(ScriptedTransport::new(vec![]));
        let tool = SignalSend::new(t.clone(), vec![SAM.into()]);
        let out = tool.execute(json!({"to": "+15559999999", "text": "hi"}), &ctx()).await;
        assert!(matches!(out, ToolOutcome::Failed(_)));
        assert!(t.sent().await.is_empty());
        for bad in [json!({"to": SAM}), json!({"text": "x"}), json!({"to": SAM, "text": "  "})] {
            assert!(matches!(tool.execute(bad, &ctx()).await, ToolOutcome::Failed(_)));
        }
    }

    #[tokio::test]
    async fn an_untrusted_recipient_key_says_to_verify() {
        let t = Arc::new(ScriptedTransport::new(vec![]).failing_send(TransportError::Refused(
            "Failed to send message due to untrusted identities".into(),
        )));
        let tool = SignalSend::new(t, vec![SAM.into()]);
        let out = tool.execute(json!({"to": SAM, "text": "hi"}), &ctx()).await;
        let ToolOutcome::Failed(e) = out else { panic!() };
        assert!(e.to_string().contains("signal verify"), "{e}");
    }
}
```

`tools/mod.rs`:
```rust
//! The `signal.*` tools, served by the `aivyx-signal` tool process.

pub mod send;

pub use send::SignalSend;
```

- [ ] **Step 2:** `cargo test -j 8 -p aivyx-signal toolconf:: tools::` → FAIL to compile.
- [ ] **Step 3: Write the modules** (above the tests):

`toolconf.rs`:
```rust
//! The instance's signal tool folder (`tool_process_dir("signal")`):
//! signal-cli's socket, and `signal-tool.toml` — the part of `[signal]` the
//! sandboxed `signal.send` tool needs (`send_to`), since it can't read
//! `aivyx-pa.toml`. Written by `aivyx-pa connect signal`.

use std::path::{Path, PathBuf};

use aivyx_instance::InstancePaths;
use serde::{Deserialize, Serialize};

pub const SOCKET_FILE: &str = "signal-cli.sock";
pub const TOOL_CONFIG_FILE: &str = "signal-tool.toml";

/// `<tool_process_dir("signal")>/signal-cli.sock` — where the operator's
/// signal-cli daemon listens (`--socket`).
pub fn socket_path(instance: &InstancePaths) -> Option<PathBuf> {
    instance.tool_process_dir("signal").map(|d| d.join(SOCKET_FILE))
}

/// `<tool_process_dir("signal")>/signal-tool.toml`.
pub fn tool_config_path(instance: &InstancePaths) -> Option<PathBuf> {
    instance
        .tool_process_dir("signal")
        .map(|d| d.join(TOOL_CONFIG_FILE))
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolConfig {
    /// The numbers `signal.send` may message.
    pub send_to: Vec<String>,
}

/// `None` when `connect signal` hasn't run.
pub fn load(path: &Path) -> Result<Option<ToolConfig>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text)
            .map(Some)
            .map_err(|e| format!("read {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

/// Creates the folder (`0700`) and writes the file (`0600`) via an
/// exclusively-created temporary file and a rename.
pub fn save(path: &Path, cfg: &ToolConfig) -> Result<(), String> {
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
    let text = toml::to_string(cfg).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("toml.tmp");
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("remove {}: {e}", tmp.display())),
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
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

`tools/send.rs`:
```rust
//! `signal.send` — a text message from the assistant's Signal number to a
//! contact the operator listed in `[signal] send_to`. Scope
//! `message.send`: Trusted-only, and the turn loop pauses for approval on
//! every call (a withheld-integration base, like `email.send`).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use aivyx_capability::Scope;
use aivyx_core::{AivyxError, Tool, ToolContext, ToolId, ToolOutcome, Verification};

use crate::session::split_reply;
use crate::transport::SignalTransport;

/// The most text one call may send (several Signal messages).
const MAX_TEXT: usize = 8 * 1024;

pub struct SignalSend {
    id: ToolId,
    schema: Value,
    transport: Arc<dyn SignalTransport>,
    send_to: Vec<String>,
}

impl SignalSend {
    pub fn new(transport: Arc<dyn SignalTransport>, send_to: Vec<String>) -> Self {
        SignalSend {
            id: ToolId::new(),
            schema: json!({
                "type": "object",
                "properties": {
                    "to": {"type": "string", "description": "A number from the operator's [signal] send_to list, e.g. +15550002222."},
                    "text": {"type": "string", "description": "Plain text."}
                },
                "required": ["to", "text"],
                "additionalProperties": false
            }),
            transport,
            send_to: send_to.iter().map(|n| n.trim().to_string()).collect(),
        }
    }

    fn fail(&self, detail: impl std::fmt::Display) -> ToolOutcome {
        ToolOutcome::Failed(AivyxError::Tool {
            tool: self.id,
            detail: format!("signal.send: {detail}"),
        })
    }
}

#[async_trait]
impl Tool for SignalSend {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "signal.send"
    }

    fn description(&self) -> &str {
        "Send a Signal text message from the assistant's own Signal number \
         to one of the contacts the operator listed (`[signal] send_to`). \
         **It goes out immediately — there is no undo.** Input: `to` (a \
         listed number, e.g. +15550002222) and `text`. Long text is sent as \
         several messages. Asks the operator to approve every send."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("message.send").expect("message.send is in KNOWN_BASES")
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let Some(to) = input.get("to").and_then(Value::as_str).map(str::trim) else {
            return self.fail("`to` is required");
        };
        let Some(text) = input.get("text").and_then(Value::as_str) else {
            return self.fail("`text` is required");
        };
        if !self.send_to.iter().any(|n| n == to) {
            return self.fail(format!(
                "{to} isn't in [signal] send_to — the operator must add it (and run \
                 `aivyx-pa connect signal`)"
            ));
        }
        if text.trim().is_empty() {
            return self.fail("`text` is empty");
        }
        if text.len() > MAX_TEXT {
            return self.fail("`text` is longer than 8 KiB");
        }
        let parts = split_reply(text);
        let mut timestamp = 0;
        for part in &parts {
            match self.transport.send_text(to, part).await {
                Ok(ts) => timestamp = ts,
                Err(e) if e.is_untrusted_recipient() => {
                    return self.fail(format!(
                        "{to}'s safety number isn't verified — the operator must run \
                         `aivyx-pa signal verify` first"
                    ));
                }
                Err(e) => return self.fail(e),
            }
        }
        ToolOutcome::Completed {
            output: json!({"sent": true, "to": to, "parts": parts.len(), "timestamp": timestamp}),
            verified: Verification::Unverified,
        }
    }
}
```

- [ ] **Step 4: The binary** — `crates/aivyx-signal/src/main.rs`:

```rust
//! `aivyx-signal` — the `signal.send` tool process. The daemon spawns it via
//! `[[tool_process]] name = "signal"` (sandboxed like Gmail); it talks to
//! the operator's signal-cli daemon over the socket in the instance's
//! signal tool folder and reads `send_to` from `signal-tool.toml`.

use std::process::ExitCode;
use std::sync::Arc;

use aivyx_core::Tool;
use aivyx_instance::InstancePaths;
use aivyx_signal::toolconf;
use aivyx_signal::tools::SignalSend;
use aivyx_signal::transport::{CliTransport, SignalTransport};
use aivyx_tool::multi_harness::run_multi_tool_subprocess;

#[tokio::main]
async fn main() -> ExitCode {
    if std::env::args().len() > 1 {
        eprintln!(
            "aivyx-signal takes no arguments: the aivyx-pa daemon runs it as a \
             [[tool_process]]. Set it up with `aivyx-pa connect signal`."
        );
        return ExitCode::from(2);
    }
    let Ok(instance) = InstancePaths::current() else {
        eprintln!("aivyx-signal: can't resolve the instance's folders (is HOME set?)");
        return ExitCode::from(2);
    };
    let (Some(socket), Some(conf)) = (toolconf::socket_path(&instance), toolconf::tool_config_path(&instance)) else {
        eprintln!("aivyx-signal: can't resolve the instance's folders (is HOME set?)");
        return ExitCode::from(2);
    };
    let send_to = match toolconf::load(&conf) {
        Ok(Some(c)) => c.send_to,
        Ok(None) => {
            eprintln!("aivyx-signal: not set up — run `aivyx-pa connect signal` first");
            return ExitCode::from(2);
        }
        Err(e) => {
            eprintln!("aivyx-signal: {e}");
            return ExitCode::from(2);
        }
    };
    let transport: Arc<dyn SignalTransport> = Arc::new(CliTransport::new(socket));
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(SignalSend::new(transport, send_to))];
    match run_multi_tool_subprocess(tools, "aivyx-signal", None).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aivyx-signal: harness exited with error: {e}");
            ExitCode::from(1)
        }
    }
}
```

and `lib.rs` is now:

```rust
//! `aivyx-signal` — Signal for Aivyx PA, through the operator's own
//! signal-cli daemon (JSON-RPC on a Unix socket): the channel
//! (`--channel signal`) and `signal.send` (the `aivyx-signal` tool
//! process). signal-cli holds the account; pa holds no Signal credential.
//!
//! Shape: like `aivyx-proton` — a JSON-RPC client (`rpc`), envelope
//! parsing (`envelope`), a transport trait + scripted double
//! (`transport`), the pure trust policy (`policy`), a channel
//! (`signal_channel`) and the receive loop (`session`). See
//! `docs/superpowers/specs/2026-10-12-signal-channel-design.md`.

pub mod envelope;
pub mod lock;
pub mod policy;
pub mod rpc;
pub mod session;
pub mod signal_channel;
pub mod toolconf;
pub mod tools;
pub mod transport;

pub use session::{
    run_signal_session_with_transport, SignalSessionConfig, SignalSessionReport,
};

#[cfg(test)]
mod tests;
```

- [ ] **Step 5: Run** `cargo test -j 8 -p aivyx-signal && cargo clippy -j 8 -p aivyx-signal --all-targets -- -D warnings && cargo build -j 8 -p aivyx-signal --bin aivyx-signal` → PASS; `./target/debug/aivyx-signal x` exits 2 ("takes no arguments").

- [ ] **Step: Commit**

```bash
git add crates/aivyx-signal/src
git commit -s -m "feat(signal): signal.send and the aivyx-signal tool process" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9: `[signal]` config

**Files:**
- Modify: `crates/aivyx-config/src/lib.rs` (`AivyxConfig::signal`, `SignalConfig` before the struct-level `/// Phase 109 — [git] configuration for the git.status / git.diff tools (Amendment A12).` comment — not the field doc of the same name inside `AivyxConfig` —, `RawSignal`, `is_e164`, `is_aci`, the parse block before `// --- slack (Phase 108)`, the struct literal)
- Modify: `crates/aivyx-config/src/tests.rs` (two tests before `matrix_accepts_loopback_http_homeservers`)
- Modify: `crates/aivyx-cli/src/bin/aivyx.rs` (`run_async` destructures `AivyxConfig` exhaustively: after the `proton,` line add `// [signal] — bound by the ChannelKind::Signal arm (next tasks).` and `signal: _,`)

**Interfaces:**
- Produces: `pub struct SignalConfig { account: String, allowed_senders: Vec<String>, send_to: Vec<String>, team_run_channel: bool, team_trigger_rate_limit: Option<u32>, team_command_allowed_senders: Vec<String> }`; `AivyxConfig::signal: Option<SignalConfig>` (`Some` when `account` is set). Numbers trimmed and lowercased; `allowed_senders` and `team_command_allowed_senders` accept E.164 or ACI, `send_to` E.164 only; empty `allowed_senders` or one containing `account` is rejected.

- [ ] **Step 1: Failing tests** (the `tests.rs` hunk) → `cargo test -j 8 -p aivyx-config signal` FAILS.
- [ ] **Step 2: Implement** (the `lib.rs` hunks):

```diff
diff --git a/crates/aivyx-config/src/lib.rs b/crates/aivyx-config/src/lib.rs
index ca506f03..43ec65e6 100644
--- a/crates/aivyx-config/src/lib.rs
+++ b/crates/aivyx-config/src/lib.rs
@@ -1045,6 +1045,9 @@ pub struct AivyxConfig {
     /// `[proton]` — Proton Mail through Bridge. `None` unless the section
     /// sets a key; see [`ProtonConfig`].
     pub proton: Option<ProtonConfig>,
+    /// `[signal]` — Signal through the operator's signal-cli daemon. `None`
+    /// unless the section sets `account`; see [`SignalConfig`].
+    pub signal: Option<SignalConfig>,
     /// Phase 108 — Slack channel config. `None` when no
     /// Slack field is set. Filled in whenever any Slack
     /// field is set so the startup banner can warn about
@@ -1851,6 +1854,25 @@ pub struct ProtonConfig {
     pub team_command_allowed_senders: Vec<String>,
 }
 
+/// Signal through the operator's own signal-cli daemon: the channel
+/// (`--channel signal`) and `signal.send`. signal-cli holds the account;
+/// nothing secret is config.
+#[derive(Debug, Clone, PartialEq, Eq)]
+pub struct SignalConfig {
+    /// The assistant's own Signal number (E.164, e.g. `+15559990000`) — the one signal-cli runs for.
+    pub account: String,
+    /// Your Signal numbers (E.164) or ACIs. Only these, with a safety number you verified (`aivyx-pa signal verify`), can talk to the assistant.
+    pub allowed_senders: Vec<String>,
+    /// The numbers (E.164) `signal.send` may message. Run `aivyx-pa connect signal` again after changing it.
+    pub send_to: Vec<String>,
+    /// Operator opt-in for `/team run <goal>` from Signal.
+    pub team_run_channel: bool,
+    /// Max `/team run` confirmations accepted per rolling hour. Unset = unlimited.
+    pub team_trigger_rate_limit: Option<u32>,
+    /// The numbers allowed to issue `/team ...` commands. Empty: nobody.
+    pub team_command_allowed_senders: Vec<String>,
+}
+
 /// Phase 109 — `[git]` configuration for the `git.status` /
 /// `git.diff` tools (Amendment A12). The `repos` field is the
 /// allow-set the tools gate against; each entry is canonicalized
@@ -4138,6 +4160,8 @@ struct RawToml {
     #[serde(default)]
     proton: RawProton,
     #[serde(default)]
+    signal: RawSignal,
+    #[serde(default)]
     slack: RawSlack,
     #[serde(default)]
     git: RawGit,
@@ -5223,6 +5247,36 @@ fn plausible_email(s: &str) -> bool {
             .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | ',' | ';' | '"'))
 }
 
+/// `[signal]` TOML section. Present when `account` is set.
+#[derive(Debug, Default, Deserialize)]
+struct RawSignal {
+    #[serde(default)]
+    account: Option<String>,
+    #[serde(default)]
+    allowed_senders: Vec<String>,
+    #[serde(default)]
+    send_to: Vec<String>,
+    #[serde(default)]
+    team_run_channel: bool,
+    #[serde(default)]
+    team_trigger_rate_limit: Option<u32>,
+    #[serde(default)]
+    team_command_allowed_senders: Vec<String>,
+}
+
+/// `+` and 7–15 digits (E.164).
+fn is_e164(s: &str) -> bool {
+    s.strip_prefix('+')
+        .is_some_and(|d| (7..=15).contains(&d.len()) && d.bytes().all(|b| b.is_ascii_digit()) && !d.starts_with('0'))
+}
+
+/// A Signal ACI: a UUID (8-4-4-4-12 hex).
+fn is_aci(s: &str) -> bool {
+    let parts: Vec<&str> = s.split('-').collect();
+    parts.len() == 5
+        && parts.iter().zip([8, 4, 4, 4, 12]).all(|(p, n)| p.len() == n && p.bytes().all(|b| b.is_ascii_hexdigit()))
+}
+
 /// Phase 108 — `[slack]` TOML section deserialize target.
 /// Three optional fields: bot token, app token (Socket Mode),
 /// optional team_id constraint.
@@ -7131,6 +7185,61 @@ impl AivyxConfig {
             })
         };
 
+        // --- signal ------------------------------------------------
+        // `[signal]` is `Some` when `account` is set. signal-cli holds the
+        // account; nothing here is secret.
+        let signal = match toml.signal.account.clone() {
+            None => None,
+            Some(account) => {
+                let raw = &toml.signal;
+                let invalid = |field: &'static str, reason: &str| ConfigError::Invalid {
+                    field,
+                    reason: reason.into(),
+                };
+                let account = account.trim().to_string();
+                if !is_e164(&account) {
+                    return Err(invalid("signal.account", "must be a phone number like +15559990000"));
+                }
+                let numbers = |list: &[String], field: &'static str, aci_ok: bool| {
+                    list.iter()
+                        .map(|n| {
+                            let n = n.trim().to_ascii_lowercase();
+                            if is_e164(&n) || (aci_ok && is_aci(&n)) {
+                                Ok(n)
+                            } else {
+                                Err(invalid(field, "each entry must be a phone number like +15550001111"))
+                            }
+                        })
+                        .collect::<Result<Vec<_>, _>>()
+                };
+                let allowed_senders = numbers(&raw.allowed_senders, "signal.allowed_senders", true)?;
+                if allowed_senders.is_empty() {
+                    return Err(invalid(
+                        "signal.allowed_senders",
+                        "list your Signal number, e.g. [\"+15550001111\"]",
+                    ));
+                }
+                if allowed_senders.contains(&account) {
+                    return Err(invalid(
+                        "signal.allowed_senders",
+                        "must not include the assistant's own number (`account`)",
+                    ));
+                }
+                Some(SignalConfig {
+                    account,
+                    allowed_senders,
+                    send_to: numbers(&raw.send_to, "signal.send_to", false)?,
+                    team_run_channel: raw.team_run_channel,
+                    team_trigger_rate_limit: raw.team_trigger_rate_limit,
+                    team_command_allowed_senders: numbers(
+                        &raw.team_command_allowed_senders,
+                        "signal.team_command_allowed_senders",
+                        true,
+                    )?,
+                })
+            }
+        };
+
         // --- slack (Phase 108) -------------------------------------
         // Same shape as Telegram + Discord: SlackConfig is
         // constructed whenever any Slack source fires. Both
@@ -8604,6 +8713,7 @@ impl AivyxConfig {
             discord,
             matrix,
             proton,
+            signal,
             slack,
             git,
             email,
diff --git a/crates/aivyx-config/src/tests.rs b/crates/aivyx-config/src/tests.rs
index b2195b73..2c69e33b 100644
--- a/crates/aivyx-config/src/tests.rs
+++ b/crates/aivyx-config/src/tests.rs
@@ -682,6 +682,46 @@ fn proton_rejects_unsafe_or_malformed_settings() {
     drop(env);
 }
 
+#[test]
+fn signal_section_parses_and_absent_means_none() {
+    let env = EnvScope::new();
+    let toml = r#"
+[signal]
+account = "+15559990000"
+allowed_senders = ["+15550001111", "8F3C7A1E-0B5D-4C2A-9E61-3D7F2B9A4C10"]
+send_to = ["+15550002222"]
+team_command_allowed_senders = ["+15550001111"]
+"#;
+    let s = load_with_toml(toml, "signal-parse").signal.expect("[signal] is Some");
+    assert_eq!(s.account, "+15559990000");
+    assert_eq!(s.allowed_senders, vec!["+15550001111", "8f3c7a1e-0b5d-4c2a-9e61-3d7f2b9a4c10"]);
+    assert_eq!(s.send_to, vec!["+15550002222"]);
+    assert_eq!(s.team_command_allowed_senders, vec!["+15550001111"]);
+    assert!(!s.team_run_channel);
+    assert!(load_with_toml("", "signal-absent").signal.is_none());
+    drop(env);
+}
+
+#[test]
+fn signal_rejects_malformed_numbers() {
+    let env = EnvScope::new();
+    let ok = "allowed_senders = [\"+15550001111\"]\n";
+    let cases: &[(&str, &str, &str)] = &[
+        ("15559990000", ok, "signal.account"),
+        ("+1555", ok, "signal.account"),
+        ("+15559990000", "allowed_senders = []\n", "signal.allowed_senders"),
+        ("+15559990000", "allowed_senders = [\"me\"]\n", "signal.allowed_senders"),
+        ("+15559990000", "allowed_senders = [\"+15559990000\"]\n", "signal.allowed_senders"),
+        ("+15559990000", "allowed_senders = [\"+15550001111\"]\nsend_to = [\"8f3c7a1e-0b5d-4c2a-9e61-3d7f2b9a4c10\"]\n", "signal.send_to"),
+        ("+15559990000", "allowed_senders = [\"+15550001111\"]\nteam_command_allowed_senders = [\"x\"]\n", "signal.team_command_allowed_senders"),
+    ];
+    for (account, rest, field) in cases {
+        let body = format!("[signal]\naccount = \"{account}\"\n{rest}");
+        assert_eq!(matrix_invalid_field(&body, "signal-bad"), *field, "for:\n{body}");
+    }
+    drop(env);
+}
+
 #[test]
 fn matrix_accepts_loopback_http_homeservers() {
     let env = EnvScope::new();
```

- [ ] **Step 3:** add the `signal: _,` binding in `aivyx.rs` (above). **Run** `cargo test -j 8 -p aivyx-config && cargo clippy -j 8 --workspace --all-targets -- -D warnings` → PASS.

- [ ] **Step: Commit**

```bash
git add crates/aivyx-config/src crates/aivyx-cli/src/bin/aivyx.rs
git commit -s -m "feat(config): [signal] — the assistant's number, your numbers, send_to" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 10: Daemon mode

**Files:**
- Modify: `crates/aivyx-ipc/src/protocol.rs` (`FrontendType::Signal`)
- Create: `crates/aivyx-channel/src/signal_daemon_frontend.rs`
- Modify: `crates/aivyx-channel/src/lib.rs` (`pub mod signal_daemon_frontend;`), `crates/aivyx-channel/Cargo.toml` (`aivyx-signal` dependency), `crates/aivyx-channel/tests/daemon_roundtrip_e2e.rs` (two exhaustive `FrontendType` matches: `| FrontendType::Signal`)
- Modify: `crates/aivyx-cli/src/bin/aivyx.rs` (the channel factory's `FrontendType::Signal` arm, after the `Proton` arm):

```rust
                aivyx_channel::daemon_ipc::FrontendType::Signal => Arc::new(
                    aivyx_channel::signal_daemon_frontend::SignalDaemonChannel::new(),
                ),
```

**Interfaces:**
- Consumes: `aivyx_signal::{policy::{OncePerSender, SignalPolicy}, session::{route, send_reply, turn_text, Routed}, transport::{SignalTransport, TransportError}, lock}`, `crate::channel_team_chain::handle_team_command`.
- Produces: `SignalDaemonChannel::new()` (platform `Signal`, `SemiTrusted`); `run_signal_daemon_session<T: SignalTransport + ?Sized>(&T, &SignalPolicy, state_dir: &Path, socket_path: PathBuf, role: Option<String>, shutdown, team_run_channel, team_trigger_rate_limit, team_command_allowed_senders: Vec<String>) -> Result<(), DaemonError>` — `ChannelFatal` for an unregistered account or a held lock; other errors (daemon failures) let the binary fall back.

One `DaemonSession` per sender (≤ 8 open). The session is connected **before** the typing indicator starts, so a daemon failure never leaves "typing…" on.

- [ ] **Step 1:** add `FrontendType::Signal` (the `protocol.rs` diff), the `Cargo.toml`/`lib.rs` lines, and the e2e match arms:

```diff
diff --git a/crates/aivyx-channel/Cargo.toml b/crates/aivyx-channel/Cargo.toml
index c0c19ce5..9c851e0d 100644
--- a/crates/aivyx-channel/Cargo.toml
+++ b/crates/aivyx-channel/Cargo.toml
@@ -154,6 +154,7 @@ aivyx-telegram = { path = "../aivyx-telegram" }
 aivyx-discord = { path = "../aivyx-discord" }
 aivyx-matrix = { path = "../aivyx-matrix" }
 aivyx-proton = { path = "../aivyx-proton" }
+aivyx-signal = { path = "../aivyx-signal" }
 # Phase 108 — Slack channel adapter. Same binary-only-import
 # pattern as `aivyx-telegram` and `aivyx-discord`. The
 # production transport is currently stubbed (Phase-108-internal
diff --git a/crates/aivyx-channel/src/lib.rs b/crates/aivyx-channel/src/lib.rs
index 20b2e92b..b67fbc1e 100644
--- a/crates/aivyx-channel/src/lib.rs
+++ b/crates/aivyx-channel/src/lib.rs
@@ -189,6 +189,7 @@ pub mod telegram_daemon_frontend;
 pub mod discord_daemon_frontend;
 pub mod matrix_daemon_frontend;
 pub mod proton_daemon_frontend;
+pub mod signal_daemon_frontend;
 pub mod channel_team_chain;
 pub mod gate_command;
 pub mod team_command;
diff --git a/crates/aivyx-channel/tests/daemon_roundtrip_e2e.rs b/crates/aivyx-channel/tests/daemon_roundtrip_e2e.rs
index 192e6fc5..ef479ec1 100644
--- a/crates/aivyx-channel/tests/daemon_roundtrip_e2e.rs
+++ b/crates/aivyx-channel/tests/daemon_roundtrip_e2e.rs
@@ -1185,7 +1185,8 @@ async fn telegram_frontend_type_gets_telegram_channel() {
         | FrontendType::Discord
         | FrontendType::Slack
         | FrontendType::Matrix
-        | FrontendType::Proton => {
+        | FrontendType::Proton
+        | FrontendType::Signal => {
             Arc::new(LocalChannel::new("test-local", Vec::<u8>::new()))
         }
     });
@@ -1334,7 +1335,8 @@ async fn mixed_local_and_telegram_frontends_on_same_daemon() {
         | FrontendType::Discord
         | FrontendType::Slack
         | FrontendType::Matrix
-        | FrontendType::Proton => {
+        | FrontendType::Proton
+        | FrontendType::Signal => {
             Arc::new(LocalChannel::new("test-local", Vec::<u8>::new()))
         }
     });
diff --git a/crates/aivyx-ipc/src/protocol.rs b/crates/aivyx-ipc/src/protocol.rs
index 3c6a1803..de991e75 100644
--- a/crates/aivyx-ipc/src/protocol.rs
+++ b/crates/aivyx-ipc/src/protocol.rs
@@ -95,6 +95,10 @@ pub enum FrontendType {
     /// connection and applies the email trust policy; only approved mail
     /// reaches the daemon, one connection per email thread.
     Proton,
+    /// Signal daemon-frontend. The frontend process talks to signal-cli and
+    /// applies the verified-safety-number policy; only approved messages
+    /// reach the daemon, one connection per sender.
+    Signal,
 }
 
 // ---------------------------------------------------------------------------
@@ -2017,6 +2021,7 @@ pub enum WireChannelPlatform {
     Slack,
     Matrix,
     Email,
+    Signal,
     Rest,
     Voice,
 }
```

(`protocol.rs` also shows Task 2's `WireChannelPlatform::Signal`, already in place.)

- [ ] **Step 2:** write `signal_daemon_frontend.rs` — tests at the bottom first (watch them fail to compile), then the body:

```rust
//! Daemon-mode Signal frontend: this process talks to signal-cli and
//! applies `aivyx_signal::policy` (via `aivyx_signal::session::route`);
//! only approved messages are submitted to the daemon, over one IPC session
//! per sender. `/team …` and gate commands are gated as on Matrix.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use aivyx_core::{
    CancellationToken, ChannelContext, ChannelError, ChannelPlatform, SessionId, StreamEvent,
    TurnOutcome,
};
use aivyx_signal::policy::{OncePerSender, SignalPolicy};
use aivyx_signal::session::{route, send_reply, turn_text, Routed};
use aivyx_signal::transport::{SignalTransport, TransportError};

use crate::daemon_client::DaemonSession;
use crate::daemon_ipc::FrontendType;
use crate::daemon_server::DaemonError;
use crate::gate_command;
use crate::team_trigger_state::PendingTrigger;

/// The daemon-side channel for `FrontendType::Signal` connections.
pub struct SignalDaemonChannel {
    session: SessionId,
    token: Mutex<CancellationToken>,
}

impl SignalDaemonChannel {
    pub fn new() -> Self {
        SignalDaemonChannel {
            session: SessionId::new(),
            token: Mutex::new(CancellationToken::new()),
        }
    }
}

impl Default for SignalDaemonChannel {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ChannelContext for SignalDaemonChannel {
    fn channel_name(&self) -> &str {
        "aivyx-signal-daemon"
    }
    fn platform(&self) -> ChannelPlatform {
        ChannelPlatform::Signal
    }
    fn trust_tier(&self) -> aivyx_capability::TrustTier {
        // The frontend submits only messages from an allowed sender whose
        // safety number the operator verified.
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

/// Daemon sessions kept open at once (one per recent sender).
const MAX_SENDER_SESSIONS: usize = 8;

struct Sessions {
    socket_path: PathBuf,
    role: Option<String>,
    open: VecDeque<(String, DaemonSession)>,
}

impl Sessions {
    async fn for_sender(&mut self, sender: &str) -> Result<&mut DaemonSession, DaemonError> {
        if let Some(i) = self.open.iter().position(|(s, _)| s == sender) {
            let entry = self.open.remove(i).expect("index from position");
            self.open.push_back(entry);
        } else {
            if self.open.len() >= MAX_SENDER_SESSIONS
                && let Some((_, old)) = self.open.pop_front()
            {
                let _ = old.disconnect().await;
            }
            let s = DaemonSession::connect(&self.socket_path, self.role.clone(), Some(FrontendType::Signal))
                .await?;
            self.open.push_back((sender.to_string(), s));
        }
        Ok(&mut self.open.back_mut().expect("just pushed").1)
    }

    async fn close_all(&mut self) {
        for (_, s) in self.open.drain(..) {
            let _ = s.disconnect().await;
        }
    }
}

/// A Signal call's error: fatal ends the session, anything else is logged.
fn settle(what: &str, r: Result<(), TransportError>) -> Result<(), DaemonError> {
    match r {
        Ok(()) => Ok(()),
        Err(e) if e.is_fatal() => Err(DaemonError::ChannelFatal(e.to_string())),
        Err(e) => {
            eprintln!("aivyx-signal(daemon): {what} failed: {e}");
            Ok(())
        }
    }
}

/// Runs the Signal channel against a running daemon. `state_dir` is the
/// instance's `signal_dir()` (it holds the lock). An unregistered account
/// or a second running channel is `DaemonError::ChannelFatal` (no
/// in-process fallback can help); a daemon failure is another error, so
/// the binary falls back.
#[allow(clippy::too_many_arguments)]
pub async fn run_signal_daemon_session<T: SignalTransport + ?Sized>(
    transport: &T,
    policy: &SignalPolicy,
    state_dir: &Path,
    socket_path: PathBuf,
    role: Option<String>,
    shutdown: CancellationToken,
    team_run_channel: bool,
    team_trigger_rate_limit: Option<u32>,
    team_command_allowed_senders: Vec<String>,
) -> Result<(), DaemonError> {
    let _lock = aivyx_signal::lock::acquire(state_dir).map_err(DaemonError::ChannelFatal)?;
    let mut sessions = Sessions {
        socket_path: socket_path.clone(),
        role,
        open: VecDeque::new(),
    };
    let result = pump(
        transport,
        policy,
        &socket_path,
        &mut sessions,
        shutdown,
        team_run_channel,
        team_trigger_rate_limit,
        &team_command_allowed_senders,
    )
    .await;
    sessions.close_all().await;
    result
}

#[allow(clippy::too_many_arguments)]
async fn pump<T: SignalTransport + ?Sized>(
    transport: &T,
    policy: &SignalPolicy,
    socket_path: &Path,
    sessions: &mut Sessions,
    shutdown: CancellationToken,
    team_run_channel: bool,
    team_trigger_rate_limit: Option<u32>,
    team_command_allowed_senders: &[String],
) -> Result<(), DaemonError> {
    let mut asked = OncePerSender::default();
    let mut pending: Option<PendingTrigger<String>> = None;
    let mut history: Vec<Instant> = Vec::new();
    let mut backoff = Duration::from_secs(1);
    loop {
        let event = tokio::select! {
            biased;
            _ = shutdown.cancelled() => return Ok(()),
            r = transport.next_event() => r,
        };
        let event = match event {
            Ok(e) => {
                backoff = Duration::from_secs(1);
                e
            }
            Err(e) if e.is_fatal() => return Err(DaemonError::ChannelFatal(e.to_string())),
            Err(e) => {
                eprintln!("aivyx-signal(daemon): {e}; retrying in {}s", backoff.as_secs());
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => return Ok(()),
                    _ = tokio::time::sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(Duration::from_secs(60));
                continue;
            }
        };
        let m = match route(transport, policy, &mut asked, event).await {
            Ok(Routed::Turn(m)) => m,
            Ok(Routed::Handled) => continue,
            Err(e) => {
                settle("route", Err(e))?;
                continue;
            }
        };
        let sender = m.sender().to_string();
        let first_line = m.text.trim();
        // Read between turns, so it can't stop one already running (as on
        // Matrix and Discord in daemon mode).
        if first_line == "/cancel" {
            continue;
        }
        if let Some(reply) = crate::channel_team_chain::handle_team_command(
            FrontendType::Signal,
            first_line,
            socket_path,
            &mut pending,
            &mut history,
            team_run_channel,
            team_trigger_rate_limit,
            sender.clone(),
            team_command_allowed_senders,
        )
        .await
        {
            settle("send", send_reply(transport, &sender, &reply, &[]).await)?;
            continue;
        }
        if let Some(gate) = gate_command::parse(first_line) {
            let session = sessions.for_sender(&sender).await?;
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
            settle("send", send_reply(transport, &sender, &reply, &[]).await)?;
            continue;
        }
        let (text, notes) = turn_text(&m);
        let session = sessions.for_sender(&sender).await?;
        settle("typing", transport.typing(&sender, false).await)?;
        let submitted = session.submit_input(text).await;
        settle("typing", transport.typing(&sender, true).await)?;
        let (events, outcome) = submitted?;
        let reply = crate::daemon_ipc::chat_reply(
            crate::discord_daemon_frontend::render_events_for_discord(&events),
            &events,
            &outcome,
        );
        settle("send", send_reply(transport, &sender, &reply, &notes).await)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_signal::envelope::{Inbound, TextMessage};
    use aivyx_signal::transport::{ScriptedTransport, TrustLevel};

    const ME: &str = "+15550001111";

    fn policy() -> SignalPolicy {
        SignalPolicy::new(&[ME.to_string()])
    }

    #[test]
    fn daemon_channel_is_signal_semitrusted() {
        let c = SignalDaemonChannel::new();
        assert_eq!(c.platform(), ChannelPlatform::Signal);
        assert_eq!(c.trust_tier(), aivyx_capability::TrustTier::SemiTrusted);
        assert_eq!(c.channel_name(), "aivyx-signal-daemon");
    }

    #[tokio::test]
    async fn an_unregistered_account_is_channel_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let t = ScriptedTransport::new(vec![])
            .then_fail(|| TransportError::Unregistered("Authorization failed".into()));
        let r = tokio::time::timeout(
            Duration::from_secs(5),
            run_signal_daemon_session(
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
        assert!(e.to_string().contains("connect signal"), "{e}");
    }

    #[tokio::test]
    async fn a_missing_daemon_is_not_channel_fatal_so_the_binary_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let t = ScriptedTransport::new(vec![Inbound::Text(TextMessage {
            sender_number: Some(ME.into()),
            sender_uuid: None,
            timestamp: 1,
            text: "hello".into(),
            attachments: 0,
        })])
        .with_identity(ME, TrustLevel::TrustedVerified, "1");
        let r = run_signal_daemon_session(
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
        assert!(matches!(r, Err(ref e) if !matches!(e, DaemonError::ChannelFatal(_))), "{r:?}");
        // No typing indicator is left running.
        assert!(t.typing_events().await.last().is_none_or(|(_, stop)| *stop));
    }

    #[tokio::test]
    async fn an_unverified_sender_gets_the_verify_reply_without_a_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let t = ScriptedTransport::new(vec![Inbound::Text(TextMessage {
            sender_number: Some(ME.into()),
            sender_uuid: None,
            timestamp: 1,
            text: "hello".into(),
            attachments: 0,
        })])
        .with_identity(ME, TrustLevel::TrustedUnverified, "1");
        let shutdown = CancellationToken::new();
        let s2 = shutdown.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            s2.cancel();
        });
        run_signal_daemon_session(
            &t,
            &policy(),
            dir.path(),
            PathBuf::from("/nonexistent.sock"),
            None,
            shutdown,
            false,
            None,
            vec![],
        )
        .await
        .unwrap();
        assert_eq!(t.sent().await.len(), 1);
        assert_eq!(t.sent().await[0].1, aivyx_signal::policy::ASK_TO_VERIFY);
    }
}
```

- [ ] **Step 3:** the factory arm in `aivyx.rs` (above).
- [ ] **Step 4: Run** `cargo test -j 8 -p aivyx-channel --lib signal_daemon && cargo test -j 8 -p aivyx-channel --test daemon_roundtrip_e2e && cargo clippy -j 8 --workspace --all-targets -- -D warnings` → PASS.

- [ ] **Step: Commit**

```bash
git add crates/aivyx-ipc/src/protocol.rs crates/aivyx-channel crates/aivyx-cli/src/bin/aivyx.rs
git commit -s -m "feat(channel): Signal daemon frontend" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 11: The CLI — `--channel signal`, `connect signal`, `signal verify`

**Files:**
- Modify: `crates/aivyx-channel/src/role_render.rs` (`ChannelKind::Signal`, label `"signal"`)
- Modify: `crates/aivyx-cli/Cargo.toml` (`aivyx-signal`), `crates/aivyx-cli/src/bin/aivyx.rs`, `crates/aivyx-cli/src/bin/aivyx_modules/help.rs` (the `signal` command; `--channel` line keeps within 80 columns)
- Create: `crates/aivyx-cli/src/bin/aivyx_modules/signal_cli.rs`
- Modify: `docs/manual/reference/01-cli.md` (`### aivyx-pa connect signal`, `### aivyx-pa signal` headings, `signal` in the `--channel` list — the `every_command_has_a_cli_reference_heading` drift test needs the `signal` heading in this task)

**Interfaces:**
- Consumes: `aivyx_signal::{toolconf, transport::{CliTransport, SignalTransport, TrustLevel}, policy::SignalPolicy, SignalSessionConfig, run_signal_session_with_transport}`, `aivyx_channel::signal_daemon_frontend::run_signal_daemon_session`, `AivyxConfig::signal`.
- Produces: `CliMode::{SignalConnect, SignalVerify}` (both excluded from `mode_needs_api_key`), `ChannelKind::Signal`, `signal_cli::{run_connect, run_verify}`.

`connect signal` and `signal verify` run right after the config loads (next to `connect proton`): no passphrase, store or daemon. `signal verify` covers `allowed_senders` and `send_to`, skipping a number signal-cli has no key for (with a hint), and calls `trust` only on an explicit `yes`.

- [ ] **Step 1: Failing test** — `signal_commands_parse` and the two new modes in `connect_commands_need_no_api_key` (in the `aivyx.rs` hunk) → `cargo test -j 8 -p aivyx-cli signal_commands_parse` FAILS.
- [ ] **Step 2: Implement** — apply (Task 9's `signal: _,` becomes `signal,`; Task 10's factory arm is already in place):

```diff
diff --git a/crates/aivyx-channel/src/role_render.rs b/crates/aivyx-channel/src/role_render.rs
index 75f457e1..3cc8cd7c 100644
--- a/crates/aivyx-channel/src/role_render.rs
+++ b/crates/aivyx-channel/src/role_render.rs
@@ -93,6 +93,8 @@ pub enum ChannelKind {
     Matrix,
     /// Proton Mail (email) channel — same SemiTrusted registration posture
     Proton,
+    /// Signal channel — same SemiTrusted registration posture
+    Signal,
     /// Phase 135 — Voice I/O channel. Operator speaks
     /// through the microphone; agent speaks back through
     /// the speakers. Same `Trusted` tier posture as
@@ -215,6 +217,7 @@ pub fn render_role_envelope(
             ChannelKind::Slack => "slack",
             ChannelKind::Matrix => "matrix",
             ChannelKind::Proton => "proton",
+            ChannelKind::Signal => "signal",
             ChannelKind::Voice => "voice",
         }
     )
diff --git a/crates/aivyx-cli/Cargo.toml b/crates/aivyx-cli/Cargo.toml
index 2cd60f11..401278e5 100644
--- a/crates/aivyx-cli/Cargo.toml
+++ b/crates/aivyx-cli/Cargo.toml
@@ -118,6 +118,7 @@ aivyx-discord = { path = "../aivyx-discord" }
 aivyx-slack = { path = "../aivyx-slack" }
 aivyx-matrix = { path = "../aivyx-matrix" }
 aivyx-proton = { path = "../aivyx-proton" }
+aivyx-signal = { path = "../aivyx-signal" }
 aivyx-voice = { path = "../aivyx-voice", optional = true }
 # Task 8 (Chapter Passport): `aivyx federation yubikey-init` drives
 # `aivyx-yubi`'s discovery/pin/provision flow directly (not re-exported by
diff --git a/crates/aivyx-cli/src/bin/aivyx.rs b/crates/aivyx-cli/src/bin/aivyx.rs
index b91b30af..8499a062 100644
--- a/crates/aivyx-cli/src/bin/aivyx.rs
+++ b/crates/aivyx-cli/src/bin/aivyx.rs
@@ -110,6 +110,8 @@ mod connect_kitchen;
 mod matrix_cli;
 #[path = "aivyx_modules/proton_cli.rs"]
 mod proton_cli;
+#[path = "aivyx_modules/signal_cli.rs"]
+mod signal_cli;
 #[path = "aivyx_modules/cost.rs"]
 mod cost;
 #[path = "aivyx_modules/daemon_service.rs"]
@@ -379,7 +381,8 @@ fn build_shell_exec_for_channel(
         | ChannelKind::Discord
         | ChannelKind::Slack
         | ChannelKind::Matrix
-        | ChannelKind::Proton => Ok(None),
+        | ChannelKind::Proton
+        | ChannelKind::Signal => Ok(None),
     }
 }
 
@@ -433,7 +436,8 @@ fn build_fs_delete_for_channel(
         | ChannelKind::Discord
         | ChannelKind::Slack
         | ChannelKind::Matrix
-        | ChannelKind::Proton => Ok(None),
+        | ChannelKind::Proton
+        | ChannelKind::Signal => Ok(None),
     }
 }
 
@@ -1286,6 +1290,21 @@ fn run() -> Result<(), String> {
         return rt.block_on(proton_cli::run_connect(&config));
     }
 
+    // `aivyx-pa connect signal` / `signal verify`: need the config
+    // ([signal]) but no passphrase, store or daemon — they talk only to the
+    // operator's signal-cli daemon.
+    if matches!(mode, CliMode::SignalConnect | CliMode::SignalVerify) {
+        let rt = tokio::runtime::Builder::new_current_thread()
+            .enable_all()
+            .build()
+            .map_err(|e| format!("failed to build tokio runtime: {e}"))?;
+        return if mode == CliMode::SignalConnect {
+            rt.block_on(signal_cli::run_connect(&config))
+        } else {
+            rt.block_on(signal_cli::run_verify(&config))
+        };
+    }
+
     // Model routing Part 3a — `aivyx-pa routing status`. Like
     // `--print-role`, it needs no passphrase, store, or sandbox: it runs
     // the daemon's startup discovery + merge against `[routing]` and prints
@@ -2258,10 +2277,18 @@ fn run_mcp_status() -> Result<(), String> {
 // and any future IPC frontends share one discriminator.
 
 /// Whether `mode` can need the LLM provider's API key. `connect proton`,
-/// `connect matrix` and `matrix verify` talk only to Bridge / the
-/// homeserver, so they load without one.
+/// `connect matrix`, `matrix verify`, `connect signal` and `signal verify`
+/// talk only to Bridge / the homeserver / signal-cli, so they load without
+/// one.
 fn mode_needs_api_key(mode: &CliMode) -> bool {
-    !matches!(mode, CliMode::ProtonConnect | CliMode::MatrixConnect | CliMode::MatrixVerify)
+    !matches!(
+        mode,
+        CliMode::ProtonConnect
+            | CliMode::MatrixConnect
+            | CliMode::MatrixVerify
+            | CliMode::SignalConnect
+            | CliMode::SignalVerify
+    )
 }
 
 /// Phase 17 Task 3 — the binary's primary mode of operation.
@@ -2301,6 +2328,10 @@ enum CliMode {
     MatrixVerify,
     /// `aivyx-pa connect proton` — pins Bridge's certificate and stores its login.
     ProtonConnect,
+    /// `aivyx-pa connect signal` — checks signal-cli answers; writes the tool's `send_to`.
+    SignalConnect,
+    /// `aivyx-pa signal verify` — compare and mark safety numbers verified.
+    SignalVerify,
     /// `aivyx-pa instances list | create <name> | remove <name>`: named
     /// instances (several separate agents for one OS user).
     Instances(InstancesCmd),
@@ -3826,6 +3857,18 @@ fn parse_cli_args_from(args: &[String]) -> Result<CliArgs, String> {
         if args.len() > 2 {
             return Err("`aivyx-pa connect` takes at most one service name".into());
         }
+        if service.as_deref() == Some("signal") {
+            return Ok(CliArgs {
+                mode: CliMode::SignalConnect,
+                channel: ChannelKind::Local,
+                role: None,
+                no_daemon: false,
+                mcp_servers: vec![],
+                mcp_sse_servers: vec![],
+                provider: None,
+                web_ui_port: None,
+            });
+        }
         if service.as_deref() == Some("proton") {
             return Ok(CliArgs {
                 mode: CliMode::ProtonConnect,
@@ -3862,6 +3905,23 @@ fn parse_cli_args_from(args: &[String]) -> Result<CliArgs, String> {
         });
     }
 
+    // `aivyx-pa signal verify`: compare and mark safety numbers verified.
+    if !args.is_empty() && args[0] == "signal" {
+        if args.get(1).map(String::as_str) != Some("verify") || args.len() > 2 {
+            return Err("usage: `aivyx-pa signal verify`".into());
+        }
+        return Ok(CliArgs {
+            mode: CliMode::SignalVerify,
+            channel: ChannelKind::Local,
+            role: None,
+            no_daemon: false,
+            mcp_servers: vec![],
+            mcp_sse_servers: vec![],
+            provider: None,
+            web_ui_port: None,
+        });
+    }
+
     // `aivyx-pa matrix verify`: SAS-verify an operator device.
     if !args.is_empty() && args[0] == "matrix" {
         if args.get(1).map(String::as_str) != Some("verify") || args.len() > 2 {
@@ -5669,7 +5729,7 @@ fn parse_cli_args_from(args: &[String]) -> Result<CliArgs, String> {
             }
             "--channel" => {
                 let value = args.get(i + 1).ok_or_else(|| {
-                    "`--channel` requires a value: `local`, `telegram`, `discord`, `slack`, `matrix`, `proton`, or `voice`"
+                    "`--channel` requires a value: `local`, `telegram`, `discord`, `slack`, `matrix`, `proton`, `signal`, or `voice`"
                         .to_string()
                 })?;
                 channel = match value.as_str() {
@@ -5681,12 +5741,13 @@ fn parse_cli_args_from(args: &[String]) -> Result<CliArgs, String> {
                     "slack" => ChannelKind::Slack,
                     "matrix" => ChannelKind::Matrix,
                     "proton" => ChannelKind::Proton,
+                    "signal" => ChannelKind::Signal,
                     // Phase 135 — Voice channel parse arm.
                     "voice" => ChannelKind::Voice,
                     other => {
                         return Err(format!(
                             "unrecognized channel `{other}`. \
-                             Supported: local, telegram, discord, slack, matrix, proton, voice"
+                             Supported: local, telegram, discord, slack, matrix, proton, signal, voice"
                         ));
                     }
                 };
@@ -6831,6 +6892,8 @@ async fn run_async(
         matrix,
         // `[proton]` config consumed by the `ChannelKind::Proton` arm below.
         proton,
+        // `[signal]` config consumed by the `ChannelKind::Signal` arm below.
+        signal,
         // Phase 108 — `slack` config consumed by the
         // `ChannelKind::Slack` dispatch arm below; carries
         // the bot + app tokens through to
@@ -10778,6 +10841,9 @@ async fn run_async(
                 aivyx_channel::daemon_ipc::FrontendType::Proton => Arc::new(
                     aivyx_channel::proton_daemon_frontend::ProtonDaemonChannel::new(),
                 ),
+                aivyx_channel::daemon_ipc::FrontendType::Signal => Arc::new(
+                    aivyx_channel::signal_daemon_frontend::SignalDaemonChannel::new(),
+                ),
                 // Phase 111 — Slack daemon-mode channel stub.
                 // Mirrors TelegramDaemonChannel and
                 // DiscordDaemonChannel; the slack_daemon_frontend.rs
@@ -11889,6 +11955,89 @@ async fn run_async(
             .map(|_| ())
         }
 
+        ChannelKind::Signal => {
+            let sc = signal.ok_or("`--channel signal` needs a [signal] section in your config")?;
+            let instance = aivyx_instance::InstancePaths::current().map_err(|e| e.to_string())?;
+            let state_dir = instance
+                .signal_dir()
+                .ok_or("can't resolve the instance's data folder")?;
+            let socket = aivyx_signal::toolconf::socket_path(&instance)
+                .ok_or("can't resolve the instance's folders (is HOME set?)")?;
+            let transport = aivyx_signal::transport::CliTransport::new(socket);
+            let policy = aivyx_signal::policy::SignalPolicy::new(&sc.allowed_senders);
+            let shutdown = CancellationToken::new();
+            let shutdown_for_signal = shutdown.clone();
+            tokio::spawn(async move {
+                if tokio::signal::ctrl_c().await.is_err() {
+                    std::process::exit(130);
+                }
+                eprintln!("\naivyx: shutting down the Signal channel.");
+                shutdown_for_signal.cancel();
+            });
+            let daemon_socket = match default_socket_path() {
+                Ok(sp) if !no_daemon && aivyx_channel::daemon_client::daemon_is_running(&sp).await => Some(sp),
+                _ => None,
+            };
+            if let Some(sp) = daemon_socket {
+                eprintln!(
+                    "aivyx-pa {} (daemon) — answering Signal messages to {}",
+                    env!("CARGO_PKG_VERSION"),
+                    sc.account
+                );
+                match aivyx_channel::signal_daemon_frontend::run_signal_daemon_session(
+                    &transport,
+                    &policy,
+                    &state_dir,
+                    sp.clone(),
+                    Some(active_role_name.clone()),
+                    shutdown.clone(),
+                    sc.team_run_channel,
+                    sc.team_trigger_rate_limit,
+                    sc.team_command_allowed_senders.clone(),
+                )
+                .await
+                {
+                    Ok(()) => return Ok(()),
+                    // An unregistered account or a second channel would
+                    // fail the same way in-process.
+                    Err(e @ aivyx_channel::daemon_server::DaemonError::ChannelFatal(_)) => return Err(e.to_string()),
+                    Err(e) => eprintln!("aivyx-pa: daemon Signal session failed ({e}), falling back to in-process."),
+                }
+            }
+            eprintln!(
+                "aivyx-pa {} — answering Signal messages to {} (in-process)",
+                env!("CARGO_PKG_VERSION"),
+                sc.account
+            );
+            let signal_config = aivyx_signal::SignalSessionConfig {
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
+            aivyx_signal::run_signal_session_with_transport(
+                &transport, &policy, &state_dir, signal_config, provider, audit, checkpointer.clone(), shutdown,
+            )
+            .await
+            .map(|_| ())
+        }
+
         ChannelKind::Proton => {
             let pc = proton.ok_or("`--channel proton` needs a [proton] section in your config")?;
             let channel_address = pc.channel_address.clone().ok_or(
@@ -14816,12 +14965,27 @@ mod tests {
 
     #[test]
     fn connect_commands_need_no_api_key() {
-        for mode in [CliMode::ProtonConnect, CliMode::MatrixConnect, CliMode::MatrixVerify] {
+        for mode in [
+            CliMode::ProtonConnect,
+            CliMode::MatrixConnect,
+            CliMode::MatrixVerify,
+            CliMode::SignalConnect,
+            CliMode::SignalVerify,
+        ] {
             assert!(!mode_needs_api_key(&mode), "{mode:?}");
         }
         assert!(mode_needs_api_key(&parse_cli_args_from(&argv(&[])).unwrap().mode));
     }
 
+    #[test]
+    fn signal_commands_parse() {
+        assert_eq!(parse_cli_args_from(&argv(&["connect", "signal"])).unwrap().mode, CliMode::SignalConnect);
+        assert_eq!(parse_cli_args_from(&argv(&["signal", "verify"])).unwrap().mode, CliMode::SignalVerify);
+        assert!(parse_cli_args_from(&argv(&["signal"])).is_err());
+        assert!(parse_cli_args_from(&argv(&["signal", "verify", "x"])).is_err());
+        assert_eq!(parse_cli_args_from(&argv(&["--channel", "signal"])).unwrap().channel, ChannelKind::Signal);
+    }
+
     #[test]
     fn proton_commands_parse() {
         assert_eq!(parse_cli_args_from(&argv(&["connect", "proton"])).unwrap().mode, CliMode::ProtonConnect);
diff --git a/crates/aivyx-cli/src/bin/aivyx_modules/help.rs b/crates/aivyx-cli/src/bin/aivyx_modules/help.rs
index 5104282d..ccf602ff 100644
--- a/crates/aivyx-cli/src/bin/aivyx_modules/help.rs
+++ b/crates/aivyx-cli/src/bin/aivyx_modules/help.rs
@@ -282,6 +282,12 @@ pub const COMMANDS: &[CommandHelp] = &[
         summary: "Guided OAuth credential onboarding",
         usage: &["aivyx-pa connect", "aivyx-pa connect <service>"],
     },
+    CommandHelp {
+        name: "signal",
+        group: Group::Tools,
+        summary: "Verify Signal safety numbers",
+        usage: &["aivyx-pa signal verify"],
+    },
     CommandHelp {
         name: "matrix",
         group: Group::Tools,
@@ -467,7 +473,7 @@ pub fn render_top_level_help() -> String {
     out.push_str("  --instance <name>      Use a named instance (multi-instance support).\n");
     out.push_str("  --verify-only          Forensic: verify the audit chain and exit.\n");
     out.push_str("  --channel <kind>       local | telegram | discord | slack | matrix |\n");
-    out.push_str("                         proton | voice.\n");
+    out.push_str("                         proton | signal | voice.\n");
     out.push_str("  --role <name>          Use a named role for this session.\n");
     out.push_str("  --print-role <name>    Print a role's rendered envelope and exit.\n");
     out.push_str("  --no-daemon            Chat in-process: don't connect to a running\n");
```

`crates/aivyx-cli/src/bin/aivyx_modules/signal_cli.rs`:

```rust
//! `aivyx-pa connect signal` and `aivyx-pa signal verify`.

use std::io::{BufRead, Write};

use aivyx_config::{AivyxConfig, SignalConfig};
use aivyx_signal::toolconf::{self, ToolConfig};
use aivyx_signal::transport::{CliTransport, SignalTransport, TrustLevel};

fn signal_config(config: &AivyxConfig) -> Result<&SignalConfig, String> {
    config.signal.as_ref().ok_or_else(|| {
        "no [signal] section in your config. Add account and allowed_senders first — \
         see `docs/guide/13-chat-apps-and-accounts.md`."
            .to_string()
    })
}

fn transport() -> Result<(CliTransport, std::path::PathBuf), String> {
    let instance = aivyx_instance::InstancePaths::current().map_err(|e| e.to_string())?;
    let socket = toolconf::socket_path(&instance)
        .ok_or("can't resolve the instance's folders (is HOME set?)")?;
    Ok((CliTransport::new(socket.clone()), socket))
}

/// The signal-cli command line `connect signal` tells the operator to run.
fn daemon_command(account: &str, socket: &std::path::Path) -> String {
    format!(
        "signal-cli -a {account} --trust-new-identities never daemon \\\n  --socket {} \\\n  \
         --receive-mode manual --ignore-attachments",
        socket.display()
    )
}

/// Checks signal-cli answers on the instance's socket and writes the
/// tool's `send_to` (`signal-tool.toml`).
pub async fn run_connect(config: &AivyxConfig) -> Result<(), String> {
    let s = signal_config(config)?;
    let (t, socket) = transport()?;
    let version = t.version().await.map_err(|e| {
        format!(
            "{e}\n\nStart it (the guide has a systemd user unit) with:\n\n{}",
            daemon_command(&s.account, &socket)
        )
    })?;
    let instance = aivyx_instance::InstancePaths::current().map_err(|e| e.to_string())?;
    let conf = toolconf::tool_config_path(&instance)
        .ok_or("can't resolve the instance's folders (is HOME set?)")?;
    toolconf::save(&conf, &ToolConfig { send_to: s.send_to.clone() })?;
    eprintln!("Connected to signal-cli {version} at {}.", socket.display());
    eprintln!(
        "Next: `aivyx-pa signal verify`, then `aivyx-pa --channel signal`. For \
         signal.send, add to aivyx-pa.toml and restart the daemon:\n\n[[tool_process]]\n\
         name = \"signal\"\ncommand = \"aivyx-signal\"\n"
    );
    Ok(())
}

/// For each allowed sender and `send_to` contact: show the safety number,
/// and mark it verified if the operator says it matches their phone.
pub async fn run_verify(config: &AivyxConfig) -> Result<(), String> {
    let s = signal_config(config)?;
    let (t, _) = transport()?;
    let mut people: Vec<String> = s.allowed_senders.clone();
    for n in &s.send_to {
        if !people.contains(n) {
            people.push(n.clone());
        }
    }
    let mut verified = 0;
    for who in &people {
        match t.identity(who).await.map_err(|e| e.to_string())? {
            None => eprintln!(
                "{who}: no key yet. Send the assistant a message from that number (or try \
                 a signal.send to it), then run this again."
            ),
            Some(id) if id.trust == TrustLevel::TrustedVerified => {
                eprintln!("{who}: already verified.");
                verified += 1;
            }
            Some(id) => {
                eprintln!(
                    "\n{who}: safety number\n\n  {}\n\nOn that phone, open the chat with the \
                     assistant → View safety number.",
                    id.safety_number
                );
                eprint!("Is it the same? Type yes if so: ");
                let _ = std::io::stderr().flush();
                let mut line = String::new();
                let _ = std::io::stdin().lock().read_line(&mut line);
                if line.trim().eq_ignore_ascii_case("yes") {
                    t.trust_verified(who, &id.safety_number)
                        .await
                        .map_err(|e| e.to_string())?;
                    eprintln!("{who}: verified.");
                    verified += 1;
                } else {
                    eprintln!("{who}: not verified.");
                }
            }
        }
    }
    eprintln!("\n{verified} of {} verified.", people.len());
    Ok(())
}
```

and in `docs/manual/reference/01-cli.md`:

````diff
diff --git a/docs/manual/reference/01-cli.md b/docs/manual/reference/01-cli.md
index c317b101..a6188033 100644
--- a/docs/manual/reference/01-cli.md
+++ b/docs/manual/reference/01-cli.md
@@ -15,7 +15,7 @@ These work with any command.
 |---|---|
 | `--instance <name>` | Use a named instance — a separate agent with its own config, store and Studio. Same as setting `AIVYX_PA_INSTANCE`; the flag wins. Without it you get the `default` instance. See [Named instances](../../guide/16-named-instances.md). |
 | `--verify-only` | Verify the audit chain and exit (forensic check; no session). |
-| `--channel <kind>` | Run the session on a channel instead of the terminal: `local`, `telegram`, `discord`, `slack`, `matrix`, `proton` or `voice`. |
+| `--channel <kind>` | Run the session on a channel instead of the terminal: `local`, `telegram`, `discord`, `slack`, `matrix`, `proton`, `signal` or `voice`. |
 | `--role <name>` | Use a named role (from `[[role]]` in your config) for this session. |
 | `--print-role <name>` | Print a role's full capability envelope and exit. |
 | `--no-daemon` | Chat in-process: don't connect to a running daemon or start one. |
@@ -462,6 +462,31 @@ aivyx-pa connect proton
 
 Then run the channel with `aivyx-pa --channel proton`.
 
+### `aivyx-pa connect signal`
+
+Checks that your signal-cli daemon answers on the instance's socket
+(`tool-processes/signal/signal-cli.sock`) using the `[signal]` section of your
+config, and saves `send_to` for the `signal.send` tool. If signal-cli isn't
+reachable it prints the command to start it. Needs no passphrase and no
+daemon. Run it again after changing `send_to`.
+
+```
+aivyx-pa connect signal
+```
+
+### `aivyx-pa signal`
+
+Verifies safety numbers. For each `allowed_senders` and `send_to` number it
+shows the safety number signal-cli has; compare it with that phone (the chat
+with the assistant → View safety number) and type `yes` to mark it verified.
+A number signal-cli has no key for yet is skipped with a hint.
+
+```
+aivyx-pa signal verify
+```
+
+Then run the channel with `aivyx-pa --channel signal`.
+
 ### `aivyx-pa pack`
 
 Vertical packs: signed bundles that turn an assistant into a domain
````

- [ ] **Step 3: Run** `cargo test -j 8 -p aivyx-cli && cargo clippy -j 8 --workspace --all-targets -- -D warnings && cargo test -j 8 --no-fail-fast --workspace` → PASS. Then with no `[signal]`: `./target/debug/aivyx-pa connect signal` → "no [signal] section …"; `./target/debug/aivyx-pa --channel signal` → "needs a [signal] section".

- [ ] **Step: Commit**

```bash
git add crates/aivyx-channel/src/role_render.rs crates/aivyx-cli docs/manual/reference/01-cli.md
git commit -s -m "feat(cli): --channel signal, connect signal and signal verify" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 12: Docs

**Files:**
- Modify: `scripts/gen-config-reference.py`, `scripts/gen-tools-reference.py` (diff below), then regenerate `docs/manual/reference/02-configuration.md` and `03-tools.md`
- Modify: `docs/guide/13-chat-apps-and-accounts.md` (a `### Signal` section before `### Discord and Slack`), `docs/THREAT_MODEL.md` (the SemiTrusted row example and a **Signal.** paragraph after **Proton Mail.** in §5.8)

- [ ] **Step 1: Generators and references:**

```diff
diff --git a/scripts/gen-config-reference.py b/scripts/gen-config-reference.py
index 575d6b97..23141035 100755
--- a/scripts/gen-config-reference.py
+++ b/scripts/gen-config-reference.py
@@ -69,7 +69,7 @@ GROUPS = [
         "skill_refinement", "skill_authoring", "skill_defaults", "tool_relevance",
         "reflection_schedule",
     ]),
-    ("Chat apps and voice", ["telegram", "discord", "slack", "matrix", "proton", "voice", "email"]),
+    ("Chat apps and voice", ["telegram", "discord", "slack", "matrix", "proton", "signal", "voice", "email"]),
     ("Tools and integrations", ["mcp_server", "tool_process", "applications"]),
     ("Routines and the autonomous loop", [
         "schedule", "webhook", "file_watch", "notify_target", "reminders", "proactive", "loop",
@@ -131,6 +131,7 @@ INTROS = {
     "slack": f"The Slack bot channel (Socket Mode). See [Chat apps and accounts]({GUIDE}/13-chat-apps-and-accounts.md).",
     "matrix": f"The Matrix channel: an end-to-end-encrypted room with your assistant. See [Chat apps and accounts]({GUIDE}/13-chat-apps-and-accounts.md).",
     "proton": f"Proton Mail through Proton Mail Bridge: the email channel and the `proton.*` tools. See [Chat apps and accounts]({GUIDE}/13-chat-apps-and-accounts.md).",
+    "signal": f"Signal through your own signal-cli daemon: the channel and `signal.send`. See [Chat apps and accounts]({GUIDE}/13-chat-apps-and-accounts.md).",
     "voice": "The voice channel (`--channel voice`): speech recognition and text-to-speech engines and devices.",
     "email": "Outgoing mail (SMTP) for the email tools and notifications.",
     "mcp_server": "MCP servers whose tools the agent can use. One block per server.",
diff --git a/scripts/gen-tools-reference.py b/scripts/gen-tools-reference.py
index 9aabba0e..51b7a632 100755
--- a/scripts/gen-tools-reference.py
+++ b/scripts/gen-tools-reference.py
@@ -25,6 +25,7 @@ GROUPS = {
     "aivyx-channel": ("Agent machinery", "Missions, routines, reminders, skills, reflection and the like — the agent managing its own work."),
     "aivyx-toolkit": ("Toolkit", "Everyday utilities — tasks, budget, dates, units, health checks. A tool process: install `aivyx-toolkit` and add it as a `[[tool_process]]` (see [INSTALL.md](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/INSTALL.md))."),
     "aivyx-gmail": ("Gmail", "Needs `aivyx-pa connect gmail`."),
+    "aivyx-signal": ("Signal", f"Needs your signal-cli daemon and `aivyx-pa connect signal`, then the `aivyx-signal` `[[tool_process]]`. See [Chat apps and accounts]({GUIDE}/13-chat-apps-and-accounts.md)."),
     "aivyx-proton": ("Proton Mail", f"Needs Proton Mail Bridge and `aivyx-pa connect proton`, then the `aivyx-proton` `[[tool_process]]`. See [Chat apps and accounts]({GUIDE}/13-chat-apps-and-accounts.md)."),
     "aivyx-calendar": ("Google Calendar", "Needs `aivyx-pa connect calendar`."),
     "aivyx-drive": ("Google Drive", "Needs `aivyx-pa connect drive`."),
```

Run: `python3 scripts/gen-config-reference.py > /tmp/conf.md && cp /tmp/conf.md docs/manual/reference/02-configuration.md && python3 scripts/gen-tools-reference.py > /tmp/tools.md && cp /tmp/tools.md docs/manual/reference/03-tools.md && git diff --stat docs/manual/reference && grep -c '{GUIDE}' docs/manual/reference/*.md`
Expected: a `[signal]` section (6 keys) and a "Signal" tools section; only Signal lines change; no `{GUIDE}` left anywhere.

- [ ] **Step 2: Guide and threat model:**

````diff
diff --git a/docs/THREAT_MODEL.md b/docs/THREAT_MODEL.md
index 7f92e45d..cabe7fc3 100644
--- a/docs/THREAT_MODEL.md
+++ b/docs/THREAT_MODEL.md
@@ -53,7 +53,7 @@ the trust tiers in `aivyx-capability/src/lib.rs:503` (D5):
 |---|---|---|---|
 | `Kernel` | Aivyx PA itself | the turn loop, audit writer | unconditional (internal use only) |
 | `Trusted` | the operator at their own keyboard | Local CLI, Web UI on `127.0.0.1` | near-total, with extra audit on destructive ops |
-| `SemiTrusted` | the operator over a remote, authenticated channel | their own Telegram bot, with chat-id allowlisted; the Matrix home room, from a verified operator device; an email to the Proton channel address, from you (proven by Sent) or an allowlisted sender with Proton's DMARC pass | narrowed — no unqualified shell, no `fs.delete`, qualifier required on `fs.*` and `net.post` |
+| `SemiTrusted` | the operator over a remote, authenticated channel | their own Telegram bot, with chat-id allowlisted; the Matrix home room, from a verified operator device; an email to the Proton channel address, from you (proven by Sent) or an allowlisted sender with Proton's DMARC pass; a Signal message from an allowed number with a verified safety number | narrowed — no unqualified shell, no `fs.delete`, qualifier required on `fs.*` and `net.post` |
 | `Untrusted` | anyone the operator has not authenticated | webhook requests, unallowlisted senders | near-empty — read public memory, that's it |
 
 A turn loop computes the **effective capability set** exactly
@@ -852,6 +852,21 @@ are `Trusted`, the channel `SemiTrusted`. Gate commands by email are honoured
 only from you (or `team_command_allowed_senders`); the daemon's `ResolveGate`
 doesn't check who owns the mission, so the frontend does.
 
+**Signal.** A message is a turn only from `allowed_senders`, as a 1:1 text
+message, when signal-cli reports the sender's identity key as
+`TRUSTED_VERIFIED` — the operator compared the safety number with their phone
+(`aivyx-pa signal verify`). signal-cli runs with `--trust-new-identities
+never`, so no key is trusted until verified, and a changed key (a
+re-registered or SIM-swapped number) is refused until verified again. An
+allowed sender with an unverified key gets one reply per run asking to
+verify; everyone else, and group messages, are ignored. signal-cli is a
+separate process the operator runs (libsignal is AGPL and can't be linked
+into BUSL-1.1 code): pa talks JSON-RPC to it over a Unix socket in the
+instance's signal tool folder and holds no Signal credential. `signal.send`
+(scope `message.send`, `Trusted`-only, approval on every call) can only
+message numbers in `send_to`, and only once their safety number is verified,
+so a hijacked agent can't message strangers from the assistant's number.
+
 ### 5.9 Adversarial co-tenants
 
 A second OS user on the same machine who has been granted access
diff --git a/docs/guide/13-chat-apps-and-accounts.md b/docs/guide/13-chat-apps-and-accounts.md
index f595e185..f4fc128a 100644
--- a/docs/guide/13-chat-apps-and-accounts.md
+++ b/docs/guide/13-chat-apps-and-accounts.md
@@ -199,6 +199,93 @@ Good to know:
   `imap_addr` or `smtp_addr`.
 - **One channel process at a time** per instance.
 
+### Signal
+
+Message your assistant on Signal and get the answer back there — and let it
+send a Signal message to a contact you list, with your approval each time.
+The assistant has **its own Signal number**; it talks through
+[signal-cli](https://github.com/AsamK/signal-cli), which you run yourself, so
+pa never holds a Signal credential.
+
+1. Get a number for the assistant (a spare SIM, or a VoIP number that can
+   receive an SMS or call) and register it with signal-cli (it needs Java 25):
+
+   ```
+   signal-cli -a +15559990000 register        # add --captcha … if asked
+   signal-cli -a +15559990000 verify 123-456  # the code you received
+   ```
+
+2. Run signal-cli as a daemon. A systemd user unit,
+   `~/.config/systemd/user/signal-cli.service` (adjust the number and, for a
+   named instance, the socket path — `connect signal` prints the right one):
+
+   ```ini
+   [Unit]
+   Description=signal-cli for Aivyx PA
+
+   [Service]
+   ExecStart=signal-cli -a +15559990000 --trust-new-identities never daemon --socket %h/.aivyx-pa/tool-processes/signal/signal-cli.sock --receive-mode manual --ignore-attachments
+   Restart=on-failure
+
+   [Install]
+   WantedBy=default.target
+   ```
+
+   then `systemctl --user enable --now signal-cli`. Keep signal-cli up to date
+   — Signal stops accepting releases older than about three months.
+
+3. Add to your config:
+
+   ```toml
+   [signal]
+   account = "+15559990000"          # the assistant's number
+   allowed_senders = ["+15550001111"] # your number
+   # send_to = ["+15550002222"]       # contacts signal.send may message
+   ```
+
+4. `aivyx-pa connect signal` — checks signal-cli answers (and tells you how
+   to start it if not).
+5. Send the assistant a message from your phone, then `aivyx-pa signal
+   verify`: it shows the safety number; compare it with your phone (the chat
+   with the assistant → View safety number) and type `yes`.
+6. `aivyx-pa --channel signal`.
+7. For `signal.send`, add this and restart the daemon:
+
+   ```toml
+   [[tool_process]]
+   name = "signal"
+   command = "aivyx-signal"
+   ```
+
+It answers only you (`allowed_senders`), and only once you've verified your
+safety number — a SIM-swapped or re-registered number can't drive it. With an
+unverified (or changed) number it replies once asking you to verify; everyone
+else, and group messages, are ignored.
+
+Good to know:
+
+- **Messages aren't lost while the channel is off.** signal-cli fetches them
+  only for the channel; they wait on Signal's server until it runs.
+- **One conversation per sender.** A message is read up to 4 KiB;
+  attachments aren't downloaded (the reply says it ignored them). Long
+  replies arrive as several messages. You see "typing…" while it works.
+- **Disappearing messages:** signal-cli applies the chat's timer to the
+  assistant's replies.
+- **`/cancel`** stops the current turn when the channel runs without the
+  daemon; through the daemon it's read between turns. `/team` and gate
+  commands work only through the daemon.
+- **`signal.send`** only messages numbers in `send_to`, needs the trusted
+  terminal or Studio (it's not available inside a Signal turn), and asks you
+  to approve every send (unless you turned off confirmations with `[access]
+  confirm_destructive = false`). Each contact's safety number must be
+  verified too (`signal verify` covers `send_to`); signal-cli learns a
+  contact's key once they've messaged the assistant or after a first send
+  attempt. Run `aivyx-pa connect signal` again after changing `send_to`.
+- **If the number is unregistered** (re-registered elsewhere), the channel
+  stops and tells you to re-register it with signal-cli. If signal-cli
+  isn't running, the channel waits for it.
+- **One channel process at a time** per instance.
+
 ### Discord and Slack
 
 Both follow the same pattern with a few more steps on the chat app's side
````

- [ ] **Step 3: Run** `cargo test -j 8 -p aivyx-cli && cargo test -j 8 -p aivyx-config && cargo test -j 8 -p aivyx-capability` → PASS (drift tests).

- [ ] **Step: Commit**

```bash
git add docs scripts
git commit -s -m "docs: the Signal channel and signal.send — guide, references, threat model" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

## Final check

- [ ] `cargo clippy -j 8 --workspace --all-targets -- -D warnings` — clean
- [ ] `cargo test -j 8 --no-fail-fast --workspace` — all pass
- [ ] **Manual, against the operator's real assistant number** (needs signal-cli, Java 25, and a registered number; can't run in CI). Record results in the progress ledger:
  1. `aivyx-pa connect signal` with signal-cli stopped prints the start command; started, it reports the version and writes `signal-tool.toml`.
  2. A message from your phone before verifying → exactly one "please verify" reply; `aivyx-pa signal verify` shows the same safety number your phone does; after `yes`, a message gets a real answer, with "typing…" while it works and a read receipt.
  3. Record real `receive` envelopes (a direct message, a group message, a reaction, a receipt) — `socat - UNIX-CONNECT:<socket>` and send `{"jsonrpc":"2.0","id":1,"method":"subscribeReceive"}` — redact numbers, and replace the synthetic fixtures with them; the envelope tests must still pass.
  4. A group message to the assistant is ignored; a stranger's message is ignored.
  5. Re-install Signal on your phone (or link a fresh identity) → refused once, until verified again.
  6. A long answer arrives as several messages; a disappearing-messages chat's replies disappear on the same timer.
  7. Stop signal-cli mid-run → the channel logs and waits; start it → it carries on, and messages sent meanwhile arrive.
  8. `signal.send` to a `send_to` contact from the terminal asks for approval; to an unlisted number it's refused; to a contact whose key isn't verified it says to run `signal verify`.
  9. Through the daemon: a turn, `/team status` (if allowed), and Ctrl-C during a turn.
