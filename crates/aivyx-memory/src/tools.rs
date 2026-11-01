//! # Memory tools — the three `Tool` impls from Phase 6 task 3.
//!
//! [`MemoryReadTool`], [`MemoryWriteTool`], and [`MemoryForgetTool`]
//! are the user-facing surface of D1's "memory is a tool, not ambient
//! system" commitment. Each one wraps an `Arc<dyn Memory>` (so the
//! same substrate can be driven by any implementation — redb in
//! production, [`crate::InMemoryMemory`] in tests), owns its
//! pre-built input schema, and routes every call through the
//! capability-typed [`aivyx_core::Tool`] trait that the turn loop
//! already enforces.
//!
//! ## Q5 resolution (for future archaeologists)
//!
//! Phase 6 entered open on "does `Tool::required_scope` need an
//! `input: &Value` parameter?" A careful read of `aivyx-core` at task
//! 3 start revealed that Phase 1's task 3 already folded the R1
//! refinement into the live trait: `fn required_scope(&self, input:
//! &serde_json::Value) -> Scope` has been shipped since commit
//! `33012be`. Phase 4's `FsReadTool` has been using the signature for
//! two phases. No DESIGN.md amendment needed; the five-phase
//! empty-diff streak rolls forward to six. The stale DESIGN.md
//! line-1038 note is a known drift marker and stays as-is per the
//! "evidence-driven amendments" discipline — a future phase that
//! *actually* amends D3 will clean it up in the same diff.
//!
//! ## Q3 resolution — topic filter semantics
//!
//! `memory.read` requires a topic. The `"*"` sentinel from the Phase
//! 6 open-question writeup is **not** implemented in this task; the
//! substrate itself has no cross-topic scan primitive, and the audit
//! story for "one call surfaces arbitrary state" is exactly the kind
//! of thing that deserves deliberate design rather than a reserved
//! string squeezed in at task 3. If Phase 7+ wants it, the call site
//! will be `MemoryReadTool::execute`, not a substrate change. This is
//! in line with option (1) from Phase 6's Q3: require a topic
//! argument, force the planner to name what it recalls.
//!
//! ## Scope derivation (the R1 payoff)
//!
//! Each tool derives a scope that *mentions* the specific topic the
//! call will touch, so that an agent holding `memory.read:topic:notes`
//! cannot use `memory.read` to peek at topic `secrets`. The qualifier
//! shape matches DESIGN.md's D4 taxonomy: `<base>:topic:<topic>`.
//! Malformed input (missing topic, wrong type) routes to a "deny
//! scope" — a scope no agent can possibly hold, which the turn
//! loop's scope gate converts into `ToolOutcome::Denied` before
//! `execute` ever runs. This is exactly the Phase 4 `FsReadTool`
//! pattern; the only shift is that memory uses a topic qualifier
//! instead of a path qualifier.
//!
//! The "deny scope" construction has a subtlety: it must still be a
//! *valid* `Scope` (the `Tool` trait's return type is bare `Scope`,
//! not `Result`), and its `base` must be one of `aivyx-capability`'s
//! `KNOWN_BASES` or `Scope::parse` refuses to build it. The reserved
//! sentinel qualifier `topic:\x00denied` meets both constraints: the
//! base is real (`memory.read`), so the parse succeeds, but no agent
//! will ever be granted a scope with a NUL-containing qualifier, so
//! the gate denies every call that lands on it. Same trick as
//! Phase 4's fs deny scope, adapted to this crate's qualifier shape.
//!
//! ## What does NOT happen here
//!
//! - **No scope checking.** The turn loop's scope gate in
//!   `aivyx-core` is the *only* thing that compares `required_scope`
//!   against the agent's held capabilities. `execute` trusts that
//!   it was invoked with an in-scope call — see the `FsReadTool`
//!   docstring for the same explicit reliance.
//! - **No audit writes.** Each tool builds an `AuditTag::MemoryAccess`
//!   for the turn loop to emit, but this task does not wire
//!   audit-on-execute directly — that's the turn loop's job via
//!   `ToolContext::audit`. The tools **do** emit a MemoryAccess
//!   event at the start of `execute`, so the audit chain records
//!   every memory touch even when the substrate no-ops (e.g., a
//!   read of a never-written topic).
//! - **No topic validation beyond non-empty.** UTF-8 well-formedness
//!   is already guaranteed by the `String` type, and the substrate
//!   itself rejects `EmptyTopic`. Topic length caps, character
//!   restrictions, and reserved prefixes are deferred to Phase 7+
//!   if real-world usage surfaces a need.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use aivyx_capability::Scope;
use aivyx_core::{
    AivyxError, AuditTag, MemoryOperation, Tool, ToolContext, ToolId,
    ToolOutcome, Verification,
};

use crate::{EmbeddingHook, Memory, MemoryError};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Default cap on the number of entries a single `memory.read` call
/// can return. Const, not config — same philosophy as
/// `FsReadTool::MAX_READ_BYTES`. 32 is big enough for any realistic
/// "recent memories" recall while keeping a single tool call from
/// flooding an LLM turn if the agent sets `limit` to something wild
/// (or omits it, in which case this is the default).
pub const DEFAULT_READ_LIMIT: usize = 16;

/// Hard ceiling on `memory.read` limit even when explicitly set.
/// An agent can ask for up to this many; anything larger is clamped.
/// Prevents a single malformed planner step from pulling the entire
/// store into context.
pub const MAX_READ_LIMIT: usize = 64;

/// Phase 10 Task 1 — default per-topic limit when the wildcard
/// variant (`{"topics": "*"}`) is used. Smaller than
/// [`DEFAULT_READ_LIMIT`] because the wildcard variant is for
/// cross-topic discovery, not deep recall — four recent entries per
/// topic is plenty to see what each topic is about. Callers that
/// want deeper recall per topic can pass an explicit `limit` (still
/// clamped to [`MAX_READ_LIMIT`]).
pub const DEFAULT_WILDCARD_READ_LIMIT: usize = 4;

/// Phase 7 task 5 — default ceiling on the number of entries a single
/// topic may hold before `memory.write` starts refusing new entries.
/// Not a hard disk-space bound (the redb file can still grow from
/// *many topics*); this is the per-topic GC tripwire that prevents a
/// single topic from becoming an unbounded log. 10_000 is generous
/// enough that realistic "remember X" usage never trips it, but small
/// enough that a runaway loop gets caught long before the substrate
/// chokes.
///
/// The binary reads `AIVYX_PA_MEMORY_MAX_PER_TOPIC` once at startup and
/// passes the resolved cap through `MemoryWriteTool::set_max_per_topic`.
/// Tests override it with the same builder. No ambient global config.
pub const DEFAULT_MAX_PER_TOPIC: usize = 10_000;

/// Construct a `Scope` no agent will ever be granted. Uses the
/// qualifier `"topic:\x00denied"` — a NUL byte inside a qualifier is
/// unreachable through normal `Scope::parse` on user-supplied
/// strings because D4-legal qualifiers never contain NULs, so any
/// agent holding a memory scope will have a NUL-free qualifier and
/// the gate's equality check fails. `base` must be a real base or
/// `Scope::parse` refuses the construction, which is why the
/// function takes the base as an argument.
fn deny_scope(base: &str) -> Scope {
    // `.expect` is fine here: this function is only ever called with
    // one of the three hard-coded base strings below, each of which
    // is in KNOWN_BASES. If someone adds a fourth call site with a
    // typo, the test suite catches it at first run.
    Scope::parse(&format!("{base}:topic:\x00denied"))
        .expect("deny_scope base must be a known scope base")
}

/// Extract a non-empty topic string from a tool-input JSON value.
/// Returns `None` for missing field, wrong type, or empty string.
/// Shared so the three tools' `required_scope` functions derive
/// scopes from exactly the same shape, and `execute` can't disagree
/// with the gate about what "valid" means.
fn topic_from_input(input: &Value) -> Option<&str> {
    let s = input.get("topic")?.as_str()?;
    if s.is_empty() { None } else { Some(s) }
}

/// Phase 10 Task 1 — classify a `memory.read` input as either the
/// single-topic form or the wildcard (cross-topic) form. The two
/// variants are mutually exclusive: passing both `topic` and
/// `topics` is an error that routes to the deny scope.
///
/// - `Single(topic)` — `{"topic": "notes", ...}`.
/// - `Wildcard` — `{"topics": "*", ...}`. The only legal value of
///   the `topics` field is the string `"*"`; any other value is
///   rejected so a future extension of the wildcard grammar can
///   pick its own sentinel without being confused with a typo.
/// - `Invalid` — missing both fields, present-but-wrong-type,
///   literal `\x01` prefix, empty topic, or both fields set at
///   once. The caller routes this to `deny_scope`.
enum MemoryReadShape<'a> {
    Single(&'a str),
    Wildcard,
    Invalid,
}

fn classify_memory_read_input(input: &Value) -> MemoryReadShape<'_> {
    let has_topic = input.get("topic").is_some();
    let has_topics = input.get("topics").is_some();

    if has_topic && has_topics {
        return MemoryReadShape::Invalid;
    }

    if has_topics {
        return match input.get("topics").and_then(|v| v.as_str()) {
            Some("*") => MemoryReadShape::Wildcard,
            _ => MemoryReadShape::Invalid,
        };
    }

    match topic_from_input(input) {
        Some(t) if !topic_uses_reserved_prefix(t) => MemoryReadShape::Single(t),
        _ => MemoryReadShape::Invalid,
    }
}

/// Extract the optional session partition string from a tool-input
/// JSON value. Phase 8 Task 2 introduced this field to namespace
/// memory per `ChannelContext::session_partition()` — a multi-chat
/// Telegram bot sees a different `session` value per chat, so two
/// chats can write `topic:notes` without seeing each other's entries.
///
/// The field is **never** set by the LLM or by the tool caller's
/// hand-written JSON; the turn loop in `aivyx-core` injects it after
/// the LLM emits the tool call and before `required_scope` runs. See
/// the module doc on `ChannelContext::session_partition` and the
/// turn-loop insertion site for the exact contract.
///
/// A missing field, wrong type, or empty string all map to `None`,
/// which is the single-partition default (`LocalChannel` and any
/// other channel whose `session_partition()` returns `None`).
fn session_from_input(input: &Value) -> Option<&str> {
    let s = input.get("session")?.as_str()?;
    if s.is_empty() { None } else { Some(s) }
}

/// Extract the optional role-derived topic prefix from a tool-input
/// JSON value. Phase 11 Task 2 introduced this alongside `session`:
/// the turn loop injects it from `cfg.roles[active_role]
/// .memory_topic_prefix` before `required_scope` runs so that two
/// roles writing to the same logical topic (e.g. `notes`) get
/// isolated namespaces.
///
/// The field is **never** set by the LLM or by the tool caller's
/// hand-written JSON; `agent.rs::run_tool_call` inserts it right
/// after the session-partition insert. A missing field, wrong type,
/// or empty string all map to `None`, which is the no-role-prefix
/// path that preserves Phase 6–10 behavior exactly.
fn role_prefix_from_input(input: &Value) -> Option<&str> {
    let s = input.get("role_prefix")?.as_str()?;
    if s.is_empty() { None } else { Some(s) }
}

/// Prepend the role-derived prefix (if any) to a logical topic.
/// `None` returns the topic unchanged. A `Some("coder/")` and
/// topic `"notes"` returns `"coder/notes"`. The prefix is opaque to
/// this helper — it can end in `/`, `:`, or anything else; the
/// config layer decides the convention. Two-role isolation requires
/// the prefix to be *different* between roles, not *any specific
/// shape*.
fn logical_with_role_prefix(role_prefix: Option<&str>, topic: &str) -> String {
    match role_prefix {
        None => topic.to_string(),
        Some(p) => format!("{p}{topic}"),
    }
}

/// The reserved prefix marker that identifies a session-namespaced
/// physical topic. Uses ASCII `0x01` bytes as separators so no
/// well-formed agent-chosen topic can collide: topics are UTF-8
/// strings and the Phase 6 contract already forbids NUL, but `0x01`
/// is equally unusable as a literal string by a language model and
/// equally easy to reject at the tool-input validation step.
///
/// A physical topic under session `12345` with logical topic `notes`
/// is stored as `\x01s\x0112345\x01notes`. Phase 7 stores open cleanly
/// because they were written under logical topics like `notes` whose
/// literal bytes never start with `\x01`.
const SESSION_PREFIX: &str = "\x01s\x01";

/// Build a session-namespaced physical topic string. When `session`
/// is `None`, returns the logical topic unchanged — this is the
/// single-partition path that preserves every Phase 6/7 on-disk
/// layout exactly. When `session` is `Some`, builds
/// `\x01s\x01<session>\x01<topic>` which can never collide with a
/// logical topic because logical topics cannot contain `\x01`.
fn namespaced_topic(session: Option<&str>, topic: &str) -> String {
    match session {
        None => topic.to_string(),
        Some(s) => format!("{SESSION_PREFIX}{s}\x01{topic}"),
    }
}

/// Reject topics whose literal form starts with the reserved
/// session-namespace prefix. Returning `true` from the caller means
/// "route this input to the deny scope" so the gate never admits a
/// call that would have collided with a physical-topic name. Phase 8
/// Task 2 makes this a hard validation at both `required_scope` and
/// `execute`.
///
/// `pub` (re-exported at the crate root) so other crates' memory-
/// adjacent tools can share this one source of truth instead of
/// re-deriving the `\x01` sentinel — `aivyx_channel::memory_gc_tool`
/// picked it up 2026-07-07 after a guard-coverage audit found
/// `memory.gc` was the only mutating memory tool that didn't check it.
pub fn topic_uses_reserved_prefix(topic: &str) -> bool {
    topic.starts_with('\x01')
}

/// A topic `memory.write` accepts: not reserved, and no control characters
/// (a newline in a topic would start a line in the auto-recalled prompt
/// block, audit P9).
fn topic_is_writable(topic: &str) -> bool {
    !topic_uses_reserved_prefix(topic) && !topic.chars().any(char::is_control)
}

