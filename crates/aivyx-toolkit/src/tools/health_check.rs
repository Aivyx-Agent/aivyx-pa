//! `health.check.*` — three operator-facing tools wrapping
//! the polling-loop substrate from [`crate::health_polling`]
//! and the storage substrate from [`crate::health_store`].
//!
//! Phase 125 Task 6 + Phase 147. Two scopes:
//! - `health.write` for `health.check.add` (registers a new
//!   URL watcher) and `health.check.remove` (Phase 147 —
//!   idempotent removal).
//! - `health.read` for `health.check.list` (current state of
//!   every watcher) and `health.check.recent_changes` (state
//!   transitions in a window for agent-side alert
//!   composition).
//!
//! ## Alert composition pattern (operator-side recipe)
//!
//! The polling loop records state transitions; the agent
//! composes alerts. Operator's setup:
//!
//! 1. Operator schedules a cron via `schedule.create` to
//!    fire every N minutes (e.g. hourly).
//! 2. The fired turn prompts the agent: "check for any
//!    health-monitor state changes in the last hour and
//!    notify me of any."
//! 3. The agent invokes `health.check.recent_changes
//!    {window_minutes: 60}`.
//! 4. If `changes` is non-empty, the agent composes a
//!    `notify.send` message summarizing each transition.
//! 5. If `changes` is empty, the agent reports "all
//!    watchers stable" or just exits without sending.
//!
//! Substrate-minimal: no daemon-side automatic alert IPC
//! (deferred to Phase 126+ per the open doc).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use aivyx_capability::Scope;
use aivyx_core::{
    AivyxError, Tool, ToolContext, ToolId, ToolOutcome, Verification,
};

use crate::health_store::{HealthStore, Transition, Watcher, WatcherState};
// Phase 147 — health.check.remove uses
// HealthStore::remove_watcher.

/// Hard cap on the `window_minutes` parameter to
/// `health.check.recent_changes`. 1440 = 24h; longer windows
/// don't make sense for "recent changes" semantics.
const MAX_WINDOW_MINUTES: u64 = 1440;
const DEFAULT_WINDOW_MINUTES: u64 = 60;

/// Default `expect_status` when the operator doesn't override.
const DEFAULT_EXPECT_STATUS: u64 = 200;

// =====================================================================
// health.check.add
// =====================================================================

pub struct HealthCheckAdd {
    id: ToolId,
    schema: Value,
    store: Arc<HealthStore>,
}

impl HealthCheckAdd {
    pub fn new(store: Arc<HealthStore>) -> Self {
        Self {
            id: ToolId::new(),
            schema: add_schema(),
            store,
        }
    }
}

#[async_trait]
impl Tool for HealthCheckAdd {
    fn id(&self) -> ToolId {
        self.id
    }
    fn name(&self) -> &str {
        "health.check.add"
    }
    fn description(&self) -> &str {
        "Register a URL for periodic health monitoring. The \
         tool-process-side polling loop will probe the URL on \
         the configured interval and record state \
         transitions. Input: `{name: string (required, \
         unique identifier no whitespace/slashes), url: \
         string (required, http:// or https://), \
         interval_secs: u32 (required, 60-86400), \
         expect_status: u16 (optional, default 200 — the \
         status code that counts as `ok`)}`. Returns the \
         registered watcher record. Errors with \
         `tool_failed` on duplicate name or out-of-range \
         interval. Scope: `health.write`. To compose alerts \
         from recorded transitions, schedule a cron that \
         calls `health.check.recent_changes` and conditionally \
         `notify.send`."
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("health.write")
            .expect("health.write must parse — it is in KNOWN_BASES from Phase 125")
    }
    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let parsed = match parse_add_input(&input) {
            Ok(p) => p,
            Err(reason) => return failed(self.id, format!("health.check.add: {reason}")),
        };
        match self
            .store
            .add_watcher(parsed.name, parsed.url, parsed.interval_secs, parsed.expect_status)
            .await
        {
            Ok(watcher) => ToolOutcome::Completed {
                output: shape_watcher_brief(&watcher),
                verified: Verification::Verified,
            },
            Err(e) => failed(self.id, format!("health.check.add: {e}")),
        }
    }
}

