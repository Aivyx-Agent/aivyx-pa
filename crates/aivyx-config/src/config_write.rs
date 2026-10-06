//! Chapter U — section-scoped writes back to `aivyx-pa.toml`.
//!
//! Before Chapter U, the only writer of `aivyx-pa.toml` was the CLI's
//! `aivyx-pa access set` (`crates/aivyx-cli/.../access.rs`): it loaded the file
//! into a [`toml_edit::DocumentMut`], patched the `[access]` keys in place,
//! and wrote the document back at `0600` — patching keys in place rather than
//! re-serializing the whole config (which would reflow the file and drop the
//! operator's comments).
//!
//! Chapter U adds a **second** writer: the daemon's Settings IPC handlers
//! (`SetAccessLevel` / `SetBudget`). Two independent writers of the same file
//! that must agree byte-for-byte on the schema is exactly the kind of drift
//! this module exists to prevent — so the rewrite logic lives here, once, and
//! both the CLI and the daemon call it.
//!
//! What stays with the *callers*, not here:
//! - the **confirm-first** decision for expanded access levels (the CLI prompts
//!   on stdin; the daemon enforces an explicit `confirm` flag) — see
//!   [`AccessLevel::is_expanded`]. This module performs the *write*; the policy
//!   of whether the operator may perform it is the caller's.
//!
//! What lives here:
//! - the structural **root rules** ([`AccessLevel::Workspace`] /
//!   [`AccessLevel::Custom`] require a `root`; the auto-derived levels reject an
//!   explicit one), so neither caller can write a nonsensical `[access]`;
//! - the **budget validation** mirrored from the loader (non-negative caps;
//!   `alert_at` within `[0.0, 1.0]`) so a bad cap is refused before it touches
//!   disk rather than failing the *next* daemon load;
//! - the `0600` permission posture (the file may carry secrets in other
//!   sections).

use std::path::Path;

use aivyx_cost::{BudgetAction, BudgetConfig};
use toml_edit::{value, DocumentMut};

use crate::{AccessLevel, AutonomyLevel};

/// Failure modes for a section-scoped `aivyx-pa.toml` rewrite. Carries enough
/// structure that the daemon can map a write failure to a typed IPC error;
/// the CLI renders the `Display` string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigWriteError {
    /// `workspace` / `custom` were requested without a `root`.
    RootRequired { level: AccessLevel },
    /// A `root` was supplied for a level that derives its own.
    RootNotAllowed { level: AccessLevel },
    /// A budget field is out of range (negative cap, or `alert_at` outside
    /// `[0.0, 1.0]`). Mirrors the loader's validation so the write is refused
    /// before it can corrupt the next load.
    InvalidBudget { reason: String },
    /// An `[[mcp_server]]` entry is structurally invalid — mirrors the
    /// loader's own transport-field validation (`aivyx-config/src/lib.rs`'s
    /// mcp_servers parsing) so a bad write is refused before it corrupts
    /// the next daemon load, same principle as `InvalidBudget`.
    InvalidMcpServer { reason: String },
    /// A `[[notify_target]]` entry is structurally invalid — mirrors the
    /// loader's own per-kind validation (`aivyx-config/src/lib.rs`'s
    /// notify_targets parsing) so a bad write is refused before it
    /// corrupts the next daemon load, same principle as `InvalidMcpServer`.
    InvalidNotifyTarget { reason: String },
    /// The `[email]` section, after this write is merged onto whatever's
    /// already on disk, would leave the loader's all-or-nothing rule
    /// broken — `aivyx-config/src/lib.rs`'s `build_email_config`: if ANY
    /// of `host`/`port`/`tls_mode`/`username`/`password`/`from` is set,
    /// `host`/`username`/`password`/`from` must ALL be present or the
    /// loader hard-fails `ConfigError::Invalid` and the daemon refuses to
    /// boot (final-review finding #1 — a password-only write on a fresh
    /// install used to brick the next daemon start). Refused here instead.
    InvalidEmailConfig { reason: String },
    /// A `[[reflection_schedule]]` entry is structurally invalid — mirrors
    /// the loader's own validation (`aivyx-config/src/lib.rs:7251`-7301:
    /// non-empty name/cron, lookback bounds, name uniqueness against both
    /// `[[reflection_schedule]]` and `[[schedule]]`) so a bad write is
    /// refused before it corrupts the next daemon load, same principle as
    /// `InvalidNotifyTarget`.
    InvalidReflectionSchedule { reason: String },
    /// `[memory] profile` is not one of the loader's recognized values.
    /// The loader itself (`MemoryProfile::from_arg`) is permissive —
    /// any unrecognized string silently falls back to `Off` rather than
    /// erroring — but the Studio picker only ever offers 3 concrete
    /// values, so a 4th string reaching this function means a
    /// non-Studio caller sent something wrong; refuse it rather than
    /// silently defaulting.
    InvalidMemoryProfile { reason: String },
    /// An `[embedding]` field is blank where a value was explicitly
    /// supplied (an explicit-but-blank `base_url`/`model` would still
    /// write an empty string, which the loader then treats differently
    /// from "absent" — refused here instead).
    InvalidEmbeddingConfig { reason: String },
    /// The `[proactive]` section, after this write is merged onto
    /// whatever's already on disk, would fail the loader's own
    /// `enabled = true` requirements (`build_proactive_config`,
    /// `aivyx-config/src/lib.rs:8689`): non-empty `target`,
    /// `max_per_window >= 1`, `window_secs >= 1`. Also enforces one
    /// check the loader itself does NOT make — that `target` names an
    /// existing `[[notify_target]]` entry — deliberately stricter than
    /// today's loader, not a mirror of an existing check (see this
    /// plan's design spec, "Corrections" §2).
    InvalidProactiveConfig { reason: String },
    /// The existing file did not parse as TOML.
    Parse { reason: String },
    /// The file could not be read or written.
    Io { reason: String },
}

impl std::fmt::Display for ConfigWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigWriteError::RootRequired { level } => write!(
                f,
                "access level `{}` needs an explicit root directory",
                level.as_str()
            ),
            ConfigWriteError::RootNotAllowed { level } => write!(
                f,
                "access level `{}` derives its root automatically; an explicit root does not apply",
                level.as_str()
            ),
            ConfigWriteError::InvalidBudget { reason } => write!(f, "invalid budget: {reason}"),
            ConfigWriteError::InvalidMcpServer { reason } => write!(f, "invalid MCP server entry: {reason}"),
            ConfigWriteError::InvalidNotifyTarget { reason } => write!(f, "invalid notify target entry: {reason}"),
            ConfigWriteError::InvalidEmailConfig { reason } => write!(f, "invalid email config: {reason}"),
            ConfigWriteError::InvalidReflectionSchedule { reason } => write!(f, "invalid reflection schedule entry: {reason}"),
            ConfigWriteError::InvalidMemoryProfile { reason } => write!(f, "invalid memory profile: {reason}"),
            ConfigWriteError::InvalidEmbeddingConfig { reason } => write!(f, "invalid embedding config: {reason}"),
            ConfigWriteError::InvalidProactiveConfig { reason } => write!(f, "invalid proactive config: {reason}"),
            ConfigWriteError::Parse { reason } => write!(f, "failed to parse aivyx-pa.toml: {reason}"),
            ConfigWriteError::Io { reason } => write!(f, "{reason}"),
        }
    }
}

impl std::error::Error for ConfigWriteError {}

/// Rewrite the `[access]` section of the TOML file at `path`, preserving every
/// other section and the operator's comments.
///
/// Sets `level`, sets-or-clears `root` (per the level's root rules), and
/// removes `confirm_destructive` so the autonomy level decides whether deletes
/// and overwrites ask. A missing file is treated as empty (the section is
/// created). The result is written at `0600`.
///
/// The caller is responsible for the **confirm-first** decision on expanded
/// levels; this function only enforces the structural root rules.
pub fn write_access_section(
    path: &Path,
    level: AccessLevel,
    root: Option<&str>,
) -> Result<(), ConfigWriteError> {
    // Structural root rules — neither caller may write a nonsensical access
    // section (workspace/custom need a directory; the derived levels reject
    // an explicit one that would shadow their derivation).
    match level {
        AccessLevel::Workspace | AccessLevel::Custom if root.is_none() => {
            return Err(ConfigWriteError::RootRequired { level });
        }
        AccessLevel::Sandbox | AccessLevel::Home | AccessLevel::Full if root.is_some() => {
            return Err(ConfigWriteError::RootNotAllowed { level });
        }
        _ => {}
    }

    let mut doc = load_document(path)?;

    doc["access"]["level"] = value(level.as_str());
    match root {
        Some(r) => doc["access"]["root"] = value(r),
        // Switching to a level that derives its root: drop any stale
        // `[access] root` so it doesn't shadow the derivation.
        None => {
            if let Some(t) = doc.get_mut("access").and_then(|a| a.as_table_mut()) {
                t.remove("root");
            }
        }
    }
    // Whether deletes and overwrites ask is the autonomy level's call, not the
    // access level's. Drop any `confirm_destructive` here — including one an
    // older version wrote (`false` for sandbox silently switched confirmation
    // off at every autonomy level) — so the autonomy level decides again.
    if let Some(t) = doc.get_mut("access").and_then(|a| a.as_table_mut()) {
        t.remove("confirm_destructive");
    }

    write_toml_0600(path, &doc.to_string())
}

/// Rewrite the `[autonomy] level` key of the TOML file at `path`, preserving
/// every other section, the operator's comments, and any existing
/// `[[autonomy.override]]` / `[autonomy.auto_approve]` sub-tables (Chapter Reins
/// RN.6). Only the `level` is rewritten — per-domain overrides and the
/// allowlist are edited elsewhere, so this never clobbers them.
///
/// Mirrors [`write_access_section`]: the *policy* of whether the operator may
/// pick an autonomy-granting level (the stdin confirm / the IPC `confirm` flag)
/// stays with the caller; this performs the structural `0600` write so the CLI
/// and a future `SetAutonomyLevel` IPC handler agree byte-for-byte.
pub fn write_autonomy_section(path: &Path, level: AutonomyLevel) -> Result<(), ConfigWriteError> {
    let mut doc = load_document(path)?;
    doc["autonomy"]["level"] = value(level.as_str());
    write_toml_0600(path, &doc.to_string())
}

/// Rewrite the `[budget]` section of the TOML file at `path`, preserving every
/// other section and the operator's comments.
///
/// `None` caps clear their key (uncapped is the default). `on_exceeded` and
/// `alert_at` are always written. Validation mirrors the loader
/// (`AivyxConfig::load`): negative caps and an out-of-range `alert_at` are
/// refused here so a bad write can never reach the gate's reservation math.
pub fn write_budget_section(path: &Path, budget: &BudgetConfig) -> Result<(), ConfigWriteError> {
    for (name, cap) in [
        ("per_run_usd", budget.per_run_usd),
        ("per_day_usd", budget.per_day_usd),
    ] {
        if let Some(c) = cap {
            if c < 0.0 {
                return Err(ConfigWriteError::InvalidBudget {
                    reason: format!("{name} must be non-negative"),
                });
            }
        }
    }
    if let Some(frac) = budget.alert_at {
        if !(0.0..=1.0).contains(&frac) {
            return Err(ConfigWriteError::InvalidBudget {
                reason: "alert_at must be within [0.0, 1.0]".to_string(),
            });
        }
    }

    let mut doc = load_document(path)?;

    set_or_clear_f64(&mut doc, "per_run_usd", budget.per_run_usd);
    set_or_clear_f64(&mut doc, "per_day_usd", budget.per_day_usd);
    doc["budget"]["on_exceeded"] = value(budget_action_str(budget.on_exceeded));
    set_or_clear_f64(&mut doc, "alert_at", budget.alert_at);

    write_toml_0600(path, &doc.to_string())
}

/// Rewrite `[agent] cycle_detection` — the small-cycle breaker switch
/// ([`crate::AivyxConfig::cycle_detection`]). Writes the explicit boolean so the
/// operator sees the current state in the file; preserves every other section
/// and comments (the surgical-splice posture). Takes effect on the next daemon
/// start. There is nothing to validate (a plain bool), so this only fails on I/O
/// or a malformed existing file.
pub fn write_agent_cycle_detection(path: &Path, enabled: bool) -> Result<(), ConfigWriteError> {
    let mut doc = load_document(path)?;
    doc["agent"]["cycle_detection"] = value(enabled);
    write_toml_0600(path, &doc.to_string())
}

/// The six operator-declared `[profile]` fields, in the carrier the daemon's
/// `SetProfile` handler fills from IPC. Mirrors the loader's `RawProfile`
/// shape (Chapter V §9.1): all fields optional, with **clear-on-`None`**
/// semantics — a `None` removes the key (the loader falls back to its default,
/// e.g. `assistant_name` → `"Aivyx PA"`), a `Some` writes it.
///
/// Values are normalized on write: scalars are trimmed (an all-whitespace
/// scalar clears the key), and list entries are trimmed with empties dropped —
/// so the form's blank rows never reach disk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileWrite {
    pub assistant_name: Option<String>,
    pub operator_profile: Option<String>,
    pub communication_style: Option<String>,
    pub primary_use_cases: Option<Vec<String>>,
    pub behavioral_preferences: Option<Vec<String>>,
    pub behavioral_constraints: Option<Vec<String>>,
}

/// Rewrite the `[profile]` section of the TOML file at `path`, preserving every
/// other section and the operator's comments — the surgical-splice posture of
/// `aivyx-pa profile edit`, but driven by structured fields instead of an editor
/// round-trip.
///
/// Each field follows clear-on-`None` (see [`ProfileWrite`]): a present value
/// is normalized and written; an absent one removes the key so the loader's
/// default applies. There is nothing to *validate* here — Profile fields are
/// free-form declarations — so this never fails on content, only on I/O or a
/// malformed existing file.
pub fn write_profile_section(path: &Path, profile: &ProfileWrite) -> Result<(), ConfigWriteError> {
    let mut doc = load_document(path)?;

    set_or_clear_profile_str(&mut doc, "assistant_name", profile.assistant_name.as_deref());
    set_or_clear_profile_str(&mut doc, "operator_profile", profile.operator_profile.as_deref());
    set_or_clear_profile_str(
        &mut doc,
        "communication_style",
        profile.communication_style.as_deref(),
    );
    set_or_clear_profile_list(&mut doc, "primary_use_cases", profile.primary_use_cases.as_deref());
    set_or_clear_profile_list(
        &mut doc,
        "behavioral_preferences",
        profile.behavioral_preferences.as_deref(),
    );
    set_or_clear_profile_list(
        &mut doc,
        "behavioral_constraints",
        profile.behavioral_constraints.as_deref(),
    );

    write_toml_0600(path, &doc.to_string())
}

/// Set `[profile].<key>` to the trimmed scalar, or remove the key when the
/// value is `None` or trims to empty (the loader's default then applies).
fn set_or_clear_profile_str(doc: &mut DocumentMut, key: &str, v: Option<&str>) {
    match v.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => doc["profile"][key] = value(s),
        None => remove_profile_key(doc, key),
    }
}

/// Set `[profile].<key>` to a TOML array of the trimmed, non-empty entries, or
/// remove the key when the list is `None`. An explicit `Some(vec![])` (or a
/// list of only blanks) writes an empty array — "declared, but empty" — which
/// is distinct from the key being absent.
fn set_or_clear_profile_list(doc: &mut DocumentMut, key: &str, v: Option<&[String]>) {
    match v {
        Some(items) => {
            let mut arr = toml_edit::Array::new();
            for item in items {
                let t = item.trim();
                if !t.is_empty() {
                    arr.push(t);
                }
            }
            doc["profile"][key] = value(arr);
        }
        None => remove_profile_key(doc, key),
    }
}

/// Remove `key` from the `[profile]` table if the table exists.
fn remove_profile_key(doc: &mut DocumentMut, key: &str) {
    if let Some(t) = doc.get_mut("profile").and_then(|p| p.as_table_mut()) {
        t.remove(key);
    }
}