/// Build a qualified memory scope.
///
/// Two shapes:
/// - `<base>:topic:<topic>` — Phase 6 single-partition form (when
///   `session` is `None`).
/// - `<base>:topic:<topic>:session:<session>` — Phase 8 Task 2
///   dual-qualifier form (when `session` is `Some`), for
///   multi-chat channels like Telegram.
///
/// Qualifier order is fixed so two logically equivalent scopes
/// always stringify identically — otherwise the audit chain would
/// see non-canonical variants on the same held capability and
/// `Scope::is_granted_by` would flip on qualifier reordering.
fn memory_scope(base: &str, topic: &str, session: Option<&str>) -> Scope {
    let qualified = match session {
        None => format!("{base}:topic:{topic}"),
        Some(s) => format!("{base}:topic:{topic}:session:{s}"),
    };
    Scope::parse(&qualified).unwrap_or_else(|| deny_scope(base))
}

/// Convert a [`MemoryError`] to a [`ToolOutcome::Failed`] with the
/// right `ToolId`. Substrate errors are never successful tool runs.
fn memory_err_to_failed(tool: ToolId, err: MemoryError) -> ToolOutcome {
    ToolOutcome::Failed(AivyxError::Tool {
        tool,
        detail: format!("memory substrate error: {err}"),
    })
}

/// Turn a [`MemoryEntry`] into the JSON shape the tool emits. Kept
/// out of `MemoryEntry`'s own `Serialize` impl because the on-disk
/// at-rest shape (task 1) and the tool output shape can diverge
/// later — the latter is an agent-facing contract, the former is
/// an internal storage detail.
fn entry_to_json(entry: &crate::MemoryEntry) -> Value {
    json!({
        "topic": entry.topic,
        "body": entry.body,
        "seq": entry.seq,
        "created_at_secs": entry.created_at_secs,
    })
}

// ---------------------------------------------------------------------------
// MemoryReadTool
// ---------------------------------------------------------------------------

/// `memory.read` — recall up to `limit` most recent entries for a
/// given topic.
///
/// Input shape:
/// ```json
/// { "topic": "notes", "limit": 10 }
/// ```
/// `limit` is optional; missing means [`DEFAULT_READ_LIMIT`], and any
/// value larger than [`MAX_READ_LIMIT`] is clamped down. Output is
/// `{"entries": [...]}` with `entries` newest-first.
pub struct MemoryReadTool {
    id: ToolId,
    memory: Arc<dyn Memory>,
    schema: Value,
}

impl std::fmt::Debug for MemoryReadTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryReadTool")
            .field("id", &self.id)
            .field("memory", &"Arc<dyn Memory>")
            .finish()
    }
}

impl MemoryReadTool {
    /// Construct a new `memory.read` tool over the given substrate.
    /// Infallible — the substrate handle is already live, there is
    /// nothing to canonicalize or check.
    pub fn new(memory: Arc<dyn Memory>) -> Self {
        MemoryReadTool {
            id: ToolId::new(),
            memory,
            schema: read_input_schema_value(),
        }
    }
}

fn read_input_schema_value() -> Value {
    json!({
        "type": "object",
        "properties": {
            "topic": {
                "type": "string",
                "description": "Non-empty topic tag to recall. \
                                Memory is strictly topic-scoped; a \
                                reader cannot see topics other than \
                                this one. Mutually exclusive with \
                                `topics`."
            },
            "topics": {
                "type": "string",
                "enum": ["*"],
                "description": "Cross-topic wildcard recall. The only \
                                legal value is the literal string \
                                \"*\", which returns up to \
                                `limit` (default 4) entries from every \
                                topic in the current session. Requires \
                                the agent to hold \
                                `memory.read:topic:*:session:<session>`, \
                                which is not granted by any default \
                                tier ceiling — the wildcard is a \
                                deliberate opt-in. Mutually exclusive \
                                with `topic`."
            },
            "limit": {
                "type": "integer",
                "minimum": 1,
                "maximum": MAX_READ_LIMIT,
                "description": "Maximum number of recent entries to return. \
                                Defaults to 16 for the single-topic \
                                variant and 4 per topic for the \
                                wildcard variant, capped at 64."
            }
        },
        "additionalProperties": false,
    })
}

#[async_trait]
impl Tool for MemoryReadTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "memory.read"
    }

    fn description(&self) -> &str {
        "Recall recent memory entries stored under a given topic. \
         Returns up to `limit` entries (default 16, max 64), newest first. \
         The agent must hold `memory.read:topic:<topic>` — scopes are \
         per-topic, so reading one topic does not grant access to others."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, input: &Value) -> Scope {
        match classify_memory_read_input(input) {
            MemoryReadShape::Single(topic) => {
                memory_scope("memory.read", topic, session_from_input(input))
            }
            // Phase 10 Task 1 — the wildcard shape requires a scope
            // whose topic qualifier is the literal `*` glob. The
            // existing `is_granted_by` machinery treats `*` as a
            // glob wildcard, so an agent holding
            // `memory.read:topic:*:session:<session>` grants
            // `memory.read:topic:<anything>:session:<session>` — but
            // crucially the needed scope *is* the wildcard, not the
            // per-topic form, so only an agent explicitly holding
            // the wildcard can run this call. Cross-topic read is a
            // deliberate opt-in.
            MemoryReadShape::Wildcard => {
                memory_scope("memory.read", "*", session_from_input(input))
            }
            // Missing topic/topics, wrong type, empty topic, reserved
            // `\x01` prefix, or both fields set at once — deny.
            MemoryReadShape::Invalid => deny_scope("memory.read"),
        }
    }

    async fn execute(&self, input: Value, ctx: &ToolContext<'_>) -> ToolOutcome {
        // `session` is the namespacing source from the channel's
        // `session_partition()`, injected by the turn loop. `None`
        // preserves Phase 6 single-partition behavior.
        let session = session_from_input(&input).map(str::to_string);
        // Phase 11 Task 2 — `role_prefix` is the role-derived
        // logical-topic prefix, also injected by the turn loop.
        // `None` preserves Phase 6–10 behavior (bare logical topics).
        let role_prefix = role_prefix_from_input(&input).map(str::to_string);

        let requested_limit = input.get("limit").and_then(|v| v.as_u64()).map(|n| n as usize);

        match classify_memory_read_input(&input) {
            MemoryReadShape::Single(topic) => {
                let topic = topic.to_string();
                let limit = requested_limit
                    .unwrap_or(DEFAULT_READ_LIMIT)
                    .clamp(1, MAX_READ_LIMIT);

                // Audit records the *logical* scope and the *logical*
                // topic the agent asked for. The physical topic is an
                // internal storage detail and must never leak into the
                // audit chain — otherwise `verify_from_disk` would have
                // to know about namespacing to round-trip a chain,
                // breaking D1's "audit verifies without live substrate"
                // rule. The role prefix is part of the physical key, not
                // the logical topic, so it is *not* in the audited
                // scope or query_or_key either — the audit chain is
                // byte-identical whether the role is `default`,
                // `coder`, or anything else.
                ctx.audit.on_event(AuditTag::MemoryAccess {
                    turn_id: ctx.turn_id,
                    operation: MemoryOperation::Read,
                    scope: memory_scope("memory.read", &topic, session.as_deref()),
                    query_or_key: topic.clone(),
                });

                let role_qualified = logical_with_role_prefix(role_prefix.as_deref(), &topic);
                let physical = namespaced_topic(session.as_deref(), &role_qualified);
                let mut entries = match self.memory.get_recent(&physical, limit).await {
                    Ok(v) => v,
                    Err(e) => return memory_err_to_failed(self.id, e),
                };
                // Restore the logical topic on the way out — the agent
                // asked for `notes`, not `\x01s\x0112345\x01notes`.
                // Same reasoning as the audit event above.
                for entry in entries.iter_mut() {
                    entry.topic = topic.clone();
                }

                let json_entries: Vec<Value> = entries.iter().map(entry_to_json).collect();

                ToolOutcome::Completed {
                    output: json!({
                        "topic": topic,
                        "entries": json_entries,
                        "count": json_entries.len(),
                    }),
                    // Read is inherently a query — "verification" is
                    // meaningless for something that didn't mutate state.
                    verified: Verification::NotApplicable,
                }
            }

            // Phase 10 Task 1 — wildcard cross-topic read. Audit
            // records the *logical* wildcard scope (`memory.read:topic:*:
            // session:<s>`) and a sentinel query string, so a
            // `verify_from_disk` walk can see that a wildcard read
            // happened without having to know anything about the
            // session namespacing below the tool layer.
            MemoryReadShape::Wildcard => {
                let per_topic_limit = requested_limit
                    .unwrap_or(DEFAULT_WILDCARD_READ_LIMIT)
                    .clamp(1, MAX_READ_LIMIT);

                ctx.audit.on_event(AuditTag::MemoryAccess {
                    turn_id: ctx.turn_id,
                    operation: MemoryOperation::Read,
                    scope: memory_scope("memory.read", "*", session.as_deref()),
                    query_or_key: "*".to_string(),
                });

                // The scan prefix is the session-namespace prefix for
                // the caller's session, optionally extended by the
                // role prefix. When the caller has no session
                // (single-partition channels like LocalChannel) and
                // no role, the scan prefix is empty, which means
                // "every topic in the substrate." That is deliberately
                // permissive at the substrate, because the capability
                // gate above already required the agent to hold the
                // wildcard scope.
                //
                // Phase 11 Task 2 appends the role prefix so a
                // wildcard from one role enumerates only that role's
                // topics: e.g. `\x01s\x0142\x01coder/` isolates the
                // `coder` role in session `42` from the `researcher`
                // role in the same session.
                let session_scan_prefix = match session.as_deref() {
                    Some(s) => format!("{SESSION_PREFIX}{s}\x01"),
                    None => String::new(),
                };
                let role_segment = role_prefix.as_deref().unwrap_or("");
                let scan_prefix = format!("{session_scan_prefix}{role_segment}");

                let grouped = match self
                    .memory
                    .scan_prefix(&scan_prefix, per_topic_limit)
                    .await
                {
                    Ok(v) => v,
                    Err(e) => return memory_err_to_failed(self.id, e),
                };

                // Strip the session *and* role prefixes from each
                // physical topic on the way out, so the agent sees
                // logical topic names (`notes`, not `coder/notes`
                // and certainly not `\x01s\x0142\x01coder/notes`).
                // Single-partition no-role callers see the physical
                // topic unchanged, which is correct because their
                // physical and logical topics are identical.
                let mut topic_objects: Vec<Value> = Vec::with_capacity(grouped.len());
                for (physical_topic, mut entries) in grouped {
                    let logical_topic = if physical_topic.starts_with(&scan_prefix)
                        && !scan_prefix.is_empty()
                    {
                        physical_topic[scan_prefix.len()..].to_string()
                    } else {
                        physical_topic.clone()
                    };
                    // #G — a wildcard recall enumerates every topic; keep
                    // the daemon's internal bookkeeping archives
                    // (`context:pruned:*`) out of the agent's own tool
                    // results. An explicit single-topic read still resolves.
                    if crate::is_internal_topic(&logical_topic) {
                        continue;
                    }
                    for entry in entries.iter_mut() {
                        entry.topic = logical_topic.clone();
                    }
                    let json_entries: Vec<Value> =
                        entries.iter().map(entry_to_json).collect();
                    let count = json_entries.len();
                    topic_objects.push(json!({
                        "topic": logical_topic,
                        "entries": json_entries,
                        "count": count,
                    }));
                }

                let topic_count = topic_objects.len();
                ToolOutcome::Completed {
                    output: json!({
                        "topics": topic_objects,
                        "topic_count": topic_count,
                    }),
                    verified: Verification::NotApplicable,
                }
            }

            MemoryReadShape::Invalid => {
                // Same invariant-violation argument as the original
                // single-topic path: `required_scope` would have
                // returned the deny scope for this input, so reaching
                // `execute` means the gate was bypassed or the
                // gate-logic has a bug. Fail loudly in audit.
                ToolOutcome::Failed(AivyxError::Internal(
                    "memory.read: reached execute with malformed input \
                     (topic/topics missing, wrong type, reserved prefix, \
                     or both fields set) after scope gate admitted the \
                     call"
                        .to_string(),
                ))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// MemoryWriteTool
// ---------------------------------------------------------------------------

/// `memory.write` — store a single entry under a topic.
///
/// Input shape:
/// ```json
/// { "topic": "notes", "body": "the user's favorite color is purple" }
/// ```
/// Output is `{"seq": <u64>}`, the substrate's assigned monotonic
/// sequence number. The tool verifies the write by calling
/// `get_recent(topic, 1)` and checking the top entry matches what it
/// just wrote — this is what `Verification::Verified` exists for per
/// D1's "tool success ≠ intent completed" rule.
pub struct MemoryWriteTool {
    id: ToolId,
    memory: Arc<dyn Memory>,
    schema: Value,
    /// Phase 7 task 5 — per-topic GC tripwire. When a topic already
    /// holds at least this many entries, `execute` refuses further
    /// writes with a `ToolOutcome::Failed` carrying a
    /// recovery-actionable error detail. Defaults to
    /// [`DEFAULT_MAX_PER_TOPIC`]; binaries and tests can override via
    /// [`MemoryWriteTool::set_max_per_topic`].
    max_per_topic: usize,
    /// Phase 75 — optional write-time embedding hook. When set
    /// (the daemon supplies one iff `[embedding]` is configured),
    /// a successful `put` is followed by an `embed_one` +
    /// `put_vector`. A `None` from the hook is non-fatal: the
    /// entry stands, only its vector is deferred to the hourly
    /// backfill. `None` here = no embedding configured = today's
    /// behavior.
    embedding: Option<Arc<dyn EmbeddingHook>>,
}

impl std::fmt::Debug for MemoryWriteTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryWriteTool")
            .field("id", &self.id)
            .field("memory", &"Arc<dyn Memory>")
            .field("max_per_topic", &self.max_per_topic)
            .field(
                "embedding",
                &self.embedding.as_ref().map(|_| "Arc<dyn EmbeddingHook>"),
            )
            .finish()
    }
}

impl MemoryWriteTool {
    pub fn new(memory: Arc<dyn Memory>) -> Self {
        MemoryWriteTool {
            id: ToolId::new(),
            memory,
            schema: write_input_schema_value(),
            max_per_topic: DEFAULT_MAX_PER_TOPIC,
            embedding: None,
        }
    }

    /// Phase 75 — attach a write-time embedding hook. Builder-
    /// style to match `set_max_per_topic`. The binary calls this
    /// only when `[embedding]` is configured; otherwise the tool
    /// keeps its pre-Phase-75 behavior (no vectors written).
    pub fn with_embedding_hook(
        mut self,
        hook: Arc<dyn EmbeddingHook>,
    ) -> Self {
        self.embedding = Some(hook);
        self
    }

    /// Override the per-topic write cap. Returns `self` for the
    /// builder-style chaining the binary uses when
    /// `AIVYX_PA_MEMORY_MAX_PER_TOPIC` is set, and that tests use to drive
    /// the tripwire with a manageable number of seeded entries.
    ///
    /// A cap of `0` is legal and means "refuse every write" — useful
    /// for testing the refusal path but nothing else. The tool does
    /// not silently reinterpret `0` as "unlimited"; if you want that,
    /// use `usize::MAX`.
    pub fn set_max_per_topic(mut self, cap: usize) -> Self {
        self.max_per_topic = cap;
        self
    }
}

fn write_input_schema_value() -> Value {
    json!({
        "type": "object",
        "properties": {
            "topic": {
                "type": "string",
                "description": "Non-empty topic tag to store the entry under. \
                                Agents must hold `memory.write:topic:<topic>`."
            },
            "body": {
                "type": "string",
                "description": "Freeform UTF-8 body of what to remember."
            }
        },
        "required": ["topic", "body"],
        "additionalProperties": false,
    })
}

/// Normalized word set — lowercase alphanumeric runs. Case and
/// punctuation tweaks don't count as new information.
fn word_set(s: &str) -> std::collections::HashSet<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

/// Dogfood 2026-07-04 — a refine/rewrite of a fact lands as a second
/// near-identical entry (the loop wrote a walking-benefit one-liner,
/// then rewrote it with a citation appended: two entries, one fact).
/// `old` is a near-duplicate of `new` when their word sets are
/// essentially the same (Jaccard ≥ 0.85) or virtually every word of
/// the old entry appears in a strictly larger new one (containment
/// ≥ 0.95 — the "rewrite with an addition" signature). Thresholds
/// are deliberately conservative: a survivor is recall noise (the
/// pre-fix status quo), a false match deletes a distinct fact — so
/// "…Tuesday…" vs "…Thursday…" (one word swapped, 0.9 containment,
/// equal size) must stay apart. The containment rule also needs ≥ 5
/// words so a tiny entry can't be swallowed by any longer one
/// mentioning its words.
fn is_near_duplicate_of(old: &str, new: &str) -> bool {
    let a = word_set(old);
    let b = word_set(new);
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let inter = a.intersection(&b).count();
    let union = a.len() + b.len() - inter;
    if inter as f64 / union as f64 >= 0.85 {
        return true;
    }
    a.len() >= 5 && b.len() > a.len() && inter as f64 / a.len() as f64 >= 0.95
}

#[async_trait]
impl Tool for MemoryWriteTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "memory.write"
    }

    fn description(&self) -> &str {
        "Store a new memory entry under a topic. Returns the assigned \
         monotonic sequence number. The tool re-reads the topic after \
         writing and returns `Verified` only if the new entry is at \
         the head of the recall order."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, input: &Value) -> Scope {
        match topic_from_input(input) {
            Some(topic) if topic_is_writable(topic) => {
                memory_scope("memory.write", topic, session_from_input(input))
            }
            _ => deny_scope("memory.write"),
        }
    }

    async fn execute(&self, input: Value, ctx: &ToolContext<'_>) -> ToolOutcome {
        let topic = match topic_from_input(&input) {
            Some(t) if topic_is_writable(t) => t.to_string(),
            _ => {
                return ToolOutcome::Failed(AivyxError::Internal(
                    "memory.write: reached execute with malformed input \
                     (topic missing, non-string, reserved prefix, or control characters) \
                     after scope gate admitted the call"
                        .to_string(),
                ));
            }
        };
        let session = session_from_input(&input).map(str::to_string);
        let role_prefix = role_prefix_from_input(&input).map(str::to_string);
        let body = match input.get("body").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: "input must have a string `body` field".to_string(),
                });
            }
        };

        ctx.audit.on_event(AuditTag::MemoryAccess {
            turn_id: ctx.turn_id,
            operation: MemoryOperation::Write,
            scope: memory_scope("memory.write", &topic, session.as_deref()),
            query_or_key: topic.clone(),
        });

        // Phase 7 task 5 — GC tripwire. Count the topic's existing
        // entries via a bounded `get_recent` call. `RedbMemory::get_recent`
        // does a full `scan_prefix` on the topic regardless of limit
        // (decode cost is what scales with `limit`), so passing
        // `max_per_topic` gives us the smallest decoded prefix that
        // can still answer "is the topic at or above cap?" The audit
        // event above has already been emitted — a refused write is
        // still a write *intent*, and recording it is the whole point
        // of the audit chain. Only after the tripwire fires do we
        // surface the refusal to the caller.
        //
        // Phase 8 Task 2 — the tripwire counts entries in the
        // *physical* (namespaced) topic, not the logical one.
        // Otherwise chat A filling its `notes` bucket would cap chat
        // B's unrelated `notes` bucket — the whole point of session
        // partitioning is that these are independent quotas.
        //
        // Phase 11 Task 2 extends this with the role prefix: the
        // `coder` role and the `researcher` role both writing to
        // logical `notes` get independent quotas, for the exact same
        // "independent namespaces are independent" reason.
        let role_qualified = logical_with_role_prefix(role_prefix.as_deref(), &topic);
        let physical = namespaced_topic(session.as_deref(), &role_qualified);
        let existing = match self
            .memory
            .get_recent(&physical, self.max_per_topic)
            .await
        {
            Ok(v) => v,
            Err(e) => return memory_err_to_failed(self.id, e),
        };
        if existing.len() >= self.max_per_topic {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!(
                    "topic '{topic}' has reached the per-topic size cap \
                     ({} entries); call memory.forget for this topic or \
                     write under a different topic",
                    self.max_per_topic
                ),
            });
        }

        let seq = match self.memory.put(&physical, &body).await {
            Ok(s) => s,
            Err(e) => return memory_err_to_failed(self.id, e),
        };

        // Phase 75 — write-time embed. Strictly non-fatal: a
        // `None` from the hook (provider down, no key, transient
        // error) leaves the entry without a vector and the
        // hourly backfill retries it. A `put_vector` failure is
        // likewise swallowed — the entry write is the only thing
        // `memory.write` promises, so Verified must not hinge on
        // the vector landing.
        if let Some(hook) = &self.embedding {
            if let Some(vector) = hook.embed_one(&body).await {
                let _ =
                    self.memory.put_vector(&physical, seq, vector).await;
            }
        }

        // Dogfood 2026-07-04 — supersede near-duplicates. A rewrite
        // or refinement of an existing fact should replace it, not
        // sit beside it as recall noise. The new entry is already in
        // (write first, delete second — a failure in between leaves
        // duplicate noise, never data loss), so drop every older
        // entry the new body repeats or subsumes. Best-effort: a
        // delete failure leaves a survivor, which is the pre-fix
        // status quo.
        let mut superseded: Vec<u64> = Vec::new();
        for old in &existing {
            if is_near_duplicate_of(&old.body, &body)
                && matches!(
                    self.memory.delete_entry(&physical, old.seq).await,
                    Ok(true)
                )
            {
                superseded.push(old.seq);
            }
        }

        // Verification fence. Re-read the topic's newest entry and
        // confirm it is what we just wrote. This catches substrate
        // drift: if a put-then-read races with a concurrent
        // `forget`, or if RedbMemory's seq counter somehow gets
        // out of sync with the store, we want `Verified` to be a
        // lie only when the write truly landed. D1 calls this the
        // "verify what you did" rule.
        let verified = match self.memory.get_recent(&physical, 1).await {
            Ok(latest) => match latest.first() {
                Some(top) if top.seq == seq && top.body == body => Verification::Verified,
                _ => Verification::Unverified,
            },
            // A read failure after a successful write is *still* a
            // successful write from the substrate's point of view;
            // report it as Unverified rather than demoting the whole
            // call to Failed.
            Err(_) => Verification::Unverified,
        };

        // `superseded` only appears when a near-duplicate was
        // replaced, so the common case keeps its exact output shape.
        let mut output = json!({
            "topic": topic,
            "seq": seq,
        });
        if !superseded.is_empty() {
            output["superseded"] = json!(superseded);
        }
        ToolOutcome::Completed { output, verified }
    }
}