#[derive(Debug)]
struct ParsedAddInput {
    name: String,
    url: String,
    interval_secs: u64,
    expect_status: u16,
}

fn parse_add_input(input: &Value) -> Result<ParsedAddInput, String> {
    let name = required_string(input, "name")?;
    let url = required_string(input, "url")?;
    // Chapter Rampart, applied here rather than daemon-side: this
    // watcher persists and gets probed on an operator-chosen interval
    // from this tool-process subprocess, which the daemon-side
    // EgressPolicy instance guarding web.fetch/web.extract/web.post
    // structurally cannot reach. Reuse the same classify() check
    // directly here so a model-chosen metadata/loopback/private-network
    // URL is refused before it's ever persisted.
    //
    // Known simplification: this uses EgressPolicy::default() (SSRF
    // guard on, no host allow-list) rather than the operator's real
    // `[access]` config, which lives daemon-side and isn't reachable
    // from this tool-process crate without new IPC plumbing. The
    // default policy still closes the metadata/loopback/private-network
    // cases, which is the exploit this fix targets; an operator-
    // configured host allow-list for this specific tool is a smaller
    // follow-on, not required to close the reported vulnerability.
    if let Some(reason) = aivyx_core::egress::EgressPolicy::default().classify(&url) {
        return Err(format!("refusing to reach {url} — {reason}."));
    }
    let interval_secs = input
        .get("interval_secs")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "input must include an `interval_secs` integer field".to_string())?;
    let expect_status = match input.get("expect_status") {
        None | Some(Value::Null) => DEFAULT_EXPECT_STATUS,
        Some(v) => v
            .as_u64()
            .ok_or_else(|| "`expect_status` must be a non-negative integer".to_string())?,
    };
    if !(100..=599).contains(&expect_status) {
        return Err("`expect_status` must be a valid HTTP status code (100-599)".to_string());
    }
    Ok(ParsedAddInput {
        name,
        url,
        interval_secs,
        // Both bounds fit in u16 (100..=599 < 65535).
        expect_status: expect_status as u16,
    })
}

fn add_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": {
                "type": "string",
                "minLength": 1,
                "description": "Unique identifier for this watcher (no whitespace, no slashes)."
            },
            "url": {
                "type": "string",
                "pattern": "^https?://",
                "description": "URL to probe. Must start with http:// or https://."
            },
            "interval_secs": {
                "type": "integer",
                "minimum": 60,
                "maximum": 86400,
                "description": "Polling interval in seconds (60-86400). Lower bound prevents abuse; upper bound is 24 hours."
            },
            "expect_status": {
                "type": "integer",
                "minimum": 100,
                "maximum": 599,
                "default": 200,
                "description": "HTTP status code that counts as `ok`. Default 200."
            }
        },
        "required": ["name", "url", "interval_secs"],
        "additionalProperties": false
    })
}

// =====================================================================
// health.check.list
// =====================================================================

pub struct HealthCheckList {
    id: ToolId,
    schema: Value,
    store: Arc<HealthStore>,
}

impl HealthCheckList {
    pub fn new(store: Arc<HealthStore>) -> Self {
        Self {
            id: ToolId::new(),
            schema: list_schema(),
            store,
        }
    }
}

#[async_trait]
impl Tool for HealthCheckList {
    fn id(&self) -> ToolId {
        self.id
    }
    fn name(&self) -> &str {
        "health.check.list"
    }
    fn description(&self) -> &str {
        "List every registered URL watcher with its current \
         state. Input: `{}` (no parameters). Returns \
         `{watchers: [{name, url, interval_secs, \
         expect_status, last_check_at?, last_status_code?, \
         last_ok}]}`. `last_check_at` is `null` for watchers \
         that haven't been polled yet (just-registered). \
         Scope: `health.read`."
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("health.read")
            .expect("health.read must parse — it is in KNOWN_BASES from Phase 125")
    }
    async fn execute(&self, _input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let watchers = self.store.list_watchers().await;
        let shaped: Vec<Value> = watchers
            .iter()
            .map(|(w, s)| shape_watcher_with_state(w, s))
            .collect();
        ToolOutcome::Completed {
            output: json!({"watchers": shaped}),
            verified: Verification::NotApplicable,
        }
    }
}