/// The `[voice]` fields the Studio's Voice screen edits, in the carrier the
/// daemon's `SetVoice` handler fills from IPC. Mirrors `VoiceOptions` (Chapter
/// Voice §12.1): all optional, **clear-on-`None`** — an absent field removes the
/// key (the voice loader's default then applies). Strings are trimmed (an
/// all-whitespace value clears the key); paths are carried as strings (TOML
/// stores them as strings regardless).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VoiceWrite {
    pub asr_engine: Option<String>,
    pub tts_engine: Option<String>,
    pub asr_model_path: Option<String>,
    pub asr_language: Option<String>,
    pub asr_beam_size: Option<u32>,
    pub tts_model_dir: Option<String>,
    pub tts_voice_name: Option<String>,
    pub tts_speed: Option<f32>,
    pub input_device: Option<String>,
    pub output_device: Option<String>,
}

/// Rewrite the `[voice]` section of the TOML file at `path`, preserving every
/// other section and the operator's comments — the section-scoped `toml_edit`
/// posture of the access/budget/profile writers.
///
/// Each field follows clear-on-`None`: a present value is normalized and
/// written; an absent one removes the key. There is nothing to *validate* here
/// (the voice channel validates engine names + that the model files exist at
/// launch), so this never fails on content — only on I/O or a malformed file.
pub fn write_voice_section(path: &Path, voice: &VoiceWrite) -> Result<(), ConfigWriteError> {
    let mut doc = load_document(path)?;

    set_or_clear_voice_str(&mut doc, "asr_engine", voice.asr_engine.as_deref());
    set_or_clear_voice_str(&mut doc, "tts_engine", voice.tts_engine.as_deref());
    set_or_clear_voice_str(&mut doc, "asr_model_path", voice.asr_model_path.as_deref());
    set_or_clear_voice_str(&mut doc, "asr_language", voice.asr_language.as_deref());
    match voice.asr_beam_size {
        Some(n) => doc["voice"]["asr_beam_size"] = value(n as i64),
        None => remove_voice_key(&mut doc, "asr_beam_size"),
    }
    set_or_clear_voice_str(&mut doc, "tts_model_dir", voice.tts_model_dir.as_deref());
    set_or_clear_voice_str(&mut doc, "tts_voice_name", voice.tts_voice_name.as_deref());
    match voice.tts_speed {
        Some(s) => doc["voice"]["tts_speed"] = value(s as f64),
        None => remove_voice_key(&mut doc, "tts_speed"),
    }
    set_or_clear_voice_str(&mut doc, "input_device", voice.input_device.as_deref());
    set_or_clear_voice_str(&mut doc, "output_device", voice.output_device.as_deref());

    write_toml_0600(path, &doc.to_string())
}

/// Set `[voice].<key>` to the trimmed string, or remove the key when the value
/// is `None` or trims to empty.
fn set_or_clear_voice_str(doc: &mut DocumentMut, key: &str, v: Option<&str>) {
    match v.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => doc["voice"][key] = value(s),
        None => remove_voice_key(doc, key),
    }
}

/// Remove `key` from the `[voice]` table if the table exists.
fn remove_voice_key(doc: &mut DocumentMut, key: &str) {
    if let Some(t) = doc.get_mut("voice").and_then(|v| v.as_table_mut()) {
        t.remove(key);
    }
}

/// One `[[mcp_server]]` entry as Studio's write form submits it. A plain
/// (non-wire) struct — `aivyx-ipc` has its own serde-derived mirror type
/// for the wire; `aivyx-channel`'s daemon handler converts between them,
/// matching how `write_budget_section` takes `&aivyx_cost::BudgetConfig`
/// rather than a wire type directly.
#[derive(Debug, Clone, PartialEq)]
pub struct McpServerEntryWrite {
    pub name: String,
    /// `"stdio"`, `"sse"`, or `"http"` — matches the loader's own accepted
    /// strings (`aivyx-config/src/lib.rs`'s mcp_servers parsing; that parser
    /// also accepts `"streamable-http"` as an alias for `"http"`, but writes
    /// always normalize to `"http"`).
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub url: Option<String>,
    pub enabled: bool,
}

/// Add or replace (by `name`) one `[[mcp_server]]` entry, preserving every
/// other entry, section, and the operator's comments. Validates the
/// transport-specific required field the same way the loader does
/// (`command` for stdio, `url` for sse/http) — refused here rather than
/// failing the next daemon load.
pub fn write_mcp_server_section(
    path: &Path,
    server: &McpServerEntryWrite,
) -> Result<(), ConfigWriteError> {
    if server.name.trim().is_empty() {
        return Err(ConfigWriteError::InvalidMcpServer {
            reason: "name must not be empty".to_string(),
        });
    }
    let transport_key = match server.transport.as_str() {
        "stdio" => "stdio",
        "sse" => "sse",
        "http" | "streamable-http" => "http",
        other => {
            return Err(ConfigWriteError::InvalidMcpServer {
                reason: format!(
                    "server {:?}: unknown transport {:?} (expected \"stdio\", \"sse\", or \"http\")",
                    server.name, other
                ),
            });
        }
    };
    if transport_key == "stdio" && server.command.is_none() {
        return Err(ConfigWriteError::InvalidMcpServer {
            reason: format!("server {:?}: stdio transport requires `command`", server.name),
        });
    }
    if transport_key != "stdio" && server.url.is_none() {
        return Err(ConfigWriteError::InvalidMcpServer {
            reason: format!("server {:?}: {transport_key} transport requires `url`", server.name),
        });
    }

    let mut doc = load_document(path)?;
    let arr = mcp_server_array_mut(&mut doc);

    let mut table = toml_edit::Table::new();
    table["name"] = value(server.name.as_str());
    table["transport"] = value(transport_key);
    table["enabled"] = value(server.enabled);
    if let Some(cmd) = &server.command {
        table["command"] = value(cmd.as_str());
    }
    if !server.args.is_empty() {
        let mut arr_val = toml_edit::Array::new();
        for a in &server.args {
            arr_val.push(a.as_str());
        }
        table["args"] = toml_edit::Item::Value(arr_val.into());
    }
    if !server.env.is_empty() {
        let mut env_table = toml_edit::InlineTable::new();
        for (k, v) in &server.env {
            env_table.insert(k, v.as_str().into());
        }
        table["env"] = toml_edit::Item::Value(env_table.into());
    }
    // `headers` is for the remote transports only — the loader itself
    // rejects a stdio entry that declares `headers` (`aivyx-config/src/lib.rs`'s
    // mcp_servers parsing). Rather than erroring here (the Studio form never
    // sends headers for a stdio entry, but this primitive is called by other
    // callers too), just drop them so a stdio write can never produce a file
    // the loader then refuses at next boot.
    if transport_key != "stdio" && !server.headers.is_empty() {
        let mut headers_table = toml_edit::InlineTable::new();
        for (k, v) in &server.headers {
            headers_table.insert(k, v.as_str().into());
        }
        table["headers"] = toml_edit::Item::Value(headers_table.into());
    }
    if let Some(url) = &server.url {
        table["url"] = value(url.as_str());
    }

    // Fields this write schema doesn't know about — `sandbox`
    // (`[mcp_server.sandbox]`) and `bundled` — are deliberately not exposed
    // to Studio (POLISH_WAVES.md sub-project 7 scope), but an upsert must
    // never destroy them on an existing entry: sandboxing is the documented
    // default posture (`docs/MCP_RECIPES.md`), and silently stripping it on
    // an unrelated field edit would drop a THREAT_MODEL.md security control.
    // Preserve every key the existing table has that isn't one of the 8
    // fields this function itself writes, defensively covering any future
    // schema growth too.
    const KNOWN_KEYS: &[&str] =
        &["name", "transport", "command", "args", "env", "headers", "url", "enabled"];

    let idx = arr.iter().position(|t| t.get("name").and_then(|v| v.as_str()) == Some(server.name.as_str()));
    match idx {
        Some(i) => {
            if let Some(existing) = arr.get(i) {
                for (k, v) in existing.iter() {
                    if KNOWN_KEYS.contains(&k) {
                        continue;
                    }
                    // `sandbox` (`[mcp_server.sandbox]`) is stdio-only — the
                    // loader itself rejects a sandbox block on a non-stdio
                    // transport (`aivyx-config/src/lib.rs`'s mcp_servers
                    // parsing), and that rejection aborts loading the
                    // *entire* aivyx-pa.toml, not just this one entry. If this
                    // upsert switched the entry's transport away from
                    // stdio, carrying the old sandbox block over would
                    // brick the daemon at next boot — the exact failure
                    // class the headers-gating above exists to prevent,
                    // reintroduced through a different route. `bundled` has
                    // no such transport restriction in the loader, so it's
                    // still preserved unconditionally.
                    if k == "sandbox" && transport_key != "stdio" {
                        continue;
                    }
                    table.insert(k, v.clone());
                }
            }
            *arr.get_mut(i).expect("index just found") = table;
        }
        None => {
            arr.push(table);
        }
    }

    write_toml_0600(path, &doc.to_string())
}

/// Remove one `[[mcp_server]]` entry by `name`. A no-op (not an error) when
/// no entry with that name exists — matches DELETE-idempotent semantics
/// used elsewhere in this codebase's IPC handlers.
pub fn remove_mcp_server_section(path: &Path, name: &str) -> Result<(), ConfigWriteError> {
    let mut doc = load_document(path)?;
    let arr = mcp_server_array_mut(&mut doc);
    let idx = arr.iter().position(|t| t.get("name").and_then(|v| v.as_str()) == Some(name));
    if let Some(i) = idx {
        arr.remove(i);
    }
    write_toml_0600(path, &doc.to_string())
}

/// Read every `[[mcp_server]]` entry **as literally written on disk** —
/// `${VAR}` placeholders and all, no `${VAR}` interpolation, no environment
/// lookups. This is the read-side counterpart to [`write_mcp_server_section`]
/// and deliberately does *not* go through `AivyxConfig::load_from_env_and_toml`
/// (the full loader): that loader resolves `env`/`headers` against the
/// daemon's real environment, and a `GetX` response must never carry a
/// resolved secret value (see the design spec's binding secret-field
/// convention) — the second-order failure mode is worse than the leak
/// itself, since a naive edit-and-save round-trip would bake the resolved
/// secret back into `aivyx-pa.toml` as a literal, permanently destroying the
/// `${VAR}` placeholder. A missing file reads as an empty list (mirrors
/// [`load_document`]'s missing-file-is-empty posture); a malformed file
/// (bad TOML syntax) is a real error the caller should surface rather than
/// silently claim "zero servers".
///
/// Deliberately loose about anything beyond `${VAR}`-literal extraction: no
/// transport/command/url validation (that's [`write_mcp_server_section`]'s
/// job on the way *in*) — a disabled or even structurally incomplete entry
/// still round-trips here so the operator can see and fix it in Studio,
/// unlike the full loader which validates strictly and skips
/// `enabled = false` entries entirely.
pub fn read_mcp_server_entries(path: &Path) -> Result<Vec<McpServerEntryWrite>, ConfigWriteError> {
    let doc = load_document(path)?;
    let Some(arr) = doc.get("mcp_server").and_then(toml_edit::Item::as_array_of_tables) else {
        return Ok(Vec::new());
    };
    Ok(arr.iter().map(raw_table_to_entry).collect())
}

/// One `[[mcp_server]]` TOML table → its literal-value `McpServerEntryWrite`
/// mirror. Every field is read as a plain string/bool/array — no
/// interpolation, no defaulting beyond what makes an incomplete entry
/// displayable (e.g. a missing `enabled` reads as `true`, matching the
/// loader's own `default_true`).
fn raw_table_to_entry(table: &toml_edit::Table) -> McpServerEntryWrite {
    let str_field = |key: &str| table.get(key).and_then(|v| v.as_str()).map(str::to_string);
    let pairs_field = |key: &str| -> Vec<(String, String)> {
        table
            .get(key)
            .and_then(toml_edit::Item::as_table_like)
            .map(|t| {
                t.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.to_string(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default()
    };
    McpServerEntryWrite {
        name: str_field("name").unwrap_or_default(),
        transport: str_field("transport").unwrap_or_else(|| "stdio".to_string()),
        command: str_field("command"),
        args: table
            .get("args")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).map(str::to_string).collect())
            .unwrap_or_default(),
        env: pairs_field("env"),
        headers: pairs_field("headers"),
        url: str_field("url"),
        enabled: table.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true),
    }
}