// ---------------------------------------------------------------------------
// MemoryForgetTool
// ---------------------------------------------------------------------------

/// `memory.forget` — delete every entry under a topic.
///
/// Input shape:
/// ```json
/// { "topic": "notes" }
/// ```
/// Output is `{"deleted": <usize>}`. Verification re-reads the topic
/// and confirms it's empty.
pub struct MemoryForgetTool {
    id: ToolId,
    memory: Arc<dyn Memory>,
    schema: Value,
}

impl std::fmt::Debug for MemoryForgetTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryForgetTool")
            .field("id", &self.id)
            .field("memory", &"Arc<dyn Memory>")
            .finish()
    }
}

impl MemoryForgetTool {
    pub fn new(memory: Arc<dyn Memory>) -> Self {
        MemoryForgetTool {
            id: ToolId::new(),
            memory,
            schema: forget_input_schema_value(),
        }
    }
}

fn forget_input_schema_value() -> Value {
    json!({
        "type": "object",
        "properties": {
            "topic": {
                "type": "string",
                "description": "Non-empty topic tag to forget. Every \
                                entry under this topic is deleted. \
                                Agents must hold \
                                `memory.forget:topic:<topic>`."
            }
        },
        "required": ["topic"],
        "additionalProperties": false,
    })
}

#[async_trait]
impl Tool for MemoryForgetTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "memory.forget"
    }

    fn description(&self) -> &str {
        "Delete every memory entry under a topic. Returns the number \
         of entries removed. Verification re-reads the topic and \
         confirms it is empty."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, input: &Value) -> Scope {
        match topic_from_input(input) {
            Some(topic) if !topic_uses_reserved_prefix(topic) => {
                memory_scope("memory.forget", topic, session_from_input(input))
            }
            _ => deny_scope("memory.forget"),
        }
    }

    async fn execute(&self, input: Value, ctx: &ToolContext<'_>) -> ToolOutcome {
        let topic = match topic_from_input(&input) {
            Some(t) if !topic_uses_reserved_prefix(t) => t.to_string(),
            _ => {
                return ToolOutcome::Failed(AivyxError::Internal(
                    "memory.forget: reached execute with malformed input \
                     (topic missing, non-string, or reserved prefix) \
                     after scope gate admitted the call"
                        .to_string(),
                ));
            }
        };
        let session = session_from_input(&input).map(str::to_string);
        let role_prefix = role_prefix_from_input(&input).map(str::to_string);

        ctx.audit.on_event(AuditTag::MemoryAccess {
            turn_id: ctx.turn_id,
            operation: MemoryOperation::Forget,
            scope: memory_scope("memory.forget", &topic, session.as_deref()),
            query_or_key: topic.clone(),
        });

        let role_qualified = logical_with_role_prefix(role_prefix.as_deref(), &topic);
        let physical = namespaced_topic(session.as_deref(), &role_qualified);
        let deleted = match self.memory.forget(&physical).await {
            Ok(n) => n,
            Err(e) => return memory_err_to_failed(self.id, e),
        };

        // Verification: read 1 from the topic, confirm we get nothing.
        // A concurrent writer could in principle add an entry between
        // forget and get_recent; we still report Verified because the
        // forget call itself did land — the new entry is a fresh
        // write, not a survivor. But if the substrate returns an
        // error on the read, we cannot prove the forget worked, so
        // report Unverified.
        let verified = match self.memory.get_recent(&physical, 1).await {
            Ok(v) if v.is_empty() => Verification::Verified,
            Ok(_) => Verification::Unverified,
            Err(_) => Verification::Unverified,
        };

        ToolOutcome::Completed {
            output: json!({
                "topic": topic,
                "deleted": deleted,
            }),
            verified,
        }
    }
}

// ---------------------------------------------------------------------------
// MemorySearchTool — Phase 74
// ---------------------------------------------------------------------------

/// Default cap on a `memory.search` call. Lower than
/// `MemoryReadTool::DEFAULT_READ_LIMIT` because search results
/// cross topic boundaries and the agent rarely needs more than a
/// dozen hits to find what it was looking for.
const DEFAULT_SEARCH_LIMIT: usize = 16;

/// Hard ceiling on a `memory.search` call regardless of operator
/// override. Matches `MAX_READ_LIMIT`.
const MAX_SEARCH_LIMIT: usize = 64;

/// `memory.search` — case-insensitive substring search across
/// every topic + body in the substrate. Returns up to `limit`
/// matching entries, newest first.
///
/// The scope required is the cross-topic wildcard
/// `memory.read:topic:*:session:<session>` so the same opt-in
/// gate that controls cross-topic `memory.read` also controls
/// search — operators who don't grant the wildcard get no
/// search capability.
pub struct MemorySearchTool {
    id: ToolId,
    memory: Arc<dyn Memory>,
    schema: Value,
    /// Phase 75 — query-embedding hook for `mode = "semantic"`.
    /// `None` (no `[embedding]` config) means a semantic
    /// request transparently degrades to keyword.
    embedding: Option<Arc<dyn EmbeddingHook>>,
}

impl std::fmt::Debug for MemorySearchTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemorySearchTool")
            .field("id", &self.id)
            .field("memory", &"Arc<dyn Memory>")
            .field(
                "embedding",
                &self.embedding.as_ref().map(|_| "Arc<dyn EmbeddingHook>"),
            )
            .finish()
    }
}

impl MemorySearchTool {
    pub fn new(memory: Arc<dyn Memory>) -> Self {
        MemorySearchTool {
            id: ToolId::new(),
            memory,
            schema: search_input_schema_value(),
            embedding: None,
        }
    }