fn list_schema() -> Value {
    json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false
    })
}

// =====================================================================
// health.check.recent_changes
// =====================================================================

pub struct HealthCheckRecentChanges {
    id: ToolId,
    schema: Value,
    store: Arc<HealthStore>,
}

impl HealthCheckRecentChanges {
    pub fn new(store: Arc<HealthStore>) -> Self {
        Self {
            id: ToolId::new(),
            schema: recent_changes_schema(),
            store,
        }
    }
}

#[async_trait]
impl Tool for HealthCheckRecentChanges {
    fn id(&self) -> ToolId {
        self.id
    }
    fn name(&self) -> &str {
        "health.check.recent_changes"
    }
    fn description(&self) -> &str {
        "List state transitions (ok-to-down or down-to-ok) \
         within a recent time window. The substrate that \
         enables agent-side alert composition: call this on \
         a schedule (via `schedule.create`), see what \
         flipped, decide whether to `notify.send`. Input: \
         `{window_minutes: u32 (optional, default 60, max \
         1440)}`. Returns `{changes: [{watcher_name, \
         transitioned_at, from_ok, to_ok, status_code?}], \
         count}`. Empty `changes` array means no flips in \
         the window — operator's services are stable. \
         Scope: `health.read`."
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("health.read")
            .expect("health.read must parse — it is in KNOWN_BASES from Phase 125")
    }
    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let window_minutes = match parse_window_minutes(&input) {
            Ok(m) => m,
            Err(reason) => {
                return failed(
                    self.id,
                    format!("health.check.recent_changes: {reason}"),
                );
            }
        };
        let window = Duration::from_secs(window_minutes * 60);
        let now = chrono::Utc::now();
        let transitions = self.store.recent_transitions_within(now, window).await;
        let shaped: Vec<Value> = transitions.iter().map(shape_transition).collect();
        let count = shaped.len();
        ToolOutcome::Completed {
            output: json!({"changes": shaped, "count": count}),
            verified: Verification::NotApplicable,
        }
    }
}

fn parse_window_minutes(input: &Value) -> Result<u64, String> {
    let raw = match input.get("window_minutes") {
        None | Some(Value::Null) => DEFAULT_WINDOW_MINUTES,
        Some(v) => v
            .as_u64()
            .ok_or_else(|| "`window_minutes` must be a non-negative integer".to_string())?,
    };
    if raw == 0 {
        return Err("`window_minutes` must be >= 1".to_string());
    }
    Ok(raw.min(MAX_WINDOW_MINUTES))
}

fn recent_changes_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "window_minutes": {
                "type": "integer",
                "minimum": 1,
                "maximum": MAX_WINDOW_MINUTES,
                "default": DEFAULT_WINDOW_MINUTES,
                "description": "Look back this many minutes for transitions. Default 60; capped at 1440 (24h)."
            }
        },
        "additionalProperties": false
    })
}

// =====================================================================
// health.check.remove — Phase 147
// =====================================================================

pub struct HealthCheckRemove {
    id: ToolId,
    schema: Value,
    store: Arc<HealthStore>,
}

impl HealthCheckRemove {
    pub fn new(store: Arc<HealthStore>) -> Self {
        Self {
            id: ToolId::new(),
            schema: remove_schema(),
            store,
        }
    }
}

#[async_trait]
impl Tool for HealthCheckRemove {
    fn id(&self) -> ToolId {
        self.id
    }
    fn name(&self) -> &str {
        "health.check.remove"
    }
    fn description(&self) -> &str {
        "Remove a registered URL watcher by name. \
         Idempotent — removing a missing name \
         succeeds with `was_already_removed: true` \
         rather than erroring (same posture as \
         `calendar.delete_event` and \
         `budget.delete`). Input: `{name: string}`. \
         Returns `{name, was_already_removed}`. \
         The watcher's state (last_check_at, \
         last_ok, etc.) is cleared along with the \
         registration; a future re-add of the same \
         name starts fresh. Scope: `health.write`."
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("health.write")
            .expect("health.write must parse — it is in KNOWN_BASES from Phase 125")
    }
    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let name = match required_string(&input, "name") {
            Ok(s) => s,
            Err(reason) => {
                return failed(self.id, format!("health.check.remove: {reason}"));
            }
        };
        match self.store.remove_watcher(&name).await {
            Ok(outcome) => ToolOutcome::Completed {
                output: json!({
                    "name": outcome.name,
                    "was_already_removed": outcome.was_already_removed,
                }),
                verified: Verification::Verified,
            },
            Err(e) => failed(self.id, format!("health.check.remove: {e}")),
        }
    }
}

