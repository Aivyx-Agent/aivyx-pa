# Security Audit Fixes (High + Medium) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close every High- and Medium-severity finding from the 2026-09-16 full-codebase security audit of `aivyx-pa` (see the audit synthesis in the originating conversation — no separate spec doc exists; this plan's task descriptions are the spec).

**Architecture:** Each task is a narrow, self-contained fix to one existing mechanism (an egress filter, a permission check, a file-write ordering, a trust-tier assignment). No task introduces new architecture; several extend an existing, already-proven pattern in the same codebase (e.g. `write_secure`'s atomic-0600 pattern, `safe_relative`'s path-traversal guard) to a second call site that was missing it.

**Tech Stack:** Rust, tokio, the existing `aivyx-*` workspace crates. No new dependencies.

## Global Constraints

- Every task must leave `cargo clippy --all-targets -- -D warnings` clean (default-members scope — do not add `--workspace`, which pulls in `aivyx-web`/`aivyx-desktop`).
- Every task must leave `cargo test -p <affected-crate>` (and, before the final commit of the whole plan, a full `cargo test`) green.
- Do not change any *public* API signature unless the task explicitly says to — several fixes are internal-only.
- Every fix needs a real regression test proving the specific vulnerability is closed, not just a happy-path test.
- One commit per task, on the branch `security/audit-fixes-2026-09-16` created off `main` before Task 1.
- Line numbers cited below are from the repo state as read on 2026-09-16 during the audit. **Re-read the current file before editing** — a few lines may have drifted from earlier tasks in this same plan landing first.

---

## Setup

- [ ] Create the working branch:

```bash
cd /home/julian/Projects/Rust/aivyx-pa
git checkout main && git pull --ff-only
git checkout -b security/audit-fixes-2026-09-16
```

---

### Task 1: Authenticate the webhook listener (HIGH)

**Finding:** `crates/aivyx-channel/src/webhook_listener.rs`'s `handle_request` checks only the URL path and HTTP method — no shared secret, no signature, no bearer token. Any local process, or any webpage the operator merely visits (CORS "simple request," no preflight), can fire an agent turn.

**Files:**
- Modify: `crates/aivyx-channel/src/webhook.rs` (the `WebhookRecord` struct — find it via `grep -n "struct WebhookRecord" crates/aivyx-channel/src/webhook.rs`; add a `secret: String` field, generated at webhook-creation time, never displayed again after creation).
- Modify: `crates/aivyx-channel/src/webhook_listener.rs:88-124` (`handle_request`) and `:128-190` (`fire_webhook`).
- Test: inline `#[cfg(test)] mod tests` in `webhook_listener.rs`.

**Interfaces:**
- Consumes: `WebhookRecord` (from `webhook.rs`) — read its current fields first via `grep -n "pub struct WebhookRecord" -A 20 crates/aivyx-channel/src/webhook.rs` and `grep -n "fn create_webhook\|fn get_webhook" crates/aivyx-channel/src/webhook.rs`.
- Produces: `handle_request` now requires a valid `Authorization: Bearer <secret>` header (constant-time compared) matching the looked-up record's `secret` before calling `fire_webhook`. A request with a missing/wrong header returns `401 Unauthorized` with body `{"error":"unauthorized"}`, without ever calling `dispatch.fire(...)`.

- [ ] **Step 1: Read the current webhook record shape and creation path**

```bash
grep -n "pub struct WebhookRecord" -A 20 crates/aivyx-channel/src/webhook.rs
grep -n "pub async fn create_webhook\|pub async fn get_webhook\|pub async fn update_webhook" crates/aivyx-channel/src/webhook.rs
```

- [ ] **Step 2: Add a `secret` field to `WebhookRecord`, generated at creation**

In `webhook.rs`, add `pub secret: String` to `WebhookRecord`. In `create_webhook` (or wherever a new record is constructed — likely the CLI command that adds a webhook), generate it with:

```rust
use rand::RngCore;
fn generate_webhook_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    // hex, not base64 — no padding/URL-unsafe chars to worry about in a Bearer header
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
```

(If `rand` is not already a dependency of `aivyx-channel`, check `Cargo.toml` first — `aivyx-crypto` likely already depends on it and can be used instead; do not add a new dependency if an existing one covers this.)

Print the secret to the operator exactly once, at creation time, in whatever CLI output already reports the new webhook's ID/URL (find that print site via `grep -rn "webhook_id" crates/aivyx-cli/src/bin/aivyx_modules/`) — e.g. append a line: `Secret (save this, shown once): {secret}\nInclude it as: Authorization: Bearer {secret}`.

- [ ] **Step 3: Write the failing test**

```rust
// in webhook_listener.rs's #[cfg(test)] mod tests
#[tokio::test]
async fn fire_webhook_without_bearer_token_is_rejected() {
    // Build a DomainHandle-backed store with one enabled webhook record
    // (reuse whatever test helper this file/webhook.rs already has for
    // constructing an in-memory store — grep `fn test_store\|InMemory`
    // in webhook.rs's own test module first and reuse it rather than
    // inventing a second helper).
    let store = test_store().await;
    let secret = "test-secret-abc123".to_string();
    webhook::create_webhook_for_test(&store, "wh1", "do the thing", secret.clone()).await;

    let dispatch = TriggerDispatch::for_test(); // reuse existing test constructor if present

    let req = Request::builder()
        .method("POST")
        .uri("/trigger/wh1")
        .body(Incoming::default()) // adapt to whatever the real handler signature needs in tests
        .unwrap();

    let resp = handle_request(req, &dispatch, &store).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn fire_webhook_with_correct_bearer_token_succeeds() {
    // same setup, but attach `Authorization: Bearer test-secret-abc123`
    // and assert StatusCode::ACCEPTED
}
```