    /// Phase 75 — attach the query-embedding hook. Builder-
    /// style; the binary calls this only when `[embedding]` is
    /// configured.
    pub fn with_embedding_hook(
        mut self,
        hook: Arc<dyn EmbeddingHook>,
    ) -> Self {
        self.embedding = Some(hook);
        self
    }
}

fn search_input_schema_value() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": {
                "type": "string",
                "description": "Substring to look for. Matches \
                                case-insensitively in either the topic \
                                name or the entry body. An empty string \
                                returns the newest `limit` entries \
                                across every topic."
            },
            "limit": {
                "type": "integer",
                "minimum": 1,
                "maximum": MAX_SEARCH_LIMIT,
                "description": "Maximum number of matching entries to \
                                return (default 16, max 64). Server-side \
                                clamped to [1, 64]."
            },
            "mode": {
                "type": "string",
                "enum": ["keyword", "semantic"],
                "description": "Retrieval mode. `keyword` (default) \
                                is case-insensitive substring match. \
                                `semantic` ranks by embedding cosine \
                                similarity — meaning-based recall. If \
                                semantic embedding is unavailable the \
                                tool transparently falls back to \
                                keyword and sets \
                                `fell_back_to_keyword: true`."
            }
        },
        "required": ["query"],
        "additionalProperties": false,
    })
}