/// The `[[mcp_server]]` array, creating an empty one if the section is
/// absent from the document yet.
fn mcp_server_array_mut(doc: &mut DocumentMut) -> &mut toml_edit::ArrayOfTables {
    if doc.get("mcp_server").and_then(toml_edit::Item::as_array_of_tables).is_none() {
        doc["mcp_server"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    doc["mcp_server"]
        .as_array_of_tables_mut()
        .expect("just ensured present")
}

/// One `[[notify_target]]` entry as Studio's write form submits it. A
/// plain (non-wire) struct — `aivyx-ipc` has its own serde-derived mirror;
/// `aivyx-channel`'s daemon handler converts between them. `chat_id`/
/// `url`/`to` are mutually exclusive per `kind` (mirrors
/// `NotifyTargetKind`'s own shape) but all three are plain `Option<String>`
/// here since only one is ever populated for a given `kind`.
pub struct NotifyTargetEntryWrite {
    pub name: String,
    /// `"telegram"`, `"webhook"`, `"email"`, or `"web-ui"`.
    pub kind: String,
    pub chat_id: Option<String>,
    pub url: Option<String>,
    pub to: Option<String>,
    pub enabled: bool,
    pub is_default: bool,
    pub retry_count: u32,
    pub retry_backoff_ms_start: u64,
    pub rate_limit_max: Option<u32>,
    pub rate_limit_window_secs: Option<u64>,
}

/// Add or replace (by `name`) one `[[notify_target]]` entry, preserving
/// every other entry, section, and the operator's comments. Validates the
/// same per-kind requirements the loader does (`aivyx-config/src/lib.rs`'s
/// notify_targets parsing: telegram needs a non-empty `chat_id`, webhook
/// needs an `http(s)://` `url`, email needs an `@`-containing `to`) plus
/// the at-most-one-default rule, refused here rather than failing the
/// next daemon load.
pub fn write_notify_target_section(
    path: &Path,
    target: &NotifyTargetEntryWrite,
) -> Result<(), ConfigWriteError> {
    if target.name.trim().is_empty() {
        return Err(ConfigWriteError::InvalidNotifyTarget {
            reason: "name must not be empty".to_string(),
        });
    }

    // Loaded up front (rather than after the per-kind match, as before)
    // because the "email" arm below needs to inspect the document for a
    // top-level `[email]` section.
    let mut doc = load_document(path)?;

    match target.kind.as_str() {
        "telegram" => {
            if target.chat_id.as_deref().is_none_or(str::is_empty) {
                return Err(ConfigWriteError::InvalidNotifyTarget {
                    reason: format!(
                        "target {:?}: kind \"telegram\" requires a non-empty `chat_id`",
                        target.name
                    ),
                });
            }
        }
        "webhook" => {
            let url = target.url.as_deref().unwrap_or("");
            if !url.starts_with("http://") && !url.starts_with("https://") {
                return Err(ConfigWriteError::InvalidNotifyTarget {
                    reason: format!(
                        "target {:?}: kind \"webhook\" requires a `url` starting with http:// or https://",
                        target.name
                    ),
                });
            }
        }
        "email" => {
            if !target.to.as_deref().unwrap_or("").contains('@') {
                return Err(ConfigWriteError::InvalidNotifyTarget {
                    reason: format!(
                        "target {:?}: kind \"email\" requires a `to` address containing `@`",
                        target.name
                    ),
                });
            }
            // Mirrors the loader's own check (`aivyx-config/src/lib.rs`'s
            // notify_targets parsing, `email.is_none()`): an email-kind
            // target requires `build_email_config` to actually return
            // `Some`, which happens only when at least one of the six real
            // `[email]` fields is set. A bare `[email]` header with every
            // key commented out — a normal operator hand-edit — has NO
            // fields set, so the loader treats it exactly like "no [email]
            // section at all" and the daemon refuses to boot next start
            // (final-review finding #2: the earlier `doc.get("email").is_none()`
            // check only tested for the TOML header, not for any field
            // being set, so it passed this case through). Checking the
            // header's mere presence is therefore not sufficient — check
            // for an actual field.
            if !email_section_has_any_field(&doc) {
                return Err(ConfigWriteError::InvalidNotifyTarget {
                    reason: format!(
                        "target {:?}: kind = \"email\" requires a top-level [email] section with SMTP credentials",
                        target.name
                    ),
                });
            }
        }
        "web-ui" => {}
        other => {
            return Err(ConfigWriteError::InvalidNotifyTarget {
                reason: format!(
                    "target {:?}: unknown kind {:?} (expected \"telegram\", \"webhook\", \"email\", or \"web-ui\")",
                    target.name, other
                ),
            });
        }
    }

    // The following three checks mirror the loader's own validation
    // (`aivyx-config/src/lib.rs`'s notify_targets parsing) exactly,
    // constants and all. Not reachable from the shipped Studio form
    // (which only round-trips existing values), but this function is a
    // public IPC surface any future caller could hit, and its own doc
    // comment claims to mirror the loader.
    if target.retry_count > crate::MAX_RETRY_COUNT {
        return Err(ConfigWriteError::InvalidNotifyTarget {
            reason: format!(
                "target {:?}: retry_count = {} exceeds the hard cap of {}",
                target.name,
                target.retry_count,
                crate::MAX_RETRY_COUNT,
            ),
        });
    }
    if target.retry_backoff_ms_start < crate::MIN_RETRY_BACKOFF_MS_START {
        return Err(ConfigWriteError::InvalidNotifyTarget {
            reason: format!(
                "target {:?}: retry_backoff_ms_start = {} ms is below the {} ms minimum",
                target.name,
                target.retry_backoff_ms_start,
                crate::MIN_RETRY_BACKOFF_MS_START,
            ),
        });
    }
    match (target.rate_limit_max, target.rate_limit_window_secs) {
        (Some(_), None) => {
            return Err(ConfigWriteError::InvalidNotifyTarget {
                reason: format!(
                    "target {:?}: declares `rate_limit_max` without `rate_limit_window_secs`; both fields must be set together (or neither)",
                    target.name
                ),
            });
        }
        (None, Some(_)) => {
            return Err(ConfigWriteError::InvalidNotifyTarget {
                reason: format!(
                    "target {:?}: declares `rate_limit_window_secs` without `rate_limit_max`; both fields must be set together (or neither)",
                    target.name
                ),
            });
        }
        (Some(0), _) => {
            return Err(ConfigWriteError::InvalidNotifyTarget {
                reason: format!(
                    "target {:?}: rate_limit_max = 0 is meaningless (no dispatches would ever be allowed)",
                    target.name
                ),
            });
        }
        (_, Some(0)) => {
            return Err(ConfigWriteError::InvalidNotifyTarget {
                reason: format!(
                    "target {:?}: rate_limit_window_secs = 0 is meaningless",
                    target.name
                ),
            });
        }
        _ => {}
    }

    let arr = notify_target_array_mut(&mut doc);

    if target.is_default {
        let other_default = arr.iter().find(|t| {
            t.get("name").and_then(|v| v.as_str()) != Some(target.name.as_str())
                && t.get("default").and_then(|v| v.as_bool()).unwrap_or(false)
        });
        if let Some(other) = other_default {
            let other_name = other.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            return Err(ConfigWriteError::InvalidNotifyTarget {
                reason: format!(
                    "target {:?}: another notify_target is already the default: {other_name} — at most one is allowed",
                    target.name
                ),
            });
        }
    }

    let mut table = toml_edit::Table::new();
    table["name"] = value(target.name.as_str());
    table["kind"] = value(target.kind.as_str());
    table["enabled"] = value(target.enabled);
    table["default"] = value(target.is_default);
    if let Some(chat_id) = &target.chat_id {
        table["chat_id"] = value(chat_id.as_str());
    }
    if let Some(url) = &target.url {
        table["url"] = value(url.as_str());
    }
    if let Some(to) = &target.to {
        table["to"] = value(to.as_str());
    }
    if target.retry_count != 0 {
        table["retry_count"] = value(target.retry_count as i64);
    }
    if target.retry_backoff_ms_start != 500 {
        table["retry_backoff_ms_start"] = value(target.retry_backoff_ms_start as i64);
    }
    if let Some(max) = target.rate_limit_max {
        table["rate_limit_max"] = value(max as i64);
    }
    if let Some(secs) = target.rate_limit_window_secs {
        table["rate_limit_window_secs"] = value(secs as i64);
    }

    // Preserve any key this write schema doesn't know about, exactly
    // mirroring `write_mcp_server_section`'s own defensive convention —
    // no known unexposed field exists on `[[notify_target]]` today, but
    // an upsert must never silently destroy one a future schema adds.
    const KNOWN_KEYS: &[&str] = &[
        "name", "kind", "enabled", "default", "chat_id", "url", "to",
        "retry_count", "retry_backoff_ms_start", "rate_limit_max", "rate_limit_window_secs",
    ];
    let idx = arr.iter().position(|t| t.get("name").and_then(|v| v.as_str()) == Some(target.name.as_str()));
    match idx {
        Some(i) => {
            if let Some(existing) = arr.get(i) {
                for (k, v) in existing.iter() {
                    if KNOWN_KEYS.contains(&k) {
                        continue;
                    }
                    table.insert(k, v.clone());
                }
            }
            *arr.get_mut(i).expect("index just found") = table;
        }
        None => arr.push(table),
    }

    write_toml_0600(path, &doc.to_string())
}

/// Remove one `[[notify_target]]` entry by `name`. A no-op (not an error)
/// when no entry with that name exists.
pub fn remove_notify_target_section(path: &Path, name: &str) -> Result<(), ConfigWriteError> {
    let mut doc = load_document(path)?;
    let arr = notify_target_array_mut(&mut doc);
    let idx = arr.iter().position(|t| t.get("name").and_then(|v| v.as_str()) == Some(name));
    if let Some(i) = idx {
        arr.remove(i);
    }
    write_toml_0600(path, &doc.to_string())
}

/// Read every `[[notify_target]]` entry as literally written on disk — no
/// resolution of anything, matching [`read_mcp_server_entries`]'s own
/// raw-TOML convention (this section carries no secrets of its own today,
/// but reading it the same way as every other section here keeps one
/// convention, not two, for future maintainers to reason about).
pub fn read_notify_target_entries(path: &Path) -> Result<Vec<NotifyTargetEntryWrite>, ConfigWriteError> {
    let doc = load_document(path)?;
    let Some(arr) = doc.get("notify_target").and_then(toml_edit::Item::as_array_of_tables) else {
        return Ok(Vec::new());
    };
    Ok(arr.iter().map(raw_table_to_notify_target).collect())
}

fn raw_table_to_notify_target(table: &toml_edit::Table) -> NotifyTargetEntryWrite {
    let str_field = |key: &str| table.get(key).and_then(|v| v.as_str()).map(str::to_string);
    NotifyTargetEntryWrite {
        name: str_field("name").unwrap_or_default(),
        kind: str_field("kind").unwrap_or_default(),
        chat_id: str_field("chat_id"),
        url: str_field("url"),
        to: str_field("to"),
        enabled: table.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true),
        is_default: table.get("default").and_then(|v| v.as_bool()).unwrap_or(false),
        retry_count: table.get("retry_count").and_then(|v| v.as_integer()).unwrap_or(0) as u32,
        retry_backoff_ms_start: table
            .get("retry_backoff_ms_start")
            .and_then(|v| v.as_integer())
            .unwrap_or(500) as u64,
        rate_limit_max: table.get("rate_limit_max").and_then(|v| v.as_integer()).map(|n| n as u32),
        rate_limit_window_secs: table
            .get("rate_limit_window_secs")
            .and_then(|v| v.as_integer())
            .map(|n| n as u64),
    }
}

/// The `[[notify_target]]` array, creating an empty one if the section is
/// absent from the document yet.
fn notify_target_array_mut(doc: &mut DocumentMut) -> &mut toml_edit::ArrayOfTables {
    if doc.get("notify_target").and_then(toml_edit::Item::as_array_of_tables).is_none() {
        doc["notify_target"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    doc["notify_target"]
        .as_array_of_tables_mut()
        .expect("just ensured present")
}

/// The `[email]` section as Studio's write form submits it — every field
/// `Option`, `None` meaning "leave this key untouched on disk" (the
/// partial-update convention every singleton section in this file uses).
pub struct EmailEntryWrite {
    pub host: Option<String>,
    pub port: Option<u16>,
    /// `"starttls"` or `"implicit"` (absent defaults to `"starttls"`) —
    /// matches the loader's own accepted strings. `"none"` is NOT
    /// accepted: `build_email_config` (`aivyx-config/src/lib.rs`) hard-
    /// rejects it, since Aivyx requires TLS for PLAIN/LOGIN auth.
    pub tls_mode: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub from: Option<String>,
}

/// The six real `[email]` fields the loader's `build_email_config`
/// (`aivyx-config/src/lib.rs`) checks for its own `any_set` gate — kept as
/// one constant so [`write_email_section`]'s merge logic and
/// [`email_section_has_any_field`] can never drift apart on what
/// "configured" means.
const EMAIL_FIELD_KEYS: &[&str] = &["host", "port", "tls_mode", "username", "password", "from"];

/// True if the on-disk `[email]` table (if present at all) has at least one
/// of its six real fields set — mirrors `build_email_config`'s own
/// `any_set` check exactly. A bare `[email]` header with every key absent
/// (all commented out, or simply never written) is, to the loader,
/// indistinguishable from no `[email]` section at all: `build_email_config`
/// returns `Ok(None)` either way. Used by [`write_notify_target_section`]'s
/// email-kind guard so it tests the same thing the loader tests, not merely
/// whether the `[email]` TOML header exists.
fn email_section_has_any_field(doc: &DocumentMut) -> bool {
    let Some(existing) = doc.get("email").and_then(toml_edit::Item::as_table_like) else {
        return false;
    };
    EMAIL_FIELD_KEYS.iter().any(|k| existing.contains_key(k))
}

/// Patch the `[email]` section, touching only the `Some` fields.
///
/// Before writing, validates the **post-write, merged** state against
/// `build_email_config`'s (`aivyx-config/src/lib.rs`) all-or-nothing rule:
/// if ANY of `host`/`port`/`tls_mode`/`username`/`password`/`from` ends up
/// set, `host`/`username`/`password` must be non-empty and `from` must
/// contain `@`, or this call is refused — mirroring the loader's own
/// `ConfigError::Invalid` exactly, refused here instead of bricking the
/// next daemon boot (final-review finding #1: a password-only write on a
/// fresh install used to save successfully and then fail to boot).
/// "Merged" means fields this call leaves `None` still count if they're
/// already set on disk — a save that only rotates `password` while
/// `host`/`username`/`from` are already on disk from an earlier save must
/// keep succeeding.
pub fn write_email_section(path: &Path, entry: &EmailEntryWrite) -> Result<(), ConfigWriteError> {
    let mut doc = load_document(path)?;

    let (merged_host, merged_username, merged_password, merged_from, merged_tls_mode, any_set) = {
        let existing = doc.get("email").and_then(toml_edit::Item::as_table_like);
        let existing_str =
            |key: &str| existing.and_then(|t| t.get(key)).and_then(|v| v.as_str()).map(str::to_string);
        let existing_has = |key: &str| existing.is_some_and(|t| t.contains_key(key));

        let merged_host = entry.host.clone().or_else(|| existing_str("host"));
        let merged_username = entry.username.clone().or_else(|| existing_str("username"));
        let merged_password = entry.password.clone().or_else(|| existing_str("password"));
        let merged_from = entry.from.clone().or_else(|| existing_str("from"));
        let merged_tls_mode = entry.tls_mode.clone().or_else(|| existing_str("tls_mode"));
        let merged_port_set = entry.port.is_some() || existing_has("port");
        let merged_tls_mode_set = entry.tls_mode.is_some() || existing_has("tls_mode");

        let any_set = merged_host.is_some()
            || merged_port_set
            || merged_tls_mode_set
            || merged_username.is_some()
            || merged_password.is_some()
            || merged_from.is_some();

        (merged_host, merged_username, merged_password, merged_from, merged_tls_mode, any_set)
    };

    if any_set {
        if merged_host.as_deref().unwrap_or("").trim().is_empty() {
            return Err(ConfigWriteError::InvalidEmailConfig {
                reason: "[email] section has a field set but `host` would be missing or empty \
                         — this would fail to boot the daemon on the next start"
                    .to_string(),
            });
        }
        if merged_username.as_deref().unwrap_or("").trim().is_empty() {
            return Err(ConfigWriteError::InvalidEmailConfig {
                reason: "[email] section has a field set but `username` would be missing or \
                         empty — this would fail to boot the daemon on the next start"
                    .to_string(),
            });
        }
        if merged_password.as_deref().unwrap_or("").trim().is_empty() {
            return Err(ConfigWriteError::InvalidEmailConfig {
                reason: "[email] section has a field set but `password` would be missing or \
                         empty — this would fail to boot the daemon on the next start"
                    .to_string(),
            });
        }
        if !merged_from.as_deref().unwrap_or("").contains('@') {
            return Err(ConfigWriteError::InvalidEmailConfig {
                reason: "[email] section has a field set but `from` would be missing or not \
                         contain `@` — this would fail to boot the daemon on the next start"
                    .to_string(),
            });
        }
        // Mirrors the loader's own accepted set exactly
        // (`build_email_config`, `aivyx-config/src/lib.rs`): a missing
        // `tls_mode` defaults to `starttls`, but an explicit value must be
        // `"starttls"` or `"implicit"` — `"none"` is hard-rejected (Aivyx
        // requires TLS for PLAIN/LOGIN auth) and any other string is
        // unrecognized. Writing either currently succeeds here without this
        // check and then bricks the next daemon boot — found during this
        // section's first re-review (a distinct issue from the original
        // final review's #1 password-only bug and #2 [email]-presence gap
        // above; same failure class as both, discovered one round later).
        if let Some(mode) = merged_tls_mode.as_deref() {
            if mode != "starttls" && mode != "implicit" {
                return Err(ConfigWriteError::InvalidEmailConfig {
                    reason: format!(
                        "[email] tls_mode = {mode:?} is not accepted by the loader — \
                         supported: \"starttls\" (default), \"implicit\" — this would fail \
                         to boot the daemon on the next start"
                    ),
                });
            }
        }
    }

    if let Some(v) = &entry.host {
        doc["email"]["host"] = value(v.as_str());
    }
    if let Some(v) = entry.port {
        doc["email"]["port"] = value(v as i64);
    }
    if let Some(v) = &entry.tls_mode {
        doc["email"]["tls_mode"] = value(v.as_str());
    }
    if let Some(v) = &entry.username {
        doc["email"]["username"] = value(v.as_str());
    }
    if let Some(v) = &entry.password {
        doc["email"]["password"] = value(v.as_str());
    }
    if let Some(v) = &entry.from {
        doc["email"]["from"] = value(v.as_str());
    }
    write_toml_0600(path, &doc.to_string())
}

/// Read the `[email]` section as literally written on disk — no
/// resolution of anything. Absent section (or absent key) reads as `None`.
pub fn read_email_section(path: &Path) -> Result<EmailEntryWrite, ConfigWriteError> {
    let doc = load_document(path)?;
    if let Some(email_item) = doc.get("email") {
        if let Some(table) = email_item.as_table_like() {
            let str_field = |key: &str| table.get(key).and_then(|v| v.as_str()).map(str::to_string);
            return Ok(EmailEntryWrite {
                host: str_field("host"),
                port: table.get("port").and_then(|v| v.as_integer()).map(|n| n as u16),
                tls_mode: str_field("tls_mode"),
                username: str_field("username"),
                password: str_field("password"),
                from: str_field("from"),
            });
        }
    }
    // If the section doesn't exist, return all None fields
    Ok(EmailEntryWrite {
        host: None,
        port: None,
        tls_mode: None,
        username: None,
        password: None,
        from: None,
    })
}

/// The `[telegram]` section as Studio's write form submits it. Partial
/// update, same convention as [`EmailEntryWrite`].
pub struct TelegramEntryWrite {
    pub token: Option<String>,
    pub chat_id: Option<i64>,
    pub team_run_channel: Option<bool>,
    pub team_trigger_rate_limit: Option<u32>,
    pub team_command_allowed_senders: Option<Vec<i64>>,
}

pub fn write_telegram_section(path: &Path, entry: &TelegramEntryWrite) -> Result<(), ConfigWriteError> {
    let mut doc = load_document(path)?;
    if let Some(v) = &entry.token {
        doc["telegram"]["token"] = value(v.as_str());
    }
    if let Some(v) = entry.chat_id {
        doc["telegram"]["chat_id"] = value(v);
    }
    if let Some(v) = entry.team_run_channel {
        doc["telegram"]["team_run_channel"] = value(v);
    }
    if let Some(v) = entry.team_trigger_rate_limit {
        doc["telegram"]["team_trigger_rate_limit"] = value(v as i64);
    }
    if let Some(ids) = &entry.team_command_allowed_senders {
        let mut arr = toml_edit::Array::new();
        for id in ids {
            arr.push(*id);
        }
        doc["telegram"]["team_command_allowed_senders"] = toml_edit::Item::Value(arr.into());
    }
    write_toml_0600(path, &doc.to_string())
}

pub fn read_telegram_section(path: &Path) -> Result<TelegramEntryWrite, ConfigWriteError> {
    let doc = load_document(path)?;
    if let Some(item) = doc.get("telegram") {
        if let Some(table) = item.as_table_like() {
            return Ok(TelegramEntryWrite {
                token: table.get("token").and_then(|v| v.as_str()).map(str::to_string),
                chat_id: table.get("chat_id").and_then(|v| v.as_integer()),
                team_run_channel: table.get("team_run_channel").and_then(|v| v.as_bool()),
                team_trigger_rate_limit: table
                    .get("team_trigger_rate_limit")
                    .and_then(|v| v.as_integer())
                    .map(|n| n as u32),
                team_command_allowed_senders: table
                    .get("team_command_allowed_senders")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_integer()).collect()),
            });
        }
    }
    Ok(TelegramEntryWrite {
        token: None,
        chat_id: None,
        team_run_channel: None,
        team_trigger_rate_limit: None,
        team_command_allowed_senders: None,
    })
}

/// The `[discord]` section as Studio's write form submits it. Partial
/// update, same convention as [`EmailEntryWrite`].
pub struct DiscordEntryWrite {
    pub token: Option<String>,
    pub application_id: Option<u64>,
    pub team_run_channel: Option<bool>,
    pub team_trigger_rate_limit: Option<u32>,
    pub team_command_allowed_senders: Option<Vec<u64>>,
}

pub fn write_discord_section(path: &Path, entry: &DiscordEntryWrite) -> Result<(), ConfigWriteError> {
    let mut doc = load_document(path)?;
    if let Some(v) = &entry.token {
        doc["discord"]["token"] = value(v.as_str());
    }
    if let Some(v) = entry.application_id {
        doc["discord"]["application_id"] = value(v as i64);
    }
    if let Some(v) = entry.team_run_channel {
        doc["discord"]["team_run_channel"] = value(v);
    }
    if let Some(v) = entry.team_trigger_rate_limit {
        doc["discord"]["team_trigger_rate_limit"] = value(v as i64);
    }
    if let Some(ids) = &entry.team_command_allowed_senders {
        let mut arr = toml_edit::Array::new();
        for id in ids {
            arr.push(*id as i64);
        }
        doc["discord"]["team_command_allowed_senders"] = toml_edit::Item::Value(arr.into());
    }
    write_toml_0600(path, &doc.to_string())
}

pub fn read_discord_section(path: &Path) -> Result<DiscordEntryWrite, ConfigWriteError> {
    let doc = load_document(path)?;
    if let Some(item) = doc.get("discord") {
        if let Some(table) = item.as_table_like() {
            return Ok(DiscordEntryWrite {
                token: table.get("token").and_then(|v| v.as_str()).map(str::to_string),
                application_id: table
                    .get("application_id")
                    .and_then(|v| v.as_integer())
                    .map(|n| n as u64),
                team_run_channel: table.get("team_run_channel").and_then(|v| v.as_bool()),
                team_trigger_rate_limit: table
                    .get("team_trigger_rate_limit")
                    .and_then(|v| v.as_integer())
                    .map(|n| n as u32),
                team_command_allowed_senders: table
                    .get("team_command_allowed_senders")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_integer()).map(|n| n as u64).collect()),
            });
        }
    }
    Ok(DiscordEntryWrite {
        token: None,
        application_id: None,
        team_run_channel: None,
        team_trigger_rate_limit: None,
        team_command_allowed_senders: None,
    })
}

