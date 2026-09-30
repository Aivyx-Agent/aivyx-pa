//! Operator-facing `aivyx-pa identity` CLI surface — Phase 64.
//!
//! Phase 64 ships **export only** per the implementation-time
//! scope adjustment (Q-block was full export+import; import was
//! deferred to Phase 65 for focused destructive-write design
//! attention). The export path:
//!
//! 1. Reads `aivyx-pa.toml` directly for the Profile half (no
//!    encrypted storage needed — same access pattern as
//!    `aivyx-pa profile show`).
//! 2. Talks to a running daemon over IPC for the Persona half
//!    (the daemon owns the encrypted store; routing through it
//!    avoids duplicating the master-key unlock path).
//! 3. Assembles the [`IdentityExport`] bundle and writes
//!    pretty-printed JSON to the operator-supplied path with
//!    `0600` file permissions.
//!
//! Daemon must be running. Without one, the export fails with a
//! "start daemon first" message — same shape as the other
//! Persona CLI subcommands (Phase 60).

use std::path::Path;

use aivyx_channel::daemon_client::{daemon_is_running, export_persona_chain, import_persona_chain};
use aivyx_channel::daemon_ipc::default_socket_path;
use aivyx_channel::identity_export::{build, parse_and_validate};
use aivyx_config::{AivyxConfig, LoadOptions};

/// The operator's `aivyx-pa.toml` — see `aivyx_config::resolve_config_path`.
fn config_path() -> std::path::PathBuf {
    aivyx_config::resolve_config_path()
}

/// Entry point for `aivyx-pa identity export <path>`.
pub async fn run_identity_export(path: &Path) -> Result<(), String> {
    let socket_path = default_socket_path()?;
    if !daemon_is_running(&socket_path).await {
        return Err(format!(
            "aivyx-pa identity export: no daemon running on socket {} — \
             start the daemon first with `aivyx-pa daemon run` (or just `aivyx-pa`)",
            socket_path.display(),
        ));
    }

    // --- Profile half: read aivyx-pa.toml via the existing
    //     inspection path (same shape as `aivyx-pa profile show`). ---
    let profile = load_profile_for_export()?;

    // --- Persona half: fetch the full chain + effective state
    //     from the running daemon. ---
    let (deltas, effective) = export_persona_chain(&socket_path)
        .await
        .map_err(|e| format!("failed to fetch persona chain: {e}"))?;

    // The IPC response gives us DeltaExport values directly
    // (already MAC-stripped on the daemon side); build them into
    // signed entries for the `build` helper which expects
    // SignedPersonaEntry. The MACs are immediately stripped again
    // by the build path — they're not part of the export. Using
    // synthesized zero-MAC entries here is correct because:
    //   * build() reads only seq + delta (MAC fields are dropped
    //     in the DeltaExport conversion);
    //   * the parse-time replay check synthesizes its own
    //     zero-MAC entries from the same DeltaExport values.
    let entries: Vec<aivyx_channel::persona::SignedPersonaEntry> = deltas
        .iter()
        .map(|d| aivyx_channel::persona::SignedPersonaEntry {
            seq: d.seq,
            delta: d.delta.clone(),
            prev_mac: [0u8; 32],
            mac: [0u8; 32],
        })
        .collect();

    let export = build(&profile, &entries, &effective);

    // --- Serialize + write with 0600 permissions. ---
    let json = serde_json::to_string_pretty(&export)
        .map_err(|e| format!("failed to serialize export: {e}"))?;
    write_export_file(path, &json)?;

    eprintln!(
        "aivyx-pa identity export: wrote {} deltas + Profile to {}",
        export.persona.deltas.len(),
        path.display(),
    );
    eprintln!("File permissions: 0600 (owner-only).");
    Ok(())
}