fn remove_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": {
                "type": "string",
                "minLength": 1,
                "description": "Name of the watcher to remove. Idempotent if no match."
            }
        },
        "required": ["name"],
        "additionalProperties": false
    })
}

// =====================================================================
// shared shapers
// =====================================================================

fn shape_watcher_brief(w: &Watcher) -> Value {
    json!({
        "name": w.name,
        "url": w.url,
        "interval_secs": w.interval_secs,
        "expect_status": w.expect_status,
    })
}

fn shape_watcher_with_state(w: &Watcher, s: &WatcherState) -> Value {
    let mut out = json!({
        "name": w.name,
        "url": w.url,
        "interval_secs": w.interval_secs,
        "expect_status": w.expect_status,
        "last_ok": s.last_ok,
    });
    if let Some(ts) = s.last_check_at {
        out["last_check_at"] = json!(ts);
    }
    if let Some(code) = s.last_status_code {
        out["last_status_code"] = json!(code);
    }
    out
}

fn shape_transition(t: &Transition) -> Value {
    let mut out = json!({
        "watcher_name": t.watcher_name,
        "transitioned_at": t.transitioned_at,
        "from_ok": t.from_ok,
        "to_ok": t.to_ok,
    });
    if let Some(code) = t.status_code {
        out["status_code"] = json!(code);
    }
    out
}

fn required_string(input: &Value, field: &str) -> Result<String, String> {
    let s = input
        .get(field)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("input must include a `{field}` string field"))?;
    if s.trim().is_empty() {
        return Err(format!("`{field}` must not be empty"));
    }
    Ok(s.to_string())
}