/// The `[slack]` section as Studio's write form submits it. Partial
/// update, same convention as [`EmailEntryWrite`] — two independent
/// secrets (`bot_token`/`app_token`), each individually optional so one
/// can be rotated without touching the other.
pub struct SlackEntryWrite {
    pub bot_token: Option<String>,
    pub app_token: Option<String>,
    pub team_id: Option<String>,
    pub team_run_channel: Option<bool>,
    pub team_trigger_rate_limit: Option<u32>,
    pub team_command_allowed_senders: Option<Vec<String>>,
}

pub fn write_slack_section(path: &Path, entry: &SlackEntryWrite) -> Result<(), ConfigWriteError> {
    let mut doc = load_document(path)?;
    if let Some(v) = &entry.bot_token {
        doc["slack"]["bot_token"] = value(v.as_str());
    }
    if let Some(v) = &entry.app_token {
        doc["slack"]["app_token"] = value(v.as_str());
    }
    if let Some(v) = &entry.team_id {
        doc["slack"]["team_id"] = value(v.as_str());
    }
    if let Some(v) = entry.team_run_channel {
        doc["slack"]["team_run_channel"] = value(v);
    }
    if let Some(v) = entry.team_trigger_rate_limit {
        doc["slack"]["team_trigger_rate_limit"] = value(v as i64);
    }
    if let Some(ids) = &entry.team_command_allowed_senders {
        let mut arr = toml_edit::Array::new();
        for id in ids {
            arr.push(id.as_str());
        }
        doc["slack"]["team_command_allowed_senders"] = toml_edit::Item::Value(arr.into());
    }
    write_toml_0600(path, &doc.to_string())
}

pub fn read_slack_section(path: &Path) -> Result<SlackEntryWrite, ConfigWriteError> {
    let doc = load_document(path)?;
    if let Some(item) = doc.get("slack") {
        if let Some(table) = item.as_table_like() {
            return Ok(SlackEntryWrite {
                bot_token: table.get("bot_token").and_then(|v| v.as_str()).map(str::to_string),
                app_token: table.get("app_token").and_then(|v| v.as_str()).map(str::to_string),
                team_id: table.get("team_id").and_then(|v| v.as_str()).map(str::to_string),
                team_run_channel: table.get("team_run_channel").and_then(|v| v.as_bool()),
                team_trigger_rate_limit: table
                    .get("team_trigger_rate_limit")
                    .and_then(|v| v.as_integer())
                    .map(|n| n as u32),
                team_command_allowed_senders: table
                    .get("team_command_allowed_senders")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str()).map(str::to_string).collect()),
            });
        }
    }
    Ok(SlackEntryWrite {
        bot_token: None,
        app_token: None,
        team_id: None,
        team_run_channel: None,
        team_trigger_rate_limit: None,
        team_command_allowed_senders: None,
    })
}

/// Stable `[budget] on_exceeded` token — matches `BudgetAction`'s
/// `#[serde(rename_all = "snake_case")]` repr so a written file round-trips
/// through the loader unchanged.
fn budget_action_str(action: BudgetAction) -> &'static str {
    match action {
        BudgetAction::Alert => "alert",
        BudgetAction::Deny => "deny",
    }
}

/// Set `[budget].<key>` to `v`, or remove the key when `v` is `None`
/// (uncapped — the loader's default).
fn set_or_clear_f64(doc: &mut DocumentMut, key: &str, v: Option<f64>) {
    match v {
        Some(n) => doc["budget"][key] = value(n),
        None => {
            if let Some(t) = doc.get_mut("budget").and_then(|b| b.as_table_mut()) {
                t.remove(key);
            }
        }
    }
}

/// Parse the file at `path` into an editable document; a missing file is an
/// empty document (the caller's section is created).
fn load_document(path: &Path) -> Result<DocumentMut, ConfigWriteError> {
    let original = if path.exists() {
        std::fs::read_to_string(path).map_err(|e| ConfigWriteError::Io {
            reason: format!("failed to read {}: {e}", path.display()),
        })?
    } else {
        String::new()
    };
    original
        .parse::<DocumentMut>()
        .map_err(|e| ConfigWriteError::Parse {
            reason: format!("{} is not valid TOML: {e}", path.display()),
        })
}

/// Write `contents` to `path` and pin it to `0600` (the file may carry secrets
/// in other sections — same posture every config writer uses). Public so other
/// native config writers that own a whole file (Chapter Roster's team-config
/// writer) reuse the one permission-pinning path.
pub fn write_toml_0600(path: &Path, contents: &str) -> Result<(), ConfigWriteError> {
    std::fs::write(path, contents).map_err(|e| ConfigWriteError::Io {
        reason: format!("failed to write {}: {e}", path.display()),
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(path, perms).map_err(|e| ConfigWriteError::Io {
            reason: format!("failed to set permissions on {}: {e}", path.display()),
        })?;
    }
    Ok(())
}

/// The `[[reflection_schedule]]` array, as Studio's write form submits
/// it. `role_override`/`skip_when_idle`/`min_audit_entries_to_fire` stay
/// TOML-only — an upsert must preserve them via the `KNOWN_KEYS`
/// mechanism below, never silently drop them.
#[derive(Debug, Clone, PartialEq)]
pub struct ReflectionScheduleEntryWrite {
    pub name: String,
    pub cron: String,
    pub lookback_window_secs: u64,
    pub enabled: bool,
}

/// Add or replace (by `name`) one `[[reflection_schedule]]` entry,
/// preserving every other entry, section, and the operator's comments.
/// Validates the same rules the loader does
/// (`aivyx-config/src/lib.rs:7251`-7301): non-empty `name`/`cron`,
/// `lookback_window_secs` within `[MIN_REFLECTION_LOOKBACK_SECS,
/// MAX_REFLECTION_LOOKBACK_SECS]` (60s – 30 days), and name uniqueness
/// against `[[schedule]]` too — the loader rejects a reflection-schedule
/// name that collides with a regular schedule name (shared namespace).
/// Uniqueness *within* `[[reflection_schedule]]` itself needs no explicit
/// check: this function always upserts by name, so a matching existing
/// name is a replace, never a collision.
pub fn write_reflection_schedule_section(
    path: &Path,
    entry: &ReflectionScheduleEntryWrite,
) -> Result<(), ConfigWriteError> {
    if entry.name.trim().is_empty() {
        return Err(ConfigWriteError::InvalidReflectionSchedule {
            reason: "name must not be empty".to_string(),
        });
    }
    if entry.cron.trim().is_empty() {
        return Err(ConfigWriteError::InvalidReflectionSchedule {
            reason: format!("entry {:?}: cron must not be empty", entry.name),
        });
    }
    if entry.lookback_window_secs < crate::MIN_REFLECTION_LOOKBACK_SECS
        || entry.lookback_window_secs > crate::MAX_REFLECTION_LOOKBACK_SECS
    {
        return Err(ConfigWriteError::InvalidReflectionSchedule {
            reason: format!(
                "entry {:?}: lookback_window_secs = {} is outside the allowed range [{}, {}] (60s to 30 days)",
                entry.name,
                entry.lookback_window_secs,
                crate::MIN_REFLECTION_LOOKBACK_SECS,
                crate::MAX_REFLECTION_LOOKBACK_SECS,
            ),
        });
    }

    let mut doc = load_document(path)?;

    // Cross-array uniqueness against [[schedule]] — the loader's own
    // check, mirrored here (aivyx-config/src/lib.rs:7292-7300).
    if let Some(sched_arr) = doc.get("schedule").and_then(toml_edit::Item::as_array_of_tables) {
        if sched_arr
            .iter()
            .any(|t| t.get("name").and_then(|v| v.as_str()) == Some(entry.name.as_str()))
        {
            return Err(ConfigWriteError::InvalidReflectionSchedule {
                reason: format!(
                    "entry {:?}: collides with a [[schedule]] entry of the same name — names share a namespace",
                    entry.name
                ),
            });
        }
    }

    let arr = reflection_schedule_array_mut(&mut doc);

    let mut table = toml_edit::Table::new();
    table["name"] = value(entry.name.as_str());
    table["cron"] = value(entry.cron.as_str());
    table["lookback_window_secs"] = value(entry.lookback_window_secs as i64);
    table["enabled"] = value(entry.enabled);

    // Preserve any key this write schema doesn't know about —
    // role_override/skip_when_idle/min_audit_entries_to_fire — mirroring
    // write_mcp_server_section's/write_notify_target_section's own
    // defensive convention (plan 1's [mcp_server.sandbox] incident is
    // exactly the failure class this guards against).
    const KNOWN_KEYS: &[&str] = &["name", "cron", "lookback_window_secs", "enabled"];
    let idx = arr
        .iter()
        .position(|t| t.get("name").and_then(|v| v.as_str()) == Some(entry.name.as_str()));
    match idx {
        Some(i) => {
            if let Some(existing) = arr.get(i) {
                for (k, v) in existing.iter() {
                    if KNOWN_KEYS.contains(&k) {
                        continue;
                    }
                    table.insert(k, v.clone());
                }
            }
            *arr.get_mut(i).expect("index just found") = table;
        }
        None => arr.push(table),
    }

    write_toml_0600(path, &doc.to_string())
}

/// Remove one `[[reflection_schedule]]` entry by `name`. A no-op (not an
/// error) when no entry with that name exists.
pub fn remove_reflection_schedule_section(path: &Path, name: &str) -> Result<(), ConfigWriteError> {
    let mut doc = load_document(path)?;
    let arr = reflection_schedule_array_mut(&mut doc);
    let idx = arr.iter().position(|t| t.get("name").and_then(|v| v.as_str()) == Some(name));
    if let Some(i) = idx {
        arr.remove(i);
    }
    write_toml_0600(path, &doc.to_string())
}

/// Read every `[[reflection_schedule]]` entry as literally written on
/// disk — no resolution, and critically NOT the resolving loader's
/// `reflection_schedules: Vec<ReflectionScheduleConfig>`, which silently
/// drops any entry with `enabled = false` entirely (see this plan's
/// design spec, "Corrections" §3). The Studio list view must show
/// disabled entries too, so an operator can re-enable one.
pub fn read_reflection_schedule_entries(path: &Path) -> Result<Vec<ReflectionScheduleEntryWrite>, ConfigWriteError> {
    let doc = load_document(path)?;
    let Some(arr) = doc.get("reflection_schedule").and_then(toml_edit::Item::as_array_of_tables) else {
        return Ok(Vec::new());
    };
    Ok(arr.iter().map(raw_table_to_reflection_schedule).collect())
}

fn raw_table_to_reflection_schedule(table: &toml_edit::Table) -> ReflectionScheduleEntryWrite {
    let str_field = |key: &str| table.get(key).and_then(|v| v.as_str()).map(str::to_string);
    ReflectionScheduleEntryWrite {
        name: str_field("name").unwrap_or_default(),
        cron: str_field("cron").unwrap_or_default(),
        lookback_window_secs: table
            .get("lookback_window_secs")
            .and_then(|v| v.as_integer())
            .unwrap_or(86_400) as u64,
        enabled: table.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true),
    }
}