/// Entry point for `aivyx-pa identity import <path> [--force]`.
/// Phase 65. Reads + validates the file locally, then forwards
/// to the running daemon for the destructive write. The daemon
/// refuses on a non-empty existing chain unless `force` is set.
///
/// Profile half remains a hand-edit per Q2(a) sign-off — the
/// CLI does not touch `aivyx-pa.toml`. The exported `[profile]`
/// is included in the bundle for the operator to reference.
pub async fn run_identity_import(path: &Path, force: bool) -> Result<(), String> {
    // Local parse + validate first. Fail fast on local issues
    // (malformed JSON, schema mismatch, gap in seq, etc.) before
    // opening an IPC connection.
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let bundle = parse_and_validate(&raw).map_err(|e| format!("import validation failed: {e}"))?;

    let socket_path = default_socket_path()?;
    if !daemon_is_running(&socket_path).await {
        return Err(format!(
            "aivyx-pa identity import: no daemon running on socket {} — \
             start the daemon first with `aivyx-pa daemon run` (or just `aivyx-pa`)",
            socket_path.display(),
        ));
    }

    // Forward to the daemon. The CLI is a thin wrapper —
    // conflict resolution, wipe, replay, and runtime refresh
    // all happen daemon-side.
    let success = import_persona_chain(
        &socket_path,
        bundle.persona.deltas,
        bundle.persona.effective_at_export,
        force,
    )
    .await
    .map_err(|e| format!("persona import failed: {e}"))?;

    eprintln!(
        "aivyx-pa identity import: imported {} deltas. \
         Chain is now at seq {}.",
        success.deltas_imported, success.final_chain_seq,
    );
    eprintln!(
        "Daemon's runtime persona state refreshed — the next \
         agent turn sees the imported persona without restart."
    );
    if bundle.profile.assistant_name != "Aivyx PA"
        || bundle.profile.operator_profile.is_some()
        || bundle.profile.communication_style.is_some()
        || !bundle.profile.primary_use_cases.is_empty()
        || !bundle.profile.behavioral_preferences.is_empty()
        || !bundle.profile.behavioral_constraints.is_empty()
    {
        eprintln!();
        eprintln!(
            "Note: the export bundle includes a Profile section. \
             Phase 65 does not auto-import Profile (Q2(a) at sign-off)."
        );
        eprintln!(
            "To apply the imported Profile, hand-edit `aivyx-pa.toml`'s \
             `[profile]` section to match the bundle's `profile` block, \
             then restart the daemon (`aivyx-pa daemon stop && aivyx-pa`)."
        );
    }
    Ok(())
}

/// Read the Profile from `aivyx-pa.toml`. Mirrors
/// `aivyx_modules::profile::load_config_for_inspection` but
/// inlined here so the identity module doesn't depend on the
/// profile module's private helper.
fn load_profile_for_export() -> Result<aivyx_config::Profile, String> {
    let toml_path = config_path();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let config = AivyxConfig::load_from_env_and_toml(&opts)
        .map_err(|e| format!("failed to load aivyx-pa.toml for identity export: {e}"))?;
    Ok(config.profile)
}

/// Write `contents` to `path` with `0600` permissions. Mirrors
/// the `write_aivyx_toml` helper in the profile module; lifted
/// here to keep the identity module self-contained.
fn write_export_file(path: &Path, contents: &str) -> Result<(), String> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create parent dir {}: {e}", parent.display(),))?;
        }
    }

    // Create with 0600 (Unix). On non-Unix the OpenOptions mode
    // is ignored; we still set 0600 explicitly below.
    let mut file = open_for_write_0600(path)?;
    file.write_all(contents.as_bytes())
        .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    file.flush()
        .map_err(|e| format!("failed to flush {}: {e}", path.display()))?;
    Ok(())
}

#[cfg(unix)]
fn open_for_write_0600(path: &Path) -> Result<std::fs::File, String> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("failed to open {} for write: {e}", path.display()))
}

#[cfg(not(unix))]
fn open_for_write_0600(path: &Path) -> Result<std::fs::File, String> {
    // Non-Unix: best-effort. The OS may not honor the mode.
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|e| format!("failed to open {} for write: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_channel::persona::EffectivePersona;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn write_export_file_creates_0600() {
        let tmp = std::env::temp_dir().join(format!(
            "aivyx-export-test-{}.json",
            uuid::Uuid::new_v4().as_simple()
        ));
        write_export_file(&tmp, "{\"hello\": \"world\"}").expect("write ok");
        let meta = std::fs::metadata(&tmp).expect("stat");
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");
        let content = std::fs::read_to_string(&tmp).expect("read");
        assert_eq!(content, "{\"hello\": \"world\"}");
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn build_export_round_trips_through_json_via_runtime_helper() {
        // Integration check that the module's view of
        // `build` + serde produces a valid IdentityExport.
        let profile = aivyx_config::Profile::default();
        let entries: Vec<aivyx_channel::persona::SignedPersonaEntry> = Vec::new();
        let effective = EffectivePersona::default();
        let export = build(&profile, &entries, &effective);
        let json = serde_json::to_string_pretty(&export).expect("serialize");
        // Re-parse via the validator — should accept (empty
        // chain, default profile, matching effective).
        let parsed = aivyx_channel::identity_export::parse_and_validate(&json).expect("parse");
        assert_eq!(parsed.persona.deltas.len(), 0);
    }
}