#[async_trait]
impl Tool for MemorySearchTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "memory.search"
    }

    fn description(&self) -> &str {
        "Substring-search memory entries by topic + body. Returns \
         matching entries newest-first. Cross-topic — requires the \
         agent to hold `memory.read:topic:*:session:<session>` \
         (same wildcard as cross-topic `memory.read`)."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, input: &Value) -> Scope {
        // Cross-topic discovery → wildcard scope. Operators who
        // don't grant the wildcard get no search capability.
        memory_scope("memory.read", "*", session_from_input(input))
    }

    async fn execute(&self, input: Value, ctx: &ToolContext<'_>) -> ToolOutcome {
        let query = match input.get("query").and_then(|v| v.as_str()) {
            Some(q) => q.to_string(),
            None => {
                return ToolOutcome::Failed(AivyxError::Internal(
                    "memory.search: missing `query` field after scope \
                     gate admitted the call"
                        .into(),
                ));
            }
        };
        let limit = input
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_SEARCH_LIMIT)
            .clamp(1, MAX_SEARCH_LIMIT);
        // Phase 75 — `mode` (default keyword). Anything other
        // than the literal "semantic" is keyword.
        let semantic_requested = input
            .get("mode")
            .and_then(|v| v.as_str())
            .map(|m| m.eq_ignore_ascii_case("semantic"))
            .unwrap_or(false);

        let session = session_from_input(&input).map(str::to_string);
        let role_prefix = role_prefix_from_input(&input).map(str::to_string);

        // Audit records the wildcard scope + the query as the
        // searchable key (analogous to the existing wildcard
        // `memory.read` audit shape).
        ctx.audit.on_event(AuditTag::MemoryAccess {
            turn_id: ctx.turn_id,
            operation: MemoryOperation::Read,
            scope: memory_scope("memory.read", "*", session.as_deref()),
            query_or_key: format!("search:{query}"),
        });

        // Build the session + role prefix to filter matches by.
        // Pre-Phase-10 callers (no session, no role) see every
        // entry; post-Phase-11 callers see only their own
        // namespace.
        let session_scan_prefix = match session.as_deref() {
            Some(s) => format!("{SESSION_PREFIX}{s}\x01"),
            None => String::new(),
        };
        let role_segment = role_prefix.as_deref().unwrap_or("");
        let full_prefix = format!("{session_scan_prefix}{role_segment}");

        // Phase 75 — pick the retrieval path. Semantic falls
        // back to keyword (flagged) when: no embedding hook is
        // wired, the query embed returns `None`, or the corpus
        // has zero vectors (semantic over an empty index is
        // strictly worse than keyword there).
        let mut fell_back_to_keyword = false;
        let keyword = |tool: &Self| {
            let mem = tool.memory.clone();
            let q = query.clone();
            async move { mem.search(&q, MAX_SEARCH_LIMIT * 4).await }
        };
        let hits = if semantic_requested {
            let qvec = match &self.embedding {
                Some(h) => h.embed_one(&query).await,
                None => None,
            };
            let has_vectors = match self.memory.load_all_vectors().await {
                Ok(v) => !v.is_empty(),
                Err(e) => return memory_err_to_failed(self.id, e),
            };
            match qvec {
                Some(qvec) if has_vectors => {
                    match self
                        .memory
                        .semantic_search(&qvec, MAX_SEARCH_LIMIT * 4)
                        .await
                    {
                        Ok(v) => v,
                        Err(e) => return memory_err_to_failed(self.id, e),
                    }
                }
                _ => {
                    fell_back_to_keyword = true;
                    match keyword(self).await {
                        Ok(v) => v,
                        Err(e) => return memory_err_to_failed(self.id, e),
                    }
                }
            }
        } else {
            match keyword(self).await {
                Ok(v) => v,
                Err(e) => return memory_err_to_failed(self.id, e),
            }
        };
        // The chosen path returns every match across the store.
        // Filter to those whose physical topic starts with our
        // session+role prefix, then strip the prefix to restore
        // the logical topic the agent sees.
        let mut filtered: Vec<Value> = Vec::with_capacity(limit);
        for entry in hits {
            if !full_prefix.is_empty() && !entry.topic.starts_with(&full_prefix) {
                continue;
            }
            let logical_topic = if !full_prefix.is_empty() {
                entry.topic[full_prefix.len()..].to_string()
            } else {
                entry.topic.clone()
            };
            filtered.push(json!({
                "topic": logical_topic,
                "body": entry.body,
                "seq": entry.seq,
                "created_at_secs": entry.created_at_secs,
            }));
            if filtered.len() >= limit {
                break;
            }
        }

        let match_count = filtered.len();
        let effective_mode =
            if semantic_requested && !fell_back_to_keyword {
                "semantic"
            } else {
                "keyword"
            };
        ToolOutcome::Completed {
            output: json!({
                "query": query,
                "matches": filtered,
                "count": match_count,
                "mode": effective_mode,
                "fell_back_to_keyword": fell_back_to_keyword,
            }),
            verified: Verification::Verified,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
//
// Unit tests exercise the three tools against `InMemoryMemory` so they
// stay at microsecond speed. The integration test in task 5 will run
// the same code paths against `RedbMemory` over a real scratch store.
//
// Test coverage matches the PHASE_6.md task 3 contract line-by-line:
//   - scope-checked happy path (write → read → correct entry)
//   - scope derivation is input-dependent (R1 payoff)
//   - scope denial (malformed input → deny scope)
//   - empty result (read of unknown topic → {"entries": [], "count": 0})
//   - forget clears a topic and the verification reports Verified
//   - write reports Verified after put+re-read
//   - substrate error surfaces as ToolOutcome::Failed

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryMemory;
    use aivyx_core::{
        AgentId, CancellationToken, ChannelContext, ChannelError, ChannelPlatform,
        NullAuditHook, SessionId, StreamEvent, TurnId, TurnOutcome,
    };

    // ---- Test helpers ----------------------------------------------

    /// A minimal `ChannelContext` that ignores every event. Mirrors
    /// the `NoopChannel` in `aivyx-core::tools::fs`'s own test module
    /// — the Phase 4 reference harness — adapted here because the
    /// memory tools never actually call into the channel (unlike
    /// `fs.read`, they don't stream progress). Kept verbose rather
    /// than hidden behind a macro because the `#[async_trait]`
    /// impl is the whole point of the fake and hiding it would
    /// obscure the assertions.
    struct NoopChannel {
        session: SessionId,
        token: CancellationToken,
    }

    #[async_trait]
    impl ChannelContext for NoopChannel {
        fn channel_name(&self) -> &str {
            "test"
        }
        fn platform(&self) -> ChannelPlatform {
            ChannelPlatform::Local
        }
        fn trust_tier(&self) -> aivyx_capability::TrustTier {
            aivyx_capability::TrustTier::Trusted
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
            self.token.clone()
        }
    }

    /// Build a fresh `ToolContext` from a channel + audit + cancel.
    /// Takes them by reference because the resulting context borrows
    /// them for its lifetime.
    fn make_ctx<'a>(
        channel: &'a NoopChannel,
        audit: &'a dyn aivyx_core::AuditHook,
    ) -> ToolContext<'a> {
        ToolContext {
            agent_id: AgentId::new(),
            session_id: channel.session,
            turn_id: TurnId::new(),
            channel,
            audit,
            cancellation: &channel.token,
            message_origin: aivyx_core::MessageOrigin::Operator,
        }
    }

    fn fresh_channel() -> NoopChannel {
        NoopChannel {
            session: SessionId::new(),
            token: CancellationToken::new(),
        }
    }

    fn fresh_memory() -> Arc<dyn Memory> {
        Arc::new(InMemoryMemory::new())
    }

    // ---- Scope derivation (R1 payoff) ------------------------------

    #[test]
    fn read_scope_is_derived_from_topic() {
        let tool = MemoryReadTool::new(fresh_memory());
        let scope = tool.required_scope(&json!({"topic": "notes"}));
        assert_eq!(scope.base(), "memory.read");
        assert_eq!(scope.qualifier(), Some("topic:notes"));
    }

    #[test]
    fn write_scope_is_derived_from_topic() {
        let tool = MemoryWriteTool::new(fresh_memory());
        let scope =
            tool.required_scope(&json!({"topic": "secrets", "body": "hunter2"}));
        assert_eq!(scope.base(), "memory.write");
        assert_eq!(scope.qualifier(), Some("topic:secrets"));
    }

    #[test]
    fn forget_scope_is_derived_from_topic() {
        let tool = MemoryForgetTool::new(fresh_memory());
        let scope = tool.required_scope(&json!({"topic": "notes"}));
        assert_eq!(scope.base(), "memory.forget");
        assert_eq!(scope.qualifier(), Some("topic:notes"));
    }

    #[test]
    fn read_scope_from_two_different_topics_is_not_equal() {
        // The whole point of deriving the scope from the input: an
        // agent with `memory.read:topic:notes` must not be able to
        // use `memory.read` to peek at topic `secrets`. Scope
        // equality drives that, so this test proves the derivation
        // returns genuinely distinct scopes.
        let tool = MemoryReadTool::new(fresh_memory());
        let a = tool.required_scope(&json!({"topic": "notes"}));
        let b = tool.required_scope(&json!({"topic": "secrets"}));
        assert_ne!(a, b);
    }

    // ---- Deny-scope fallback for malformed input -------------------

    #[test]
    fn read_scope_for_missing_topic_is_deny_scope() {
        let tool = MemoryReadTool::new(fresh_memory());
        let scope = tool.required_scope(&json!({}));
        // Deny-scope qualifier contains a NUL — any real granted
        // scope has a NUL-free qualifier, so equality fails.
        assert!(scope.qualifier().unwrap().contains('\x00'));
    }

    #[test]
    fn read_scope_for_wrong_type_topic_is_deny_scope() {
        let tool = MemoryReadTool::new(fresh_memory());
        let scope = tool.required_scope(&json!({"topic": 42}));
        assert!(scope.qualifier().unwrap().contains('\x00'));
    }

    #[test]
    fn read_scope_for_empty_topic_is_deny_scope() {
        let tool = MemoryReadTool::new(fresh_memory());
        let scope = tool.required_scope(&json!({"topic": ""}));
        assert!(scope.qualifier().unwrap().contains('\x00'));
    }

    #[test]
    fn write_scope_for_missing_topic_is_deny_scope() {
        let tool = MemoryWriteTool::new(fresh_memory());
        let scope = tool.required_scope(&json!({"body": "hi"}));
        assert!(scope.qualifier().unwrap().contains('\x00'));
    }

    #[test]
    fn forget_scope_for_missing_topic_is_deny_scope() {
        let tool = MemoryForgetTool::new(fresh_memory());
        let scope = tool.required_scope(&json!({}));
        assert!(scope.qualifier().unwrap().contains('\x00'));
    }

    // ---- Happy path execution --------------------------------------

    #[tokio::test]
    async fn write_then_read_round_trips_through_the_tool_surface() {
        let mem = fresh_memory();
        let writer = MemoryWriteTool::new(mem.clone());
        let reader = MemoryReadTool::new(mem.clone());
        let chan = fresh_channel();
        let audit = NullAuditHook;

        let ctx = make_ctx(&chan, &audit);
        let write_outcome = writer
            .execute(
                json!({"topic": "notes", "body": "purple"}),
                &ctx,
            )
            .await;
        match write_outcome {
            ToolOutcome::Completed { output, verified } => {
                assert_eq!(output["topic"], "notes");
                assert_eq!(output["seq"], 0);
                assert_eq!(verified, Verification::Verified);
            }
            other => panic!("write should complete, got {other:?}"),
        }

        let ctx = make_ctx(&chan, &audit);
        let read_outcome = reader
            .execute(json!({"topic": "notes"}), &ctx)
            .await;
        match read_outcome {
            ToolOutcome::Completed { output, verified } => {
                assert_eq!(output["topic"], "notes");
                assert_eq!(output["count"], 1);
                assert_eq!(verified, Verification::NotApplicable);
                let entries = output["entries"].as_array().unwrap();
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0]["body"], "purple");
                assert_eq!(entries[0]["seq"], 0);
            }
            other => panic!("read should complete, got {other:?}"),
        }
    }

    // ---- Near-duplicate supersession (dogfood 2026-07-04) ----------

    #[test]
    fn near_duplicate_metric_separates_rewrites_from_distinct_facts() {
        // The dogfood case: the same fact rewritten with a citation
        // appended is a near-duplicate…
        let plain = "Short walking breaks during the workday improve \
                     focus and reduce fatigue.";
        let cited = "Short walking breaks during the workday improve \
                     focus and reduce fatigue (PMCID: PMC9432722).";
        assert!(is_near_duplicate_of(plain, cited));
        // …and punctuation/case tweaks don't count as new information.
        assert!(is_near_duplicate_of(plain, &plain.to_uppercase()));
        // But a one-word factual difference is a DISTINCT fact.
        assert!(!is_near_duplicate_of(
            "Meeting with Bob on Tuesday at 3pm about the roadmap",
            "Meeting with Bob on Thursday at 3pm about the roadmap",
        ));
        // A tiny entry is never swallowed by a longer one that merely
        // mentions its words.
        assert!(!is_near_duplicate_of(
            "buy milk",
            "buy milk, eggs, flour, butter, and a birthday card for Sam",
        ));
        assert!(!is_near_duplicate_of("", "anything"));
    }

    #[tokio::test]
    async fn write_supersedes_a_near_duplicate_rewrite() {
        let mem = fresh_memory();
        let writer = MemoryWriteTool::new(mem.clone());
        let chan = fresh_channel();
        let audit = NullAuditHook;

        let plain = "Short walking breaks during the workday improve \
                     focus and reduce fatigue.";
        let cited = "Short walking breaks during the workday improve \
                     focus and reduce fatigue (PMCID: PMC9432722).";

        let ctx = make_ctx(&chan, &audit);
        writer
            .execute(json!({"topic": "movement-tips", "body": plain}), &ctx)
            .await;
        let ctx = make_ctx(&chan, &audit);
        let outcome = writer
            .execute(json!({"topic": "movement-tips", "body": cited}), &ctx)
            .await;

        // The rewrite reports what it replaced…
        match outcome {
            ToolOutcome::Completed { output, verified } => {
                assert_eq!(output["seq"], 1);
                assert_eq!(output["superseded"], json!([0]));
                assert_eq!(verified, Verification::Verified);
            }
            other => panic!("write should complete, got {other:?}"),
        }
        // …and only the refined entry remains in the topic. (No
        // `session` in the input ⇒ physical topic = logical topic.)
        let entries = mem.get_recent("movement-tips", 16).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].body, cited);
    }

    #[tokio::test]
    async fn write_keeps_distinct_facts_side_by_side() {
        let mem = fresh_memory();
        let writer = MemoryWriteTool::new(mem.clone());
        let chan = fresh_channel();
        let audit = NullAuditHook;

        let ctx = make_ctx(&chan, &audit);
        writer
            .execute(
                json!({"topic": "notes", "body": "Meeting with Bob on Tuesday at 3pm about the roadmap"}),
                &ctx,
            )
            .await;
        let ctx = make_ctx(&chan, &audit);
        let outcome = writer
            .execute(
                json!({"topic": "notes", "body": "Meeting with Bob on Thursday at 3pm about the roadmap"}),
                &ctx,
            )
            .await;

        // No supersession — the output keeps its pre-fix shape…
        match outcome {
            ToolOutcome::Completed { output, .. } => {
                assert!(output.get("superseded").is_none());
            }
            other => panic!("write should complete, got {other:?}"),
        }
        // …and both facts survive.
        assert_eq!(mem.get_recent("notes", 16).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn read_of_unknown_topic_returns_empty_list_not_error() {
        let reader = MemoryReadTool::new(fresh_memory());
        let chan = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&chan, &audit);

        let outcome = reader
            .execute(json!({"topic": "never-written"}), &ctx)
            .await;
        match outcome {
            ToolOutcome::Completed { output, verified } => {
                assert_eq!(output["count"], 0);
                assert!(output["entries"].as_array().unwrap().is_empty());
                assert_eq!(verified, Verification::NotApplicable);
            }
            other => panic!("unknown-topic read must still Complete, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn read_limit_defaults_and_is_capped() {
        // Seed strictly more than MAX_READ_LIMIT so the clamp
        // assertion has enough supply to actually saturate. With 50
        // seeded entries and a 10_000-limit request, the clamp would
        // fire but `get_recent` would still only return 50 — the
        // assertion would pass for the wrong reason (not enough data
        // to notice the clamp).
        let mem = fresh_memory();
        for i in 0..(MAX_READ_LIMIT + 16) {
            mem.put("notes", &format!("entry {i}")).await.unwrap();
        }
        let reader = MemoryReadTool::new(mem);
        let chan = fresh_channel();
        let audit = NullAuditHook;

        // Default limit (no `limit` field) -> DEFAULT_READ_LIMIT.
        let ctx = make_ctx(&chan, &audit);
        let out = reader.execute(json!({"topic": "notes"}), &ctx).await;
        if let ToolOutcome::Completed { output, .. } = out {
            assert_eq!(output["count"], DEFAULT_READ_LIMIT);
        } else {
            panic!("default-limit read should Complete");
        }

        // Explicit limit below cap.
        let ctx = make_ctx(&chan, &audit);
        let out = reader
            .execute(json!({"topic": "notes", "limit": 5}), &ctx)
            .await;
        if let ToolOutcome::Completed { output, .. } = out {
            assert_eq!(output["count"], 5);
        } else {
            panic!("explicit-limit read should Complete");
        }

        // Explicit limit above MAX_READ_LIMIT — clamped.
        let ctx = make_ctx(&chan, &audit);
        let out = reader
            .execute(json!({"topic": "notes", "limit": 10_000}), &ctx)
            .await;
        if let ToolOutcome::Completed { output, .. } = out {
            assert_eq!(output["count"], MAX_READ_LIMIT);
        } else {
            panic!("clamped-limit read should Complete");
        }
    }

    #[tokio::test]
    async fn forget_clears_topic_and_reports_verified() {
        let mem = fresh_memory();
        mem.put("notes", "x").await.unwrap();
        mem.put("notes", "y").await.unwrap();
        mem.put("todos", "z").await.unwrap();

        let forget = MemoryForgetTool::new(mem.clone());
        let chan = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&chan, &audit);

        let outcome = forget.execute(json!({"topic": "notes"}), &ctx).await;
        match outcome {
            ToolOutcome::Completed { output, verified } => {
                assert_eq!(output["deleted"], 2);
                assert_eq!(verified, Verification::Verified);
            }
            other => panic!("forget should Complete, got {other:?}"),
        }
        // And the other topic survived.
        assert_eq!(mem.get_recent("todos", 10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn forget_of_unknown_topic_reports_zero_and_verified() {
        let forget = MemoryForgetTool::new(fresh_memory());
        let chan = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&chan, &audit);

        let outcome = forget.execute(json!({"topic": "nothing"}), &ctx).await;
        match outcome {
            ToolOutcome::Completed { output, verified } => {
                assert_eq!(output["deleted"], 0);
                assert_eq!(verified, Verification::Verified);
            }
            other => panic!("unknown-topic forget should Complete, got {other:?}"),
        }
    }

    // ---- Malformed input paths through execute ---------------------

    #[tokio::test]
    async fn write_missing_body_fails_loudly() {
        let writer = MemoryWriteTool::new(fresh_memory());
        let chan = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&chan, &audit);

        // Topic is present (so the scope gate would admit the call),
        // but body is missing. Execute should return Failed.
        let outcome = writer.execute(json!({"topic": "notes"}), &ctx).await;
        assert!(matches!(
            outcome,
            ToolOutcome::Failed(AivyxError::Tool { .. })
        ));
    }

    #[tokio::test]
    async fn read_missing_topic_in_execute_is_internal_error() {
        // This path is unreachable through the real turn loop — the
        // scope gate would deny the call. The test proves that *if*
        // the gate is ever bypassed, execute still fails closed with
        // an Internal error (not a silent success), matching
        // FsReadTool's same invariant.
        let reader = MemoryReadTool::new(fresh_memory());
        let chan = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&chan, &audit);

        let outcome = reader.execute(json!({}), &ctx).await;
        assert!(matches!(
            outcome,
            ToolOutcome::Failed(AivyxError::Internal(_))
        ));
    }

    // ---- Tool metadata sanity --------------------------------------

    #[test]
    fn tool_names_are_the_expected_d4_strings() {
        // D4 scope taxonomy lists these exact base strings. Tool
        // names should match so registry lookups are obvious.
        assert_eq!(MemoryReadTool::new(fresh_memory()).name(), "memory.read");
        assert_eq!(MemoryWriteTool::new(fresh_memory()).name(), "memory.write");
        assert_eq!(
            MemoryForgetTool::new(fresh_memory()).name(),
            "memory.forget"
        );
    }

    // ---- Phase 7 task 5: per-topic GC tripwire ---------------------

    #[tokio::test]
    async fn write_at_cap_still_succeeds() {
        // With `max_per_topic = 3` and two existing entries, the next
        // write is still legal: `existing.len() == 2 < 3`, so the
        // tripwire does not fire. This is the boundary case — one
        // more write takes the topic *exactly to* the cap, and the
        // next one after that is the first one that must fail.
        let mem = fresh_memory();
        mem.put("notes", "first").await.unwrap();
        mem.put("notes", "second").await.unwrap();

        let writer = MemoryWriteTool::new(mem.clone()).set_max_per_topic(3);
        let chan = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&chan, &audit);

        let outcome = writer
            .execute(json!({"topic": "notes", "body": "third"}), &ctx)
            .await;
        match outcome {
            ToolOutcome::Completed { output, verified } => {
                assert_eq!(output["topic"], "notes");
                assert_eq!(output["seq"], 2);
                assert_eq!(verified, Verification::Verified);
            }
            other => panic!("at-cap write should Complete, got {other:?}"),
        }
        // And the topic is now exactly at cap — the next write must
        // refuse.
        assert_eq!(mem.get_recent("notes", 10).await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn write_over_cap_fails_with_tool_error_naming_topic_and_cap() {
        let mem = fresh_memory();
        for i in 0..3 {
            mem.put("notes", &format!("entry {i}")).await.unwrap();
        }

        let writer = MemoryWriteTool::new(mem.clone()).set_max_per_topic(3);
        let chan = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&chan, &audit);

        let outcome = writer
            .execute(
                json!({"topic": "notes", "body": "one too many"}),
                &ctx,
            )
            .await;
        match outcome {
            ToolOutcome::Failed(AivyxError::Tool { tool, detail }) => {
                assert_eq!(tool, writer.id());
                // Recovery hint must name the topic (so the agent knows
                // *which* forget call to make) and the cap (so the
                // agent has grounds to tell the user this was a
                // configured tripwire, not substrate failure).
                assert!(
                    detail.contains("notes"),
                    "detail should name topic, got {detail}"
                );
                assert!(
                    detail.contains('3'),
                    "detail should mention the cap, got {detail}"
                );
                assert!(
                    detail.contains("memory.forget"),
                    "detail should suggest memory.forget, got {detail}"
                );
            }
            other => panic!("over-cap write must Fail with Tool error, got {other:?}"),
        }
        // The refused write must not have landed.
        assert_eq!(mem.get_recent("notes", 10).await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn forget_clears_the_cap_so_next_write_succeeds() {
        // A full topic + forget + retry is the agent's documented
        // recovery path. This test walks it end-to-end through the
        // tool surface, to prove the detail message's advice actually
        // works.
        let mem = fresh_memory();
        for i in 0..3 {
            mem.put("notes", &format!("entry {i}")).await.unwrap();
        }

        let writer = MemoryWriteTool::new(mem.clone()).set_max_per_topic(3);
        let forget = MemoryForgetTool::new(mem.clone());
        let chan = fresh_channel();
        let audit = NullAuditHook;

        // Sanity: the next write fails.
        let ctx = make_ctx(&chan, &audit);
        let first = writer
            .execute(json!({"topic": "notes", "body": "blocked"}), &ctx)
            .await;
        assert!(matches!(first, ToolOutcome::Failed(_)));

        // Recovery: forget the topic.
        let ctx = make_ctx(&chan, &audit);
        let cleared = forget.execute(json!({"topic": "notes"}), &ctx).await;
        match cleared {
            ToolOutcome::Completed { output, .. } => {
                assert_eq!(output["deleted"], 3);
            }
            other => panic!("forget should Complete, got {other:?}"),
        }

        // Retry: the same write now succeeds.
        let ctx = make_ctx(&chan, &audit);
        let retry = writer
            .execute(json!({"topic": "notes", "body": "blocked"}), &ctx)
            .await;
        match retry {
            ToolOutcome::Completed { output, verified } => {
                assert_eq!(output["topic"], "notes");
                assert_eq!(verified, Verification::Verified);
            }
            other => panic!("retry after forget should Complete, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cap_is_per_topic_not_global() {
        // Fill topic `notes` to cap, then prove that `todos` — a
        // different topic — still accepts writes under the same tool
        // instance. This pins the cap as per-*qualifier*, matching
        // the scope model where `memory.write:topic:notes` and
        // `memory.write:topic:todos` are distinct capabilities.
        let mem = fresh_memory();
        for i in 0..3 {
            mem.put("notes", &format!("note {i}")).await.unwrap();
        }

        let writer = MemoryWriteTool::new(mem.clone()).set_max_per_topic(3);
        let chan = fresh_channel();
        let audit = NullAuditHook;

        let ctx = make_ctx(&chan, &audit);
        let blocked = writer
            .execute(json!({"topic": "notes", "body": "nope"}), &ctx)
            .await;
        assert!(matches!(blocked, ToolOutcome::Failed(_)));

        let ctx = make_ctx(&chan, &audit);
        let ok = writer
            .execute(json!({"topic": "todos", "body": "unrelated"}), &ctx)
            .await;
        match ok {
            ToolOutcome::Completed { output, verified } => {
                assert_eq!(output["topic"], "todos");
                assert_eq!(verified, Verification::Verified);
            }
            other => panic!("other-topic write should Complete, got {other:?}"),
        }
    }

    #[test]
    fn tool_ids_are_unique_per_construction() {
        let a = MemoryReadTool::new(fresh_memory()).id();
        let b = MemoryReadTool::new(fresh_memory()).id();
        assert_ne!(a, b, "ToolId::new must mint fresh UUIDs");
    }

    // ---- Phase 8 task 2: session-scoped qualifier derivation ------

    #[test]
    fn read_scope_with_session_yields_both_qualifiers() {
        // With the turn loop's injected `session` field, the derived
        // scope gains a trailing `:session:<id>` qualifier. This is
        // what lets a capability bundle for chat A
        // (`memory.read:topic:notes:session:A`) fail to grant a read
        // attempt from chat B.
        let tool = MemoryReadTool::new(fresh_memory());
        let scope = tool.required_scope(&json!({
            "topic": "notes",
            "session": "12345",
        }));
        assert_eq!(scope.base(), "memory.read");
        assert_eq!(scope.qualifier(), Some("topic:notes:session:12345"));
    }

    #[test]
    fn read_scopes_for_same_topic_different_sessions_are_distinct() {
        let tool = MemoryReadTool::new(fresh_memory());
        let a = tool.required_scope(&json!({"topic": "notes", "session": "A"}));
        let b = tool.required_scope(&json!({"topic": "notes", "session": "B"}));
        assert_ne!(
            a, b,
            "session qualifier must distinguish otherwise-identical reads"
        );
    }

    #[test]
    fn write_scope_with_session_is_session_qualified() {
        let tool = MemoryWriteTool::new(fresh_memory());
        let scope = tool.required_scope(&json!({
            "topic": "secrets",
            "body": "x",
            "session": "chatA",
        }));
        assert_eq!(scope.qualifier(), Some("topic:secrets:session:chatA"));
    }

    #[test]
    fn forget_scope_with_session_is_session_qualified() {
        let tool = MemoryForgetTool::new(fresh_memory());
        let scope = tool.required_scope(&json!({
            "topic": "notes",
            "session": "chatB",
        }));
        assert_eq!(scope.qualifier(), Some("topic:notes:session:chatB"));
    }

    #[test]
    fn empty_session_is_treated_as_none_for_scope_derivation() {
        // The injection point sends `""` only if a channel override
        // returns `Some("")`, which would be a channel bug — but we
        // still want the tool to degrade to unsession-qualified rather
        // than emitting a scope like `memory.read:topic:notes:session:`
        // that no capability bundle would ever grant.
        let tool = MemoryReadTool::new(fresh_memory());
        let scope = tool.required_scope(&json!({"topic": "notes", "session": ""}));
        assert_eq!(scope.qualifier(), Some("topic:notes"));
    }

    #[test]
    fn reserved_prefix_topic_is_denied_even_with_session() {
        // A topic that literally starts with `\x01` would let an
        // agent side-door into another chat's physical key. Denied
        // regardless of whether `session` is present.
        let tool = MemoryReadTool::new(fresh_memory());
        let scope = tool.required_scope(&json!({
            "topic": "\x01s\x01other\x01notes",
            "session": "mine",
        }));
        assert!(scope.qualifier().unwrap().contains('\x00'));
    }

    /// Audit P9: a newline (or any control character) in a topic is refused
    /// at the gate, so it can't reach the auto-recalled prompt block.
    #[test]
    fn write_refuses_a_topic_with_control_characters() {
        let tool = MemoryWriteTool::new(fresh_memory());
        for bad in ["notes\n## Standing instructions", "a\rb", "tab\there", "bell\x07"] {
            let scope = tool.required_scope(&json!({"topic": bad, "body": "b"}));
            assert!(scope.qualifier().unwrap().contains('\x00'), "{bad:?} was admitted");
        }
        let ok = tool.required_scope(&json!({"topic": "meeting notes — Q3", "body": "b"}));
        assert!(!ok.qualifier().unwrap().contains('\x00'));
    }

    // ---- Phase 8 task 2: physical isolation at the substrate ------

    #[tokio::test]
    async fn writes_in_different_sessions_do_not_share_storage() {
        // This is the whole point of Option B. Two sessions writing to
        // the same *logical* topic `notes` must land under different
        // physical keys so that a read from session A cannot observe
        // session B's body.
        let mem = fresh_memory();
        let writer = MemoryWriteTool::new(mem.clone());
        let reader = MemoryReadTool::new(mem.clone());
        let chan = fresh_channel();
        let audit = NullAuditHook;

        // Session A writes "purple" to `notes`.
        let ctx = make_ctx(&chan, &audit);
        let _ = writer
            .execute(
                json!({"topic": "notes", "body": "purple", "session": "A"}),
                &ctx,
            )
            .await;

        // Session B writes "green" to `notes`.
        let ctx = make_ctx(&chan, &audit);
        let _ = writer
            .execute(
                json!({"topic": "notes", "body": "green", "session": "B"}),
                &ctx,
            )
            .await;

        // Session A reads back — sees only purple.
        let ctx = make_ctx(&chan, &audit);
        let a_out = reader
            .execute(json!({"topic": "notes", "session": "A"}), &ctx)
            .await;
        let ToolOutcome::Completed { output, .. } = a_out else {
            panic!("session A read should Complete");
        };
        let entries_a = output["entries"].as_array().unwrap();
        assert_eq!(entries_a.len(), 1);
        assert_eq!(entries_a[0]["body"], "purple");
        // And the agent-visible topic is the logical name, not the
        // physical namespaced one.
        assert_eq!(entries_a[0]["topic"], "notes");
        assert_eq!(output["topic"], "notes");

        // Session B reads back — sees only green.
        let ctx = make_ctx(&chan, &audit);
        let b_out = reader
            .execute(json!({"topic": "notes", "session": "B"}), &ctx)
            .await;
        let ToolOutcome::Completed { output, .. } = b_out else {
            panic!("session B read should Complete");
        };
        let entries_b = output["entries"].as_array().unwrap();
        assert_eq!(entries_b.len(), 1);
        assert_eq!(entries_b[0]["body"], "green");
        assert_eq!(entries_b[0]["topic"], "notes");
    }

    #[tokio::test]
    async fn forget_in_one_session_does_not_clear_another() {
        let mem = fresh_memory();
        let writer = MemoryWriteTool::new(mem.clone());
        let forget = MemoryForgetTool::new(mem.clone());
        let reader = MemoryReadTool::new(mem.clone());
        let chan = fresh_channel();
        let audit = NullAuditHook;

        let ctx = make_ctx(&chan, &audit);
        let _ = writer
            .execute(
                json!({"topic": "notes", "body": "keep me", "session": "A"}),
                &ctx,
            )
            .await;
        let ctx = make_ctx(&chan, &audit);
        let _ = writer
            .execute(
                json!({"topic": "notes", "body": "me too", "session": "B"}),
                &ctx,
            )
            .await;

        // B forgets — A's entry must survive.
        let ctx = make_ctx(&chan, &audit);
        let cleared = forget
            .execute(json!({"topic": "notes", "session": "B"}), &ctx)
            .await;
        match cleared {
            ToolOutcome::Completed { output, .. } => {
                assert_eq!(output["deleted"], 1);
            }
            other => panic!("forget B should Complete, got {other:?}"),
        }

        let ctx = make_ctx(&chan, &audit);
        let a_out = reader
            .execute(json!({"topic": "notes", "session": "A"}), &ctx)
            .await;
        if let ToolOutcome::Completed { output, .. } = a_out {
            let entries = output["entries"].as_array().unwrap();
            assert_eq!(entries.len(), 1, "session A should be untouched");
            assert_eq!(entries[0]["body"], "keep me");
        } else {
            panic!("session A read should still Complete");
        }
    }

    #[tokio::test]
    async fn per_topic_cap_is_per_session() {
        // Filling session A's `notes` to cap must not refuse session
        // B's unrelated `notes` writes.
        let mem = fresh_memory();
        let writer = MemoryWriteTool::new(mem.clone()).set_max_per_topic(2);
        let chan = fresh_channel();
        let audit = NullAuditHook;

        // A fills to cap.
        for i in 0..2 {
            let ctx = make_ctx(&chan, &audit);
            let out = writer
                .execute(
                    json!({
                        "topic": "notes",
                        "body": format!("a{i}"),
                        "session": "A",
                    }),
                    &ctx,
                )
                .await;
            assert!(matches!(out, ToolOutcome::Completed { .. }));
        }

        // A's next write is refused.
        let ctx = make_ctx(&chan, &audit);
        let blocked = writer
            .execute(
                json!({"topic": "notes", "body": "over", "session": "A"}),
                &ctx,
            )
            .await;
        assert!(matches!(blocked, ToolOutcome::Failed(_)));

        // But B's first write still succeeds — different bucket.
        let ctx = make_ctx(&chan, &audit);
        let ok = writer
            .execute(
                json!({"topic": "notes", "body": "b0", "session": "B"}),
                &ctx,
            )
            .await;
        match ok {
            ToolOutcome::Completed { output, verified } => {
                assert_eq!(output["topic"], "notes");
                assert_eq!(verified, Verification::Verified);
            }
            other => panic!("B's first write should Complete, got {other:?}"),
        }
    }

    // ---- Phase 10 task 1: cross-topic `memory.read` wildcard -------
    //
    // These tests cover the `{"topics": "*"}` shape end-to-end at the
    // tool layer. The substrate primitive (`Memory::scan_prefix`) has
    // its own tests in `lib.rs` and `redb.rs`; this suite proves the
    // tool-layer plumbing: classifier, scope derivation, session
    // namespacing, and the grouped output shape.

    #[tokio::test]
    async fn wildcard_read_fans_out_across_topics_in_one_session() {
        let mem = fresh_memory();
        let writer = MemoryWriteTool::new(mem.clone());
        let reader = MemoryReadTool::new(mem.clone());
        let chan = fresh_channel();
        let audit = NullAuditHook;

        // Two different topics, same session.
        let ctx = make_ctx(&chan, &audit);
        let _ = writer
            .execute(
                json!({"topic": "notes", "body": "purple", "session": "A"}),
                &ctx,
            )
            .await;
        let ctx = make_ctx(&chan, &audit);
        let _ = writer
            .execute(
                json!({"topic": "todos", "body": "ship it", "session": "A"}),
                &ctx,
            )
            .await;

        let ctx = make_ctx(&chan, &audit);
        let out = reader
            .execute(json!({"topics": "*", "session": "A"}), &ctx)
            .await;
        match out {
            ToolOutcome::Completed { output, verified } => {
                assert_eq!(verified, Verification::NotApplicable);
                assert_eq!(output["topic_count"], 2);
                let topics = output["topics"].as_array().unwrap();
                // Logical topic names — no session prefix bleed-through.
                let names: Vec<&str> =
                    topics.iter().map(|t| t["topic"].as_str().unwrap()).collect();
                assert!(names.contains(&"notes"));
                assert!(names.contains(&"todos"));
                // Each topic carries its own entries array with the
                // matching body.
                for topic in topics {
                    let entries = topic["entries"].as_array().unwrap();
                    assert_eq!(entries.len(), 1);
                    let body = entries[0]["body"].as_str().unwrap();
                    assert!(body == "purple" || body == "ship it");
                    // The entry's topic field must also be the logical
                    // name — task 1 strips the session prefix on the
                    // way out in both the group object and the per-
                    // entry topic.
                    let entry_topic = entries[0]["topic"].as_str().unwrap();
                    assert!(entry_topic == "notes" || entry_topic == "todos");
                }
            }
            other => panic!("wildcard read should Complete, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn wildcard_read_hides_internal_pruned_topics() {
        // #G — a wildcard recall must not surface the daemon's internal
        // `context:pruned:*` bookkeeping archives (no session → empty scan
        // prefix → "every topic", the prune-sink's single-partition case).
        let mem = fresh_memory();
        let writer = MemoryWriteTool::new(mem.clone());
        let reader = MemoryReadTool::new(mem.clone());
        let chan = fresh_channel();
        let audit = NullAuditHook;

        let ctx = make_ctx(&chan, &audit);
        let _ = writer
            .execute(json!({"topic": "notes", "body": "purple"}), &ctx)
            .await;
        // The prune sink writes this topic directly (no session).
        mem.put("context:pruned:sess-1", "42 messages pruned")
            .await
            .unwrap();

        let ctx = make_ctx(&chan, &audit);
        let out = reader.execute(json!({"topics": "*"}), &ctx).await;
        match out {
            ToolOutcome::Completed { output, .. } => {
                let names: Vec<&str> = output["topics"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|t| t["topic"].as_str().unwrap())
                    .collect();
                assert!(names.contains(&"notes"), "real topic present: {names:?}");
                assert!(
                    !names.iter().any(|n| n.starts_with("context:pruned:")),
                    "internal prune topic must be hidden: {names:?}"
                );
            }
            other => panic!("wildcard read should Complete, got {other:?}"),
        }
        // But it is still reachable by an explicit single-topic read.
        let ctx = make_ctx(&chan, &audit);
        let out = reader
            .execute(json!({"topic": "context:pruned:sess-1"}), &ctx)
            .await;
        assert!(
            matches!(out, ToolOutcome::Completed { .. }),
            "explicit single-topic read of an internal topic still resolves"
        );
    }

    #[tokio::test]
    async fn wildcard_read_is_session_scoped() {
        // Sessions A and B each have a `notes` topic. A wildcard read
        // under session A must only return A's `notes`, never B's.
        // This is the core isolation guarantee: the scan prefix on
        // `scan_prefix` is the namespaced session prefix, not the
        // empty string.
        let mem = fresh_memory();
        let writer = MemoryWriteTool::new(mem.clone());
        let reader = MemoryReadTool::new(mem.clone());
        let chan = fresh_channel();
        let audit = NullAuditHook;

        let ctx = make_ctx(&chan, &audit);
        let _ = writer
            .execute(
                json!({"topic": "notes", "body": "a-only", "session": "A"}),
                &ctx,
            )
            .await;
        let ctx = make_ctx(&chan, &audit);
        let _ = writer
            .execute(
                json!({"topic": "notes", "body": "b-only", "session": "B"}),
                &ctx,
            )
            .await;

        let ctx = make_ctx(&chan, &audit);
        let out = reader
            .execute(json!({"topics": "*", "session": "A"}), &ctx)
            .await;
        if let ToolOutcome::Completed { output, .. } = out {
            assert_eq!(output["topic_count"], 1);
            let topics = output["topics"].as_array().unwrap();
            assert_eq!(topics[0]["topic"], "notes");
            let entries = topics[0]["entries"].as_array().unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0]["body"], "a-only");
        } else {
            panic!("wildcard read under A should Complete");
        }
    }

    #[tokio::test]
    async fn wildcard_read_respects_per_topic_default_limit() {
        // Seed one topic with more entries than the default wildcard
        // limit. The single-topic default (DEFAULT_READ_LIMIT = 16) is
        // higher than DEFAULT_WILDCARD_READ_LIMIT, so this test also
        // proves the wildcard path uses its own, smaller default.
        let mem = fresh_memory();
        let writer = MemoryWriteTool::new(mem.clone());
        let reader = MemoryReadTool::new(mem.clone());
        let chan = fresh_channel();
        let audit = NullAuditHook;

        for i in 0..(DEFAULT_WILDCARD_READ_LIMIT + 4) {
            let ctx = make_ctx(&chan, &audit);
            let _ = writer
                .execute(
                    json!({
                        "topic": "notes",
                        "body": format!("n{i}"),
                        "session": "A",
                    }),
                    &ctx,
                )
                .await;
        }

        let ctx = make_ctx(&chan, &audit);
        let out = reader
            .execute(json!({"topics": "*", "session": "A"}), &ctx)
            .await;
        if let ToolOutcome::Completed { output, .. } = out {
            let topics = output["topics"].as_array().unwrap();
            assert_eq!(topics.len(), 1);
            let entries = topics[0]["entries"].as_array().unwrap();
            assert_eq!(
                entries.len(),
                DEFAULT_WILDCARD_READ_LIMIT,
                "wildcard default must cap each topic at DEFAULT_WILDCARD_READ_LIMIT"
            );
        } else {
            panic!("wildcard read should Complete");
        }
    }

    // ---- Phase 10 task 1: scope derivation for wildcard -----------

    #[test]
    fn wildcard_read_scope_has_wildcard_topic_qualifier() {
        let tool = MemoryReadTool::new(fresh_memory());
        let scope = tool.required_scope(&json!({"topics": "*", "session": "A"}));
        assert_eq!(scope.base(), "memory.read");
        // The qualifier must be the wildcard form — glob matching in
        // `aivyx-capability` turns `topic:*:session:A` into a grant
        // that covers `topic:<anything>:session:A`, but the needed
        // scope itself is the literal wildcard, so an agent that only
        // holds `memory.read:topic:notes:session:A` is NOT granted.
        assert_eq!(scope.qualifier(), Some("topic:*:session:A"));
    }

    #[test]
    fn wildcard_read_scope_is_distinct_from_single_topic_scope() {
        let tool = MemoryReadTool::new(fresh_memory());
        let single = tool.required_scope(&json!({"topic": "notes", "session": "A"}));
        let wild = tool.required_scope(&json!({"topics": "*", "session": "A"}));
        assert_ne!(
            single, wild,
            "cross-topic read must be a strictly different scope"
        );
    }

    #[test]
    fn literal_topic_star_is_still_a_single_topic_read() {
        // `topic: "*"` is a legal literal topic at the substrate
        // layer — the classifier routes through `topic_from_input`,
        // which does not special-case `*`. Only the `topics` field
        // name triggers wildcard semantics. This is a regression
        // lock: if someone ever "simplifies" the classifier into
        // matching any `*` value, cross-topic read silently becomes
        // reachable through a scope that only granted a single
        // literal topic.
        let tool = MemoryReadTool::new(fresh_memory());
        let scope = tool.required_scope(&json!({"topic": "*", "session": "A"}));
        assert_eq!(scope.base(), "memory.read");
        // This is the wildcard-literal-string qualifier — at the
        // capability layer `topic:*` happens to be a valid glob and
        // would *also* grant every topic, which is why the classifier
        // and the grant surface must stay decoupled. The point of the
        // test is that this scope came from the **single**-topic path,
        // so its shape equals the one for a normal literal topic
        // (topic:<literal>:session:<s>) — not the separate wildcard
        // shape that task 1 introduced.
        assert_eq!(scope.qualifier(), Some("topic:*:session:A"));
    }

    #[test]
    fn read_scope_for_both_topic_and_topics_set_is_deny_scope() {
        // Ambiguous input: both fields present at once. The
        // classifier returns `Invalid`, which maps to the deny scope.
        // This matters because otherwise the tool surface has two
        // ways to spell the same request and an attacker can pick the
        // one whose required-scope check they happen to hold.
        let tool = MemoryReadTool::new(fresh_memory());
        let scope = tool.required_scope(&json!({
            "topic": "notes",
            "topics": "*",
            "session": "A",
        }));
        assert!(scope.qualifier().unwrap().contains('\x00'));
    }

    #[test]
    fn read_scope_for_topics_wrong_value_is_deny_scope() {
        // `topics` is a sentinel field whose only legal value is the
        // literal string `"*"`. Any other value — a list of names, a
        // boolean, an empty string — must fall through to deny. Lock
        // that in so a future "convenience" extension to accept e.g.
        // `topics: ["a", "b"]` has to walk past this test.
        let tool = MemoryReadTool::new(fresh_memory());
        let scope = tool.required_scope(&json!({"topics": ["notes", "todos"]}));
        assert!(scope.qualifier().unwrap().contains('\x00'));

        let scope = tool.required_scope(&json!({"topics": ""}));
        assert!(scope.qualifier().unwrap().contains('\x00'));

        let scope = tool.required_scope(&json!({"topics": "notes"}));
        assert!(scope.qualifier().unwrap().contains('\x00'));
    }

    #[tokio::test]
    async fn wildcard_read_with_no_session_returns_empty_substrate_is_empty() {
        // Single-partition caller (no `session` field). The scan
        // prefix is empty, which means "every topic in the
        // substrate." An empty substrate still returns `topic_count
        // 0` — same as a single-topic read of an unknown topic.
        let reader = MemoryReadTool::new(fresh_memory());
        let chan = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&chan, &audit);

        let out = reader.execute(json!({"topics": "*"}), &ctx).await;
        if let ToolOutcome::Completed { output, verified } = out {
            assert_eq!(output["topic_count"], 0);
            assert!(output["topics"].as_array().unwrap().is_empty());
            assert_eq!(verified, Verification::NotApplicable);
        } else {
            panic!("empty-substrate wildcard read should Complete");
        }
    }

    // ---- Phase 11 Task 2 — role-aware memory topic prefixing -------
    //
    // These tests simulate what `agent.rs::run_tool_call` does at
    // dispatch time: it injects a reserved `"role_prefix"` key into
    // the tool input *after* JSON-schema validation and *alongside*
    // session-partition injection, taking its value from
    // `cfg.roles[active_role].memory_topic_prefix`. Here we inject
    // the same key directly via `json!` so the unit tests stay at
    // microsecond speed and do not need a full turn loop.
    //
    // The guarantee being tested is that two roles writing to the
    // same logical topic in the same session get isolated stores —
    // the whole point of the role namespace — and that the no-
    // prefix path preserves Phase 6–10 behavior byte-for-byte.

    #[tokio::test]
    async fn two_roles_same_topic_see_isolated_stores() {
        let mem = fresh_memory();
        let writer = MemoryWriteTool::new(mem.clone());
        let reader = MemoryReadTool::new(mem.clone());
        let channel = fresh_channel();
        let audit = Arc::new(NullAuditHook);
        let ctx = make_ctx(&channel, audit.as_ref());

        // Role `coder` writes "red" to logical `notes`.
        let _ = writer
            .execute(
                json!({
                    "topic": "notes",
                    "body": "red",
                    "role_prefix": "coder/",
                }),
                &ctx,
            )
            .await;

        // Role `researcher` writes "blue" to the same logical
        // `notes`. In Phase 10 these would have collided in the
        // single `notes` bucket; with role-aware prefixing they
        // land in `coder/notes` and `researcher/notes`
        // respectively.
        let _ = writer
            .execute(
                json!({
                    "topic": "notes",
                    "body": "blue",
                    "role_prefix": "researcher/",
                }),
                &ctx,
            )
            .await;

        // Read from the coder role — should see only "red".
        let out = reader
            .execute(
                json!({"topic": "notes", "role_prefix": "coder/"}),
                &ctx,
            )
            .await;
        if let ToolOutcome::Completed { output, .. } = out {
            assert_eq!(output["count"], 1);
            assert_eq!(output["entries"][0]["body"], "red");
            // Logical topic on the way out is bare `notes`, not
            // `coder/notes` — the prefix is invisible to the model.
            assert_eq!(output["topic"], "notes");
        } else {
            panic!("coder read should Complete");
        }

        // Read from the researcher role — should see only "blue".
        let out = reader
            .execute(
                json!({"topic": "notes", "role_prefix": "researcher/"}),
                &ctx,
            )
            .await;
        if let ToolOutcome::Completed { output, .. } = out {
            assert_eq!(output["count"], 1);
            assert_eq!(output["entries"][0]["body"], "blue");
            assert_eq!(output["topic"], "notes");
        } else {
            panic!("researcher read should Complete");
        }
    }

    #[tokio::test]
    async fn absent_role_prefix_preserves_phase10_behavior() {
        // A read with no `role_prefix` injected (or an empty one)
        // must see exactly the entries that a Phase 10 read would
        // have seen: no namespacing, no hidden prefix. This is the
        // backwards-compat anchor for every existing test that
        // omits the field.
        let mem = fresh_memory();
        let writer = MemoryWriteTool::new(mem.clone());
        let reader = MemoryReadTool::new(mem.clone());
        let channel = fresh_channel();
        let audit = Arc::new(NullAuditHook);
        let ctx = make_ctx(&channel, audit.as_ref());

        // Legacy write (no role_prefix): stored under logical
        // `notes` exactly as Phase 10 would have done.
        let _ = writer
            .execute(
                json!({"topic": "notes", "body": "legacy entry"}),
                &ctx,
            )
            .await;

        // Read without role_prefix sees it.
        let out = reader
            .execute(json!({"topic": "notes"}), &ctx)
            .await;
        if let ToolOutcome::Completed { output, .. } = out {
            assert_eq!(output["count"], 1);
            assert_eq!(output["entries"][0]["body"], "legacy entry");
        } else {
            panic!("legacy read should Complete");
        }

        // Read WITH a role_prefix does NOT see the legacy entry,
        // because the legacy entry lives at bare `notes` and the
        // roled read looks under `coder/notes`. Crucially this
        // also means a Phase 11 role does not silently inherit a
        // Phase 10 legacy store — it starts empty.
        let out = reader
            .execute(
                json!({"topic": "notes", "role_prefix": "coder/"}),
                &ctx,
            )
            .await;
        if let ToolOutcome::Completed { output, .. } = out {
            assert_eq!(output["count"], 0);
        } else {
            panic!("roled read of legacy store should Complete empty");
        }
    }

    #[tokio::test]
    async fn empty_role_prefix_string_falls_through_to_no_prefix() {
        // `role_prefix_from_input` normalizes `""` to `None`, so
        // an empty-string prefix must behave identically to an
        // absent field. This matches `session_from_input`'s
        // identical empty-string normalization — the two
        // dispatch-layer fields share semantics deliberately.
        let mem = fresh_memory();
        let writer = MemoryWriteTool::new(mem.clone());
        let reader = MemoryReadTool::new(mem.clone());
        let channel = fresh_channel();
        let audit = Arc::new(NullAuditHook);
        let ctx = make_ctx(&channel, audit.as_ref());

        let _ = writer
            .execute(
                json!({"topic": "notes", "body": "x", "role_prefix": ""}),
                &ctx,
            )
            .await;

        // Read with bare topic sees the write (proving the
        // empty-string prefix was equivalent to no prefix on the
        // write path).
        let out = reader
            .execute(json!({"topic": "notes"}), &ctx)
            .await;
        if let ToolOutcome::Completed { output, .. } = out {
            assert_eq!(output["count"], 1);
            assert_eq!(output["entries"][0]["body"], "x");
        } else {
            panic!("empty-prefix read should Complete");
        }
    }

    #[tokio::test]
    async fn wildcard_read_with_role_prefix_scans_only_own_namespace() {
        // A wildcard `memory.read` from the `coder` role must
        // enumerate only `coder/*` topics, even when the session
        // partition also contains `researcher/*` writes. This is
        // the integration-of-two-dimensions test: the scan prefix
        // must compose (session prefix || role prefix) correctly.
        let mem = fresh_memory();
        let writer = MemoryWriteTool::new(mem.clone());
        let reader = MemoryReadTool::new(mem.clone());
        let channel = fresh_channel();
        let audit = Arc::new(NullAuditHook);
        let ctx = make_ctx(&channel, audit.as_ref());

        let _ = writer
            .execute(
                json!({
                    "topic": "notes",
                    "body": "coder-note",
                    "role_prefix": "coder/",
                }),
                &ctx,
            )
            .await;
        let _ = writer
            .execute(
                json!({
                    "topic": "todos",
                    "body": "coder-todo",
                    "role_prefix": "coder/",
                }),
                &ctx,
            )
            .await;
        let _ = writer
            .execute(
                json!({
                    "topic": "notes",
                    "body": "researcher-note",
                    "role_prefix": "researcher/",
                }),
                &ctx,
            )
            .await;

        // Wildcard read from the coder role — should see exactly
        // two logical topics (`notes`, `todos`), each with one
        // entry and that entry's body prefixed with `coder-`.
        // The logical topic names in the output must be bare,
        // not role-qualified.
        let out = reader
            .execute(
                json!({"topics": "*", "role_prefix": "coder/"}),
                &ctx,
            )
            .await;
        if let ToolOutcome::Completed { output, .. } = out {
            assert_eq!(output["topic_count"], 2);
            let topics = output["topics"].as_array().unwrap();
            let mut seen: Vec<String> = topics
                .iter()
                .map(|t| t["topic"].as_str().unwrap().to_string())
                .collect();
            seen.sort();
            assert_eq!(seen, vec!["notes".to_string(), "todos".to_string()]);
            // Each entry's body must be the coder-written one.
            for topic_obj in topics {
                let entries = topic_obj["entries"].as_array().unwrap();
                for entry in entries {
                    let body = entry["body"].as_str().unwrap();
                    assert!(
                        body.starts_with("coder-"),
                        "researcher entry leaked into coder wildcard: {body}",
                    );
                }
            }
        } else {
            panic!("coder wildcard read should Complete");
        }
    }

    #[tokio::test]
    async fn role_and_session_cross_product_is_four_isolated_namespaces() {
        // Two roles × two sessions = four quadrants. A write in
        // one quadrant must not leak into any of the other three.
        // This is the composition test for the two dispatch-
        // layer fields: they must multiply, not collapse.
        let mem = fresh_memory();
        let writer = MemoryWriteTool::new(mem.clone());
        let reader = MemoryReadTool::new(mem.clone());
        let channel = fresh_channel();
        let audit = Arc::new(NullAuditHook);
        let ctx = make_ctx(&channel, audit.as_ref());

        let quadrants = [
            ("A", "coder/", "A-coder"),
            ("A", "researcher/", "A-research"),
            ("B", "coder/", "B-coder"),
            ("B", "researcher/", "B-research"),
        ];

        for (session, role_prefix, body) in &quadrants {
            let _ = writer
                .execute(
                    json!({
                        "topic": "notes",
                        "body": *body,
                        "session": *session,
                        "role_prefix": *role_prefix,
                    }),
                    &ctx,
                )
                .await;
        }

        // Each quadrant's single-topic read sees only its own
        // body and exactly one entry.
        for (session, role_prefix, body) in &quadrants {
            let out = reader
                .execute(
                    json!({
                        "topic": "notes",
                        "session": *session,
                        "role_prefix": *role_prefix,
                    }),
                    &ctx,
                )
                .await;
            match out {
                ToolOutcome::Completed { output, .. } => {
                    assert_eq!(
                        output["count"], 1,
                        "quadrant ({session}, {role_prefix}) saw {} entries",
                        output["count"],
                    );
                    assert_eq!(output["entries"][0]["body"], *body);
                }
                _ => panic!(
                    "quadrant ({session}, {role_prefix}) read should Complete"
                ),
            }
        }
    }

    #[tokio::test]
    async fn forget_with_role_prefix_only_deletes_own_namespace() {
        // A `memory.forget` from the coder role must clear only
        // `coder/notes`, leaving `researcher/notes` untouched.
        let mem = fresh_memory();
        let writer = MemoryWriteTool::new(mem.clone());
        let reader = MemoryReadTool::new(mem.clone());
        let forgetter = MemoryForgetTool::new(mem.clone());
        let channel = fresh_channel();
        let audit = Arc::new(NullAuditHook);
        let ctx = make_ctx(&channel, audit.as_ref());

        let _ = writer
            .execute(
                json!({
                    "topic": "notes",
                    "body": "coder-entry",
                    "role_prefix": "coder/",
                }),
                &ctx,
            )
            .await;
        let _ = writer
            .execute(
                json!({
                    "topic": "notes",
                    "body": "researcher-entry",
                    "role_prefix": "researcher/",
                }),
                &ctx,
            )
            .await;

        // Forget from coder.
        let out = forgetter
            .execute(
                json!({"topic": "notes", "role_prefix": "coder/"}),
                &ctx,
            )
            .await;
        if let ToolOutcome::Completed { output, verified } = out {
            assert_eq!(output["deleted"], 1);
            assert_eq!(verified, Verification::Verified);
        } else {
            panic!("coder forget should Complete");
        }

        // Coder's notes: empty.
        let out = reader
            .execute(
                json!({"topic": "notes", "role_prefix": "coder/"}),
                &ctx,
            )
            .await;
        if let ToolOutcome::Completed { output, .. } = out {
            assert_eq!(output["count"], 0);
        } else {
            panic!("coder post-forget read should Complete");
        }

        // Researcher's notes: still there.
        let out = reader
            .execute(
                json!({"topic": "notes", "role_prefix": "researcher/"}),
                &ctx,
            )
            .await;
        if let ToolOutcome::Completed { output, .. } = out {
            assert_eq!(output["count"], 1);
            assert_eq!(output["entries"][0]["body"], "researcher-entry");
        } else {
            panic!("researcher read after coder forget should Complete");
        }
    }

    #[test]
    fn role_prefix_is_not_declared_in_any_tool_input_schema() {
        // Validation-before-injection discipline: the reserved
        // `role_prefix` field must not appear in any advertised
        // `input_schema`. If it did, a Phase 10 validator run on
        // the post-injection input would still accept — but the
        // LLM would also see the field in the schema and could
        // then forge its own role prefix, which is precisely
        // what the dispatch-layer pattern exists to prevent.
        //
        // The same discipline protects the `session` field, which
        // Phase 8 Task 2 established and Phase 10 Task 2's
        // schema validator preserved by running before injection.
        let read = MemoryReadTool::new(fresh_memory());
        let write = MemoryWriteTool::new(fresh_memory());
        let forget = MemoryForgetTool::new(fresh_memory());

        for (name, schema) in [
            ("memory.read", read.input_schema()),
            ("memory.write", write.input_schema()),
            ("memory.forget", forget.input_schema()),
        ] {
            let props = schema
                .get("properties")
                .and_then(|p| p.as_object())
                .expect("schema has a properties object");
            assert!(
                !props.contains_key("role_prefix"),
                "{name} advertised role_prefix in its input schema",
            );
            assert!(
                !props.contains_key("session"),
                "{name} advertised session in its input schema",
            );
        }
    }

    #[tokio::test]
    async fn audit_scope_and_query_stay_logical_under_role_prefix() {
        // The audit chain must remain keyed on the *logical*
        // topic the agent typed, not on the physical role-
        // qualified topic. Otherwise two instances of Aivyx
        // using different role prefixes on the same logical
        // topic would produce different audit bytes, breaking
        // D1's "audit bytes depend only on agent-visible intent"
        // invariant.
        use aivyx_core::{AuditHook, AuditTag};
        use std::sync::Mutex;

        struct CaptureAudit {
            events: Mutex<Vec<AuditTag>>,
        }

        impl AuditHook for CaptureAudit {
            fn on_event(&self, tag: AuditTag) {
                self.events.lock().unwrap().push(tag);
            }
        }

        let mem = fresh_memory();
        let writer = MemoryWriteTool::new(mem.clone());
        let channel = fresh_channel();
        let audit = Arc::new(CaptureAudit {
            events: Mutex::new(Vec::new()),
        });
        let ctx = make_ctx(&channel, audit.as_ref());

        let _ = writer
            .execute(
                json!({
                    "topic": "notes",
                    "body": "x",
                    "role_prefix": "coder/",
                }),
                &ctx,
            )
            .await;

        let events = audit.events.lock().unwrap();
        let found = events.iter().any(|tag| {
            matches!(
                tag,
                AuditTag::MemoryAccess { query_or_key, scope, .. }
                if query_or_key == "notes"
                    && scope.qualifier() == Some("topic:notes")
            )
        });
        assert!(
            found,
            "audit event should record the logical topic `notes`, \
             not the role-qualified `coder/notes`",
        );
    }

    // ---- Phase 74 — MemorySearchTool ---------------------------------

    /// Tiny shared audit fixture for Phase 74 tool tests.
    /// Existing tests in the file use inline `CaptureAudit`
    /// structs; consolidating into one keeps the new section
    /// terse.
    #[derive(Default)]
    struct RecordingAudit {
        events: std::sync::Mutex<Vec<aivyx_core::AuditTag>>,
    }

    impl aivyx_core::AuditHook for RecordingAudit {
        fn on_event(&self, tag: aivyx_core::AuditTag) {
            self.events.lock().unwrap().push(tag);
        }
    }

    #[test]
    fn search_scope_is_wildcard_topic() {
        let tool = MemorySearchTool::new(fresh_memory());
        let scope = tool.required_scope(&json!({"query": "foo"}));
        assert_eq!(scope.base(), "memory.read");
        assert_eq!(scope.qualifier(), Some("topic:*"));
    }

    #[tokio::test]
    async fn search_returns_substring_matches_newest_first() {
        let memory = fresh_memory();
        memory.put("notes", "Build the FOO subsystem").await.unwrap();
        memory.put("docs", "Read about quux").await.unwrap();
        memory.put("notes", "fixed foo bug today").await.unwrap();
        let tool = MemorySearchTool::new(Arc::clone(&memory));
        let channel = fresh_channel();
        let audit = RecordingAudit::default();
        let ctx = make_ctx(&channel, &audit);
        let outcome = tool
            .execute(json!({"query": "foo"}), &ctx)
            .await;
        let ToolOutcome::Completed { output, .. } = outcome else {
            panic!("expected Completed, got {outcome:?}");
        };
        let matches = output.get("matches").unwrap().as_array().unwrap();
        assert_eq!(matches.len(), 2);
        // Newest first: seq=2 then seq=0.
        assert_eq!(matches[0]["seq"].as_u64(), Some(2));
        assert_eq!(matches[1]["seq"].as_u64(), Some(0));
    }

    #[tokio::test]
    async fn search_empty_query_returns_all_entries() {
        let memory = fresh_memory();
        memory.put("a", "x").await.unwrap();
        memory.put("b", "y").await.unwrap();
        let tool = MemorySearchTool::new(Arc::clone(&memory));
        let channel = fresh_channel();
        let audit = RecordingAudit::default();
        let ctx = make_ctx(&channel, &audit);
        let outcome = tool
            .execute(json!({"query": ""}), &ctx)
            .await;
        let ToolOutcome::Completed { output, .. } = outcome else {
            panic!("expected Completed");
        };
        let matches = output.get("matches").unwrap().as_array().unwrap();
        assert_eq!(matches.len(), 2);
    }

    #[tokio::test]
    async fn search_respects_limit_cap() {
        let memory = fresh_memory();
        for i in 0..5 {
            memory.put(&format!("topic-{i}"), "shared").await.unwrap();
        }
        let tool = MemorySearchTool::new(Arc::clone(&memory));
        let channel = fresh_channel();
        let audit = RecordingAudit::default();
        let ctx = make_ctx(&channel, &audit);
        let outcome = tool
            .execute(json!({"query": "shared", "limit": 2}), &ctx)
            .await;
        let ToolOutcome::Completed { output, .. } = outcome else {
            panic!("expected Completed");
        };
        let matches = output.get("matches").unwrap().as_array().unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(output.get("count").unwrap().as_u64(), Some(2));
    }

    #[tokio::test]
    async fn search_audits_with_wildcard_scope_and_query_key() {
        let memory = fresh_memory();
        memory.put("notes", "hello").await.unwrap();
        let tool = MemorySearchTool::new(Arc::clone(&memory));
        let channel = fresh_channel();
        let audit = RecordingAudit::default();
        let ctx = make_ctx(&channel, &audit);
        let _ = tool.execute(json!({"query": "hello"}), &ctx).await;
        let events = audit.events.lock().unwrap();
        let found = events.iter().any(|tag| {
            matches!(
                tag,
                AuditTag::MemoryAccess { query_or_key, scope, .. }
                if query_or_key == "search:hello"
                    && scope.qualifier() == Some("topic:*")
            )
        });
        assert!(found, "audit event must record wildcard scope + search key");
    }

    // ---- Phase 75 — semantic mode + keyword fallback --------

    struct FixedHook {
        vec: Option<Vec<f32>>,
    }

    #[async_trait]
    impl EmbeddingHook for FixedHook {
        async fn embed_one(&self, _t: &str) -> Option<Vec<f32>> {
            self.vec.clone()
        }
    }

    #[tokio::test]
    async fn search_semantic_ranks_by_vector_when_available() {
        let memory = fresh_memory();
        let s0 = memory.put("notes", "alpha").await.unwrap();
        let s1 = memory.put("notes", "beta").await.unwrap();
        memory.put_vector("notes", s0, vec![1.0, 0.0]).await.unwrap();
        memory.put_vector("notes", s1, vec![0.0, 1.0]).await.unwrap();
        let tool = MemorySearchTool::new(Arc::clone(&memory))
            .with_embedding_hook(Arc::new(FixedHook {
                vec: Some(vec![1.0, 0.0]),
            }));
        let channel = fresh_channel();
        let audit = RecordingAudit::default();
        let ctx = make_ctx(&channel, &audit);
        let ToolOutcome::Completed { output, .. } = tool
            .execute(
                json!({"query": "ignored", "mode": "semantic"}),
                &ctx,
            )
            .await
        else {
            panic!("expected Completed");
        };
        assert_eq!(output["mode"], "semantic");
        assert_eq!(output["fell_back_to_keyword"], false);
        let matches = output["matches"].as_array().unwrap();
        // Query [1,0] is closest to s0's vector.
        assert_eq!(matches[0]["seq"].as_u64(), Some(s0));
    }

    #[tokio::test]
    async fn search_semantic_without_hook_falls_back_to_keyword() {
        let memory = fresh_memory();
        memory.put("notes", "findme please").await.unwrap();
        // No hook attached → semantic impossible.
        let tool = MemorySearchTool::new(Arc::clone(&memory));
        let channel = fresh_channel();
        let audit = RecordingAudit::default();
        let ctx = make_ctx(&channel, &audit);
        let ToolOutcome::Completed { output, .. } = tool
            .execute(
                json!({"query": "findme", "mode": "semantic"}),
                &ctx,
            )
            .await
        else {
            panic!("expected Completed");
        };
        assert_eq!(output["mode"], "keyword");
        assert_eq!(output["fell_back_to_keyword"], true);
        assert_eq!(output["matches"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn search_semantic_empty_index_falls_back_to_keyword() {
        let memory = fresh_memory();
        memory.put("notes", "findme please").await.unwrap();
        // Hook present, but no vectors stored yet → fallback.
        let tool = MemorySearchTool::new(Arc::clone(&memory))
            .with_embedding_hook(Arc::new(FixedHook {
                vec: Some(vec![1.0, 0.0]),
            }));
        let channel = fresh_channel();
        let audit = RecordingAudit::default();
        let ctx = make_ctx(&channel, &audit);
        let ToolOutcome::Completed { output, .. } = tool
            .execute(
                json!({"query": "findme", "mode": "semantic"}),
                &ctx,
            )
            .await
        else {
            panic!("expected Completed");
        };
        assert_eq!(output["fell_back_to_keyword"], true);
        assert_eq!(output["matches"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn search_default_mode_is_keyword_no_fallback_flag() {
        let memory = fresh_memory();
        memory.put("notes", "hello").await.unwrap();
        let tool = MemorySearchTool::new(Arc::clone(&memory));
        let channel = fresh_channel();
        let audit = RecordingAudit::default();
        let ctx = make_ctx(&channel, &audit);
        let ToolOutcome::Completed { output, .. } = tool
            .execute(json!({"query": "hello"}), &ctx)
            .await
        else {
            panic!("expected Completed");
        };
        assert_eq!(output["mode"], "keyword");
        assert_eq!(output["fell_back_to_keyword"], false);
    }
}