/// The `[[reflection_schedule]]` array, creating an empty one if the
/// section is absent from the document yet.
fn reflection_schedule_array_mut(doc: &mut DocumentMut) -> &mut toml_edit::ArrayOfTables {
    if doc
        .get("reflection_schedule")
        .and_then(toml_edit::Item::as_array_of_tables)
        .is_none()
    {
        doc["reflection_schedule"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    doc["reflection_schedule"]
        .as_array_of_tables_mut()
        .expect("just ensured present")
}

/// `[memory]` is a shared table with fields this function does not touch
/// (`max_per_topic`, `ttl_secs`, the `[[memory.retention]]` array-of-
/// tables, `canonicalize_topics`) — only the `profile` key is
/// added/updated; every sibling key and the retention sub-array stay
/// byte-identical. `profile: None` leaves the on-disk value untouched
/// (leave-on-`None`, plan 2's convention).
pub fn write_memory_profile(path: &Path, profile: Option<&str>) -> Result<(), ConfigWriteError> {
    let Some(p) = profile else { return Ok(()) };
    let normalized = match p.trim().to_lowercase().as_str() {
        "off" | "lite" | "smart" => p.trim().to_lowercase(),
        other => {
            return Err(ConfigWriteError::InvalidMemoryProfile {
                reason: format!("{other:?} is not a valid profile — must be \"off\", \"lite\", or \"smart\""),
            })
        }
    };
    let mut doc = load_document(path)?;
    doc["memory"]["profile"] = value(normalized);
    write_toml_0600(path, &doc.to_string())
}

/// Raw read of `[memory] profile` — `None` when the section or key is
/// absent (the Studio picker's caller defaults that to `"off"`).
pub fn read_memory_profile(path: &Path) -> Result<Option<String>, ConfigWriteError> {
    let doc = load_document(path)?;
    Ok(doc
        .get("memory")
        .and_then(toml_edit::Item::as_table_like)
        .and_then(|t| t.get("profile"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string()))
}

/// The `[embedding]` section as Studio's write form submits it. Every
/// field `Option`, `None` meaning "leave this key untouched on disk" —
/// same partial-update convention every singleton section in this file
/// uses, applied uniformly here even though only `api_key` is a secret
/// (plan 2's hardened precedent, not `write_profile_section`'s older
/// clear-on-`None` convention). `dimensions`/`rag_top_k`/
/// `rag_min_similarity`/`recall_window_turns`/`recall_gate_min_chars`
/// and the Chapter Loom recall-fusion tuning fields stay TOML-only —
/// not represented here at all.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EmbeddingEntryWrite {
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
}

/// Patch the `[embedding]` section, touching only the `Some` fields. An
/// explicit-but-blank `base_url`/`model` is refused (would write an
/// empty string, which the loader's own defaulting treats differently
/// from "key absent") rather than silently accepted.
pub fn write_embedding_section(path: &Path, entry: &EmbeddingEntryWrite) -> Result<(), ConfigWriteError> {
    if let Some(v) = &entry.base_url {
        if v.trim().is_empty() {
            return Err(ConfigWriteError::InvalidEmbeddingConfig {
                reason: "base_url, if set, must not be blank".to_string(),
            });
        }
    }
    if let Some(v) = &entry.model {
        if v.trim().is_empty() {
            return Err(ConfigWriteError::InvalidEmbeddingConfig {
                reason: "model, if set, must not be blank".to_string(),
            });
        }
    }

    let mut doc = load_document(path)?;
    if let Some(v) = &entry.base_url {
        doc["embedding"]["base_url"] = value(v.as_str());
    }
    if let Some(v) = &entry.model {
        doc["embedding"]["model"] = value(v.as_str());
    }
    if let Some(v) = &entry.api_key {
        doc["embedding"]["api_key"] = value(v.as_str());
    }
    write_toml_0600(path, &doc.to_string())
}

/// Read the `[embedding]` section as literally written on disk — no
/// resolution of anything. Absent section (or absent key) reads as
/// `None` for that field.
pub fn read_embedding_section(path: &Path) -> Result<EmbeddingEntryWrite, ConfigWriteError> {
    let doc = load_document(path)?;
    let Some(table) = doc.get("embedding").and_then(toml_edit::Item::as_table_like) else {
        return Ok(EmbeddingEntryWrite::default());
    };
    Ok(EmbeddingEntryWrite {
        base_url: table.get("base_url").and_then(|v| v.as_str()).map(str::to_string),
        model: table.get("model").and_then(|v| v.as_str()).map(str::to_string),
        api_key: table.get("api_key").and_then(|v| v.as_str()).map(str::to_string),
    })
}

/// The `[proactive]` section as Studio's write form submits it. Same
/// leave-untouched-on-`None` convention as `EmbeddingEntryWrite`.
/// `signals` (the 3 `signal_*` toggles) stays TOML-only — not
/// represented here.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProactiveEntryWrite {
    pub enabled: Option<bool>,
    pub target: Option<String>,
    pub max_per_window: Option<u32>,
    pub window_secs: Option<u64>,
}

/// Patch the `[proactive]` section, touching only the `Some` fields.
///
/// Validates the **merged** post-write state (existing on-disk values
/// combined with this call's `Some` fields — a save that only flips
/// `enabled` while `target` is already on disk from an earlier save
/// must keep succeeding) against `build_proactive_config`'s
/// (`aivyx-config/src/lib.rs:8689`) own `enabled = true` requirements:
/// non-empty `target`, `max_per_window >= 1`, `window_secs >= 1`. Also
/// checks that `target` names an existing, *enabled* `[[notify_target]]`
/// entry — the loader itself does NOT make this check (it only checks
/// non-empty), so this is deliberately stricter, not a mirror. It must
/// check `enabled` too: the loader silently drops disabled
/// `[[notify_target]]` entries from its resolved list, so a target that
/// merely exists by name but is disabled would load fine and then fail
/// at dispatch time forever, silently (see `NotifyDispatcher::dispatch`
/// / `NotifyError::UnknownTarget`, which today only reaches an
/// `eprintln!` in `reflection_scheduler.rs`, never the operator).
///
/// Known limitation: this check only runs at WRITE time. If the referenced
/// [[notify_target]] is later disabled or deleted through the separate
/// notify-target CRUD surface, [proactive].target is not re-validated —
/// it silently goes stale until the operator next saves this section.
/// Fixing this would need either a boot-time cross-validation pass or a
/// delete-time cross-check in the notify-target write path; out of scope
/// here (POLISH_WAVES.md sub-project 7 plan 3's final review flagged this
/// explicitly as a documented deferral, not an oversight).
pub fn write_proactive_section(path: &Path, entry: &ProactiveEntryWrite) -> Result<(), ConfigWriteError> {
    let mut doc = load_document(path)?;

    let (merged_enabled, merged_target, merged_max_per_window, merged_window_secs) = {
        let existing = doc.get("proactive").and_then(toml_edit::Item::as_table_like);
        let existing_bool = |key: &str| existing.and_then(|t| t.get(key)).and_then(|v| v.as_bool());
        let existing_str =
            |key: &str| existing.and_then(|t| t.get(key)).and_then(|v| v.as_str()).map(str::to_string);
        let existing_int = |key: &str| existing.and_then(|t| t.get(key)).and_then(|v| v.as_integer());

        let merged_enabled = entry.enabled.or_else(|| existing_bool("enabled")).unwrap_or(false);
        let merged_target = entry.target.clone().or_else(|| existing_str("target"));
        let merged_max_per_window = entry
            .max_per_window
            .or_else(|| existing_int("max_per_window").map(|n| n as u32))
            .unwrap_or(crate::DEFAULT_PROACTIVE_MAX_PER_WINDOW);
        let merged_window_secs = entry
            .window_secs
            .or_else(|| existing_int("window_secs").map(|n| n as u64))
            .unwrap_or(crate::DEFAULT_PROACTIVE_WINDOW_SECS);

        (merged_enabled, merged_target, merged_max_per_window, merged_window_secs)
    };

    if merged_enabled {
        let target = merged_target.as_deref().unwrap_or("").trim().to_string();
        if target.is_empty() {
            return Err(ConfigWriteError::InvalidProactiveConfig {
                reason: "`target` is required when proactive is enabled (must name a [[notify_target]]) \
                         — this would fail to boot the daemon on the next start"
                    .to_string(),
            });
        }
        let matched_target = doc
            .get("notify_target")
            .and_then(toml_edit::Item::as_array_of_tables)
            .and_then(|arr| {
                arr.iter().find(|t| t.get("name").and_then(|v| v.as_str()) == Some(target.as_str()))
            });
        match matched_target {
            None => {
                return Err(ConfigWriteError::InvalidProactiveConfig {
                    reason: format!("target {target:?} does not name a configured [[notify_target]] entry"),
                });
            }
            Some(t) => {
                let target_enabled = t.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
                if !target_enabled {
                    return Err(ConfigWriteError::InvalidProactiveConfig {
                        reason: format!(
                            "target {target:?} is a configured [[notify_target]] but is disabled — \
                             enable it on the Notifications screen first, or choose a different target"
                        ),
                    });
                }
            }
        }
        if merged_max_per_window == 0 {
            return Err(ConfigWriteError::InvalidProactiveConfig {
                reason: "`max_per_window` must be >= 1 when proactive is enabled".to_string(),
            });
        }
        if merged_window_secs == 0 {
            return Err(ConfigWriteError::InvalidProactiveConfig {
                reason: "`window_secs` must be >= 1 when proactive is enabled".to_string(),
            });
        }
    }

    if let Some(v) = entry.enabled {
        doc["proactive"]["enabled"] = value(v);
    }
    if let Some(v) = &entry.target {
        doc["proactive"]["target"] = value(v.as_str());
    }
    if let Some(v) = entry.max_per_window {
        doc["proactive"]["max_per_window"] = value(v as i64);
    }
    if let Some(v) = entry.window_secs {
        doc["proactive"]["window_secs"] = value(v as i64);
    }

    write_toml_0600(path, &doc.to_string())
}

/// Read the `[proactive]` section as literally written on disk — no
/// resolution of anything. Absent section (or absent key) reads as
/// `None` for that field.
pub fn read_proactive_section(path: &Path) -> Result<ProactiveEntryWrite, ConfigWriteError> {
    let doc = load_document(path)?;
    let Some(table) = doc.get("proactive").and_then(toml_edit::Item::as_table_like) else {
        return Ok(ProactiveEntryWrite::default());
    };
    Ok(ProactiveEntryWrite {
        enabled: table.get("enabled").and_then(|v| v.as_bool()),
        target: table.get("target").and_then(|v| v.as_str()).map(str::to_string),
        max_per_window: table.get("max_per_window").and_then(|v| v.as_integer()).map(|n| n as u32),
        window_secs: table.get("window_secs").and_then(|v| v.as_integer()).map(|n| n as u64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique temp file path for one test (no external tempdir dep).
    fn temp_toml(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "aivyx-cfgwrite-{}-{}-{tag}.toml",
            std::process::id(),
            // a cheap per-call nonce so parallel tests don't collide
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        p
    }

    #[test]
    fn access_writes_level_and_drops_stale_root() {
        let path = temp_toml("access");
        std::fs::write(&path, "[access]\nlevel = \"workspace\"\nroot = \"/old\"\n").unwrap();
        write_access_section(&path, AccessLevel::Home, None).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("level = \"home\""), "{out}");
        assert!(
            !out.contains("confirm_destructive"),
            "the autonomy level decides confirmation, not the access level: {out}"
        );
        assert!(!out.contains("/old"), "stale root must be dropped: {out}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn autonomy_writes_level_and_preserves_overrides() {
        let path = temp_toml("autonomy");
        // A pre-existing override + an unrelated section must both survive a
        // level rewrite (only `[autonomy] level` is touched).
        std::fs::write(
            &path,
            "[access]\nlevel = \"home\"\n\n[autonomy]\nlevel = \"assisted\"\n\
             \n[[autonomy.override]]\ndomain = \"email\"\nlevel = \"manual\"\n",
        )
        .unwrap();
        write_autonomy_section(&path, AutonomyLevel::Autonomous).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("level = \"autonomous\""), "{out}");
        assert!(out.contains("[access]"), "other sections preserved: {out}");
        assert!(
            out.contains("[[autonomy.override]]") && out.contains("email"),
            "overrides must survive a level rewrite: {out}"
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn access_set_hands_confirmation_back_to_autonomy() {
        // An older `access set sandbox` wrote `confirm_destructive = false`,
        // silently switching off delete/overwrite confirmation at every
        // autonomy level. Changing access now removes the key instead.
        let path = temp_toml("sandbox");
        std::fs::write(&path, "[access]\nlevel = \"home\"\nconfirm_destructive = false\n").unwrap();
        write_access_section(&path, AccessLevel::Sandbox, None).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("level = \"sandbox\""), "{out}");
        assert!(!out.contains("confirm_destructive"), "{out}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn access_level_from_wire_round_trips_as_str() {
        for lvl in [
            AccessLevel::Sandbox,
            AccessLevel::Workspace,
            AccessLevel::Home,
            AccessLevel::Full,
            AccessLevel::Custom,
        ] {
            assert_eq!(AccessLevel::from_wire(lvl.as_str()), Some(lvl));
        }
        assert_eq!(AccessLevel::from_wire("bogus"), None);
    }

    #[test]
    fn access_workspace_requires_root() {
        let path = temp_toml("ws");
        let err = write_access_section(&path, AccessLevel::Workspace, None).unwrap_err();
        assert_eq!(err, ConfigWriteError::RootRequired { level: AccessLevel::Workspace });
        assert!(!path.exists(), "no file should be written on a validation error");
    }

    #[test]
    fn access_home_rejects_explicit_root() {
        let path = temp_toml("homeroot");
        let err = write_access_section(&path, AccessLevel::Home, Some("/x")).unwrap_err();
        assert_eq!(err, ConfigWriteError::RootNotAllowed { level: AccessLevel::Home });
    }

    #[test]
    fn access_custom_writes_root() {
        let path = temp_toml("custom");
        write_access_section(&path, AccessLevel::Custom, Some("/srv/agent")).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("level = \"custom\""), "{out}");
        assert!(out.contains("root = \"/srv/agent\""), "{out}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn access_preserves_other_sections_and_comments() {
        let path = temp_toml("preserve");
        std::fs::write(
            &path,
            "# my config\n[profile]\nassistant_name = \"Aivyx PA\"\n\n[access]\nlevel = \"sandbox\"\n",
        )
        .unwrap();
        write_access_section(&path, AccessLevel::Full, None).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("# my config"), "comment preserved: {out}");
        assert!(out.contains("assistant_name = \"Aivyx PA\""), "other section preserved: {out}");
        assert!(out.contains("level = \"full\""), "{out}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn budget_writes_caps_action_and_alert() {
        let path = temp_toml("budget");
        let b = BudgetConfig {
            per_run_usd: Some(5.0),
            per_day_usd: Some(20.0),
            on_exceeded: BudgetAction::Deny,
            alert_at: Some(0.8),
            ..Default::default()
        };
        write_budget_section(&path, &b).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("per_run_usd = 5.0"), "{out}");
        assert!(out.contains("per_day_usd = 20.0"), "{out}");
        assert!(out.contains("on_exceeded = \"deny\""), "{out}");
        assert!(out.contains("alert_at = 0.8"), "{out}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn cycle_detection_writes_the_agent_flag_and_preserves_siblings() {
        let path = temp_toml("cycle");
        std::fs::write(&path, "[agent]\nprovider = \"ollama\"\n").unwrap();
        write_agent_cycle_detection(&path, true).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("cycle_detection = true"), "{out}");
        assert!(out.contains("provider = \"ollama\""), "sibling key preserved: {out}");
        // Toggling off writes the explicit `false`.
        write_agent_cycle_detection(&path, false).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("cycle_detection = false"), "{out}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn budget_clears_none_caps() {
        let path = temp_toml("budget-clear");
        std::fs::write(&path, "[budget]\nper_run_usd = 9.0\nper_day_usd = 9.0\n").unwrap();
        let b = BudgetConfig {
            per_run_usd: None,
            per_day_usd: Some(15.0),
            on_exceeded: BudgetAction::Alert,
            alert_at: None,
            ..Default::default()
        };
        write_budget_section(&path, &b).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(!out.contains("per_run_usd"), "None cap must be cleared: {out}");
        assert!(out.contains("per_day_usd = 15.0"), "{out}");
        assert!(out.contains("on_exceeded = \"alert\""), "{out}");
        assert!(!out.contains("alert_at"), "None alert_at must be cleared: {out}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn budget_rejects_negative_cap() {
        let path = temp_toml("budget-neg");
        let b = BudgetConfig {
            per_run_usd: Some(-1.0),
            ..Default::default()
        };
        let err = write_budget_section(&path, &b).unwrap_err();
        assert!(matches!(err, ConfigWriteError::InvalidBudget { .. }), "{err:?}");
        assert!(!path.exists(), "no file should be written on a validation error");
    }

    #[test]
    fn budget_rejects_out_of_range_alert() {
        let path = temp_toml("budget-alert");
        let b = BudgetConfig {
            alert_at: Some(1.5),
            ..Default::default()
        };
        let err = write_budget_section(&path, &b).unwrap_err();
        assert!(matches!(err, ConfigWriteError::InvalidBudget { .. }), "{err:?}");
    }

    #[test]
    fn profile_writes_scalars_and_lists() {
        let path = temp_toml("profile");
        let p = ProfileWrite {
            assistant_name: Some("Aria".to_string()),
            operator_profile: Some("Indie game dev".to_string()),
            communication_style: Some("terse".to_string()),
            primary_use_cases: Some(vec!["coding".to_string(), "research".to_string()]),
            behavioral_preferences: Some(vec!["cite sources".to_string()]),
            behavioral_constraints: Some(vec!["no secrets in logs".to_string()]),
        };
        write_profile_section(&path, &p).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("assistant_name = \"Aria\""), "{out}");
        assert!(out.contains("operator_profile = \"Indie game dev\""), "{out}");
        assert!(out.contains("communication_style = \"terse\""), "{out}");
        assert!(out.contains(r#"primary_use_cases = ["coding", "research"]"#), "{out}");
        assert!(out.contains(r#"behavioral_preferences = ["cite sources"]"#), "{out}");
        assert!(out.contains(r#"behavioral_constraints = ["no secrets in logs"]"#), "{out}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn profile_clears_none_fields() {
        let path = temp_toml("profile-clear");
        std::fs::write(
            &path,
            "[profile]\nassistant_name = \"Old\"\noperator_profile = \"gone\"\nprimary_use_cases = [\"a\"]\n",
        )
        .unwrap();
        let p = ProfileWrite {
            assistant_name: Some("New".to_string()),
            // operator_profile + primary_use_cases left None → cleared
            ..Default::default()
        };
        write_profile_section(&path, &p).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("assistant_name = \"New\""), "{out}");
        assert!(!out.contains("operator_profile"), "None scalar must be cleared: {out}");
        assert!(!out.contains("primary_use_cases"), "None list must be cleared: {out}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn profile_normalizes_blanks() {
        let path = temp_toml("profile-blank");
        let p = ProfileWrite {
            // all-whitespace scalar → cleared (treated as None)
            assistant_name: Some("   ".to_string()),
            // list with blanks → trimmed, empties dropped
            primary_use_cases: Some(vec![
                "  coding  ".to_string(),
                "".to_string(),
                "   ".to_string(),
                "ops".to_string(),
            ]),
            ..Default::default()
        };
        write_profile_section(&path, &p).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(!out.contains("assistant_name"), "blank scalar must clear: {out}");
        assert!(out.contains(r#"primary_use_cases = ["coding", "ops"]"#), "{out}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn profile_explicit_empty_list_is_declared_empty() {
        let path = temp_toml("profile-empty-list");
        let p = ProfileWrite {
            behavioral_preferences: Some(vec![]),
            ..Default::default()
        };
        write_profile_section(&path, &p).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        // Some(vec![]) is "declared but empty" — distinct from absent.
        assert!(out.contains("behavioral_preferences = []"), "{out}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn profile_preserves_other_sections_and_comments() {
        let path = temp_toml("profile-preserve");
        std::fs::write(
            &path,
            "# top comment\n[agent]\nprovider = \"ollama\"\n\n[access]\nlevel = \"home\"\n",
        )
        .unwrap();
        let p = ProfileWrite {
            assistant_name: Some("Aivyx PA".to_string()),
            ..Default::default()
        };
        write_profile_section(&path, &p).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("# top comment"), "comment preserved: {out}");
        assert!(out.contains("provider = \"ollama\""), "[agent] preserved: {out}");
        assert!(out.contains("level = \"home\""), "[access] preserved: {out}");
        assert!(out.contains("assistant_name = \"Aivyx PA\""), "{out}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn voice_writes_strings_paths_and_beam() {
        let path = temp_toml("voice");
        let v = VoiceWrite {
            asr_engine: Some("whisper-rs".to_string()),
            tts_engine: Some("kokoro".to_string()),
            asr_model_path: Some("/models/whisper.bin".to_string()),
            asr_language: Some("en".to_string()),
            asr_beam_size: Some(5),
            tts_model_dir: Some("/models/kokoro".to_string()),
            tts_voice_name: Some("af_heart".to_string()),
            tts_speed: Some(1.25),
            input_device: None,
            output_device: None,
        };
        write_voice_section(&path, &v).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("asr_engine = \"whisper-rs\""), "{out}");
        assert!(out.contains("asr_model_path = \"/models/whisper.bin\""), "{out}");
        assert!(out.contains("asr_beam_size = 5"), "{out}");
        assert!(out.contains("tts_model_dir = \"/models/kokoro\""), "{out}");
        assert!(out.contains("tts_voice_name = \"af_heart\""), "{out}");
        assert!(out.contains("tts_speed = 1.25"), "{out}");
        assert!(!out.contains("input_device"), "None device must be absent: {out}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn voice_clears_none_and_blank_fields() {
        let path = temp_toml("voice-clear");
        std::fs::write(
            &path,
            "[voice]\nasr_model_path = \"/old.bin\"\nasr_beam_size = 8\nasr_language = \"en\"\n",
        )
        .unwrap();
        let v = VoiceWrite {
            asr_language: Some("  ".to_string()), // whitespace → cleared
            // asr_model_path + asr_beam_size left None → cleared
            ..Default::default()
        };
        write_voice_section(&path, &v).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(!out.contains("asr_model_path"), "None path must be cleared: {out}");
        assert!(!out.contains("asr_beam_size"), "None beam must be cleared: {out}");
        assert!(!out.contains("asr_language"), "blank string must be cleared: {out}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn voice_preserves_other_sections() {
        let path = temp_toml("voice-preserve");
        std::fs::write(&path, "# cfg\n[agent]\nprovider = \"ollama\"\n").unwrap();
        let v = VoiceWrite {
            tts_engine: Some("kokoro".to_string()),
            ..Default::default()
        };
        write_voice_section(&path, &v).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("# cfg"), "comment preserved: {out}");
        assert!(out.contains("provider = \"ollama\""), "[agent] preserved: {out}");
        assert!(out.contains("tts_engine = \"kokoro\""), "{out}");
        std::fs::remove_file(&path).ok();
    }

    #[cfg(unix)]
    #[test]
    fn written_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp_toml("perms");
        write_access_section(&path, AccessLevel::Sandbox, None).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "got {mode:o}");
        std::fs::remove_file(&path).ok();
    }

    fn stdio_entry(name: &str) -> McpServerEntryWrite {
        McpServerEntryWrite {
            name: name.to_string(),
            transport: "stdio".to_string(),
            command: Some("npx".to_string()),
            args: vec!["-y".to_string(), "some-server".to_string()],
            env: vec![("TOKEN".to_string(), "${GITHUB_TOKEN}".to_string())],
            headers: Vec::new(),
            url: None,
            enabled: true,
        }
    }

    #[test]
    fn mcp_server_write_adds_a_new_entry() {
        let path = temp_toml("mcp-add");
        std::fs::write(&path, "[access]\nlevel = \"sandbox\"\n").unwrap();
        write_mcp_server_section(&path, &stdio_entry("github")).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("[access]"), "unrelated section survives");
        assert!(contents.contains("[[mcp_server]]"));
        assert!(contents.contains("name = \"github\""));
        assert!(contents.contains("command = \"npx\""));
    }

    #[test]
    fn mcp_server_write_replaces_an_existing_entry_by_name() {
        let path = temp_toml("mcp-replace");
        std::fs::write(&path, "").unwrap();
        write_mcp_server_section(&path, &stdio_entry("github")).unwrap();
        let mut updated = stdio_entry("github");
        updated.command = Some("uvx".to_string());
        write_mcp_server_section(&path, &updated).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        // Exactly one entry named "github" — not two.
        assert_eq!(contents.matches("name = \"github\"").count(), 1);
        assert!(contents.contains("command = \"uvx\""));
        assert!(!contents.contains("command = \"npx\""));
    }

    #[test]
    fn mcp_server_write_preserves_a_different_existing_entry() {
        let path = temp_toml("mcp-preserve");
        std::fs::write(&path, "").unwrap();
        write_mcp_server_section(&path, &stdio_entry("github")).unwrap();
        write_mcp_server_section(&path, &stdio_entry("filesystem")).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("name = \"github\""));
        assert!(contents.contains("name = \"filesystem\""));
    }

    #[test]
    fn mcp_server_write_rejects_stdio_without_command() {
        let path = temp_toml("mcp-stdio-no-cmd");
        std::fs::write(&path, "").unwrap();
        let mut entry = stdio_entry("github");
        entry.command = None;
        let err = write_mcp_server_section(&path, &entry).unwrap_err();
        assert!(matches!(err, ConfigWriteError::InvalidMcpServer { .. }));
    }

    #[test]
    fn mcp_server_write_rejects_sse_without_url() {
        let path = temp_toml("mcp-sse-no-url");
        std::fs::write(&path, "").unwrap();
        let entry = McpServerEntryWrite {
            name: "remote".to_string(),
            transport: "sse".to_string(),
            command: None,
            args: Vec::new(),
            env: Vec::new(),
            headers: Vec::new(),
            url: None,
            enabled: true,
        };
        let err = write_mcp_server_section(&path, &entry).unwrap_err();
        assert!(matches!(err, ConfigWriteError::InvalidMcpServer { .. }));
    }

    #[test]
    fn mcp_server_remove_drops_the_named_entry_only() {
        let path = temp_toml("mcp-remove");
        std::fs::write(&path, "").unwrap();
        write_mcp_server_section(&path, &stdio_entry("github")).unwrap();
        write_mcp_server_section(&path, &stdio_entry("filesystem")).unwrap();
        remove_mcp_server_section(&path, "github").unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(!contents.contains("name = \"github\""));
        assert!(contents.contains("name = \"filesystem\""));
    }

    #[test]
    fn mcp_server_remove_of_unknown_name_is_a_harmless_no_op() {
        let path = temp_toml("mcp-remove-unknown");
        std::fs::write(&path, "").unwrap();
        write_mcp_server_section(&path, &stdio_entry("github")).unwrap();
        remove_mcp_server_section(&path, "does-not-exist").unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("name = \"github\""));
    }

    // --- Post-hoc final-review fixes -------------------------------------

    /// Fix #1 (CRITICAL): the raw read must never resolve `${VAR}` — it must
    /// return the literal placeholder unchanged, since this is exactly the
    /// secret-leak scenario a `GetMcpServerConfigs` response must avoid.
    ///
    /// This doesn't need to touch the real process environment at all:
    /// `read_mcp_server_entries` (the function under test) never reads
    /// `std::env` in the first place — that's the whole point of the fix,
    /// it's the OLD loader-based path that did interpolation, not this raw
    /// TOML one. So the property ("no interpolation happens") is provable
    /// with a placeholder that doesn't correspond to any real variable,
    /// with no env mutation and thus no need for `crate::tests::env_lock()`.
    #[test]
    fn read_mcp_server_entries_never_resolves_env_placeholders() {
        let path = temp_toml("mcp-raw-read-secret");
        std::fs::write(&path, "").unwrap();
        let mut entry = stdio_entry("github");
        entry.env = vec![("TOKEN".to_string(), "${SOME_VAR_THAT_DOES_NOT_EXIST}".to_string())];
        write_mcp_server_section(&path, &entry).unwrap();

        let entries = read_mcp_server_entries(&path).unwrap();
        assert_eq!(entries.len(), 1);
        let (_, v) = entries[0]
            .env
            .iter()
            .find(|(k, _)| k == "TOKEN")
            .expect("TOKEN env entry present");
        assert_eq!(
            v, "${SOME_VAR_THAT_DOES_NOT_EXIST}",
            "must stay a literal placeholder, never resolve"
        );

        std::fs::remove_file(&path).ok();
    }

    /// Fix #2 (CRITICAL): an upsert through this primitive must never
    /// destroy `[mcp_server.sandbox]` or `bundled` — both are TOML-only
    /// fields this write schema doesn't carry, and sandboxing is the
    /// documented default security posture.
    #[test]
    fn mcp_server_write_preserves_sandbox_and_bundled_on_update() {
        let path = temp_toml("mcp-preserve-sandbox");
        std::fs::write(
            &path,
            "[[mcp_server]]\n\
             name = \"github\"\n\
             transport = \"stdio\"\n\
             command = \"npx\"\n\
             enabled = true\n\
             bundled = true\n\
             \n\
             [mcp_server.sandbox]\n\
             wrapper = \"bwrap\"\n\
             args = [\"--ro-bind\", \"/\", \"/\"]\n",
        )
        .unwrap();

        let mut updated = stdio_entry("github");
        updated.enabled = false;
        write_mcp_server_section(&path, &updated).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("enabled = false"), "the actual edit applied: {contents}");
        assert!(contents.contains("bundled = true"), "bundled must survive: {contents}");
        assert!(
            contents.contains("[mcp_server.sandbox]") || contents.contains("sandbox"),
            "sandbox block must survive: {contents}"
        );
        assert!(contents.contains("wrapper = \"bwrap\""), "{contents}");
        assert!(contents.contains("--ro-bind"), "{contents}");
        std::fs::remove_file(&path).ok();
    }

    /// Re-review fix (IMPORTANT): switching an existing sandboxed stdio
    /// entry's transport to a remote transport (e.g. via the Studio
    /// transport dropdown) must drop the now-stale `[mcp_server.sandbox]`
    /// block, not carry it over. `sandbox` is stdio-only in the loader
    /// (`aivyx-config/src/lib.rs`'s mcp_servers parsing rejects a sandbox
    /// block on a non-stdio transport), and that rejection aborts loading
    /// the *entire* aivyx-pa.toml — not just this one entry — bricking the
    /// daemon at next start. This is the same failure class the headers-on-
    /// stdio fix below exists to prevent, reintroduced through the
    /// unrelated-key preservation loop added to fix "sandbox destroyed on
    /// upsert".
    #[test]
    fn mcp_server_write_drops_sandbox_on_transport_switch_away_from_stdio() {
        let path = temp_toml("mcp-transport-switch-drops-sandbox");
        std::fs::write(
            &path,
            "[[mcp_server]]\n\
             name = \"github\"\n\
             transport = \"stdio\"\n\
             command = \"npx\"\n\
             enabled = true\n\
             \n\
             [mcp_server.sandbox]\n\
             wrapper = \"bwrap\"\n\
             args = [\"--ro-bind\", \"/\", \"/\"]\n",
        )
        .unwrap();

        let mut updated = stdio_entry("github");
        updated.transport = "sse".to_string();
        updated.command = None;
        updated.url = Some("https://example.com/mcp".to_string());
        write_mcp_server_section(&path, &updated).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("transport = \"sse\""), "{contents}");
        assert!(
            !contents.contains("sandbox"),
            "sandbox block must not survive a switch away from stdio: {contents}"
        );

        // The loader must still accept the resulting file.
        let entries = read_mcp_server_entries(&path).unwrap();
        assert_eq!(entries.len(), 1);
    }

    /// Confirms the prior fix's "sandbox survives an edit" behavior is
    /// unaffected by the transport-gating above — this test only edits
    /// `enabled` on a stdio entry, transport unchanged, so sandbox must
    /// still survive.
    #[test]
    fn mcp_server_write_still_preserves_sandbox_when_transport_unchanged() {
        let path = temp_toml("mcp-preserve-sandbox-transport-unchanged");
        std::fs::write(
            &path,
            "[[mcp_server]]\n\
             name = \"github\"\n\
             transport = \"stdio\"\n\
             command = \"npx\"\n\
             enabled = true\n\
             \n\
             [mcp_server.sandbox]\n\
             wrapper = \"bwrap\"\n",
        )
        .unwrap();

        let mut updated = stdio_entry("github");
        updated.enabled = false;
        write_mcp_server_section(&path, &updated).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("enabled = false"), "{contents}");
        assert!(contents.contains("sandbox"), "sandbox must still survive: {contents}");
    }

    /// Fix #6 (IMPORTANT): the loader rejects a stdio entry that also
    /// declares `headers` — rather than let a bad write through, this
    /// primitive silently drops `headers` for a stdio transport so the
    /// written file always loads cleanly regardless of caller.
    #[test]
    fn mcp_server_write_drops_headers_on_stdio_transport() {
        let path = temp_toml("mcp-stdio-drops-headers");
        std::fs::write(&path, "").unwrap();
        let mut entry = stdio_entry("github");
        entry.headers = vec![("Authorization".to_string(), "Bearer x".to_string())];
        write_mcp_server_section(&path, &entry).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(!contents.contains("headers"), "{contents}");

        let entries = read_mcp_server_entries(&path).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].headers.is_empty());
        std::fs::remove_file(&path).ok();
    }

    fn telegram_target(name: &str) -> NotifyTargetEntryWrite {
        NotifyTargetEntryWrite {
            name: name.to_string(),
            kind: "telegram".to_string(),
            chat_id: Some("123456".to_string()),
            url: None,
            to: None,
            enabled: true,
            is_default: false,
            retry_count: 0,
            retry_backoff_ms_start: 500,
            rate_limit_max: None,
            rate_limit_window_secs: None,
        }
    }

    #[test]
    fn notify_target_write_adds_a_new_entry() {
        let path = temp_toml("notify-add");
        std::fs::write(&path, "[access]\nlevel = \"sandbox\"\n").unwrap();
        write_notify_target_section(&path, &telegram_target("ops")).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("[access]"), "unrelated section survives");
        assert!(contents.contains("[[notify_target]]"));
        assert!(contents.contains("name = \"ops\""));
        assert!(contents.contains("chat_id = \"123456\""));
    }

    #[test]
    fn notify_target_write_replaces_an_existing_entry_by_name() {
        let path = temp_toml("notify-replace");
        std::fs::write(&path, "").unwrap();
        write_notify_target_section(&path, &telegram_target("ops")).unwrap();
        let mut updated = telegram_target("ops");
        updated.chat_id = Some("999".to_string());
        write_notify_target_section(&path, &updated).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.matches("name = \"ops\"").count(), 1);
        assert!(contents.contains("chat_id = \"999\""));
        assert!(!contents.contains("chat_id = \"123456\""));
    }

    #[test]
    fn notify_target_write_rejects_telegram_without_chat_id() {
        let path = temp_toml("notify-tg-no-chat");
        std::fs::write(&path, "").unwrap();
        let mut entry = telegram_target("ops");
        entry.chat_id = None;
        let err = write_notify_target_section(&path, &entry).unwrap_err();
        assert!(matches!(err, ConfigWriteError::InvalidNotifyTarget { .. }));
    }

    #[test]
    fn notify_target_write_rejects_webhook_with_bad_url_scheme() {
        let path = temp_toml("notify-webhook-bad-url");
        std::fs::write(&path, "").unwrap();
        let entry = NotifyTargetEntryWrite {
            name: "alerts".to_string(),
            kind: "webhook".to_string(),
            chat_id: None,
            url: Some("ftp://example.com".to_string()),
            to: None,
            enabled: true,
            is_default: false,
            retry_count: 0,
            retry_backoff_ms_start: 500,
            rate_limit_max: None,
            rate_limit_window_secs: None,
        };
        let err = write_notify_target_section(&path, &entry).unwrap_err();
        assert!(matches!(err, ConfigWriteError::InvalidNotifyTarget { .. }));
    }

    #[test]
    fn notify_target_write_rejects_email_with_no_at_sign() {
        let path = temp_toml("notify-email-bad-to");
        std::fs::write(&path, "").unwrap();
        let entry = NotifyTargetEntryWrite {
            name: "digest".to_string(),
            kind: "email".to_string(),
            chat_id: None,
            url: None,
            to: Some("not-an-email".to_string()),
            enabled: true,
            is_default: false,
            retry_count: 0,
            retry_backoff_ms_start: 500,
            rate_limit_max: None,
            rate_limit_window_secs: None,
        };
        let err = write_notify_target_section(&path, &entry).unwrap_err();
        assert!(matches!(err, ConfigWriteError::InvalidNotifyTarget { .. }));
    }

    #[test]
    fn notify_target_write_rejects_email_kind_without_an_email_section() {
        // Same failure class as item 1, through a different door: an
        // email-kind target with a valid `to` but no `[email]` section at
        // all saves successfully today and bricks the next daemon boot
        // (the loader's `email.is_none()` check, `aivyx-config/src/lib.rs`).
        let path = temp_toml("notify-email-no-email-section");
        std::fs::write(&path, "").unwrap();
        let entry = NotifyTargetEntryWrite {
            name: "digest".to_string(),
            kind: "email".to_string(),
            chat_id: None,
            url: None,
            to: Some("ops@example.com".to_string()),
            enabled: true,
            is_default: false,
            retry_count: 0,
            retry_backoff_ms_start: 500,
            rate_limit_max: None,
            rate_limit_window_secs: None,
        };
        let err = write_notify_target_section(&path, &entry).unwrap_err();
        assert!(matches!(err, ConfigWriteError::InvalidNotifyTarget { .. }), "{err:?}");
    }

    #[test]
    fn notify_target_write_rejects_retry_count_over_max() {
        let path = temp_toml("notify-retry-count-over-max");
        std::fs::write(&path, "").unwrap();
        let mut entry = telegram_target("ops");
        entry.retry_count = crate::MAX_RETRY_COUNT + 1;
        let err = write_notify_target_section(&path, &entry).unwrap_err();
        assert!(matches!(err, ConfigWriteError::InvalidNotifyTarget { .. }), "{err:?}");
    }

    #[test]
    fn notify_target_write_rejects_backoff_below_min() {
        let path = temp_toml("notify-backoff-below-min");
        std::fs::write(&path, "").unwrap();
        let mut entry = telegram_target("ops");
        entry.retry_backoff_ms_start = crate::MIN_RETRY_BACKOFF_MS_START - 1;
        let err = write_notify_target_section(&path, &entry).unwrap_err();
        assert!(matches!(err, ConfigWriteError::InvalidNotifyTarget { .. }), "{err:?}");
    }

    #[test]
    fn notify_target_write_rejects_rate_limit_max_without_window() {
        let path = temp_toml("notify-rate-limit-max-only");
        std::fs::write(&path, "").unwrap();
        let mut entry = telegram_target("ops");
        entry.rate_limit_max = Some(5);
        entry.rate_limit_window_secs = None;
        let err = write_notify_target_section(&path, &entry).unwrap_err();
        assert!(matches!(err, ConfigWriteError::InvalidNotifyTarget { .. }), "{err:?}");
    }

    #[test]
    fn notify_target_write_rejects_a_second_default() {
        let path = temp_toml("notify-two-defaults");
        std::fs::write(&path, "").unwrap();
        let mut first = telegram_target("ops");
        first.is_default = true;
        write_notify_target_section(&path, &first).unwrap();
        let mut second = telegram_target("backup");
        second.is_default = true;
        let err = write_notify_target_section(&path, &second).unwrap_err();
        assert!(matches!(err, ConfigWriteError::InvalidNotifyTarget { .. }));
        // The conflicting target's name must be in the message so the
        // operator doesn't have to scan the list.
        assert!(err.to_string().contains("ops"), "{err}");
    }

    #[test]
    fn notify_target_write_allows_replacing_the_existing_default() {
        let path = temp_toml("notify-replace-default");
        std::fs::write(&path, "").unwrap();
        let mut first = telegram_target("ops");
        first.is_default = true;
        write_notify_target_section(&path, &first).unwrap();
        // Re-writing the SAME entry (still marked default) must not be
        // rejected as "a second default" — it's the same target.
        write_notify_target_section(&path, &first).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.matches("default = true").count(), 1);
    }

    #[test]
    fn notify_target_remove_drops_the_named_entry_only() {
        let path = temp_toml("notify-remove");
        std::fs::write(&path, "").unwrap();
        write_notify_target_section(&path, &telegram_target("ops")).unwrap();
        write_notify_target_section(&path, &telegram_target("backup")).unwrap();
        remove_notify_target_section(&path, "ops").unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(!contents.contains("name = \"ops\""));
        assert!(contents.contains("name = \"backup\""));
    }

    #[test]
    fn read_notify_target_entries_round_trips() {
        let path = temp_toml("notify-read");
        std::fs::write(&path, "").unwrap();
        write_notify_target_section(&path, &telegram_target("ops")).unwrap();
        let entries = read_notify_target_entries(&path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "ops");
        assert_eq!(entries[0].chat_id.as_deref(), Some("123456"));
    }

    fn refl_entry(name: &str) -> ReflectionScheduleEntryWrite {
        ReflectionScheduleEntryWrite {
            name: name.to_string(),
            cron: "0 0 9 * * * *".to_string(),
            lookback_window_secs: 86_400,
            enabled: true,
        }
    }

    #[test]
    fn reflection_schedule_write_adds_a_new_entry() {
        let path = temp_toml("refl-add");
        std::fs::write(&path, "[access]\nlevel = \"sandbox\"\n").unwrap();
        write_reflection_schedule_section(&path, &refl_entry("nightly")).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("[access]"), "unrelated section survives");
        assert!(contents.contains("[[reflection_schedule]]"));
        assert!(contents.contains("name = \"nightly\""));
        assert!(contents.contains("cron = \"0 0 9 * * * *\""));
    }

    #[test]
    fn reflection_schedule_write_replaces_an_existing_entry_by_name() {
        let path = temp_toml("refl-replace");
        std::fs::write(&path, "").unwrap();
        write_reflection_schedule_section(&path, &refl_entry("nightly")).unwrap();
        let mut updated = refl_entry("nightly");
        updated.lookback_window_secs = 3600;
        write_reflection_schedule_section(&path, &updated).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.matches("name = \"nightly\"").count(), 1);
        assert!(contents.contains("lookback_window_secs = 3600"));
    }

    #[test]
    fn reflection_schedule_write_preserves_unknown_keys_on_replace() {
        let path = temp_toml("refl-preserve");
        std::fs::write(
            &path,
            "[[reflection_schedule]]\nname = \"nightly\"\ncron = \"0 0 9 * * * *\"\n\
             lookback_window_secs = 86400\nenabled = true\nrole_override = \"night-owl\"\n\
             skip_when_idle = true\nmin_audit_entries_to_fire = 3\n",
        )
        .unwrap();
        let mut updated = refl_entry("nightly");
        updated.lookback_window_secs = 7200;
        write_reflection_schedule_section(&path, &updated).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("role_override = \"night-owl\""), "role_override survives");
        assert!(contents.contains("skip_when_idle = true"), "skip_when_idle survives");
        assert!(contents.contains("min_audit_entries_to_fire = 3"), "min_audit_entries_to_fire survives");
        assert!(contents.contains("lookback_window_secs = 7200"), "the actual edit applied");
    }

    #[test]
    fn reflection_schedule_write_rejects_empty_name() {
        let path = temp_toml("refl-empty-name");
        std::fs::write(&path, "").unwrap();
        let mut entry = refl_entry("nightly");
        entry.name = "  ".to_string();
        let err = write_reflection_schedule_section(&path, &entry).unwrap_err();
        assert!(matches!(err, ConfigWriteError::InvalidReflectionSchedule { .. }));
    }

    #[test]
    fn reflection_schedule_write_rejects_empty_cron() {
        let path = temp_toml("refl-empty-cron");
        std::fs::write(&path, "").unwrap();
        let mut entry = refl_entry("nightly");
        entry.cron = String::new();
        let err = write_reflection_schedule_section(&path, &entry).unwrap_err();
        assert!(matches!(err, ConfigWriteError::InvalidReflectionSchedule { .. }));
    }

    #[test]
    fn reflection_schedule_write_rejects_lookback_out_of_range() {
        let path = temp_toml("refl-bad-lookback");
        std::fs::write(&path, "").unwrap();
        let mut entry = refl_entry("nightly");
        entry.lookback_window_secs = 30; // below the 60s floor
        let err = write_reflection_schedule_section(&path, &entry).unwrap_err();
        assert!(matches!(err, ConfigWriteError::InvalidReflectionSchedule { .. }));
    }

    #[test]
    fn reflection_schedule_write_rejects_name_colliding_with_a_regular_schedule() {
        let path = temp_toml("refl-collide-schedule");
        std::fs::write(
            &path,
            "[[schedule]]\nname = \"nightly\"\ncron = \"0 0 9 * * * *\"\nprompt = \"check things\"\n",
        )
        .unwrap();
        let err = write_reflection_schedule_section(&path, &refl_entry("nightly")).unwrap_err();
        assert!(matches!(err, ConfigWriteError::InvalidReflectionSchedule { .. }));
    }

    #[test]
    fn reflection_schedule_remove_drops_the_named_entry_only() {
        let path = temp_toml("refl-remove");
        std::fs::write(&path, "").unwrap();
        write_reflection_schedule_section(&path, &refl_entry("nightly")).unwrap();
        write_reflection_schedule_section(&path, &refl_entry("weekly-digest")).unwrap();
        remove_reflection_schedule_section(&path, "nightly").unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(!contents.contains("nightly"));
        assert!(contents.contains("weekly-digest"));
    }

    #[test]
    fn read_reflection_schedule_entries_round_trips_and_includes_disabled() {
        let path = temp_toml("refl-round-trip");
        std::fs::write(&path, "").unwrap();
        write_reflection_schedule_section(&path, &refl_entry("nightly")).unwrap();
        let mut disabled = refl_entry("paused-one");
        disabled.enabled = false;
        write_reflection_schedule_section(&path, &disabled).unwrap();
        let entries = read_reflection_schedule_entries(&path).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|e| e.name == "nightly" && e.enabled));
        assert!(
            entries.iter().any(|e| e.name == "paused-one" && !e.enabled),
            "a disabled entry must still be readable — the resolving loader drops these entirely"
        );
    }

    #[test]
    fn email_write_then_read_round_trips_all_fields() {
        let path = temp_toml("email-round-trip");
        std::fs::write(&path, "").unwrap();
        write_email_section(&path, &EmailEntryWrite {
            host: Some("smtp.example.com".to_string()),
            port: Some(587),
            tls_mode: Some("starttls".to_string()),
            username: Some("bot@example.com".to_string()),
            password: Some("hunter2".to_string()),
            from: Some("bot@example.com".to_string()),
        }).unwrap();
        let read = read_email_section(&path).unwrap();
        assert_eq!(read.host.as_deref(), Some("smtp.example.com"));
        assert_eq!(read.port, Some(587));
        assert_eq!(read.password.as_deref(), Some("hunter2"));
    }

    #[test]
    fn email_write_with_none_password_leaves_existing_password_untouched() {
        let path = temp_toml("email-keep-password");
        std::fs::write(&path, "").unwrap();
        // First write must be a *complete* [email] section — a partial one
        // (e.g. host+password only) is now rejected by the all-or-nothing
        // guard (see `email_write_rejects_password_only_on_fresh_file`).
        write_email_section(&path, &EmailEntryWrite {
            host: Some("smtp.example.com".to_string()),
            port: None,
            tls_mode: None,
            username: Some("bot@example.com".to_string()),
            password: Some("original-secret".to_string()),
            from: Some("bot@example.com".to_string()),
        }).unwrap();
        // Second write: change only the host, password is None ("don't touch").
        // The merged-state guard must pull username/password/from from the
        // existing on-disk section rather than treating them as absent.
        write_email_section(&path, &EmailEntryWrite {
            host: Some("smtp2.example.com".to_string()),
            port: None,
            tls_mode: None,
            username: None,
            password: None,
            from: None,
        }).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("original-secret"), "password survives when not touched");
        assert!(contents.contains("smtp2.example.com"));
    }

    #[test]
    fn email_write_rejects_password_only_on_fresh_file() {
        // The exact bug final-review finding #1 describes: on a fresh
        // install (no [email] section yet), Studio's email card used to
        // send password-only, which saved successfully and then bricked
        // the next daemon boot (`build_email_config`'s all-or-nothing
        // rule). This write must now be refused instead.
        let path = temp_toml("email-password-only-fresh");
        std::fs::write(&path, "").unwrap();
        let err = write_email_section(&path, &EmailEntryWrite {
            host: None,
            port: None,
            tls_mode: None,
            username: None,
            password: Some("hunter2".to_string()),
            from: None,
        }).unwrap_err();
        assert!(matches!(err, ConfigWriteError::InvalidEmailConfig { .. }), "{err:?}");
        // Refused before any write — the file must genuinely be untouched,
        // not just missing the literal substring "[email]" (a FRESH
        // section always writes as an inline table via toml_edit's
        // auto-vivification, `email = { host = "…", … }`, which never
        // contains that substring regardless of whether the guard even
        // ran — so checking for real emptiness via the read-side function
        // is the only assertion that actually proves nothing was written).
        let read = read_email_section(&path).unwrap();
        assert_eq!(read.host, None);
        assert_eq!(read.port, None);
        assert_eq!(read.tls_mode, None);
        assert_eq!(read.username, None);
        assert_eq!(read.password, None);
        assert_eq!(read.from, None);
    }

    #[test]
    fn email_write_full_valid_config_still_succeeds() {
        // Regression guard: the new all-or-nothing guard must not become
        // overly strict — a write that sets every required field together
        // must keep succeeding.
        let path = temp_toml("email-full-valid");
        std::fs::write(&path, "").unwrap();
        write_email_section(&path, &EmailEntryWrite {
            host: Some("smtp.example.com".to_string()),
            port: Some(587),
            tls_mode: Some("starttls".to_string()),
            username: Some("bot@example.com".to_string()),
            password: Some("hunter2".to_string()),
            from: Some("bot@example.com".to_string()),
        }).unwrap();
        let read = read_email_section(&path).unwrap();
        assert_eq!(read.host.as_deref(), Some("smtp.example.com"));
        assert_eq!(read.username.as_deref(), Some("bot@example.com"));
        assert_eq!(read.password.as_deref(), Some("hunter2"));
        assert_eq!(read.from.as_deref(), Some("bot@example.com"));
    }

    /// A complete `[email]` entry except `tls_mode`, which the caller fills
    /// in per-case — shared by the tls_mode-value tests below so each one
    /// only varies the one field under test.
    fn complete_email_entry(tls_mode: Option<&str>) -> EmailEntryWrite {
        EmailEntryWrite {
            host: Some("smtp.example.com".to_string()),
            port: Some(587),
            tls_mode: tls_mode.map(str::to_string),
            username: Some("bot@example.com".to_string()),
            password: Some("hunter2".to_string()),
            from: Some("bot@example.com".to_string()),
        }
    }

    #[test]
    fn email_write_rejects_tls_mode_none() {
        // Found during this section's first re-review (not the original
        // final review's #1 password-only bug or #2 [email]-presence gap
        // — both already spoken for, see write_email_section's own doc
        // comment): `build_email_config` (`aivyx-config/src/lib.rs`)
        // explicitly hard-rejects `tls_mode = "none"` (Aivyx requires TLS
        // for PLAIN/LOGIN auth) — an otherwise-complete [email] write with
        // this value used to save successfully and then brick the next
        // daemon boot.
        let path = temp_toml("email-tls-mode-none");
        std::fs::write(&path, "").unwrap();
        let err = write_email_section(&path, &complete_email_entry(Some("none"))).unwrap_err();
        assert!(matches!(err, ConfigWriteError::InvalidEmailConfig { .. }), "{err:?}");
    }

    #[test]
    fn email_write_rejects_unrecognized_tls_mode() {
        let path = temp_toml("email-tls-mode-unknown");
        std::fs::write(&path, "").unwrap();
        let err = write_email_section(&path, &complete_email_entry(Some("wat"))).unwrap_err();
        assert!(matches!(err, ConfigWriteError::InvalidEmailConfig { .. }), "{err:?}");
    }

    #[test]
    fn email_write_accepts_every_loader_accepted_tls_mode() {
        // Mirrors `build_email_config`'s accepted set exactly: an explicit
        // `tls_mode` must be "starttls" or "implicit" to succeed (a missing
        // `tls_mode` also defaults to "starttls" in the loader, but this
        // writer always sends an explicit value here).
        for mode in ["starttls", "implicit"] {
            let path = temp_toml(&format!("email-tls-mode-ok-{mode}"));
            std::fs::write(&path, "").unwrap();
            write_email_section(&path, &complete_email_entry(Some(mode)))
                .unwrap_or_else(|e| panic!("tls_mode = {mode:?} should be accepted: {e:?}"));
            let read = read_email_section(&path).unwrap();
            assert_eq!(read.tls_mode.as_deref(), Some(mode));
        }
    }

    #[test]
    fn notify_target_write_rejects_email_kind_with_bare_email_header() {
        // final-review finding #2: a `[email]` header with every key
        // commented out (a normal operator hand-edit) has NO fields set,
        // so `build_email_config` treats it exactly like "no [email]
        // section at all" and the daemon refuses to boot next start. The
        // old `doc.get("email").is_none()` check only tested for the TOML
        // header's presence and let this case through.
        let path = temp_toml("notify-email-bare-header");
        std::fs::write(&path, "[email]\n").unwrap();
        let entry = NotifyTargetEntryWrite {
            name: "digest".to_string(),
            kind: "email".to_string(),
            chat_id: None,
            url: None,
            to: Some("ops@example.com".to_string()),
            enabled: true,
            is_default: false,
            retry_count: 0,
            retry_backoff_ms_start: 500,
            rate_limit_max: None,
            rate_limit_window_secs: None,
        };
        let err = write_notify_target_section(&path, &entry).unwrap_err();
        assert!(matches!(err, ConfigWriteError::InvalidNotifyTarget { .. }), "{err:?}");
    }

    #[test]
    fn notify_target_write_allows_email_kind_with_fully_configured_email() {
        // Regression guard: the bare-header fix above must not become
        // overly strict — a genuinely configured [email] section must
        // keep allowing an email-kind notify_target.
        let path = temp_toml("notify-email-configured");
        std::fs::write(&path, "").unwrap();
        write_email_section(&path, &complete_email_entry(Some("starttls"))).unwrap();
        let entry = NotifyTargetEntryWrite {
            name: "digest".to_string(),
            kind: "email".to_string(),
            chat_id: None,
            url: None,
            to: Some("ops@example.com".to_string()),
            enabled: true,
            is_default: false,
            retry_count: 0,
            retry_backoff_ms_start: 500,
            rate_limit_max: None,
            rate_limit_window_secs: None,
        };
        write_notify_target_section(&path, &entry).unwrap();
    }

    #[test]
    fn telegram_write_then_read_round_trips() {
        let path = temp_toml("telegram-round-trip");
        std::fs::write(&path, "").unwrap();
        write_telegram_section(&path, &TelegramEntryWrite {
            token: Some("123:ABC".to_string()),
            chat_id: Some(42),
            team_run_channel: Some(true),
            team_trigger_rate_limit: Some(5),
            team_command_allowed_senders: Some(vec![111, 222]),
        }).unwrap();
        let read = read_telegram_section(&path).unwrap();
        assert_eq!(read.token.as_deref(), Some("123:ABC"));
        assert_eq!(read.chat_id, Some(42));
        assert_eq!(read.team_command_allowed_senders, Some(vec![111, 222]));
    }

    #[test]
    fn telegram_write_with_none_token_leaves_existing_token_untouched() {
        let path = temp_toml("telegram-keep-token");
        std::fs::write(&path, "").unwrap();
        write_telegram_section(&path, &TelegramEntryWrite {
            token: Some("original-token".to_string()),
            chat_id: None,
            team_run_channel: None,
            team_trigger_rate_limit: None,
            team_command_allowed_senders: None,
        }).unwrap();
        write_telegram_section(&path, &TelegramEntryWrite {
            token: None,
            chat_id: Some(99),
            team_run_channel: None,
            team_trigger_rate_limit: None,
            team_command_allowed_senders: None,
        }).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("original-token"));
        assert!(contents.contains("chat_id = 99"));
    }

    #[test]
    fn discord_write_then_read_round_trips() {
        let path = temp_toml("discord-round-trip");
        std::fs::write(&path, "").unwrap();
        write_discord_section(&path, &DiscordEntryWrite {
            token: Some("discord-token".to_string()),
            application_id: Some(555),
            team_run_channel: Some(false),
            team_trigger_rate_limit: None,
            team_command_allowed_senders: Some(vec![1, 2, 3]),
        }).unwrap();
        let read = read_discord_section(&path).unwrap();
        assert_eq!(read.token.as_deref(), Some("discord-token"));
        assert_eq!(read.application_id, Some(555));
    }

    #[test]
    fn slack_write_then_read_round_trips_both_tokens() {
        let path = temp_toml("slack-round-trip");
        std::fs::write(&path, "").unwrap();
        write_slack_section(&path, &SlackEntryWrite {
            bot_token: Some("xoxb-1".to_string()),
            app_token: Some("xapp-1".to_string()),
            team_id: Some("T123".to_string()),
            team_run_channel: None,
            team_trigger_rate_limit: None,
            team_command_allowed_senders: Some(vec!["U1".to_string()]),
        }).unwrap();
        let read = read_slack_section(&path).unwrap();
        assert_eq!(read.bot_token.as_deref(), Some("xoxb-1"));
        assert_eq!(read.app_token.as_deref(), Some("xapp-1"));
    }

    #[test]
    fn slack_write_with_none_app_token_leaves_it_untouched_while_updating_bot_token() {
        let path = temp_toml("slack-partial-update");
        std::fs::write(&path, "").unwrap();
        write_slack_section(&path, &SlackEntryWrite {
            bot_token: Some("xoxb-old".to_string()),
            app_token: Some("xapp-keep-me".to_string()),
            team_id: None,
            team_run_channel: None,
            team_trigger_rate_limit: None,
            team_command_allowed_senders: None,
        }).unwrap();
        write_slack_section(&path, &SlackEntryWrite {
            bot_token: Some("xoxb-new".to_string()),
            app_token: None,
            team_id: None,
            team_run_channel: None,
            team_trigger_rate_limit: None,
            team_command_allowed_senders: None,
        }).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("xoxb-new"));
        assert!(!contents.contains("xoxb-old"));
        assert!(contents.contains("xapp-keep-me"), "untouched field survives");
    }

    #[test]
    fn email_read_of_missing_section_returns_all_none() {
        let path = temp_toml("email-missing");
        std::fs::write(&path, "[access]\nlevel = \"sandbox\"\n").unwrap();
        let read = read_email_section(&path).unwrap();
        assert_eq!(read.host, None);
        assert_eq!(read.password, None);
    }

    #[test]
    fn memory_profile_write_sets_the_key_and_preserves_siblings() {
        let path = temp_toml("mem-profile-write");
        std::fs::write(
            &path,
            "[memory]\nmax_per_topic = 200\nttl_secs = 86400\n\n[[memory.retention]]\ntopic_glob = \"daily-*\"\nretention = \"forever\"\n",
        )
        .unwrap();
        write_memory_profile(&path, Some("smart")).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("profile = \"smart\""));
        assert!(contents.contains("max_per_topic = 200"), "sibling key survives");
        assert!(contents.contains("[[memory.retention]]"), "retention array survives");
    }

    #[test]
    fn memory_profile_write_none_is_a_no_op() {
        let path = temp_toml("mem-profile-noop");
        std::fs::write(&path, "[memory]\nprofile = \"lite\"\n").unwrap();
        write_memory_profile(&path, None).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("profile = \"lite\""));
    }

    #[test]
    fn memory_profile_write_rejects_unknown_value() {
        let path = temp_toml("mem-profile-bad");
        std::fs::write(&path, "").unwrap();
        let err = write_memory_profile(&path, Some("turbo")).unwrap_err();
        assert!(matches!(err, ConfigWriteError::InvalidMemoryProfile { .. }));
    }

    #[test]
    fn read_memory_profile_round_trips() {
        let path = temp_toml("mem-profile-read");
        std::fs::write(&path, "").unwrap();
        assert_eq!(read_memory_profile(&path).unwrap(), None);
        write_memory_profile(&path, Some("smart")).unwrap();
        assert_eq!(read_memory_profile(&path).unwrap(), Some("smart".to_string()));
    }

    #[test]
    fn embedding_write_touches_only_provided_fields() {
        let path = temp_toml("embedding-partial");
        std::fs::write(&path, "[embedding]\nbase_url = \"https://old.example\"\nmodel = \"old-model\"\napi_key = \"sk-existing\"\n").unwrap();
        write_embedding_section(
            &path,
            &EmbeddingEntryWrite { base_url: None, model: Some("new-model".to_string()), api_key: None },
        )
        .unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("base_url = \"https://old.example\""), "untouched field survives");
        assert!(contents.contains("model = \"new-model\""), "the actual edit applied");
        assert!(contents.contains("api_key = \"sk-existing\""), "secret untouched by None");
    }

    #[test]
    fn embedding_write_rejects_blank_base_url() {
        let path = temp_toml("embedding-blank-url");
        std::fs::write(&path, "").unwrap();
        let err = write_embedding_section(
            &path,
            &EmbeddingEntryWrite { base_url: Some("  ".to_string()), model: None, api_key: None },
        )
        .unwrap_err();
        assert!(matches!(err, ConfigWriteError::InvalidEmbeddingConfig { .. }));
    }

    #[test]
    fn read_embedding_section_round_trips_and_never_needed_for_the_secret_itself() {
        let path = temp_toml("embedding-read");
        std::fs::write(&path, "").unwrap();
        write_embedding_section(
            &path,
            &EmbeddingEntryWrite {
                base_url: Some("https://api.openai.com".to_string()),
                model: Some("text-embedding-3-small".to_string()),
                api_key: Some("sk-real-secret".to_string()),
            },
        )
        .unwrap();
        let read = read_embedding_section(&path).unwrap();
        assert_eq!(read.base_url.as_deref(), Some("https://api.openai.com"));
        assert_eq!(read.model.as_deref(), Some("text-embedding-3-small"));
        // read_embedding_section itself returns the raw value (Task 4's
        // daemon handler is what redacts it before it reaches the wire) —
        // this test only proves the round-trip is byte-correct.
        assert_eq!(read.api_key.as_deref(), Some("sk-real-secret"));
    }

    #[test]
    fn proactive_write_rejects_enabling_without_a_known_target() {
        let path = temp_toml("proactive-no-target");
        std::fs::write(&path, "").unwrap();
        let err = write_proactive_section(
            &path,
            &ProactiveEntryWrite {
                enabled: Some(true),
                target: Some("nonexistent".to_string()),
                max_per_window: None,
                window_secs: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, ConfigWriteError::InvalidProactiveConfig { .. }));
    }

    #[test]
    fn proactive_write_rejects_enabling_with_a_disabled_target() {
        let path = temp_toml("proactive-disabled-target");
        std::fs::write(
            &path,
            "[[notify_target]]\nname = \"ops\"\nkind = \"telegram\"\nchat_id = \"123\"\nenabled = false\ndefault = false\n",
        )
        .unwrap();
        let err = write_proactive_section(
            &path,
            &ProactiveEntryWrite {
                enabled: Some(true),
                target: Some("ops".to_string()),
                max_per_window: None,
                window_secs: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, ConfigWriteError::InvalidProactiveConfig { .. }));
    }

    #[test]
    fn proactive_write_allows_enabling_with_a_known_target() {
        let path = temp_toml("proactive-known-target");
        std::fs::write(
            &path,
            "[[notify_target]]\nname = \"ops\"\nkind = \"telegram\"\nchat_id = \"123\"\nenabled = true\ndefault = false\n",
        )
        .unwrap();
        write_proactive_section(
            &path,
            &ProactiveEntryWrite {
                enabled: Some(true),
                target: Some("ops".to_string()),
                max_per_window: None,
                window_secs: None,
            },
        )
        .unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("proactive") && contents.contains("enabled = true"), "proactive section missing or malformed: {}", contents);
        assert!(contents.contains("target = \"ops\""));
    }

    #[test]
    fn proactive_write_merged_state_keeps_succeeding_on_a_later_partial_save() {
        let path = temp_toml("proactive-merged");
        std::fs::write(
            &path,
            "[[notify_target]]\nname = \"ops\"\nkind = \"telegram\"\nchat_id = \"123\"\nenabled = true\ndefault = false\n",
        )
        .unwrap();
        write_proactive_section(
            &path,
            &ProactiveEntryWrite {
                enabled: Some(true),
                target: Some("ops".to_string()),
                max_per_window: Some(5),
                window_secs: Some(3600),
            },
        )
        .unwrap();
        // A later save only touches max_per_window — enabled/target
        // must be read from disk (merged), not treated as newly absent.
        write_proactive_section(
            &path,
            &ProactiveEntryWrite { enabled: None, target: None, max_per_window: Some(9), window_secs: None },
        )
        .unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("max_per_window = 9"));
        assert!(contents.contains("target = \"ops\""), "untouched field survives the merge");
    }

    #[test]
    fn proactive_write_rejects_zero_max_per_window_when_enabled() {
        let path = temp_toml("proactive-zero-max");
        std::fs::write(
            &path,
            "[[notify_target]]\nname = \"ops\"\nkind = \"telegram\"\nchat_id = \"123\"\nenabled = true\ndefault = false\n",
        )
        .unwrap();
        let err = write_proactive_section(
            &path,
            &ProactiveEntryWrite {
                enabled: Some(true),
                target: Some("ops".to_string()),
                max_per_window: Some(0),
                window_secs: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, ConfigWriteError::InvalidProactiveConfig { .. }));
    }

    #[test]
    fn proactive_write_allows_disabling_without_a_target() {
        let path = temp_toml("proactive-disable");
        std::fs::write(&path, "").unwrap();
        write_proactive_section(
            &path,
            &ProactiveEntryWrite { enabled: Some(false), target: None, max_per_window: None, window_secs: None },
        )
        .unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("enabled = false"));
    }
}