Adapt the exact request-construction calls to match `handle_request`'s real signature and whatever test scaffolding `webhook.rs`'s own tests already use (read `crates/aivyx-channel/src/webhook.rs`'s `#[cfg(test)]` module before writing this — do not guess the store-construction helper name).

- [ ] **Step 4: Run test to verify it fails**

```bash
cargo test -p aivyx-channel fire_webhook_without_bearer_token_is_rejected -- --nocapture
```
Expected: FAIL (no auth check exists yet, so an unauthenticated request currently returns 202/404, not 401).

- [ ] **Step 5: Implement the auth check in `handle_request`**

```rust
// In handle_request, after looking up webhook_id but before calling fire_webhook:
if let Some(webhook_id) = path.strip_prefix("/trigger/") {
    if webhook_id.is_empty() { /* unchanged */ }
    if method != hyper::Method::POST { /* unchanged */ }

    let record = match webhook::get_webhook(store, webhook_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return Ok(json_response(StatusCode::NOT_FOUND, r#"{"error":"webhook not found"}"#)),
        Err(e) => {
            eprintln!("aivyx-pa webhook: storage error looking up {webhook_id:?}: {e}");
            return Ok(json_response(StatusCode::INTERNAL_SERVER_ERROR, r#"{"error":"internal error"}"#));
        }
    };

    if !authorized(&req, &record.secret) {
        return Ok(json_response(StatusCode::UNAUTHORIZED, r#"{"error":"unauthorized"}"#));
    }

    return Ok(fire_webhook_record(dispatch, store, record).await);
}
```

Add the constant-time comparison helper (do not use `==` on secrets — that's a timing side channel):

```rust
fn authorized(req: &Request<Incoming>, expected_secret: &str) -> bool {
    let Some(header) = req.headers().get(hyper::header::AUTHORIZATION) else {
        return false;
    };
    let Ok(header_str) = header.to_str() else {
        return false;
    };
    let Some(token) = header_str.strip_prefix("Bearer ") else {
        return false;
    };
    // Constant-time compare — same reasoning as web_ui.rs's ct_eq for the
    // web-UI auth token (grep that function and reuse it if it's already
    // exported from aivyx-channel; otherwise inline the same byte-by-byte
    // XOR-accumulate pattern here).
    ct_eq(token.as_bytes(), expected_secret.as_bytes())
}
```

Check first whether `crates/aivyx-channel/src/web_ui.rs` already exports a `ct_eq` (the audit report cited one at `web_ui.rs:274-283`) — if so, import and reuse it rather than duplicating the constant-time-compare logic. You will need to refactor `fire_webhook` into a version that takes an already-fetched `WebhookRecord` (`fire_webhook_record`) since the lookup now has to happen before the auth check rather than inside the old `fire_webhook`.

- [ ] **Step 6: Run test to verify it passes**

```bash
cargo test -p aivyx-channel fire_webhook -- --nocapture
```
Expected: both new tests PASS.

- [ ] **Step 7: Run full crate test suite + clippy**

```bash
cargo test -p aivyx-channel
cargo clippy -p aivyx-channel --all-targets -- -D warnings
```

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "fix: require a per-webhook bearer secret before firing (HIGH)

webhook_listener.rs checked only the URL path and HTTP method — any
local process, or any webpage the operator visited (CORS simple
request, no preflight), could fire an agent turn against a leaked or
guessed webhook ID. Every webhook now gets a random 32-byte secret at
creation time, shown once, required as \`Authorization: Bearer <secret>\`
on every trigger, compared in constant time.

Found by a full ecosystem security audit, 2026-09-16."
```

---

### Task 2: Webhook-triggered turns must run at `Untrusted`, not `Trusted` (HIGH)

**Finding:** `crates/aivyx-channel/src/trigger.rs`'s `fire()` always calls `(self.channel_factory)(FrontendType::Local)` regardless of `source`, and `LocalChannel::trust_tier()` (`local.rs:126-131`) is hardcoded `TrustTier::Trusted`. `THREAT_MODEL.md` explicitly states webhook requests run `Untrusted` by default — the code contradicts this. Chained with Task 1's gap, this used to mean an unauthenticated request got the *highest* capability ceiling; Task 1 closes the "unauthenticated" half, this task closes the "highest ceiling" half so a compromised/leaked secret still only grants a near-empty ceiling.

**Files:**
- Modify: `crates/aivyx-channel/src/local.rs` (add a small delegating wrapper — do not change `LocalChannel` itself; every other caller must keep getting `Trusted`).
- Modify: `crates/aivyx-channel/src/trigger.rs` (`fire()`, the `let channel = (self.channel_factory)(FrontendType::Local);` line — find current line number via `grep -n "channel_factory)(FrontendType::Local)" crates/aivyx-channel/src/trigger.rs`).
- Test: `crates/aivyx-channel/src/trigger.rs`'s own `#[cfg(test)]` module, or a new one if `fire()` isn't already covered there.

**Interfaces:**
- Consumes: `aivyx_core::ChannelContext` (7 required methods: `channel_name`, `platform`, `trust_tier`, `session_id`, `stream_event`, `finalize`, `cancellation_token`; plus default methods `session_partition -> Option<String>` and one more cancellation-related default — run `grep -n "fn session_partition\|fn reset_cancellation\|default_method\|trait ChannelContext" -A 60 crates/aivyx-core/src/lib.rs | grep -n "fn "` to get the exhaustive list before writing the wrapper, since a default method your wrapper doesn't override will silently use the *trait's* default rather than delegating — check whether that default matches what you want for each one).
- Produces: `trigger.rs::fire()` uses an `Untrusted`-tier channel specifically when `source == TriggerSource::Webhook`; every other trigger source (`Cron`, `FileWatch`, `Reflection`, `Loop`, `Mission`) is unaffected and keeps its current `Trusted` tier via the unmodified `channel_factory` path.

- [ ] **Step 1: Get the exhaustive `ChannelContext` trait method list**

```bash
sed -n '/pub trait ChannelContext/,/^}/p' crates/aivyx-core/src/lib.rs
```
Read the full output — note every required method (no default body) and every method with a default body, and what each default returns.

- [ ] **Step 2: Add a trust-tier-overriding wrapper in `local.rs`**

```rust
// local.rs, after the existing LocalChannel impl block

/// Wraps any `ChannelContext` and forces `trust_tier()` to a fixed value,
/// delegating every other method to the inner channel unchanged. Used to
/// downgrade a trigger source (webhooks) that would otherwise construct
/// a full-trust channel via the shared `channel_factory`, without
/// touching that channel type's own trust semantics for its normal
/// callers.
pub struct TierOverride<C> {
    inner: C,
    tier: aivyx_capability::TrustTier,
}

impl<C> TierOverride<C> {
    pub fn new(inner: C, tier: aivyx_capability::TrustTier) -> Self {
        TierOverride { inner, tier }
    }
}

#[async_trait]
impl<C: ChannelContext> ChannelContext for TierOverride<C> {
    fn channel_name(&self) -> &str {
        self.inner.channel_name()
    }
    fn platform(&self) -> ChannelPlatform {
        self.inner.platform()
    }
    fn trust_tier(&self) -> aivyx_capability::TrustTier {
        self.tier
    }
    fn session_id(&self) -> SessionId {
        self.inner.session_id()
    }
    async fn stream_event(&self, event: StreamEvent<'_>) -> Result<(), ChannelError> {
        self.inner.stream_event(event).await
    }
    async fn finalize(&self, outcome: &TurnOutcome) -> Result<(), ChannelError> {
        self.inner.finalize(outcome).await
    }
    fn cancellation_token(&self) -> CancellationToken {
        self.inner.cancellation_token()
    }
    // Add explicit delegation for every default-body method you found in
    // Step 1 (e.g. session_partition) so this wrapper is transparent
    // rather than silently reverting to the trait's own default:
    fn session_partition(&self) -> Option<String> {
        self.inner.session_partition()
    }
    // ... delegate any other default method found in Step 1 the same way.
}
```

- [ ] **Step 3: Write the failing test**

```rust
// trigger.rs test module
#[tokio::test]
async fn webhook_trigger_runs_at_untrusted_tier() {
    // Build a TriggerDispatch with a channel_factory that would normally
    // return Trusted for FrontendType::Local (mirror whatever test
    // constructor trigger.rs's existing tests already use — grep
    // `fn test_dispatch\|TriggerDispatch::new` in this file's own test
    // module first).
    let captured_tier = Arc::new(Mutex::new(None));
    let captured_tier_clone = Arc::clone(&captured_tier);
    let dispatch = /* existing test constructor, with a channel_factory
        wired to a fake ChannelContext whose stream_event/finalize record
        the tier it was constructed with into captured_tier_clone */;

    dispatch.fire(TriggerSource::Webhook, "wh1", "do it", false, &[], NotifyWhen::Always).await;

    assert_eq!(*captured_tier.lock().unwrap(), Some(TrustTier::Untrusted));
}

#[tokio::test]
async fn cron_trigger_still_runs_at_trusted_tier() {
    // Same shape, TriggerSource::Cron, assert Some(TrustTier::Trusted) —
    // proves the fix is scoped to Webhook only.
}
```

Adapt to whatever fake-channel/test-dispatch scaffolding already exists in this file — read the current `#[cfg(test)]` module fully before writing these.

- [ ] **Step 4: Run test to verify it fails**

```bash
cargo test -p aivyx-channel webhook_trigger_runs_at_untrusted_tier -- --nocapture
```
Expected: FAIL (currently always `Trusted`).

- [ ] **Step 5: Implement the branch in `fire()`**

```rust
let channel: Arc<dyn ChannelContext> = if source == TriggerSource::Webhook {
    Arc::new(TierOverride::new(
        (self.channel_factory)(FrontendType::Local),
        TrustTier::Untrusted,
    ))
} else {
    (self.channel_factory)(FrontendType::Local)
};
```

(Adjust to match whatever the real binding/type annotation on the existing `let channel = ...` line is — `TierOverride::new` takes the *dereferenced* inner channel if `channel_factory` returns `Arc<dyn ChannelContext>` rather than a concrete type; if so, wrap as `TierOverride::new(inner_arc, tier)` where `TierOverride`'s generic `C` is `Arc<dyn ChannelContext>` — `Arc<dyn ChannelContext>` itself implements `ChannelContext` via blanket impl if one exists in `aivyx-core`; check `grep -n "impl.*ChannelContext for Arc" crates/aivyx-core/src/lib.rs` first. If no such blanket impl exists, `TierOverride<C>`'s bound should be `C: Deref<Target = dyn ChannelContext>` or simply store `Arc<dyn ChannelContext>` directly as a concrete field type instead of a generic — simpler, and sufficient since there's only one call site.)

- [ ] **Step 6: Run test to verify it passes**

```bash
cargo test -p aivyx-channel webhook_trigger_runs_at_untrusted_tier cron_trigger_still_runs_at_trusted_tier -- --nocapture
```
Expected: both PASS.

- [ ] **Step 7: Run full crate suite + clippy**

```bash
cargo test -p aivyx-channel
cargo clippy -p aivyx-channel --all-targets -- -D warnings
```

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "fix: run webhook-triggered turns at Untrusted, not Trusted (HIGH)

trigger.rs::fire() always requested FrontendType::Local regardless of
trigger source, and LocalChannel::trust_tier() is hardcoded Trusted —
so a webhook-fired turn ran at the highest capability ceiling, directly
contradicting THREAT_MODEL.md's explicit 'webhooks are Untrusted by
default' claim. Added a small trust-tier-overriding channel wrapper,
scoped to TriggerSource::Webhook only; every other trigger source
(cron, file-watch, reflection, loop, mission) is unaffected.

Found by a full ecosystem security audit, 2026-09-16."
```

---

### Task 3: Harden Rampart against IPv4-mapped IPv6 and host-parser divergence (HIGH)

**Finding:** `crates/aivyx-core/src/egress.rs`'s `is_blocked_ip` only checks `IpAddr::V6::is_loopback()` (which is `::1` *only*) plus unspecified/ULA/link-local — `::ffff:169.254.169.254`, `::ffff:127.0.0.1`, and `::ffff:10.0.0.5` all pass through unblocked. Separately, `host_of` is a hand-rolled parser that disagrees with the WHATWG-compliant `url` crate `reqwest` actually uses: a backslash in the authority (`http://169.254.169.254\@example.com/`) makes `host_of` see `example.com` (via its own `rsplit('@')`) while `reqwest` connects to the IP literal; and non-dotted-decimal IPv4 (`http://2130706433:7843/`) isn't recognized as an IP by `host_l.parse::<IpAddr>()` but IS normalized to `127.0.0.1` by `reqwest`'s real parser.

**Files:**
- Modify: `crates/aivyx-core/src/egress.rs:128-176` (`host_of`, `is_blocked_ip`).
- Modify: `crates/aivyx-core/Cargo.toml` if `url` isn't already a dependency (check first — it likely already is transitively via `reqwest`; add it as a direct dependency if not, at whatever version `reqwest` already pins, checkable via `cargo tree -p aivyx-core -i url`).
- Test: `egress.rs`'s existing `#[cfg(test)] mod tests` (already has 7 tests — add to it).

**Interfaces:**
- Consumes: `url::Url::parse` and `url::Host`.
- Produces: `host_of(url: &str) -> Option<String>` — same signature, but now delegates parsing to `url::Url` instead of hand-rolled string splitting, so it agrees with what `reqwest` will actually connect to. `is_blocked_ip` additionally rejects IPv4-mapped IPv6 addresses by normalizing them to their embedded IPv4 form first.

- [ ] **Step 1: Confirm `url` crate availability**

```bash
cargo tree -p aivyx-core -i url 2>&1 | head -5
grep -n "^url" crates/aivyx-core/Cargo.toml
```
If `url` isn't a direct dependency, add `url = "2"` (or whatever version `cargo tree` shows resolving) to `crates/aivyx-core/Cargo.toml`'s `[dependencies]`.

- [ ] **Step 2: Write the failing tests**

Add to `egress.rs`'s existing test module:

```rust
#[test]
fn blocks_ipv4_mapped_ipv6_metadata_and_loopback() {
    for ip_str in ["::ffff:169.254.169.254", "::ffff:127.0.0.1", "::ffff:10.0.0.5"] {
        let ip: IpAddr = ip_str.parse().unwrap();
        assert!(is_blocked_ip(&ip), "should block IPv4-mapped {ip_str}");
    }
}

#[test]
fn host_of_agrees_with_url_crate_on_backslash_authority() {
    // A backslash after the host is treated as a path separator by the
    // WHATWG/url-crate parser reqwest actually uses — host_of must see
    // the SAME host reqwest will connect to, not whatever comes after
    // an rsplit('@').
    let parsed = host_of("http://169.254.169.254\\@example.com/").unwrap();
    assert_eq!(parsed, "169.254.169.254", "must match what reqwest actually connects to");
}

#[test]
fn host_of_normalizes_non_dotted_decimal_ipv4() {
    // 2130706433 is 127.0.0.1 as a big-endian u32 — the url crate's
    // real IPv4 parser normalizes this; host_of must match, so
    // is_blocked_ip sees a real IP literal instead of failing to parse
    // and falling through as an unrecognized hostname.
    let parsed = host_of("http://2130706433:7843/").unwrap();
    assert_eq!(parsed, "127.0.0.1");
}

#[test]
fn classify_blocks_the_two_parser_divergence_urls() {
    let p = EgressPolicy::default();
    assert!(p.classify("http://169.254.169.254\\@example.com/").is_some());
    assert!(p.classify("http://2130706433:7843/").is_some());
}
```

- [ ] **Step 3: Run tests to verify they fail**

```bash
cargo test -p aivyx-core --lib egress:: -- --nocapture
```
Expected: `blocks_ipv4_mapped_ipv6_metadata_and_loopback`, `host_of_agrees_with_url_crate_on_backslash_authority`, `host_of_normalizes_non_dotted_decimal_ipv4`, and `classify_blocks_the_two_parser_divergence_urls` all FAIL.

- [ ] **Step 4: Rewrite `host_of` to delegate to `url::Url`**

```rust
fn host_of(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    match parsed.host()? {
        url::Host::Domain(d) => Some(d.to_string()),
        url::Host::Ipv4(ip) => Some(ip.to_string()),
        url::Host::Ipv6(ip) => Some(ip.to_string()),
    }
}
```

This single change fixes both the backslash-authority divergence and the non-dotted-decimal-IPv4 divergence, because `url::Url::parse` implements the same WHATWG algorithm `reqwest` uses internally — there are no longer two independent parsers to disagree.

- [ ] **Step 5: Fix `is_blocked_ip` to catch IPv4-mapped IPv6**

```rust
pub(crate) fn is_blocked_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
        }
        IpAddr::V6(v6) => {
            // An IPv4-mapped address (::ffff:a.b.c.d) must be judged by
            // its embedded IPv4 rules, not by the IPv6 loopback/ULA/
            // link-local checks alone — ::ffff:169.254.169.254 is NOT
            // ::1 and is NOT in fc00::/7 or fe80::/10, but it IS the
            // cloud-metadata address once unwrapped.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_blocked_ip(&IpAddr::V4(v4));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}
```

- [ ] **Step 6: Run tests to verify they pass**

```bash
cargo test -p aivyx-core --lib egress:: -- --nocapture
```
Expected: all pass, including the 7 pre-existing tests (re-run the full module, not just the new tests, since `host_of`'s rewrite could regress the existing `host_extraction_handles_userinfo_ports_ipv6` test — if it fails, check that `url::Url::parse` strips IPv6 brackets and userinfo the same way the old code asserted; the `url` crate's `.host()` already returns the bracket-free host, so this should hold, but verify).

- [ ] **Step 7: Run full crate suite + clippy**

```bash
cargo test -p aivyx-core
cargo clippy -p aivyx-core --all-targets -- -D warnings
```

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "fix: close two Rampart (SSRF guard) bypasses (HIGH)

is_blocked_ip's IPv6 branch never checked for IPv4-mapped addresses, so
::ffff:169.254.169.254 / ::ffff:127.0.0.1 / ::ffff:10.0.0.5 all passed
through unblocked. Separately, host_of was a hand-rolled parser that
disagreed with the WHATWG-compliant url crate reqwest actually uses —
a backslash in the authority, or a non-dotted-decimal IPv4 literal,
made Rampart see a different host than the one reqwest would connect
to. Rewrote host_of to delegate to url::Url (the same parser reqwest
uses internally, closing both divergences at once) and taught
is_blocked_ip to unwrap IPv4-mapped IPv6 addresses before classifying.

Found by a full ecosystem security audit, 2026-09-16."
```

---

### Task 4: Withhold third-party integration capability bases from the backcompat floor, and give destructive integration actions a real confirm gate (HIGH)

**Finding:** `aivyx-core/src/lib.rs`'s `Tool::auto_grantable_in_backcompat_floor` trait doc explicitly says third-party/OAuth integrations "stay withheld unless a maintainer has explicitly reviewed the base and opted it in" — but `aivyx-tool/src/proxy.rs:132-134`'s `ToolProxy::auto_grantable_in_backcompat_floor()` unconditionally returns `true` for *every* tool-process tool, which is *every* Gmail/Drive/Notion/Obsidian/N8N/Contacts/Calendar tool. Combined with zero pre-execution confirmation gate anywhere in the tool-process dispatch path, a default install that runs `aivyx-pa connect gmail` ends up with unqualified `email.send` auto-granted and zero confirmation before it fires.

**Files:**
- Modify: `crates/aivyx-tool/src/proxy.rs:132-134`.
- Modify: `crates/aivyx-tool/src/multi_harness.rs` (the `ToolOutcome::RequiresEscalation` → `ToolError` flattening — find via `grep -n "RequiresEscalation" crates/aivyx-tool/src/multi_harness.rs`).
- Modify: `crates/aivyx-tool/src/proxy.rs` (the `InvocationOutcome::ToolError` → `ToolOutcome::Failed` mapping at the end of `execute` — this is the other half of the same flattening, on the daemon side).
- Read first, do not modify unless the design below requires it: `crates/aivyx-core/src/agent.rs`'s handling of `ToolOutcome::RequiresEscalation` / `TurnOutcome::Escalated` (`grep -n "RequiresEscalation\|Escalated" crates/aivyx-core/src/agent.rs`) — this is the existing escalation mechanism the fix must plug into, not reinvent.
- Test: `crates/aivyx-tool/src/proxy.rs` and `multi_harness.rs`'s own `#[cfg(test)]` modules.

**Interfaces:**
- Consumes: the existing `ToolOutcome::RequiresEscalation` variant (already defined in `aivyx-core`) and the existing `[access] confirm_destructive` config knob (already read by `aivyx-core/src/tools/fs.rs` and `git.rs` — reuse the same config field, don't add a new one).
- Produces: (a) `auto_grantable_in_backcompat_floor` returns `false` for any tool whose declared scope base is one of the "third-party integration write/send/delete" bases (build this list from the real scope bases you find via `grep -rn "Scope::parse" crates/aivyx-gmail crates/aivyx-drive crates/aivyx-notion crates/aivyx-obsidian crates/aivyx-n8n crates/aivyx-contacts crates/aivyx-calendar --include="*.rs" | grep -v test | grep -oP '"\K[a-z_]+\.(write|send)[a-z_.]*(?=")' | sort -u` — do not guess the list, derive it from real code); (b) a `ToolOutcome::RequiresEscalation` returned by an out-of-process tool now correctly propagates as `TurnOutcome::Escalated` all the way through the bridge, instead of being flattened into a generic `Failed`.

- [ ] **Step 1: Derive the real write/send/delete scope base list from source**

```bash
grep -rn "Scope::parse" crates/aivyx-gmail crates/aivyx-drive crates/aivyx-notion crates/aivyx-obsidian crates/aivyx-n8n crates/aivyx-contacts crates/aivyx-calendar --include="*.rs" | grep -v "#\[cfg(test)\]" | grep -v "mod tests"
```
Read every hit and list every distinct base that represents a write/send/delete/archive action (e.g. `email.send`, `drive.write`, `notion.write`, `obsidian.write`, `n8n.write`, `contacts.write`, `calendar.write` — confirm the exact literal strings from what you actually find; do not assume these are the only ones).

- [ ] **Step 2: Write the failing test for the floor-grant fix**

In `crates/aivyx-tool/src/proxy.rs`'s test module:

```rust
#[test]
fn write_scoped_tool_proxy_is_not_auto_grantable_in_backcompat_floor() {
    // Construct a ToolProxy with required_scope_str = "email.send" using
    // whatever test-construction helper this file's own tests already
    // use (check for a `fn test_proxy(...)` or similar first).
    let proxy = test_proxy_with_scope("email.send");
    assert!(!proxy.auto_grantable_in_backcompat_floor());
}

#[test]
fn read_scoped_tool_proxy_is_still_auto_grantable_in_backcompat_floor() {
    let proxy = test_proxy_with_scope("email.read");
    assert!(proxy.auto_grantable_in_backcompat_floor());
}
```

- [ ] **Step 3: Run to verify it fails**

```bash
cargo test -p aivyx-tool write_scoped_tool_proxy_is_not_auto_grantable -- --nocapture
```
Expected: FAIL (currently always `true`).

- [ ] **Step 4: Implement the scope-aware floor gate**

```rust
// proxy.rs — replace the unconditional `true`
fn auto_grantable_in_backcompat_floor(&self) -> bool {
    // Third-party/OAuth integration write/send/delete bases stay
    // withheld from the backcompat floor per aivyx-core's own trait
    // doc contract — an operator must explicitly grant these in the
    // role's own capability_scopes, not get them for free just by
    // running `aivyx-pa connect <service>`.
    const WITHHELD_BASES: &[&str] = &[
        // Fill in with the exact bases derived in Step 1 — do not
        // shorten this list from what you actually found in source.
    ];
    let base = self.required_scope.base(); // check the real method name on Scope via `grep -n "fn base" crates/aivyx-capability/src/lib.rs`
    !WITHHELD_BASES.contains(&base)
}
```

- [ ] **Step 5: Run to verify it passes**

```bash
cargo test -p aivyx-tool auto_grantable -- --nocapture
```

- [ ] **Step 6: Write the failing test for escalation propagation**

In `multi_harness.rs`'s test module:

```rust
#[tokio::test]
async fn requires_escalation_from_a_tool_process_reaches_the_daemon_as_escalation_not_a_generic_failure() {
    // Construct a fake tool-process handler that returns
    // ToolOutcome::RequiresEscalation{..} (check the exact variant
    // shape via `grep -n "RequiresEscalation" crates/aivyx-core/src/lib.rs`)
    // and drive it through multi_harness's real dispatch path, then
    // through ToolProxy::execute, and assert the final ToolOutcome the
    // daemon sees is RequiresEscalation, not Failed.
}
```

- [ ] **Step 7: Run to verify it fails**

```bash
cargo test -p aivyx-tool requires_escalation_from_a_tool_process -- --nocapture
```
Expected: FAIL (currently flattened to `ToolError`/`Failed`).

- [ ] **Step 8: Add a wire-level escalation variant and stop flattening it**

Read `crates/aivyx-tool/src/wire.rs`'s `ToolToDaemon` enum (`grep -n "enum ToolToDaemon" -A 20 crates/aivyx-tool/src/wire.rs`) — add a new variant, e.g. `RequiresEscalation { call_id: String, reason: String }`, alongside the existing `ToolError`. Update:
- The tool-process side (`multi_harness.rs`, wherever it currently maps `ToolOutcome::RequiresEscalation` into `ToolError { code: "requires_escalation", .. }`) to emit the new `ToolToDaemon::RequiresEscalation` variant instead.
- The daemon side (`proxy.rs`'s `InvocationOutcome` enum and the `match outcome` block at the end of `execute`) to add a matching `InvocationOutcome::RequiresEscalation` arm that maps to `ToolOutcome::RequiresEscalation { .. }` (matching whatever fields that real variant needs — check `agent.rs`'s handling of it from Step 0's earlier grep to get the exact shape right).

- [ ] **Step 9: Run to verify it passes**

```bash
cargo test -p aivyx-tool
```

- [ ] **Step 10: Add `confirm_destructive` gating for the withheld write bases, so an operator who explicitly grants one of them still gets a per-call confirm**

Read how `aivyx-core/src/tools/fs.rs` currently implements this (`grep -n "confirm_destructive\|is_confirmed" crates/aivyx-core/src/tools/fs.rs`) — the pattern there is the template. For tool-process tools specifically, since the tool process itself can't be trusted to self-check a daemon-side config knob, add the check on the **daemon side**, before `ToolProxy::execute` is ever called: in the same turn-loop dispatch site that already checks the capability grant (`agent.rs`, the code the Task-1-of-the-audit's "turn loop" report already traced as `run_tool_call`), add: if the tool's `required_scope(&input).base()` is in the same `WITHHELD_BASES`-style list AND `[access] confirm_destructive` is on AND the input has no operator-side confirmation already recorded, return `ToolOutcome::RequiresEscalation` before calling `tool.execute` at all — mirroring the existing `fs.rs`/`git.rs` pattern rather than inventing a new one. This is the highest-precision part of this task; read `agent.rs`'s current dispatch code in full before writing it (`sed -n '1400,1460p' crates/aivyx-core/src/agent.rs` as a starting point, adjust to wherever the capability check currently sits per Task 1 of the earlier audit's turn-loop report).

- [ ] **Step 11: Write a test proving a granted-but-destructive scope still requires confirmation**

```rust
// agent.rs or wherever the dispatch test module lives
#[tokio::test]
async fn granted_email_send_scope_still_requires_confirmation_when_confirm_destructive_is_on() {
    // Build an agent whose capability set explicitly includes
    // "email.send" (bypassing the floor-grant question entirely — this
    // test is about the confirm gate, not the floor), with
    // confirm_destructive enabled, dispatch a call to a proxy tool
    // whose required_scope is "email.send", and assert the turn outcome
    // is Escalated, not Completed.
}
```

- [ ] **Step 12: Run full test suite for both crates + clippy**

```bash
cargo test -p aivyx-tool -p aivyx-core -p aivyx-capability
cargo clippy -p aivyx-tool -p aivyx-core --all-targets -- -D warnings
```

- [ ] **Step 13: Commit**

```bash
git add -A
git commit -m "fix: withhold destructive integration scopes from the backcompat floor; make escalation real for tool-process tools (HIGH)

ToolProxy::auto_grantable_in_backcompat_floor() unconditionally
returned true for every tool-process tool, directly contradicting the
Tool trait's own doc contract that third-party/OAuth integrations stay
withheld unless explicitly opted in. A default 'aivyx-pa connect
gmail' therefore auto-granted unqualified email.send with zero
confirmation. Fixed the floor gate to withhold write/send/delete bases
by default, and closed the second half of the same gap: a tool
process's ToolOutcome::RequiresEscalation was being flattened into a
generic ToolError/Failed before it ever reached the turn loop's real
escalation handling, so even an explicitly-granted destructive scope
had no way to pause for operator confirmation. Added a wire-level
RequiresEscalation variant and a confirm_destructive check (reusing the
existing fs.rs/git.rs pattern) before dispatching to any tool whose
scope is in the withheld list.

Found by a full ecosystem security audit, 2026-09-16."
```

---

### Task 5: Add real permission enforcement to non-Google integration secret files (scoped down from the audit's broader plaintext-OAuth-storage finding)

**Finding:** `crates/aivyx-auth-cli/src/config_file.rs`'s `load_toml` reads `~/.aivyx-pa/tool-processes/<svc>/config.toml` (holding the Notion token, n8n API key, and Google `client_secret`) with no `0600` enforcement at all — unlike `tokens.json`, which already gets it via `write_secure`. (The audit's larger finding — that no integration uses `aivyx-storage`'s encrypted vault at all — is a real architectural gap, but building an IPC-reachable credential vault for out-of-process tools is substantial new infrastructure, explicitly out of scope for this fix-only plan; this task closes the cheaper, still-real permission gap on the file that already exists.)

**Files:**
- Read: `crates/aivyx-google-oauth/src/storage.rs:160-199` (`write_secure` — the pattern to reuse, do not reinvent it).
- Modify: `crates/aivyx-auth-cli/src/config_file.rs` (wherever `config.toml` is written — find via `grep -n "fn save\|fn write" crates/aivyx-auth-cli/src/config_file.rs`; if this file only reads and never writes, find the real write site via `grep -rln "config.toml" crates/aivyx-gmail crates/aivyx-notion crates/aivyx-n8n --include="*.rs"`).
- Test: wherever the write function's existing tests live.

**Interfaces:**
- Consumes: the same `write_secure`-style atomic-0600-write pattern already proven in `aivyx-google-oauth/src/storage.rs`.
- Produces: every `config.toml` write for these integrations now creates the file at `0600` atomically (no window at a wider mode), matching `tokens.json`'s existing guarantee.

- [ ] **Step 1: Find every real write site for these config files**

```bash
grep -rn "config.toml\|fn save_config\|fn write_config" crates/aivyx-auth-cli crates/aivyx-gmail crates/aivyx-notion crates/aivyx-n8n --include="*.rs" | grep -v test
```

- [ ] **Step 2: Read the `write_secure` pattern**

```bash
sed -n '160,199p' crates/aivyx-google-oauth/src/storage.rs
```

- [ ] **Step 3: Write a failing test per write site**

For each site found in Step 1, add a test in that module asserting the written file's mode is `0o600`:

```rust
#[cfg(unix)]
#[test]
fn config_file_is_written_with_0600_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    save_config(&path, &SomeConfig::default()).unwrap(); // adapt to real function name/signature
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}
```

- [ ] **Step 4: Run to verify it fails**

```bash
cargo test -p aivyx-auth-cli config_file_is_written_with_0600 -- --nocapture
```

- [ ] **Step 5: Extract `write_secure` into a shared location and use it at every site**

If `write_secure` is currently private to `aivyx-google-oauth`, either make it `pub` and add `aivyx-google-oauth` as a dependency of `aivyx-auth-cli` (check whether that creates a dependency cycle first — `aivyx-google-oauth` likely doesn't depend on `aivyx-auth-cli`, so this direction should be safe), or copy the same 3-line pattern (`OpenOptions::new().mode(0o600).write(true).create(true)`, or the atomic tmp+rename variant if that's what the real function does — read it exactly in Step 2 rather than assuming) directly into `aivyx-auth-cli/src/config_file.rs` if pulling in a new cross-crate dependency is judged too heavy for this fix — prefer reuse if the dependency direction is clean.

- [ ] **Step 6: Run to verify it passes**

```bash
cargo test -p aivyx-auth-cli
```

- [ ] **Step 7: Run full suite + clippy for every affected crate**

```bash
cargo test -p aivyx-auth-cli -p aivyx-gmail -p aivyx-notion -p aivyx-n8n
cargo clippy -p aivyx-auth-cli -p aivyx-gmail -p aivyx-notion -p aivyx-n8n --all-targets -- -D warnings
```

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "fix: write integration config.toml files at 0600 (MEDIUM, scoped down from a HIGH finding)

tokens.json already gets atomic 0600 writes via write_secure; the
adjacent config.toml (holding the Notion token, n8n API key, and
Google client_secret) got no permission enforcement at all. Reused the
existing write_secure pattern at every config.toml write site.

The audit's broader finding that no integration uses aivyx-storage's
encrypted vault at all is a real architectural gap, but building an
IPC-reachable credential vault for out-of-process tools is substantial
new infrastructure — explicitly out of scope for this fix, logged as a
follow-on.

Found by a full ecosystem security audit, 2026-09-16."
```

---

### Task 6: Validate URLs before storing/probing in `toolkit.health_check` (MEDIUM-HIGH)

**Finding:** `crates/aivyx-toolkit/src/tools/health_check.rs`'s `parse_add_input` does zero validation on the `url` field — no scheme check, no Rampart call — and the watcher persists, probing on an operator-chosen interval from the toolkit tool-process subprocess, which Rampart (daemon-side) structurally cannot reach.

**Files:**
- Modify: `crates/aivyx-toolkit/src/tools/health_check.rs` (`parse_add_input`).
- Modify: `crates/aivyx-toolkit/Cargo.toml` if `aivyx-core` (which owns `EgressPolicy`) isn't already a dependency — check first.
- Test: `health_check.rs`'s own test module.

**Interfaces:**
- Consumes: `aivyx_core::egress::EgressPolicy` — the same struct Rampart already uses for `web.fetch`. Since `EgressPolicy::classify` is `pub`, it can be called directly from `aivyx-toolkit` without going through the daemon.
- Produces: `parse_add_input` rejects any URL `EgressPolicy::default().classify(url)` flags, with the same refusal message shape `web_fetch.rs` already uses.

- [ ] **Step 1: Confirm `EgressPolicy` visibility and check the dependency graph**

```bash
grep -n "pub struct EgressPolicy\|pub fn classify" crates/aivyx-core/src/egress.rs
grep -n "^aivyx-core" crates/aivyx-toolkit/Cargo.toml
```

- [ ] **Step 2: Write the failing test**

```rust
#[test]
fn add_rejects_a_cloud_metadata_url() {
    let err = parse_add_input(&json!({
        "action": "add",
        "url": "http://169.254.169.254/latest/meta-data/iam/security-credentials/",
        "interval_secs": 60
    }));
    assert!(err.is_err());
}

#[test]
fn add_still_accepts_a_public_url() {
    let ok = parse_add_input(&json!({
        "action": "add",
        "url": "https://example.com/status",
        "interval_secs": 60
    }));
    assert!(ok.is_ok());
}
```

(Adapt to `parse_add_input`'s real signature/error type — read the function in full before writing these.)

- [ ] **Step 3: Run to verify the first fails**

```bash
cargo test -p aivyx-toolkit add_rejects_a_cloud_metadata_url -- --nocapture
```

- [ ] **Step 4: Add the check**

```rust
// inside parse_add_input, right after extracting `url`:
if let Some(reason) = aivyx_core::egress::EgressPolicy::default().classify(&url) {
    return Err(/* whatever this function's real error type/constructor is */ format!(
        "refusing to add health-check target: {reason}"
    ).into());
}
```

Note this uses `EgressPolicy::default()` (SSRF-guard on, no host allow-list) rather than plumbing the operator's actual `[access]` config through — that config lives daemon-side and isn't reachable from this tool-process crate without new IPC plumbing. Document this as a known simplification in a code comment: the default policy still closes the metadata/loopback/private-network cases, which is the exploit this fix targets; an operator-configured host allow-list for this specific tool is a smaller follow-on, not required to close the reported vulnerability.

- [ ] **Step 5: Run to verify both pass**

```bash
cargo test -p aivyx-toolkit add_rejects_a_cloud_metadata_url add_still_accepts_a_public_url -- --nocapture
```

- [ ] **Step 6: Run full suite + clippy**

```bash
cargo test -p aivyx-toolkit
cargo clippy -p aivyx-toolkit --all-targets -- -D warnings
```

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "fix: validate toolkit.health_check URLs against Rampart's SSRF policy (MEDIUM-HIGH)

health_check's parse_add_input did zero URL validation — no scheme
check, no egress guard — and the watcher persists, probing on an
operator-chosen interval from a tool-process subprocess Rampart
(daemon-side) cannot reach. Reused EgressPolicy::classify (the same
check web.fetch already applies) at the point of adding a watcher, so
a model-chosen metadata/loopback/private-network URL is refused before
it's ever persisted.

Found by a full ecosystem security audit, 2026-09-16."
```

---

### Task 7: Fix the daemon socket's bind→chmod TOCTOU (MEDIUM)

**Finding:** `crates/aivyx-channel/src/daemon_server.rs:702-715` binds the Unix socket, *then* chmods it to `0600` — no `umask()` call anywhere in the workspace, so under the common default umask `022` the socket is briefly world-accessible.

**Files:**
- Modify: `crates/aivyx-channel/src/daemon_server.rs` (both real bind sites — `run_single_connection_daemon` too, per the audit's citation of a second occurrence around line 3576).
- Test: an integration-style test if `daemon_server.rs` has one already for socket setup; otherwise a targeted unit test around a small extracted helper (see Step 2).

**Interfaces:**
- Produces: a small helper `fn bind_unix_socket_0600(path: &Path) -> Result<UnixListener, DaemonError>` used at both call sites, which sets the process umask to `0o177` immediately before `bind` and restores the prior umask immediately after, so the socket file is created at mode `0600` from the instant it exists — no chmod-after-the-fact race.

- [ ] **Step 1: Read both real bind sites in full**

```bash
grep -n "UnixListener::bind" crates/aivyx-channel/src/daemon_server.rs
```
Read ~15 lines around each hit.

- [ ] **Step 2: Extract and fix the shared bind helper**

```rust
/// Bind a Unix socket at `path` with mode 0600 from the instant it's
/// created — no bind-then-chmod window where the socket is reachable
/// at the process's default umask. Uses `libc::umask` to temporarily
/// tighten the process umask around the bind call and restores it
/// immediately after, since `UnixListener::bind` provides no direct
/// mode parameter.
#[cfg(unix)]
fn bind_unix_socket_0600(path: &std::path::Path) -> std::io::Result<UnixListener> {
    // SAFETY: umask is a per-process, not per-thread, piece of state
    // with no invariants beyond "returns the previous mask" — this is
    // the same class of libc call this codebase already makes
    // elsewhere (see aivyx-storage's own file-permission handling).
    // The call is bracketed tightly around the one bind() so the
    // narrowed umask can't affect any unrelated file creation
    // happening concurrently on another thread; if that's a real
    // concern here (multi-threaded daemon startup), consider binding
    // in a std::sync::Once-guarded startup path instead — check
    // whether socket bind can race with any other file creation
    // during daemon startup before deciding this is sufficient.
    let old_mask = unsafe { libc::umask(0o177) };
    let result = UnixListener::bind(path);
    unsafe { libc::umask(old_mask) };
    result
}
```

Check whether `libc` is already a dependency of `aivyx-channel` (`grep -n "^libc" crates/aivyx-channel/Cargo.toml`) — it very likely is already, given the codebase's existing Unix-specific code (`std::os::unix::fs::PermissionsExt` is already used at this exact call site). If not present, add it.

- [ ] **Step 3: Write the failing test**

```rust
#[cfg(unix)]
#[test]
fn socket_is_never_observable_at_a_wider_mode_than_0600() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let socket_path = tmp.path().join("test.sock");
    // Set a permissive umask deliberately, to prove the fix doesn't
    // depend on the test environment's own umask happening to be tight.
    let old = unsafe { libc::umask(0o022) };
    let listener = bind_unix_socket_0600(&socket_path).unwrap();
    unsafe { libc::umask(old) };
    let mode = std::fs::metadata(&socket_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    drop(listener);
}
```

- [ ] **Step 4: Run to verify it fails** (this test will fail to compile until `bind_unix_socket_0600` exists — that's expected; write it first per Step 2's code, then confirm the OLD bind-then-chmod code, if tested the same way, would still pass this particular assertion since the chmod does eventually land at 0600 — the real risk is the *window*, which a single-threaded test can't observe directly. State this honestly: this test proves the *final* mode is correct with the new helper; it does not by itself prove the old code was racy (that requires a concurrent-connect-during-bind test, which is higher effort than this fix warrants — the umask-based fix is correct by construction regardless).

```bash
cargo test -p aivyx-channel socket_is_never_observable_at_a_wider_mode -- --nocapture
```

- [ ] **Step 5: Replace both call sites with the helper**

At each site found in Step 1, replace:
```rust
let listener = UnixListener::bind(socket_path).map_err(...)?;
#[cfg(unix)]
{
    let perms = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(socket_path, perms).map_err(...)?;
}
```
with:
```rust
let listener = bind_unix_socket_0600(socket_path).map_err(|e| DaemonError::Bind {
    path: socket_path.display().to_string(),
    source: e,
})?;
```

- [ ] **Step 6: Also harden the parent directory creation at the fallback path**

The audit noted `default_socket_path()`'s `$HOME`-fallback directory (when `XDG_RUNTIME_DIR` is unset) is created via plain `create_dir_all` with no mode restriction. Find the `create_dir_all` call(s) at daemon_server.rs's socket-directory setup (same ~15-line region as Step 1) and set `0700` the same way — either via the same umask-bracket approach or an explicit `set_permissions` immediately after `create_dir_all` (a chmod-after window on a *directory* is lower severity than on the socket itself, since the socket inside it still isn't reachable until it exists at 0600, but tightening it is cheap and closes the last piece of this finding).

- [ ] **Step 7: Run to verify the test passes, then the full suite**

```bash
cargo test -p aivyx-channel socket_is_never_observable_at_a_wider_mode -- --nocapture
cargo test -p aivyx-channel
cargo clippy -p aivyx-channel --all-targets -- -D warnings
```

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "fix: bind the daemon Unix socket at 0600 atomically (MEDIUM)

UnixListener::bind followed by a separate set_permissions(0o600) left
a real window, at the process's default umask (often 022), where the
socket was world-accessible before the chmod landed. Bracket the bind
with a tightened umask instead, so the socket is created at 0600 from
the instant it exists. Also tightened the $HOME-fallback socket
directory to 0700.

Found by a full ecosystem security audit, 2026-09-16."
```

---

### Task 8: Fix the `daemon.env` passphrase TOCTOU + check the OS keyring first (MEDIUM)

**Finding:** `crates/aivyx-cli/src/bin/aivyx_modules/daemon_service.rs:223-225` writes the plaintext passphrase via plain `std::fs::write`, *then* chmods to `0600` — same TOCTOU class as Task 7, but for a file containing the actual master passphrase rather than a socket. Additionally, `resolve_passphrase` (`daemon_service.rs:435-448`) never consults `keyring_store::retrieve()` before falling back to writing this file, unlike the daemon's own startup path.

**Files:**
- Modify: `crates/aivyx-cli/src/bin/aivyx_modules/daemon_service.rs` (the `env_file_path` write at ~line 223, and `resolve_passphrase` at ~line 435).
- Read: `crates/aivyx-toolkit/src/secure_io.rs:66-78` (`write_secure`'s doc comment explicitly says "TOCTOU-safe via O_CREAT with mode" — reuse this exact pattern, don't reinvent it a third time in this codebase).
- Read: `crates/aivyx-cli/src/bin/aivyx.rs`'s `select_passphrase_source` (~line 5120-5150) — the pattern `resolve_passphrase` should be made consistent with.
- Test: `daemon_service.rs`'s own test module.

**Interfaces:**
- Consumes: `aivyx_toolkit::secure_io::write_secure` (check whether it's `pub` and usable from `aivyx-cli` — if `secure_io` is private to `aivyx-toolkit`, either export it or inline the same `OpenOptions::mode(0o600).create_new(true)`-based approach directly).
- Consumes: `aivyx_channel::keyring_store::retrieve` (already used by `select_passphrase_source`).
- Produces: `resolve_passphrase` checks the keyring before prompting/generating a file-backed passphrase; the env file is written with the same atomic-0600 guarantee as `write_secure`.

- [ ] **Step 1: Read all three reference implementations**

```bash
sed -n '66,78p' crates/aivyx-toolkit/src/secure_io.rs
grep -n "fn select_passphrase_source" -A 30 crates/aivyx-cli/src/bin/aivyx.rs
sed -n '200,260p' crates/aivyx-cli/src/bin/aivyx_modules/daemon_service.rs
sed -n '425,460p' crates/aivyx-cli/src/bin/aivyx_modules/daemon_service.rs
```

- [ ] **Step 2: Write the failing tests**

```rust
#[cfg(unix)]
#[test]
fn env_file_is_never_written_at_a_wider_mode_than_0600() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("daemon.env");
    let old = unsafe { libc::umask(0o022) };
    write_env_file_secure(&path, "test-passphrase").unwrap(); // the function you'll create in Step 3
    unsafe { libc::umask(old) };
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[tokio::test]
async fn resolve_passphrase_checks_keyring_before_prompting_or_writing_a_file() {
    // Seed a fake/test keyring entry (reuse whatever test double
    // select_passphrase_source's own tests already use for the keyring
    // — grep `mod tests` in aivyx.rs near select_passphrase_source
    // first) and assert resolve_passphrase returns that value without
    // ever calling the file-write or prompt path.
}
```

- [ ] **Step 3: Run to verify they fail**

```bash
cargo test -p aivyx-cli env_file_is_never_written_at_a_wider_mode resolve_passphrase_checks_keyring -- --nocapture
```

- [ ] **Step 4: Implement the atomic-0600 write, reusing or mirroring `write_secure`**

```rust
fn write_env_file_secure(path: &std::path::Path, passphrase: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true) // fails if it already exists — no window at any mode, ever
        .mode(0o600)
        .open(path)?;
    f.write_all(render_env_file(passphrase).as_bytes())?;
    f.sync_all()
}
```

If the real call site needs to *overwrite* an existing file (re-install case) rather than fail on an existing one, use `write_secure`'s actual pattern instead of `create_new` — read Step 1's output for the real approach (likely a tmp-file-then-atomic-rename, which is also TOCTOU-safe and handles overwrite) and match it exactly rather than inventing a third variant.

- [ ] **Step 5: Replace the existing write + separate chmod with the new function**

Update the call site at `daemon_service.rs:~223-225` to call `write_env_file_secure` instead of `std::fs::write` + `set_permissions`. Also apply the same fix to the macOS launchd plist write at `~362-367` if that code path similarly writes-then-chmods (read it in Step 1's `sed` output to confirm).

- [ ] **Step 6: Implement the keyring-first check in `resolve_passphrase`**

```rust
// resolve_passphrase, before the existing prompt/generate fallback:
if let Ok(Some(secret)) = aivyx_channel::keyring_store::retrieve() {
    return Ok(secret);
}
// ... existing fallback logic unchanged
```

(Match the real `keyring_store::retrieve` return type exactly — check via the Step 1 grep of `select_passphrase_source`, which already calls it, and copy its exact error-handling shape rather than guessing.)

- [ ] **Step 7: Run to verify both pass**

```bash
cargo test -p aivyx-cli env_file_is_never_written_at_a_wider_mode resolve_passphrase_checks_keyring -- --nocapture
```

- [ ] **Step 8: Run full suite + clippy**

```bash
cargo test -p aivyx-cli
cargo clippy -p aivyx-cli --all-targets -- -D warnings
```

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "fix: atomic 0600 daemon.env write; check the OS keyring before writing plaintext (MEDIUM)

daemon_service.rs wrote the plaintext master passphrase then chmod'd
separately, leaving a real window at the process's default umask.
Reused the same atomic-0600 pattern write_secure already established
elsewhere in this codebase. Also fixed resolve_passphrase to check the
OS keyring first, matching the daemon's own startup path
(select_passphrase_source) — previously, running 'daemon install' from
a session that already had the passphrase in the keyring still wrote
it to disk in plaintext.

Found by a full ecosystem security audit, 2026-09-16."
```

---

### Task 9: Fence `shell.exec` and `git.*` output through Bulwark and Picket (MEDIUM)

**Finding:** `crates/aivyx-core/src/tools/shell.rs`'s `shell.exec` and `crates/aivyx-core/src/tools/git.rs`'s `git.status`/`git.diff`/`git.commit` never override `output_is_untrusted()`, so their output (which can be attacker-authored — a `git.diff` against a pulled branch, `curl` output via `shell.exec`) enters model context completely raw and unscanned.

**Files:**
- Modify: `crates/aivyx-core/src/tools/shell.rs` (add `output_is_untrusted` override to the `Tool` impl for the shell-exec tool).
- Modify: `crates/aivyx-core/src/tools/git.rs` (same, for `git.status`/`git.diff`/`git.commit` — check whether `git.log`/other read-oriented git tools have the same gap while you're in this file).
- Test: each file's own test module.

**Interfaces:**
- Produces: `output_is_untrusted(&self) -> bool { true }` on each of these tool impls, matching the pattern already used by `fs.read` (`crates/aivyx-core/src/tools/fs.rs:234` per the audit) and every tool-process proxy.

- [ ] **Step 1: Confirm the exact `impl Tool for` blocks and current absence of the override**

```bash
grep -n "impl Tool for" crates/aivyx-core/src/tools/shell.rs crates/aivyx-core/src/tools/git.rs
grep -n "fn output_is_untrusted" crates/aivyx-core/src/tools/shell.rs crates/aivyx-core/src/tools/git.rs
```
The second command should currently return nothing for these two files — confirms the gap before fixing it.

- [ ] **Step 2: Write the failing tests**

```rust
// shell.rs
#[test]
fn shell_exec_output_is_untrusted() {
    let tool = ShellExecTool::new(/* whatever its real constructor needs */);
    assert!(tool.output_is_untrusted());
}
```

```rust
// git.rs — one per affected tool
#[test]
fn git_diff_output_is_untrusted() {
    let tool = GitDiffTool::new(/* ... */);
    assert!(tool.output_is_untrusted());
}
#[test]
fn git_status_output_is_untrusted() { /* same shape */ }
#[test]
fn git_commit_output_is_untrusted() { /* same shape */ }
```

(Use the real struct names found in Step 1 — do not guess `ShellExecTool`/`GitDiffTool` if the actual names differ.)

- [ ] **Step 3: Run to verify they fail**

```bash
cargo test -p aivyx-core shell_exec_output_is_untrusted git_diff_output_is_untrusted git_status_output_is_untrusted git_commit_output_is_untrusted -- --nocapture
```

- [ ] **Step 4: Add the override to each `impl Tool for` block**

```rust
// Chapter Bulwark/Picket — this tool's output can carry attacker-authored
// content (curl output via a shell command, a git.diff against a pulled
// branch) with no operator review before it enters model context. Fence
// it as untrusted data and run the injection scan over it, same as
// fs.read and every tool-process proxy.
fn output_is_untrusted(&self) -> bool {
    true
}
```

- [ ] **Step 5: Run to verify they pass**

```bash
cargo test -p aivyx-core shell_exec_output_is_untrusted git_diff_output_is_untrusted git_status_output_is_untrusted git_commit_output_is_untrusted -- --nocapture
```

- [ ] **Step 6: Run the full crate suite** — this is the highest-risk step in this task, since `aivyx-core` has ~700 tests and some may assert on the exact shape of shell/git tool output reaching the model (i.e. some existing test may currently expect the *un-fenced* raw output and will need updating to expect the `{"aivyx_untrusted_content_warning": ..., "data": ...}` envelope instead — this is an intentional behavior change, so update those assertions rather than treating a resulting failure as a regression).

```bash
cargo test -p aivyx-core 2>&1 | tee /tmp/core_test_output.txt
grep -c "FAILED" /tmp/core_test_output.txt
```
If any test fails on the envelope shape, update its assertion to expect the fenced envelope — do not weaken the fix to make an old assertion pass unchanged.

- [ ] **Step 7: Clippy**

```bash
cargo clippy -p aivyx-core --all-targets -- -D warnings
```

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "fix: fence shell.exec and git.* output through Bulwark and Picket (MEDIUM)

Neither tool overrode output_is_untrusted(), so shell command output
(e.g. curl'd content) and git diff/status/commit output (real
third-party-authored diff hunks and commit messages once a branch is
pulled) entered model context completely raw — unfenced by Bulwark and
unscanned by Picket. Added the override, matching the existing pattern
on fs.read and every tool-process proxy.

Found by a full ecosystem security audit, 2026-09-16."
```

---

### Task 10: Real per-sender authorization for Discord/Slack; honest tier split for unallowlisted senders (MEDIUM)

**Finding:** Telegram, Discord, and Slack channels all unconditionally return `TrustTier::SemiTrusted` with no reference to any sender allowlist. Telegram's `chat_filter` defaults to accepting every chat; Discord has no filter at all; Slack only optionally constrains by workspace. `THREAT_MODEL.md` defines `SemiTrusted` as requiring an allowlisted chat ID and `Untrusted` as the tier for unallowlisted senders — the code doesn't implement that distinction.

**Files:**
- Modify: `crates/aivyx-config/src/lib.rs` (add a `chat_filter`-equivalent config field for Discord and Slack, mirroring Telegram's existing one — find it via `grep -n "chat_filter" crates/aivyx-config/src/lib.rs`).
- Modify: `crates/aivyx-discord/src/discord_channel.rs:217-223` and `crates/aivyx-slack/src/slack_channel.rs:216-221` (`trust_tier()`).
- Modify: `crates/aivyx-telegram/src/telegram_channel.rs:228-234` (`trust_tier()`) — even though Telegram already has a filter mechanism, its `trust_tier()` doesn't consult it either; fix all three consistently.
- Test: each crate's own test module.

**Interfaces:**
- Consumes: the sender/chat identity each channel already has available at `trust_tier()` call time (check the struct fields — likely a `chat_id`/`user_id` already stored on construction).
- Produces: `trust_tier()` on all three returns `TrustTier::SemiTrusted` only when the sender/chat matches the operator's configured allowlist, and `TrustTier::Untrusted` otherwise — matching `THREAT_MODEL.md`'s definition exactly.

- [ ] **Step 1: Read the current state of all three `trust_tier()` implementations and Telegram's existing filter**

```bash
sed -n '220,240p' crates/aivyx-telegram/src/telegram_channel.rs
sed -n '210,230p' crates/aivyx-discord/src/discord_channel.rs
sed -n '210,225p' crates/aivyx-slack/src/slack_channel.rs
grep -n "chat_filter" crates/aivyx-config/src/lib.rs crates/aivyx-telegram/src/session.rs crates/aivyx-channel/src/telegram_daemon_frontend.rs
```

- [ ] **Step 2: Add `DiscordConfig`/`SlackConfig` allowlist fields matching Telegram's shape**

Read `TelegramConfig`'s `chat_filter` field type exactly (`grep -n "chat_filter" -B5 -A5 crates/aivyx-config/src/lib.rs` — likely `Option<Vec<i64>>` or similar) and add the equivalent to `DiscordConfig` (e.g. `user_filter: Option<Vec<String>>`, since Discord user IDs are snowflake strings) and confirm/extend `SlackConfig`'s existing `team_id` toward a per-user allowlist too if the audit's characterization ("only optional workspace-wide team_id") is confirmed accurate on read.

- [ ] **Step 3: Write the failing tests**

```rust
// discord_channel.rs
#[test]
fn allowlisted_user_is_semitrusted() {
    let channel = test_channel_with_filter(Some(vec!["12345".into()]), "12345");
    assert_eq!(channel.trust_tier(), TrustTier::SemiTrusted);
}
#[test]
fn non_allowlisted_user_is_untrusted() {
    let channel = test_channel_with_filter(Some(vec!["12345".into()]), "99999");
    assert_eq!(channel.trust_tier(), TrustTier::Untrusted);
}
#[test]
fn no_filter_configured_is_untrusted_by_default() {
    // This is the important behavior-change assertion: per
    // THREAT_MODEL.md, an unallowlisted-by-default channel (no filter
    // set at all) is Untrusted, not SemiTrusted — closing the gap
    // where "no config" silently meant "trust everyone as SemiTrusted."
    let channel = test_channel_with_filter(None, "anyone");
    assert_eq!(channel.trust_tier(), TrustTier::Untrusted);
}
```

Write the equivalent three tests for `slack_channel.rs` and `telegram_channel.rs`.

- [ ] **Step 4: Run to verify they fail**

```bash
cargo test -p aivyx-discord -p aivyx-slack -p aivyx-telegram trust -- --nocapture
```

- [ ] **Step 5: Implement `trust_tier()` on all three**

```rust
// discord_channel.rs (mirror in slack_channel.rs / telegram_channel.rs
// with their own real config field name)
fn trust_tier(&self) -> TrustTier {
    match &self.config.user_filter {
        Some(allowed) if allowed.iter().any(|id| id == &self.user_id) => TrustTier::SemiTrusted,
        _ => TrustTier::Untrusted,
    }
}
```

- [ ] **Step 6: Run to verify they pass**

```bash
cargo test -p aivyx-discord -p aivyx-slack -p aivyx-telegram trust -- --nocapture
```

- [ ] **Step 7: This is a real behavior change for existing operators with no filter configured — document it prominently**

Add a note to `docs/INSTALL.md` (or wherever `chat_filter` is currently documented — check `INSTALL.md:697` per the audit's citation) explaining: as of this fix, an operator who has NOT configured a sender allowlist for Telegram/Discord/Slack now gets `Untrusted` tier for all senders (near-empty capability ceiling) rather than the previous `SemiTrusted`. Point operators who want their existing bot to keep working at `SemiTrusted` toward setting the new filter field explicitly.

- [ ] **Step 8: Run full suite + clippy for all three crates**

```bash
cargo test -p aivyx-discord -p aivyx-slack -p aivyx-telegram -p aivyx-config
cargo clippy -p aivyx-discord -p aivyx-slack -p aivyx-telegram -p aivyx-config --all-targets -- -D warnings
```

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "fix: real per-sender authorization for Telegram/Discord/Slack trust tier (MEDIUM)

All three channels unconditionally returned SemiTrusted regardless of
sender, with no real allowlist enforcement — Discord had no filter
mechanism at all, Slack only an optional workspace-wide constraint,
and Telegram's own chat_filter defaulted to accepting every chat
without trust_tier() ever consulting it. THREAT_MODEL.md defines
SemiTrusted as requiring an allowlisted sender and Untrusted as the
tier for anyone else — trust_tier() on all three now actually
implements that distinction. This is a real behavior change for any
operator with no filter configured: unallowlisted senders now get
Untrusted (near-empty ceiling) instead of the previous SemiTrusted.
Documented in INSTALL.md.

Found by a full ecosystem security audit, 2026-09-16."
```

---

### Task 11: Fix the audit-chain tail-truncation test name and add real tail-truncation detection (MEDIUM)

**Finding:** `crates/aivyx-audit/src/persistent.rs`'s test `truncated_tail_is_detected_as_seq_gap` actually deletes the *middle* entry, not the tail — genuine tail deletion (the last N rows) is undetected, since `expected_seq` is derived from scan-position enumeration with no persisted length or head-MAC anchor to compare against.

**Files:**
- Modify: `crates/aivyx-audit/src/persistent.rs` (rename the mis-named test; add a small persisted anchor file).
- Test: same file's `#[cfg(test)] mod tests`.

**Interfaces:**
- Produces: a new small sidecar file (e.g. `<store>.audit.anchor`, written atomically at 0600 using the same pattern as Task 8) recording `{last_seq: u64, last_mac: String}`, updated on every successful `append`, and checked against the real last entry on every `open`/`verify_from_disk` — a mismatch (fewer entries on disk than the anchor claims, or a different `last_mac` for the same `last_seq`) is reported as a new `VerifyError::TailTruncated` variant distinct from the existing seq-gap error.

- [ ] **Step 1: Read the current test and the append/open/verify code paths in full**

```bash
grep -n "fn truncated_tail_is_detected_as_seq_gap" -A 20 crates/aivyx-audit/src/persistent.rs
grep -n "fn append\|fn open\|fn verify_entries_external\|enum VerifyError\|struct VerifyReport" crates/aivyx-audit/src/persistent.rs
```

- [ ] **Step 2: Rename the mis-named test to describe what it actually tests**

```rust
// was: truncated_tail_is_detected_as_seq_gap
// now:
#[tokio::test]
async fn middle_entry_deletion_is_detected_as_seq_gap() {
    // body unchanged — this test was always correct, just misnamed
}
```

- [ ] **Step 3: Write the new, real tail-truncation test (should fail against current code)**

```rust
#[tokio::test]
async fn genuine_tail_truncation_is_detected() {
    let (handle, key) = /* whatever this file's existing tests use to
        build a fresh store+key pair */;
    let mut log = PersistentAuditLog::open(handle.clone(), key.clone()).await.unwrap();
    log.append(/* three real entries, matching existing test fixture shape */).await.unwrap();
    drop(log);

    // Delete the LAST row (genuine tail truncation, not the middle).
    let last_key = audit_key(2); // whatever the real key-construction helper is, for the highest seq
    handle.delete(&last_key).await.unwrap();

    let result = PersistentAuditLog::open(handle, key).await;
    assert!(result.is_err(), "tail truncation must be detected, not silently accepted");
}
```

- [ ] **Step 4: Run to verify it fails**

```bash
cargo test -p aivyx-audit genuine_tail_truncation_is_detected -- --nocapture
```
Expected: FAIL (currently `Ok`, since `expected_seq` only checks internal consistency of what's actually on disk).

- [ ] **Step 5: Add the persisted anchor**

Add a new small struct and two functions near the existing `append`/`open`:

```rust
#[derive(serde::Serialize, serde::Deserialize)]
struct ChainAnchor {
    last_seq: u64,
    last_mac: String,
}

fn anchor_path(store_path: &std::path::Path) -> std::path::PathBuf {
    store_path.with_extension("audit.anchor")
}

fn write_anchor(store_path: &std::path::Path, seq: u64, mac: &str) -> std::io::Result<()> {
    let anchor = ChainAnchor { last_seq: seq, last_mac: mac.to_string() };
    let json = serde_json::to_string(&anchor).expect("ChainAnchor always serializes");
    // Reuse the same atomic-0600 write pattern as Task 8/write_secure —
    // this file isn't secret, but atomicity still matters so a crash
    // mid-write never leaves a corrupt anchor that would false-positive
    // block a legitimate reopen.
    write_atomic(&anchor_path(store_path), json.as_bytes())
}

fn read_anchor(store_path: &std::path::Path) -> Option<ChainAnchor> {
    let bytes = std::fs::read(anchor_path(store_path)).ok()?;
    serde_json::from_slice(&bytes).ok()
}
```

Call `write_anchor` at the end of every successful `append` (with the just-appended entry's `seq`/`mac`), and call `read_anchor` inside `open`/`verify_from_disk` after the existing verification walk: if an anchor exists and its `last_seq` is greater than the highest seq actually found on disk, or its `last_mac` for that seq doesn't match what's on disk, return a new error variant (add `TailTruncated` to whatever the existing `VerifyError`/chain-broken error enum is called).

Note: on the very first `open` of a store created *before* this fix landed, no anchor file will exist yet — `read_anchor` returning `None` must be treated as "no historical anchor to check against," not as a truncation, so existing stores don't break on upgrade. Write the anchor for the first time on the next successful `append` after upgrade.

- [ ] **Step 6: Run to verify the new test passes and the renamed one still passes**

```bash
cargo test -p aivyx-audit middle_entry_deletion_is_detected_as_seq_gap genuine_tail_truncation_is_detected -- --nocapture
```

- [ ] **Step 7: Update `THREAT_MODEL.md`'s tamper-detection claim**

Find the section the audit cited (`docs/THREAT_MODEL.md` around the audit-chain integrity claim) and add the now-closed caveat, or remove the caveat entirely if this fix fully closes it — re-read the current wording before editing since it may already have been softened.

- [ ] **Step 8: Run full suite + clippy**

```bash
cargo test -p aivyx-audit
cargo clippy -p aivyx-audit --all-targets -- -D warnings
```

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "fix: detect genuine audit-chain tail truncation; fix mis-named test (MEDIUM)

truncated_tail_is_detected_as_seq_gap actually deleted the middle
entry, not the tail — real tail truncation (deleting the last N rows)
was undetected, since expected_seq is derived purely from scan-
position enumeration with nothing persisted to compare the count
against. Added a small sidecar anchor file (last seq + last MAC),
updated on every append and checked on every open/verify, closing the
gap. Renamed the original (correct, just mis-named) test and added a
new one for genuine tail deletion.

Found by a full ecosystem security audit, 2026-09-16."
```

---

### Task 12: Stop double-copying the TOML-sourced passphrase in plaintext (MEDIUM)

**Finding:** `crates/aivyx-config/src/lib.rs:5609-5612`'s `RawAivyxPa.passphrase` is a plain `String`, and the load path at `:6161` does `s.clone()` before wrapping in `SecretString` — two un-zeroized plaintext copies exist transiently.

**Files:**
- Modify: `crates/aivyx-config/src/lib.rs` (the `RawAivyxPa` struct field and its one consumer).
- Test: `aivyx-config`'s own test module.

**Interfaces:**
- Produces: `RawAivyxPa.passphrase: Option<SecretString>` instead of `Option<String>`, removing the `.clone()` entirely since `SecretString` deserializes directly.

- [ ] **Step 1: Read the current struct and consumer**

```bash
sed -n '5600,5615p' crates/aivyx-config/src/lib.rs
sed -n '6155,6165p' crates/aivyx-config/src/lib.rs
grep -n "impl.*Deserialize.*for SecretString\|secrecy::SecretString" crates/aivyx-config/src/lib.rs | head -5
```
Confirm `SecretString` (from the `secrecy` crate, already a dependency per the earlier `aivyx-yubi` audit's mention of it) implements `Deserialize` directly — it does in recent `secrecy` versions when the `serde` feature is enabled; check `Cargo.toml` for that feature flag.

- [ ] **Step 2: Write the failing test**

```rust
#[test]
fn raw_passphrase_field_is_secretstring_not_plain_string() {
    // This is a compile-time property, not a runtime one — the real
    // test is that this line compiles at all after the type change:
    let raw: RawAivyxPa = toml::from_str(r#"passphrase = "test123""#).unwrap();
    let _: &SecretString = raw.passphrase.as_ref().unwrap();
}
```

(This test's real value is forcing the type change to compile; it will fail to *compile* before the fix, which counts as the "failing" state for a type-level fix like this one.)

- [ ] **Step 3: Change the field type and remove the clone**

```rust
struct RawAivyxPa {
    #[serde(default)]
    passphrase: Option<SecretString>,
}
```

At the consumer (~line 6161), change:
```rust
.map(|s| SourcedSecret::new(SecretString::from(s.clone()), FieldSource::Toml))
```
to:
```rust
.map(|s| SourcedSecret::new(s, FieldSource::Toml))
```

(Adjust if the real surrounding code takes `raw.passphrase` by reference rather than by value elsewhere — check whether removing the `.clone()` requires the caller to also stop borrowing `raw` immutably elsewhere in the same function; read the full function before editing.)

- [ ] **Step 4: Run to verify it compiles and passes**

```bash
cargo test -p aivyx-config raw_passphrase_field_is_secretstring -- --nocapture
```

- [ ] **Step 5: Run full suite + clippy** (this touches a widely-used config struct — run the full suite, not just this test)

```bash
cargo test -p aivyx-config
cargo clippy -p aivyx-config --all-targets -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "fix: stop double-copying the TOML passphrase in plaintext (MEDIUM)

RawAivyxPa.passphrase was a plain String, and the load path cloned it
again before wrapping in SecretString — two un-zeroized plaintext
copies existed transiently during every config load. Changed the raw
field to SecretString directly (deserializes without an intermediate
String) and removed the now-unnecessary clone.

Found by a full ecosystem security audit, 2026-09-16."
```

---

### Task 13: Scrub the environment before spawning tool-process subprocesses (MEDIUM)

**Finding:** `crates/aivyx-tool/src/bridge.rs:200`'s `cmd.envs(config.env.iter()...)` only *adds* to the inherited environment — no `env_clear()` anywhere in `aivyx-tool`, so every tool-process subprocess (Gmail, Notion, n8n, …) sees the daemon's entire environment: LLM API keys, the `daemon.env` passphrase variable, channel bot tokens.

**Files:**
- Modify: `crates/aivyx-tool/src/bridge.rs` (the `Command` construction around line 200).
- Test: `bridge.rs`'s own test module.

**Interfaces:**
- Produces: the spawned `Command` calls `.env_clear()` before `.envs(config.env.iter()...)`, so the subprocess starts with *only* the operator-configured `env` map for that tool process, plus whatever minimal set is required for the process to function at all (check whether `PATH` needs to be explicitly re-added — a cleared environment has no `PATH`, which would break spawning the tool-process binary itself if it relies on `PATH` lookup rather than an absolute path; read how `config.command` is invoked to determine if this matters).

- [ ] **Step 1: Read the current spawn code in full**

```bash
sed -n '180,215p' crates/aivyx-tool/src/bridge.rs
```

- [ ] **Step 2: Write the failing test**

```rust
#[tokio::test]
async fn tool_process_does_not_inherit_an_unrelated_secret_env_var() {
    std::env::set_var("AIVYX_TEST_SECRET_SHOULD_NOT_LEAK", "leaked-value");
    // Spawn a tiny tool-process test binary (or reuse whatever fixture
    // bridge.rs's existing tests already spawn — grep `Command::new`
    // in this file's own test module first) with a command that
    // echoes its environment, and assert the output does NOT contain
    // "AIVYX_TEST_SECRET_SHOULD_NOT_LEAK".
    std::env::remove_var("AIVYX_TEST_SECRET_SHOULD_NOT_LEAK");
}
```

Adapt to whatever real subprocess-spawning test fixture already exists in this file — do not invent a new one if `bridge.rs` already has an integration-style test spawning a real child process.

- [ ] **Step 3: Run to verify it fails**

```bash
cargo test -p aivyx-tool tool_process_does_not_inherit_an_unrelated_secret_env_var -- --nocapture
```

- [ ] **Step 4: Add `env_clear()` and re-add `PATH` explicitly**

```rust
cmd.env_clear();
// PATH is required for the child to resolve any command it invokes by
// name rather than absolute path (e.g. if config.command itself is a
// bare binary name resolved via PATH, or the tool process shells out
// internally) — re-add it explicitly since env_clear() removes it too.
if let Ok(path) = std::env::var("PATH") {
    cmd.env("PATH", path);
}
cmd.envs(config.env.iter().map(|(k, v)| (k, v)));
```

- [ ] **Step 5: Run to verify it passes**

```bash
cargo test -p aivyx-tool tool_process_does_not_inherit_an_unrelated_secret_env_var -- --nocapture
```

- [ ] **Step 6: Run the full crate suite** — check specifically whether any existing tool-process integration test relied on inheriting an env var implicitly (e.g. `HOME`, needed by some OAuth libraries to find a cache directory) — if so, add that specific variable to the explicit re-add list in Step 4 rather than reverting the fix.

```bash
cargo test -p aivyx-tool 2>&1 | tee /tmp/tool_test_output.txt
grep -c "FAILED" /tmp/tool_test_output.txt
```

- [ ] **Step 7: Clippy**

```bash
cargo clippy -p aivyx-tool --all-targets -- -D warnings
```

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "fix: scrub environment before spawning tool-process subprocesses (MEDIUM)

Every tool-process subprocess (Gmail, Notion, n8n, ...) inherited the
daemon's entire environment — LLM API keys, the daemon.env passphrase
variable, channel bot tokens — since bridge.rs only added the
operator-configured env map on top of the inherited one, with no
env_clear() anywhere. Cleared the environment before spawn and
explicitly re-added only PATH (required for command resolution).

Found by a full ecosystem security audit, 2026-09-16."
```

---

### Task 14: Bound `[access] deny_paths` merge, not replace, with the built-in defaults — *(aivyx-pa note: this finding was actually reported against `aivyx-coder`'s config, not aivyx-pa's — skip this task in this plan; see the aivyx-coder plan's Task 6 instead.)*

*(Intentionally left as a stub cross-reference — do not implement in this repo. Confirmed by re-checking the audit transcript: the `deny_paths`-replaces-rather-than-merges finding was specific to `aivyx-coder/crates/aivyx-config/src/lib.rs:711-719`, a different config struct in a different repo. aivyx-pa's own `deny_paths` handling was not flagged with this issue.)*

---

### Task 15: Validate tool-process self-declared scope against the operator's config at registration (MEDIUM)

**Finding:** `crates/aivyx-tool/src/multi_harness.rs`'s in-process `ToolContext` construction consults no `CapabilitySet` at all (`NoopChannel::trust_tier()` hardcoded `Trusted`, `message_origin` hardcoded `Operator`), and `ToolProxy::required_scope()`'s value is self-asserted by the tool process over the wire (`ToolRegister`) and adopted verbatim by the daemon. A substituted binary at the configured `command` path can declare whatever scope it wants.

**Files:**
- Modify: `crates/aivyx-cli/src/bin/aivyx.rs` (the `[[tool_process]]` registration/loading code — find via `grep -n "ToolRegister\|fn load_tool_process\|register_tool_process" crates/aivyx-cli/src/bin/aivyx.rs`).
- Read: whatever config schema already lets an operator specify an *expected* scope per `[[tool_process]]` entry (`scope_overrides`, per the audit's citation at `aivyx.rs:8064-8095`) — this task extends that mechanism from "narrow an over-broad declared scope" to "also reject a declared scope the operator never expected at all," which is a smaller, more surgical change than it may first look.

**Interfaces:**
- Produces: if a `[[tool_process]]` config entry specifies an expected scope (or set of scopes) for a named tool and the tool process's `ToolRegister` declares something outside that set (and not narrower, since `scope_overrides` already legitimately narrows), registration for that specific tool is refused with a logged warning, rather than silently trusting whatever the process declared.

- [ ] **Step 1: Read the real registration code end to end**

```bash
sed -n '8050,8100p' crates/aivyx-cli/src/bin/aivyx.rs
grep -n "struct ToolProcessConfig\|scope_overrides" crates/aivyx-config/src/lib.rs
```

- [ ] **Step 2: Determine, from what you read, whether an `expected_scope`/`declared_scope` field already exists per config entry**

If `scope_overrides` already lets an operator specify the scope they expect (even if today it's only used to *narrow*), the fix is: when an override is present and the process's actual declared scope is NOT `is_granted_by` the override (i.e. the process declared something the override doesn't even cover), refuse registration instead of the current narrowing-only behavior. If no such expectation mechanism exists for tools with no override configured at all, this task's realistic scope is narrower: add a config option (`expected_scope: Option<String>` per `[[tool_process]]` entry) that, when set, must exactly match (or be exactly narrowed by, matching the existing `is_granted_by` relationship) what the process declares.

- [ ] **Step 3: Write the failing test**

```rust
#[test]
fn tool_registration_is_refused_when_declared_scope_is_not_covered_by_the_configured_expectation() {
    // Construct whatever config/registration test harness this file's
    // existing tests use, set an expected/override scope of
    // "notion.read" for a tool, then simulate a ToolRegister frame
    // declaring "notion.write" (broader/different, not a narrowing),
    // and assert registration is refused (the tool never becomes
    // callable) with a logged reason.
}
```

- [ ] **Step 4: Run to verify it fails**

```bash
cargo test -p aivyx-cli tool_registration_is_refused_when_declared_scope -- --nocapture
```

- [ ] **Step 5: Implement the check** at the registration site found in Step 1, following whichever of the two designs from Step 2 matches what's actually there.

- [ ] **Step 6: Run to verify it passes, then the full suite**

```bash
cargo test -p aivyx-cli tool_registration_is_refused_when_declared_scope -- --nocapture
cargo test -p aivyx-cli
cargo clippy -p aivyx-cli --all-targets -- -D warnings
```

- [ ] **Step 7: Document the new config option (if Step 2 required adding one) in `docs/TOOLS.md` or wherever `[[tool_process]]` is documented.**

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "fix: reject a tool-process's self-declared scope when it exceeds the operator's configured expectation (MEDIUM)

A tool-process's required_scope was self-asserted over the wire
(ToolRegister) and adopted verbatim by the daemon, with the existing
scope_overrides mechanism only ever narrowing, never rejecting. A
substituted binary at the configured command path could declare
whatever scope it wanted. Registration is now refused when a
configured expectation exists and the process's actual declaration
falls outside it.

Found by a full ecosystem security audit, 2026-09-16."
```

---

### Task 16: Harden the federation replay guard (MEDIUM — latent, not yet wired into any shipped binary, but a real code defect)

**Finding:** `crates/aivyx-federation/src/identity.rs`'s replay guard is in-memory only (doesn't survive restart), evicts via a global 60-second flush rather than per-entry expiry (giving up to ~59s of replayability for a request recorded just before a flush boundary), has no future-timestamp bound (`saturating_sub` lets an artificially-future-dated header never expire), and has no cap on the set's size.

**Files:**
- Modify: `crates/aivyx-federation/src/identity.rs:32,61-89,351`.
- Test: `identity.rs`'s own test module.

**Interfaces:**
- Produces: `seen: Mutex<HashSet<String>>` replaced with a structure that tracks each nonce's own insertion time (e.g. `Mutex<BTreeMap<u64, String>>` keyed by insertion timestamp, or a `HashMap<String, u64>` with periodic sweep of entries older than `MAX_REQUEST_AGE_SECS`) so eviction is per-entry, not a global clear; `verify_request`'s freshness check rejects headers timestamped more than a small skew tolerance (e.g. 5s) into the future, not just ones too far in the past; the set is capped at a fixed maximum size (e.g. 10,000 entries), evicting the oldest on overflow.

- [ ] **Step 1: Read the current implementation in full**

```bash
sed -n '25,95p' crates/aivyx-federation/src/identity.rs
sed -n '345,375p' crates/aivyx-federation/src/identity.rs
```

- [ ] **Step 2: Write the failing tests**

```rust
#[test]
fn nonce_replay_is_rejected_even_across_a_flush_boundary() {
    // Simulate: record a nonce at t=59 (just before the old code's
    // 60s global-clear boundary), advance virtual time to t=61 (past
    // the boundary), and assert the same nonce is STILL rejected as a
    // replay — the old code would have cleared it at the t=60 flush
    // and let it through again.
}

#[test]
fn a_future_dated_header_is_rejected_not_treated_as_permanently_fresh() {
    let far_future_timestamp = now_secs() + 3600; // 1 hour in the future
    let result = verify_request(/* ... */ far_future_timestamp, /* ... */);
    assert!(result.is_err());
}

#[test]
fn the_seen_set_does_not_grow_without_bound() {
    // Insert more than the configured cap distinct nonces and assert
    // the internal set size never exceeds the cap.
}
```

(These will need real access to whatever internal test hooks `identity.rs` already exposes for its own existing replay tests — read its current test module in full before writing these, since it likely already has some form of time-control test double to reuse rather than sleeping in real time.)

- [ ] **Step 3: Run to verify they fail**

```bash
cargo test -p aivyx-federation nonce_replay_is_rejected_even_across_a_flush_boundary a_future_dated_header_is_rejected the_seen_set_does_not_grow_without_bound -- --nocapture
```

- [ ] **Step 4: Replace the flush-based eviction with per-entry expiry**

```rust
struct ReplayGuard {
    seen: Mutex<std::collections::HashMap<String, u64>>, // nonce -> insertion timestamp
}

const MAX_REPLAY_GUARD_ENTRIES: usize = 10_000;
const FUTURE_SKEW_TOLERANCE_SECS: u64 = 5;

impl ReplayGuard {
    fn check_and_record(&self, nonce: &str, now: u64) -> Result<(), FederationError> {
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        // Sweep expired entries first (per-entry, not a global clear).
        seen.retain(|_, &mut inserted| now.saturating_sub(inserted) < MAX_REQUEST_AGE_SECS);
        if seen.contains_key(nonce) {
            return Err(FederationError::Replayed);
        }
        if seen.len() >= MAX_REPLAY_GUARD_ENTRIES {
            // Drop the oldest entry to bound memory rather than refusing
            // new, legitimately-fresh requests outright.
            if let Some(oldest_key) = seen.iter().min_by_key(|(_, &t)| t).map(|(k, _)| k.clone()) {
                seen.remove(&oldest_key);
            }
        }
        seen.insert(nonce.to_string(), now);
        Ok(())
    }
}
```

Adjust field/type names to match whatever the surrounding struct is actually called and how `now_secs()` is already obtained elsewhere in this file — read Step 1's output for the exact existing shape rather than introducing a parallel one.

- [ ] **Step 5: Add the future-skew bound to `verify_request`**

```rust
// verify_request, replacing the plain `now.saturating_sub(header.timestamp) > MAX_REQUEST_AGE_SECS` check:
let age = now.saturating_sub(header.timestamp);
let is_too_old = age > MAX_REQUEST_AGE_SECS;
let is_too_far_in_future = header.timestamp > now + FUTURE_SKEW_TOLERANCE_SECS;
if is_too_old || is_too_far_in_future {
    return Err(FederationError::Expired);
}
```

- [ ] **Step 6: Run to verify all three new tests pass, then the full crate suite**

```bash
cargo test -p aivyx-federation nonce_replay_is_rejected_even_across_a_flush_boundary a_future_dated_header_is_rejected the_seen_set_does_not_grow_without_bound -- --nocapture
cargo test -p aivyx-federation
cargo clippy -p aivyx-federation --all-targets -- -D warnings
```

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "fix: harden the federation replay guard (MEDIUM, latent)

The replay guard evicted via a global 60s flush rather than per-entry
expiry, giving up to ~59s of replayability for a nonce recorded just
before a flush boundary; had no future-timestamp bound (a header
dated arbitrarily far in the future never expired, via
saturating_sub); and had no cap on the set's size (authenticated-peer
DoS). Replaced the flush with per-entry timestamped eviction, added a
5s future-skew tolerance instead of an unbounded one, and capped the
set at 10,000 entries with oldest-first eviction.

This pipeline isn't wired into any shipped binary yet (confirmed
during the audit — the only caller is a test file), so this is a
latent defect fix, not a live-exploit fix — but it's the intended
integration pattern, so worth closing before real transport lands.

Found by a full ecosystem security audit, 2026-09-16."
```

---

### Task 17: Path-traversal-validate pack manifest `bin`/`team_config` fields (MEDIUM)

**Finding:** `crates/aivyx-cli/src/bin/aivyx_modules/pack.rs:126,142` join `manifest.bin`/`manifest.team_config` onto `install_dir` with no traversal check, unlike the `name` field (which is restricted to `[A-Za-z0-9_-]`). A `bin = "../../../../usr/bin/curl"` in a signed manifest would be written verbatim into `aivyx-pa.toml` as a tool-process command.

**Files:**
- Modify: `crates/aivyx-cli/src/bin/aivyx_modules/pack.rs:126,142`.
- Read: `crates/aivyx-pack/src/lib.rs`'s `safe_relative` (~line 419-423, per the audit) — reuse this exact function, it's already proven and tested for this exact class of check.
- Test: `pack.rs`'s own test module.

**Interfaces:**
- Consumes: `aivyx_pack::safe_relative` (check whether it's already `pub` and exported from the crate root — if `pub(crate)`, make it `pub` since this fix needs to call it from `aivyx-cli`).
- Produces: `pack.rs`'s install logic rejects a manifest whose `bin` or `team_config` field, once run through `safe_relative`, would escape `install_dir`.

- [ ] **Step 1: Read `safe_relative` and both current call sites**

```bash
sed -n '415,425p' crates/aivyx-pack/src/lib.rs
sed -n '120,145p' crates/aivyx-cli/src/bin/aivyx_modules/pack.rs
grep -n "^pub " crates/aivyx-pack/src/lib.rs | grep -i "safe_relative\|mod "
```

- [ ] **Step 2: Write the failing test**

```rust
#[test]
fn install_rejects_a_manifest_with_a_traversal_bin_path() {
    let manifest = PackManifest {
        bin: "../../../../usr/bin/curl".to_string(),
        // ...other required fields, matching whatever PackManifest's
        // real shape is — check via `grep -n "struct PackManifest"`
        ..test_manifest_defaults()
    };
    let result = install_from_manifest(&manifest, /* install_dir */);
    assert!(result.is_err());
}

#[test]
fn install_still_accepts_a_normal_bin_path() {
    let manifest = PackManifest {
        bin: "kitchen-tool".to_string(),
        ..test_manifest_defaults()
    };
    assert!(install_from_manifest(&manifest, /* install_dir */).is_ok());
}
```

(Match `install_from_manifest`'s real function name/signature — the audit cited line 126/142 inside some install function; find its actual name via the Step 1 `sed` output.)

- [ ] **Step 3: Run to verify the first fails**

```bash
cargo test -p aivyx-cli install_rejects_a_manifest_with_a_traversal_bin_path -- --nocapture
```

- [ ] **Step 4: Make `safe_relative` `pub` if needed, then apply it at both sites**

```rust
// pack.rs, at the bin path join (~line 126):
let bin_rel = aivyx_pack::safe_relative(&tp.bin)
    .ok_or_else(|| /* this file's real error type */ "pack manifest 'bin' path escapes the install directory".to_string())?;
let bin_path = install_dir.join("bin").join(bin_rel);
```
Mirror the same pattern for `team_config` at line 142.

- [ ] **Step 5: Run to verify both pass**

```bash
cargo test -p aivyx-cli install_rejects_a_manifest_with_a_traversal_bin_path install_still_accepts_a_normal_bin_path -- --nocapture
```

- [ ] **Step 6: Run full suite + clippy**

```bash
cargo test -p aivyx-cli -p aivyx-pack
cargo clippy -p aivyx-cli -p aivyx-pack --all-targets -- -D warnings
```

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "fix: path-traversal-validate pack manifest bin/team_config fields (MEDIUM)

name was restricted to [A-Za-z0-9_-]; bin and team_config were joined
onto install_dir with no traversal check at all, so a signed manifest
declaring bin = \"../../../../usr/bin/curl\" would be written verbatim
into aivyx-pa.toml as a tool-process command. Reused the already-proven
safe_relative check (the same one that already guards pack archive
extraction) at both fields.

Found by a full ecosystem security audit, 2026-09-16."
```

---

### Task 18: Cap pack bundle read size before verification (MEDIUM)

**Finding:** `crates/aivyx-pack/src/lib.rs`'s `read_bundle` (~line 338-341) `read_to_end`s every outer entry into memory with no cap, on an operator-supplied but not-yet-verified file; `unpack_payload` (~428) gzip-decompresses with no expansion-ratio limit.

**Files:**
- Modify: `crates/aivyx-pack/src/lib.rs` (`read_bundle`, `unpack_payload`).
- Test: `lib.rs`'s own test module.

**Interfaces:**
- Produces: `read_bundle` refuses any outer entry larger than a fixed cap (e.g. 512 MiB — generous for a real vertical-pack bundle, small enough to bound a deliberate disk-fill attempt); `unpack_payload`'s gzip decompression is wrapped in a reader that refuses to produce more than a fixed multiple (e.g. 20x) of the compressed input size, closing a zip-bomb-style expansion attack.

- [ ] **Step 1: Read both functions in full**

```bash
sed -n '330,445p' crates/aivyx-pack/src/lib.rs
```

- [ ] **Step 2: Write the failing tests**

```rust
#[test]
fn read_bundle_refuses_an_oversized_entry() {
    // Construct a bundle (using whatever test-fixture builder this
    // file's own tests already use) with one entry claiming a size
    // larger than the new cap, and assert read_bundle returns Err.
}

#[test]
fn unpack_payload_refuses_a_decompression_bomb() {
    // Build a small gzip stream that decompresses to far more than
    // 20x its compressed size (a real, small zip-bomb-style fixture —
    // e.g. a few KB of highly-repetitive data compresses enormously;
    // construct one deterministically with flate2 in the test itself
    // rather than committing a binary fixture file) and assert
    // unpack_payload returns Err rather than allocating unbounded
    // memory.
}
```

- [ ] **Step 3: Run to verify they fail**

```bash
cargo test -p aivyx-pack read_bundle_refuses_an_oversized_entry unpack_payload_refuses_a_decompression_bomb -- --nocapture
```

- [ ] **Step 4: Add the caps**

```rust
const MAX_BUNDLE_ENTRY_BYTES: u64 = 512 * 1024 * 1024;
const MAX_DECOMPRESSION_RATIO: u64 = 20;

// In read_bundle, before/instead of the unconditional read_to_end:
if entry_size > MAX_BUNDLE_ENTRY_BYTES {
    return Err(/* real error type */ format!(
        "bundle entry exceeds the {MAX_BUNDLE_ENTRY_BYTES}-byte limit"
    ).into());
}
let mut buf = Vec::with_capacity(entry_size as usize);
entry_reader.take(MAX_BUNDLE_ENTRY_BYTES).read_to_end(&mut buf)?;
```

```rust
// In unpack_payload, wrap the gzip decoder's output read:
let compressed_len = compressed_bytes.len() as u64;
let max_decompressed = compressed_len.saturating_mul(MAX_DECOMPRESSION_RATIO).max(1024 * 1024); // floor for tiny inputs
let mut decoder = flate2::read::GzDecoder::new(compressed_bytes);
let mut out = Vec::new();
let bytes_read = decoder.take(max_decompressed + 1).read_to_end(&mut out)?;
if bytes_read as u64 > max_decompressed {
    return Err(/* real error type */ "pack payload exceeds the maximum allowed decompression ratio".into());
}
```

(Adjust to the real decompression library/API already in use in this file — check `grep -n "flate2\|GzDecoder\|use.*Decoder" crates/aivyx-pack/src/lib.rs` first rather than assuming `flate2`.)

- [ ] **Step 5: Run to verify they pass**

```bash
cargo test -p aivyx-pack read_bundle_refuses_an_oversized_entry unpack_payload_refuses_a_decompression_bomb -- --nocapture
```

- [ ] **Step 6: Run full suite + clippy** — verify no existing legitimate-size test fixture exceeds the new caps.

```bash
cargo test -p aivyx-pack
cargo clippy -p aivyx-pack --all-targets -- -D warnings
```

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "fix: cap pack bundle entry size and decompression ratio (MEDIUM)

read_bundle read every outer entry fully into memory with no size cap,
and unpack_payload's gzip decompression had no expansion-ratio limit —
both on an operator-supplied file that hasn't been signature-verified
yet, a local memory/disk-fill DoS. Capped entry size at 512 MiB and
decompression ratio at 20x the compressed input size.

Found by a full ecosystem security audit, 2026-09-16."
```

---

## Explicitly deferred (not in this plan's scope — logged, not dropped)

- **Full encrypted-at-rest OAuth token/secret storage** (the broader half of Task 5's finding). Building an IPC-reachable credential vault for out-of-process tool processes is substantial new core infrastructure — a new daemon-side RPC surface, not a bounded fix. Task 5 closes the cheaper, still-real permission gap (0600 on every secret file) in the meantime.
- **Wiring `aivyx-confine` (Landlock+seccomp) as a floor confinement layer under tool-process spawns**, so isolation doesn't depend entirely on an external `bwrap`/`firejail` binary being installed. This means nesting an in-process confiner inside a process that may *also* be wrapped in `bwrap`/`firejail`, with real questions about namespace/privilege interaction between the two layers that deserve their own design pass rather than a rushed fix bundled into this plan. Logged as a real, still-open Medium finding for a follow-up plan.
- **A real vote-tallying/consensus mechanism for `/council`** and **content-laundering around Bulwark/Picket via `memory.*`/`workspace.*`** were flagged Low in the audit, not Medium — out of this plan's High+Medium scope by the user's own agreed cut, along with the rest of the Low/Info findings (doc corrections, dead-code removal, the OAuth PKCE gap, etc.), which get fixed directly after this plan lands, without full plan ceremony.

## Final verification (after all tasks land)

- [ ] Run the complete workspace test suite once, not per-crate:

```bash
cd /home/julian/Projects/Rust/aivyx-pa
cargo test 2>&1 | tail -30
```
Expected: all green, no regressions across crate boundaries.

- [ ] Run the documented clippy command once more at the end:

```bash
cargo clippy --all-targets -- -D warnings
```
Expected: clean.

- [ ] Push the branch and open one PR covering the whole plan (or, if preferred, split into several PRs along natural boundaries — e.g. one for the webhook+trust-tier pair, one for Rampart, one for everything else — ask before choosing, since this repo's established convention throughout the preceding audit-fix work was one PR per logically-independent fix).