fn failed(id: ToolId, detail: String) -> ToolOutcome {
    ToolOutcome::Failed(AivyxError::Tool { tool: id, detail })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health_store::{HealthStore, ProbeOutcome};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    async fn scratch_store() -> (Arc<HealthStore>, PathBuf) {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let tmp = std::env::var("TMPDIR")
            .or_else(|_| std::env::var("TEMP"))
            .unwrap_or_else(|_| "/tmp".to_string());
        let dir = PathBuf::from(tmp).join(format!(
            "aivyx-toolkit-hc-tool-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = HealthStore::open(dir.join("health.json")).await.unwrap();
        (Arc::new(store), dir)
    }

    // ---- parse_add_input ---------------------------------

    #[test]
    fn parse_add_accepts_minimal_payload() {
        let p = parse_add_input(&json!({
            "name": "site-a",
            "url": "https://a.example.com/",
            "interval_secs": 300
        }))
        .expect("parse");
        assert_eq!(p.name, "site-a");
        assert_eq!(p.url, "https://a.example.com/");
        assert_eq!(p.interval_secs, 300);
        assert_eq!(p.expect_status, 200, "expect_status defaults to 200");
    }

    #[test]
    fn parse_add_honors_expect_status_override() {
        let p = parse_add_input(&json!({
            "name": "n",
            "url": "https://x/",
            "interval_secs": 60,
            "expect_status": 301
        }))
        .expect("parse");
        assert_eq!(p.expect_status, 301);
    }

    #[test]
    fn parse_add_rejects_missing_name() {
        let e = parse_add_input(&json!({"url": "https://x/", "interval_secs": 60}))
            .expect_err("must error");
        assert!(e.contains("`name`"), "{e}");
    }

    #[test]
    fn parse_add_rejects_missing_interval_secs() {
        let e = parse_add_input(&json!({"name": "n", "url": "https://x/"}))
            .expect_err("must error");
        assert!(e.contains("`interval_secs`"), "{e}");
    }

    #[test]
    fn parse_add_rejects_out_of_range_expect_status() {
        for bad in [99u64, 600, 1000] {
            let e = parse_add_input(&json!({
                "name": "n", "url": "https://x/", "interval_secs": 60,
                "expect_status": bad,
            }))
            .expect_err("must error");
            assert!(e.contains("100-599"), "for {bad}: {e}");
        }
    }

    // ---- Chapter Rampart — parse_add_input URL validation ----

    #[test]
    fn add_rejects_a_cloud_metadata_url() {
        let err = parse_add_input(&json!({
            "name": "metadata-probe",
            "url": "http://169.254.169.254/latest/meta-data/iam/security-credentials/",
            "interval_secs": 60
        }))
        .expect_err("cloud-metadata URL must be refused");
        assert!(err.contains("169.254.169.254"), "{err}");
    }

    #[test]
    fn add_rejects_a_loopback_url() {
        let err = parse_add_input(&json!({
            "name": "loopback-probe",
            "url": "http://127.0.0.1:9999/",
            "interval_secs": 60
        }))
        .expect_err("loopback URL must be refused");
        assert!(err.contains("127.0.0.1"), "{err}");
    }

    #[test]
    fn add_still_accepts_a_public_url() {
        let ok = parse_add_input(&json!({
            "name": "public-probe",
            "url": "https://example.com/status",
            "interval_secs": 60
        }));
        assert!(ok.is_ok(), "{ok:?}");
    }

    // ---- parse_window_minutes ----------------------------

    #[test]
    fn parse_window_defaults_to_60() {
        assert_eq!(parse_window_minutes(&json!({})).unwrap(), 60);
    }

    #[test]
    fn parse_window_caps_at_1440() {
        assert_eq!(parse_window_minutes(&json!({"window_minutes": 9999})).unwrap(), 1440);
    }

    #[test]
    fn parse_window_rejects_zero() {
        let e = parse_window_minutes(&json!({"window_minutes": 0})).unwrap_err();
        assert!(e.contains(">= 1"), "{e}");
    }

    #[test]
    fn parse_window_rejects_non_integer() {
        let e = parse_window_minutes(&json!({"window_minutes": "many"})).unwrap_err();
        assert!(e.contains("non-negative integer"), "{e}");
    }

    // ---- shape_watcher_with_state ------------------------

    #[test]
    fn shape_watcher_includes_state_when_checked() {
        let w = Watcher {
            name: "w".to_string(),
            url: "https://x/".to_string(),
            interval_secs: 60,
            expect_status: 200,
        };
        let now = chrono::Utc::now();
        let s = WatcherState {
            last_check_at: Some(now),
            last_status_code: Some(200),
            last_ok: true,
        };
        let v = shape_watcher_with_state(&w, &s);
        assert_eq!(v["name"], "w");
        assert_eq!(v["url"], "https://x/");
        assert_eq!(v["interval_secs"], 60);
        assert_eq!(v["expect_status"], 200);
        assert_eq!(v["last_ok"], true);
        assert_eq!(v["last_status_code"], 200);
        assert!(v.get("last_check_at").is_some());
    }

    #[test]
    fn shape_watcher_omits_state_optionals_when_unchecked() {
        let w = Watcher {
            name: "w".to_string(),
            url: "https://x/".to_string(),
            interval_secs: 60,
            expect_status: 200,
        };
        let s = WatcherState::default();
        let v = shape_watcher_with_state(&w, &s);
        assert!(v.get("last_check_at").is_none());
        assert!(v.get("last_status_code").is_none());
        // last_ok is always present (defaults to false).
        assert_eq!(v["last_ok"], false);
    }

    // ---- shape_transition --------------------------------

    #[test]
    fn shape_transition_includes_all_required_fields() {
        let t = Transition {
            watcher_name: "w".to_string(),
            transitioned_at: chrono::Utc::now(),
            from_ok: true,
            to_ok: false,
            status_code: Some(503),
        };
        let v = shape_transition(&t);
        assert_eq!(v["watcher_name"], "w");
        assert_eq!(v["from_ok"], true);
        assert_eq!(v["to_ok"], false);
        assert_eq!(v["status_code"], 503);
        assert!(v.get("transitioned_at").is_some());
    }

    #[test]
    fn shape_transition_omits_status_when_none() {
        let t = Transition {
            watcher_name: "w".to_string(),
            transitioned_at: chrono::Utc::now(),
            from_ok: false,
            to_ok: true,
            status_code: None,
        };
        let v = shape_transition(&t);
        assert!(v.get("status_code").is_none());
    }

    // ---- end-to-end against scratch store ----------------

    #[tokio::test]
    async fn add_then_list_through_store_path() {
        let (store, dir) = scratch_store().await;
        let _add = HealthCheckAdd::new(store.clone());
        let _list = HealthCheckList::new(store.clone());
        store
            .add_watcher("a".to_string(), "https://a/".to_string(), 60, 200)
            .await
            .unwrap();
        store
            .add_watcher("b".to_string(), "https://b/".to_string(), 120, 200)
            .await
            .unwrap();
        let watchers = store.list_watchers().await;
        assert_eq!(watchers.len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn recent_changes_window_filtering_through_store() {
        let (store, dir) = scratch_store().await;
        let _tool = HealthCheckRecentChanges::new(store.clone());
        store
            .add_watcher("x".to_string(), "https://x/".to_string(), 60, 200)
            .await
            .unwrap();
        // Record state flip.
        let now = chrono::Utc::now();
        store
            .record_check("x", ProbeOutcome { status_code: Some(200), ok: true }, now)
            .await
            .unwrap();
        store
            .record_check(
                "x",
                ProbeOutcome { status_code: Some(503), ok: false },
                now + chrono::Duration::seconds(60),
            )
            .await
            .unwrap();
        let recent = store
            .recent_transitions_within(
                now + chrono::Duration::seconds(120),
                Duration::from_secs(3600),
            )
            .await;
        assert_eq!(recent.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- scope pins --------------------------------------

    #[tokio::test]
    async fn read_and_write_scopes_pinned_per_tool() {
        let (store, dir) = scratch_store().await;
        let add = HealthCheckAdd::new(store.clone());
        let list = HealthCheckList::new(store.clone());
        let recent = HealthCheckRecentChanges::new(store.clone());
        assert_eq!(add.required_scope(&json!({})).to_string(), "health.write");
        assert_eq!(list.required_scope(&json!({})).to_string(), "health.read");
        assert_eq!(recent.required_scope(&json!({})).to_string(), "health.read");
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- schemas pinned ----------------------------------

    #[test]
    fn add_schema_requires_three_fields() {
        let s = add_schema();
        let req: Vec<&str> = s["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(req.contains(&"name"));
        assert!(req.contains(&"url"));
        assert!(req.contains(&"interval_secs"));
        // expect_status is OPTIONAL (default 200).
        assert!(!req.contains(&"expect_status"));
        assert_eq!(s["additionalProperties"], false);
    }

    #[test]
    fn list_schema_has_no_inputs() {
        let s = list_schema();
        assert_eq!(s["properties"], json!({}));
        assert_eq!(s["additionalProperties"], false);
    }

    #[test]
    fn recent_changes_schema_caps_window() {
        let s = recent_changes_schema();
        assert_eq!(s["properties"]["window_minutes"]["maximum"], 1440);
        assert_eq!(s["properties"]["window_minutes"]["default"], 60);
    }

    // ---- Phase 147 — health.check.remove ----

    #[test]
    fn remove_schema_requires_name() {
        let s = remove_schema();
        assert_eq!(s["required"][0], "name");
        assert_eq!(s["properties"]["name"]["type"], "string");
        assert_eq!(s["properties"]["name"]["minLength"], 1);
    }

    #[test]
    fn remove_input_extracts_name() {
        let name = required_string(&json!({"name": "site-x"}), "name").unwrap();
        assert_eq!(name, "site-x");
    }

    #[test]
    fn remove_input_rejects_missing_name() {
        let err = required_string(&json!({}), "name").unwrap_err();
        assert!(err.contains("name"), "{err}");
    }
}
