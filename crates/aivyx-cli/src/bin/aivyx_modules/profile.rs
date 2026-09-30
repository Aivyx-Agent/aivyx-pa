//! Operator-facing `aivyx-pa profile` CLI surface — Phase 58.
//!
//! Phase 57 shipped the Profile substrate (`aivyx-config::Profile`,
//! `[profile]` TOML table, `assemble_session_prompt`, init wizard,
//! startup-banner row). This module ships the operator surface that
//! closes PRODUCT.md P13:
//!
//! - `aivyx-pa profile show` — labeled, human-readable inspection.
//! - `aivyx-pa profile edit` — surgical `[profile]` section edit in
//!   `$EDITOR`, preserving the rest of `aivyx-pa.toml` via `toml_edit`
//!   (Task 3).
//!
//! Q3(a) resolution at Phase 58 sign-off: `show` reads `aivyx-pa.toml`
//! from disk (no daemon dispatch needed). Profile is operator-mutable
//! only — the agent never writes to it — so disk state and live state
//! are always equivalent modulo a pending daemon restart.
//!
//! Q5(a) resolution at Phase 58 sign-off: edit changes take effect
//! on the next daemon startup. The edit subcommand prints a restart
//! reminder after a successful save (matches the existing
//! load-time-only semantics for role configs).

use std::path::Path;

use aivyx_config::{AivyxConfig, FieldSource, LoadOptions, Profile};

/// Default TOML path. Mirrors the binary's
/// [`crate::DEFAULT_TOML_PATH`] without depending on it (this module
/// is included via `#[path = ...]` and re-exporting from the binary
/// would create a cyclic-looking dependency).
/// The operator's `aivyx-pa.toml` — see `aivyx_config::resolve_config_path`.
fn config_path() -> std::path::PathBuf {
    aivyx_config::resolve_config_path()
}

/// Entry point for `aivyx-pa profile show`. Loads `aivyx-pa.toml` via the
/// same `aivyx-config` path the daemon uses at startup, then renders
/// the resolved [`Profile`] to stdout in a labeled format mirroring
/// the startup banner.
pub fn run_profile_show() -> Result<(), String> {
    let cfg = load_config_for_inspection()?;
    let rendered = render_profile_for_show(&cfg.profile);
    print!("{rendered}");
    Ok(())
}

/// Entry point for `aivyx-pa profile edit`. Phase 58 Task 3 — Q2(a)
/// resolution.
///
/// The flow:
///
/// 1. Read `aivyx-pa.toml` (or initialize a synthesized empty document
///    if the file does not exist).
/// 2. Extract the current `[profile]` section as a standalone TOML
///    chunk and write it to a tempfile.
/// 3. Spawn `$EDITOR` (or `vi` if unset) against the tempfile and
///    wait for the operator to save and exit.
/// 4. Parse the edited content; reject TOML syntax errors with a
///    clear message pointing at the tempfile so the operator can
///    retry without losing their edits.
/// 5. Splice the new `[profile]` table back into the original
///    `aivyx-pa.toml` document via `toml_edit` — preserving every
///    other section, every comment, and the original whitespace.
/// 6. Write the merged document back to disk with `0600` permissions.
/// 7. Print a restart reminder per Q5(a) — Profile is load-time-only,
///    same as role configs.
pub fn run_profile_edit() -> Result<(), String> {
    let toml_path = &config_path();

    // Read the existing aivyx-pa.toml (or start with an empty document
    // if the operator has not run `aivyx-pa init` yet).
    let original_text = if toml_path.exists() {
        std::fs::read_to_string(toml_path)
            .map_err(|e| format!("failed to read {}: {e}", toml_path.display()))?
    } else {
        String::new()
    };

    // Parse the original document, extract the current [profile]
    // section as a starter chunk for the editor.
    let current_profile_text = extract_profile_section_for_edit(&original_text).map_err(|e| {
        format!(
            "failed to parse {} as TOML: {e}\n\
                 The existing config file is malformed. Fix it manually \
                 before running `aivyx-pa profile edit`.",
            toml_path.display()
        )
    })?;

    let edited_text = open_in_editor(&current_profile_text)?;

    let merged_text = merge_edited_profile_into_aivyx_toml(&original_text, &edited_text)?;

    write_aivyx_toml(toml_path, &merged_text)?;

    eprintln!();
    eprintln!("Profile updated in {}.", toml_path.display());
    eprintln!(
        "Restart the daemon for changes to take effect: \
         `aivyx-pa daemon stop && aivyx-pa`."
    );

    Ok(())
}

/// Entry point for `aivyx-pa profile apply-hint <proposal-id> [--yes]`.
/// Phase 119 Task 4.
///
/// Operator workflow:
/// 1. Operator has already run `aivyx-pa persona proposals approve <id>`
///    on a `ProfileHint` proposal (Q2(a) at Phase 119 sign-off —
///    separate approve and apply gestures).
/// 2. This command fetches the now-Approved proposal, validates the
///    category, parses the inner `ProfileFieldHint` payload,
///    confirms with the operator (unless `--yes`), applies the field
///    update to `aivyx-pa.toml` atomically via the Task 3 primitive,
///    records an `AuditEvent::ProfileHintApplied` event via daemon
///    IPC, and surfaces a "restart the daemon" reminder.
pub async fn run_profile_apply_hint(proposal_id: &str, yes: bool) -> Result<(), String> {
    use aivyx_channel::daemon_client::{
        apply_profile_hint, daemon_is_running, get_persona_proposal,
    };
    use aivyx_channel::daemon_ipc::default_socket_path;

    let socket_path = default_socket_path()?;
    if !daemon_is_running(&socket_path).await {
        return Err(format!(
            "aivyx-pa profile apply-hint: daemon must be running \
             (socket {}). Start it with `aivyx-pa`.",
            socket_path.display(),
        ));
    }

    let proposal = get_persona_proposal(&socket_path, proposal_id)
        .await
        .map_err(|e| format!("failed to fetch proposal: {e}"))?
        .ok_or_else(|| format!("no proposal with id `{proposal_id}`"))?;

    let hint = parse_proposal_as_profile_hint(&proposal)?;

    // Confirm with operator. The hint's rationale is part of why
    // the operator approved it; surfacing the field + value at
    // apply time is enough to catch a mistaken proposal id.
    if !yes {
        eprintln!(
            "Apply `{field}` = {value:?} to {path}?",
            field = hint.field.label(),
            value = hint.suggested_value,
            path = config_path().display(),
        );
        eprintln!("[y/N] (re-run with --yes to skip this prompt)");
        let mut answer = String::new();
        std::io::stdin()
            .read_line(&mut answer)
            .map_err(|e| format!("failed to read confirmation: {e}"))?;
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            return Err("apply cancelled by operator".to_string());
        }
    }

    let applied =
        crate::toml_edit_apply::apply_profile_hint_to_path(&config_path(), &hint)
            .map_err(|e| format!("failed to apply hint to {}: {e}", config_path().display()))?;

    // Record the audit event via daemon IPC. If the audit-record
    // step fails AFTER the aivyx-pa.toml mutation landed, surface as
    // a soft warning — the file mutation is the load-bearing
    // result; the audit event is forensic.
    let audit_result = apply_profile_hint(
        &socket_path,
        proposal_id,
        &applied.field,
        &applied.applied_value,
    )
    .await;

    eprintln!();
    eprintln!(
        "Applied `{field}` to {path}.",
        field = applied.field,
        path = config_path().display(),
    );
    match audit_result {
        Ok(()) => {
            eprintln!(
                "Audit event `ProfileHintApplied` recorded for proposal \
                 `{proposal_id}`."
            );
        }
        Err(e) => {
            eprintln!(
                "warning: audit-event record failed: {e}\n\
                 The aivyx-pa.toml mutation is in place; you can re-record \
                 the audit event by running the command again."
            );
        }
    }
    eprintln!(
        "Restart the daemon for the new value to take effect: \
         `aivyx-pa daemon stop && aivyx-pa`."
    );
    Ok(())
}

/// Pure validator: parse the proposal's wire shape into a
/// [`ProfileFieldHint`]. Fails closed:
/// - Refuses non-`ProfileHint` categories (operator picked wrong
///   proposal id).
/// - Refuses non-`Approved` statuses (apply only acts on already-
///   approved hints per Q2(a) Phase 119 sign-off).
/// - Refuses malformed JSON payloads (the proposal's
///   `applied_op.value` must decode as a `ProfileFieldHint`).
///
/// Extracted as a pure function so the validation logic is
/// testable without IPC scaffolding.
fn parse_proposal_as_profile_hint(
    proposal: &aivyx_channel::daemon_ipc::PersonaProposalSummary,
) -> Result<aivyx_core::skill_proposer::ProfileFieldHint, String> {
    if proposal.category != "ProfileHint" {
        return Err(format!(
            "proposal `{}` has category `{}`, not `ProfileHint`. \
             Use `aivyx-pa role import` for RoleDefinitionSuggestion.",
            proposal.id, proposal.category
        ));
    }
    if proposal.status != "Approved" {
        return Err(format!(
            "proposal `{}` has status `{}`; only Approved proposals can be \
             applied. Run `aivyx-pa persona proposals approve {}` first.",
            proposal.id, proposal.status, proposal.id
        ));
    }
    // The Phase 118 chain stores the payload inside applied_op
    // (operator approved → applied_op set from proposed_op verbatim
    // unless the operator edited). Fall back to proposed_op if the
    // applied_op is absent (old chain shape).
    let op = proposal
        .applied_op
        .as_ref()
        .unwrap_or(&proposal.proposed_op);
    let value = op.get("value").and_then(|v| v.as_str()).ok_or_else(|| {
        format!(
            "proposal `{}` op shape is unexpected (no AppendList.value); \
                 cannot decode ProfileFieldHint payload.",
            proposal.id
        )
    })?;
    let hint: aivyx_core::skill_proposer::ProfileFieldHint =
        serde_json::from_str(value).map_err(|e| {
            format!(
                "proposal `{}` value is not a valid ProfileFieldHint payload: {e}",
                proposal.id
            )
        })?;
    Ok(hint)
}

/// Parse `original_text` (the existing `aivyx-pa.toml`) and return the
/// `[profile]` section as a standalone TOML document the operator can
/// edit in a tempfile. If the original document has no `[profile]`
/// section, returns [`default_profile_template`] so the operator
/// sees the full shape pre-filled.
///
/// Pure function — extracted from `run_profile_edit` so tests can
/// exercise the parsing logic without spawning an editor.
fn extract_profile_section_for_edit(original_text: &str) -> Result<String, String> {
    let document: toml_edit::DocumentMut = original_text
        .parse()
        .map_err(|e: toml_edit::TomlError| e.to_string())?;
    Ok(match document.get("profile") {
        Some(item) => {
            let mut doc = toml_edit::DocumentMut::new();
            doc.insert("profile", item.clone());
            doc.to_string()
        }
        None => default_profile_template(),
    })
}

/// Splice the operator's edited `[profile]` section back into
/// `original_text`, preserving every other section and every comment
/// in the original document.
///
/// Pure function — extracted from `run_profile_edit` so tests can
/// exercise the merge invariants (other sections preserved, profile
/// replaced, comments retained, etc.) without filesystem or editor
/// I/O. The `edited_text` argument must contain a `[profile]` table.
fn merge_edited_profile_into_aivyx_toml(
    original_text: &str,
    edited_text: &str,
) -> Result<String, String> {
    let mut document: toml_edit::DocumentMut = original_text
        .parse()
        .map_err(|e: toml_edit::TomlError| format!("original config is malformed: {e}"))?;

    let edited_document: toml_edit::DocumentMut =
        edited_text.parse().map_err(|e: toml_edit::TomlError| {
            format!(
                "edited Profile is not valid TOML: {e}\n\
                 Your edits have not been applied. Re-run `aivyx-pa profile \
                 edit` to try again."
            )
        })?;

    let new_profile_table = edited_document
        .as_table()
        .get("profile")
        .ok_or_else(|| {
            "edited Profile is missing the `[profile]` header. \
             Re-run `aivyx-pa profile edit` and keep the header line."
                .to_string()
        })?
        .clone();

    document.insert("profile", new_profile_table);
    Ok(document.to_string())
}

/// Synthesize a starter `[profile]` template when the existing
/// `aivyx-pa.toml` carries no `[profile]` section. Includes one
/// commented placeholder per category so the operator sees the
/// full shape without typing it from scratch.
fn default_profile_template() -> String {
    "[profile]\n\
     # Operator-declared identity layer per PRODUCT.md P13.\n\
     # Each field is optional; remove the lines you do not want\n\
     # to declare. Restart the daemon for changes to take effect.\n\
     \n\
     # assistant_name = \"Aivyx PA\"\n\
     # operator_profile = \"\"\n\
     # communication_style = \"\"\n\
     # primary_use_cases = []\n\
     # behavioral_preferences = []\n\
     # behavioral_constraints = []\n"
        .to_string()
}

/// Open `initial_contents` in the operator's `$EDITOR` (falling back
/// to `vi`) and return whatever the editor writes back.
///
/// The tempfile lives in the OS temp directory and is removed when
/// the function returns regardless of outcome. The tempfile path
/// surfaces in error messages so the operator can recover edits
/// from `/tmp` if the post-edit parse fails.
fn open_in_editor(initial_contents: &str) -> Result<String, String> {
    use std::io::Write;

    let editor = std::env::var("EDITOR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "vi".to_string());

    // Use a unique-enough filename inside the OS temp dir. We do not
    // depend on the `tempfile` crate to keep the dep count low; a
    // process-pid-based name is sufficient since the file is deleted
    // before the function returns.
    let tempdir = std::env::temp_dir();
    let pid = std::process::id();
    let temp_path = tempdir.join(format!("aivyx-profile-edit-{pid}.toml"));

    let mut file = std::fs::File::create(&temp_path)
        .map_err(|e| format!("failed to create tempfile {}: {e}", temp_path.display()))?;
    file.write_all(initial_contents.as_bytes())
        .map_err(|e| format!("failed to write tempfile {}: {e}", temp_path.display()))?;
    drop(file);

    // Spawn the editor. We deliberately inherit stdin/stdout/stderr
    // so interactive editors (vim, nano, emacs, etc.) work normally.
    let status = std::process::Command::new(&editor)
        .arg(&temp_path)
        .status()
        .map_err(|e| {
            // Best-effort cleanup before returning the error.
            let _ = std::fs::remove_file(&temp_path);
            format!(
                "failed to launch editor `{editor}`: {e}\n\
                 Set the `EDITOR` environment variable to a valid \
                 editor command and retry."
            )
        })?;

    if !status.success() {
        let _ = std::fs::remove_file(&temp_path);
        return Err(format!(
            "editor `{editor}` exited with non-zero status: {status}. \
             Edits not applied."
        ));
    }

    let edited = std::fs::read_to_string(&temp_path).map_err(|e| {
        format!(
            "failed to read edited tempfile {}: {e}",
            temp_path.display()
        )
    })?;

    // Best-effort cleanup of the tempfile. Failure to remove is not
    // fatal — it just leaves a stray file in /tmp.
    let _ = std::fs::remove_file(&temp_path);

    Ok(edited)
}

/// Write the merged TOML back to `aivyx-pa.toml` with `0600` permissions
/// on Unix. Mirrors the init wizard's `write_config` pattern.
fn write_aivyx_toml(path: &Path, contents: &str) -> Result<(), String> {
    std::fs::write(path, contents)
        .map_err(|e| format!("failed to write {}: {e}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(path, perms)
            .map_err(|e| format!("failed to set permissions on {}: {e}", path.display()))?;
    }

    Ok(())
}

/// Load `AivyxConfig` with relaxed validation. The Profile-inspection
/// path does not require an API key, a Telegram token, or a
/// passphrase — it only needs the loader to parse `aivyx-pa.toml` and
/// populate the `profile` field (or synthesize the default).
fn load_config_for_inspection() -> Result<AivyxConfig, String> {
    let opts = LoadOptions {
        toml_path: Some(config_path()),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    AivyxConfig::load_from_env_and_toml(&opts)
        .map_err(|e| format!("failed to load {}: {e}", config_path().display()))
}

/// Render the Profile in the labeled format `show` writes to stdout.
/// Pure function — separated from `run_profile_show` so tests can
/// drive it against fixtures without touching the filesystem.
///
/// The format mirrors the startup-banner shape: one field per row,
/// `key = value (source)` for fields with provenance, `key = <unset>`
/// for absent optional fields, and bulleted lists for the three
/// `Vec<String>` categories.
///
/// A trailing `Profile injection: ENABLED/DISABLED` line tells the
/// operator at a glance whether the Profile will flavor every turn's
/// system prompt (`is_operator_declared()` predicate from Phase 57
/// Task 3).
fn render_profile_for_show(profile: &Profile) -> String {
    let mut out = String::new();
    out.push_str("Profile\n");
    out.push_str("=======\n\n");

    out.push_str(&format!(
        "  assistant_name             = {:?} ({})\n",
        profile.assistant_name.value,
        source_label(profile.assistant_name.source),
    ));

    match &profile.operator_profile {
        Some(s) => out.push_str(&format!("  operator_profile           = {s:?}\n")),
        None => out.push_str("  operator_profile           = <unset>\n"),
    }

    match &profile.communication_style {
        Some(s) => out.push_str(&format!("  communication_style        = {s:?}\n")),
        None => out.push_str("  communication_style        = <unset>\n"),
    }

    if profile.primary_use_cases.is_empty() {
        out.push_str("  primary_use_cases          = <unset>\n");
    } else {
        out.push_str("  primary_use_cases:\n");
        for case in &profile.primary_use_cases {
            out.push_str(&format!("    - {case}\n"));
        }
    }

    if profile.behavioral_preferences.is_empty() {
        out.push_str("  behavioral_preferences     = <unset>\n");
    } else {
        out.push_str("  behavioral_preferences:\n");
        for pref in &profile.behavioral_preferences {
            out.push_str(&format!("    - {pref}\n"));
        }
    }

    if profile.behavioral_constraints.is_empty() {
        out.push_str("  behavioral_constraints     = <unset>\n");
    } else {
        out.push_str("  behavioral_constraints:\n");
        for c in &profile.behavioral_constraints {
            out.push_str(&format!("    - {c}\n"));
        }
    }

    out.push('\n');
    if profile.is_operator_declared() {
        out.push_str("Profile injection: ENABLED — Profile flavors every turn's system prompt.\n");
    } else {
        out.push_str(
            "Profile injection: DISABLED — no operator content declared; daemon \
             runs without Profile injection.\n",
        );
    }
    out
}

fn source_label(src: FieldSource) -> &'static str {
    match src {
        FieldSource::Env => "env",
        FieldSource::Toml => "toml",
        FieldSource::EncryptedStore => "encrypted-store",
        FieldSource::Default => "default",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_config::Sourced;

    fn default_profile() -> Profile {
        Profile::default()
    }

    fn operator_declared_profile() -> Profile {
        Profile {
            assistant_name: Sourced::new("Codex".to_string(), FieldSource::Toml),
            operator_profile: Some("Senior Rust engineer focused on systems.".to_string()),
            communication_style: Some("terse, conclusion-first".to_string()),
            primary_use_cases: vec![
                "Rust systems programming".to_string(),
                "AI agent design".to_string(),
            ],
            behavioral_preferences: vec!["prefer integration tests".to_string()],
            behavioral_constraints: vec!["never auto-commit code".to_string()],
        }
    }

    #[test]
    fn show_default_profile_renders_aivyx_default_and_disabled_injection() {
        let out = render_profile_for_show(&default_profile());
        assert!(out.starts_with("Profile\n=======\n"));
        assert!(out.contains("assistant_name             = \"Aivyx PA\" (default)"));
        assert!(out.contains("operator_profile           = <unset>"));
        assert!(out.contains("communication_style        = <unset>"));
        assert!(out.contains("primary_use_cases          = <unset>"));
        assert!(out.contains("behavioral_preferences     = <unset>"));
        assert!(out.contains("behavioral_constraints     = <unset>"));
        assert!(out.contains("Profile injection: DISABLED"));
    }

    #[test]
    fn show_operator_declared_profile_renders_all_fields_and_enabled_injection() {
        let out = render_profile_for_show(&operator_declared_profile());

        // Assistant name with toml provenance.
        assert!(out.contains("assistant_name             = \"Codex\" (toml)"));
        // Free-text fields render with quoted value.
        assert!(out.contains("operator_profile           = \"Senior Rust"));
        assert!(out.contains("communication_style        = \"terse, conclusion-first\""));

        // List fields render as bulleted entries.
        assert!(out.contains("primary_use_cases:\n    - Rust systems programming"));
        assert!(out.contains("    - AI agent design"));
        assert!(out.contains("behavioral_preferences:\n    - prefer integration tests"));
        assert!(out.contains("behavioral_constraints:\n    - never auto-commit code"));

        assert!(out.contains("Profile injection: ENABLED"));
    }

    // -------------------------------------------------------------
    // Task 3 — `aivyx-pa profile edit` merge logic tests.
    // -------------------------------------------------------------

    const ORIGINAL_WITH_PROFILE: &str = "\
# Generated by `aivyx-pa init`

[agent]
provider = \"anthropic\"
model = \"claude-haiku-4-5-20251001\"

[anthropic]
api_key = \"sk-ant-test\"

[fs]
root = \"/home/op/aivyx-sandbox\"

[profile]
assistant_name = \"Codex\"
primary_use_cases = [\"Rust systems programming\"]
";

    const ORIGINAL_WITHOUT_PROFILE: &str = "\
[agent]
provider = \"ollama\"
model = \"llama3.2:latest\"

[fs]
root = \"/home/op/aivyx-sandbox\"
";

    #[test]
    fn extract_profile_section_returns_existing_profile_table() {
        let extracted = extract_profile_section_for_edit(ORIGINAL_WITH_PROFILE).expect("parse");
        assert!(extracted.contains("[profile]"));
        assert!(extracted.contains("assistant_name = \"Codex\""));
        assert!(extracted.contains("primary_use_cases = [\"Rust systems programming\"]"));
        // The standalone chunk must NOT carry unrelated sections.
        assert!(!extracted.contains("[agent]"));
        assert!(!extracted.contains("[anthropic]"));
        assert!(!extracted.contains("api_key"));
    }

    #[test]
    fn extract_profile_section_returns_template_when_missing() {
        let extracted = extract_profile_section_for_edit(ORIGINAL_WITHOUT_PROFILE).expect("parse");
        assert!(extracted.contains("[profile]"));
        // The starter template is fully commented out so a no-op
        // editor save leaves the file with no operator-declared
        // content — same effect as not running edit at all.
        assert!(extracted.contains("# assistant_name = \"Aivyx PA\""));
        assert!(extracted.contains("# operator_profile = \"\""));
        assert!(extracted.contains("# communication_style = \"\""));
        assert!(extracted.contains("# primary_use_cases = []"));
        assert!(extracted.contains("# behavioral_preferences = []"));
        assert!(extracted.contains("# behavioral_constraints = []"));
    }

    #[test]
    fn extract_profile_section_rejects_malformed_toml() {
        let err = extract_profile_section_for_edit("[profile\nassistant_name = \"oops\"")
            .expect_err("malformed TOML must error");
        assert!(!err.is_empty());
    }

    #[test]
    fn merge_replaces_existing_profile_and_preserves_other_sections() {
        let edited = "\
[profile]
assistant_name = \"Mira\"
operator_profile = \"Senior Rust engineer\"
behavioral_constraints = [\"never auto-commit\"]
";
        let merged =
            merge_edited_profile_into_aivyx_toml(ORIGINAL_WITH_PROFILE, edited).expect("merge");

        // New profile values appear.
        assert!(merged.contains("assistant_name = \"Mira\""));
        assert!(merged.contains("operator_profile = \"Senior Rust engineer\""));
        assert!(merged.contains("behavioral_constraints = [\"never auto-commit\"]"));

        // Old profile values are gone (the section was replaced
        // wholesale, not merged field-by-field).
        assert!(!merged.contains("assistant_name = \"Codex\""));
        assert!(!merged.contains("primary_use_cases = [\"Rust systems programming\"]"));

        // Every other section survives intact.
        assert!(merged.contains("[agent]"));
        assert!(merged.contains("provider = \"anthropic\""));
        assert!(merged.contains("[anthropic]"));
        assert!(merged.contains("api_key = \"sk-ant-test\""));
        assert!(merged.contains("[fs]"));
        assert!(merged.contains("root = \"/home/op/aivyx-sandbox\""));

        // The leading comment from the original document survives.
        assert!(merged.contains("# Generated by `aivyx-pa init`"));
    }

    #[test]
    fn merge_inserts_profile_when_original_has_none() {
        let edited = "\
[profile]
assistant_name = \"Newcomer\"
";
        let merged =
            merge_edited_profile_into_aivyx_toml(ORIGINAL_WITHOUT_PROFILE, edited).expect("merge");

        assert!(merged.contains("[profile]"));
        assert!(merged.contains("assistant_name = \"Newcomer\""));
        // Original sections survive.
        assert!(merged.contains("[agent]"));
        assert!(merged.contains("provider = \"ollama\""));
        assert!(merged.contains("[fs]"));
    }

    #[test]
    fn merge_rejects_edited_text_without_profile_header() {
        let edited = "\
# operator deleted the [profile] line by mistake
assistant_name = \"oops\"
";
        let err = merge_edited_profile_into_aivyx_toml(ORIGINAL_WITH_PROFILE, edited)
            .expect_err("missing [profile] header must error");
        assert!(
            err.contains("missing the `[profile]` header"),
            "error message must explain: {err}"
        );
    }

    #[test]
    fn merge_rejects_malformed_edited_toml() {
        let edited = "[profile\nassistant_name = oops";
        let err = merge_edited_profile_into_aivyx_toml(ORIGINAL_WITH_PROFILE, edited)
            .expect_err("malformed edited TOML must error");
        assert!(
            err.contains("not valid TOML"),
            "error message must explain: {err}"
        );
    }

    #[test]
    fn show_partial_profile_renders_mix_of_set_and_unset_fields() {
        // Only assistant_name + primary_use_cases declared.
        let profile = Profile {
            assistant_name: Sourced::new("Mira".to_string(), FieldSource::Toml),
            primary_use_cases: vec!["personal-finance analysis".to_string()],
            ..Profile::default()
        };

        let out = render_profile_for_show(&profile);

        // Declared fields render with their values.
        assert!(out.contains("assistant_name             = \"Mira\" (toml)"));
        assert!(out.contains("primary_use_cases:\n    - personal-finance analysis"));

        // Undeclared fields render as <unset>.
        assert!(out.contains("operator_profile           = <unset>"));
        assert!(out.contains("communication_style        = <unset>"));
        assert!(out.contains("behavioral_preferences     = <unset>"));
        assert!(out.contains("behavioral_constraints     = <unset>"));

        // Operator-declared assistant_name flips injection to ENABLED
        // even when most fields are unset.
        assert!(out.contains("Profile injection: ENABLED"));
    }

    // ----- Phase 119 Task 4 — parse_proposal_as_profile_hint -----

    fn proposal_fixture(
        id: &str,
        category: &str,
        status: &str,
        applied_op: Option<serde_json::Value>,
    ) -> aivyx_channel::daemon_ipc::PersonaProposalSummary {
        aivyx_channel::daemon_ipc::PersonaProposalSummary {
            id: id.into(),
            proposed_at_unix_ms: 1_715_000_000_000,
            source_reflection_session_id: "ses-118".into(),
            status: status.into(),
            category: category.into(),
            proposed_op: serde_json::json!({
                "kind": "AppendList",
                "value": "",
            }),
            proposed_reason: None,
            applied_op,
            applied_seq: Some(7),
            rejected_reason: None,
            resolved_at_unix_ms: Some(1_715_000_060_000),
            supersedes_proposal_id: None,
        }
    }

    fn profile_hint_payload(field: &str, value: &str) -> serde_json::Value {
        serde_json::json!({
            "kind": "AppendList",
            "value": serde_json::json!({
                "field": field,
                "suggested_value": value,
                "rationale": "operator pattern observed",
            })
            .to_string(),
        })
    }

    #[test]
    fn parse_proposal_accepts_approved_profile_hint() {
        let proposal = proposal_fixture(
            "pp-1",
            "ProfileHint",
            "Approved",
            Some(profile_hint_payload(
                "CommunicationStyle",
                "terse and bullet-formatted",
            )),
        );
        let hint = parse_proposal_as_profile_hint(&proposal).unwrap();
        assert_eq!(
            hint.field,
            aivyx_core::skill_proposer::ProfileField::CommunicationStyle
        );
        assert!(hint.suggested_value.contains("bullet-formatted"));
        assert!(hint.rationale.contains("pattern"));
    }

    #[test]
    fn parse_proposal_refuses_non_profile_hint_category() {
        // Operator picked an `RoleDefinitionSuggestion` id by mistake.
        let proposal = proposal_fixture(
            "pp-x",
            "RoleDefinitionSuggestion",
            "Approved",
            Some(serde_json::json!({"kind":"AppendList","value":"{}"})),
        );
        let err = parse_proposal_as_profile_hint(&proposal).unwrap_err();
        assert!(err.contains("not `ProfileHint`"));
        assert!(err.contains("aivyx-pa role import"));
    }

    #[test]
    fn parse_proposal_refuses_pending_status() {
        // Operator forgot to approve first.
        let proposal = proposal_fixture("pp-y", "ProfileHint", "Pending", None);
        let err = parse_proposal_as_profile_hint(&proposal).unwrap_err();
        assert!(err.contains("only Approved proposals"));
        assert!(err.contains("aivyx-pa persona proposals approve"));
    }

    #[test]
    fn parse_proposal_refuses_rejected_status() {
        let proposal = proposal_fixture("pp-z", "ProfileHint", "Rejected", None);
        let err = parse_proposal_as_profile_hint(&proposal).unwrap_err();
        assert!(err.contains("only Approved"));
    }

    #[test]
    fn parse_proposal_refuses_malformed_payload() {
        let proposal = proposal_fixture(
            "pp-bad",
            "ProfileHint",
            "Approved",
            Some(serde_json::json!({"kind":"AppendList","value":"not json"})),
        );
        let err = parse_proposal_as_profile_hint(&proposal).unwrap_err();
        assert!(err.contains("ProfileFieldHint"));
    }

    #[test]
    fn parse_proposal_falls_back_to_proposed_op_when_applied_op_absent() {
        // Backward-compat: old chain entries might not have
        // applied_op populated. Falls back to proposed_op.
        let mut proposal = proposal_fixture("pp-fallback", "ProfileHint", "Approved", None);
        proposal.proposed_op = profile_hint_payload("AssistantName", "Aivyx PA");
        let hint = parse_proposal_as_profile_hint(&proposal).unwrap();
        assert_eq!(
            hint.field,
            aivyx_core::skill_proposer::ProfileField::AssistantName
        );
        assert_eq!(hint.suggested_value, "Aivyx PA");
    }

    // ----- Phase 119 Task 7 — scripted e2e (profile apply pipeline) -----

    fn e2e_tempdir(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "aivyx-phase119-task7-profile-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn e2e_profile_apply_pipeline_from_approved_proposal_to_aivyx_toml() {
        // The full Phase 119 apply pipeline for a ProfileHint:
        //   1. Build the wire-shape PersonaProposalSummary the daemon
        //      would have produced after auto-propose + approve.
        //   2. Validate via the pure parser.
        //   3. Apply via the Task 3 atomic primitive against a real
        //      tempfile.
        //   4. Read back the file; assert the expected [profile] field
        //      landed verbatim.
        //   5. Append the audit event the daemon would have written
        //      after the apply, assert chain HMAC verifies.
        // This is the load-bearing assertion that every Phase 119
        // piece composes — substrate (Task 2) + primitive (Task 3) +
        // validator (Task 4) all flow end-to-end against the same
        // Phase 118 chain shape.
        use crate::toml_edit_apply::apply_profile_hint_to_path;
        let dir = e2e_tempdir("apply-pipeline");
        let aivyx_toml = dir.join("aivyx-pa.toml");
        // The operator already has an existing [profile] section
        // with a different communication_style — apply must overwrite.
        std::fs::write(
            &aivyx_toml,
            "# Top-level comment retained across apply.\n\
             [profile]\n\
             assistant_name = \"Aivyx PA\"\n\
             communication_style = \"verbose\"\n",
        )
        .unwrap();

        // Step 1 — wire-shape proposal.
        let proposal = proposal_fixture(
            "pp-e2e-1",
            "ProfileHint",
            "Approved",
            Some(profile_hint_payload(
                "CommunicationStyle",
                "terse and bullet-formatted",
            )),
        );

        // Step 2 — pure parse.
        let hint = parse_proposal_as_profile_hint(&proposal)
            .expect("Phase 118 proposal must validate as ProfileHint");
        assert_eq!(
            hint.field,
            aivyx_core::skill_proposer::ProfileField::CommunicationStyle
        );

        // Step 3 — Task 3 atomic apply.
        let applied = apply_profile_hint_to_path(&aivyx_toml, &hint)
            .expect("apply must succeed against tempfile");
        assert_eq!(applied.field, "communication_style");
        assert_eq!(applied.applied_value, "terse and bullet-formatted");

        // Step 4 — file contents reflect the apply AND the
        // operator's prior state (other field + comment) is
        // preserved.
        let post = std::fs::read_to_string(&aivyx_toml).unwrap();
        assert!(
            post.contains("communication_style = \"terse and bullet-formatted\""),
            "expected new style, got: {post}"
        );
        // Old value gone.
        assert!(
            !post.contains("\"verbose\""),
            "old value must be overwritten"
        );
        // Other Profile field untouched.
        assert!(post.contains("assistant_name = \"Aivyx PA\""));
        // Top-level comment retained.
        assert!(post.contains("# Top-level comment"));

        // Step 5 — audit event linkage. The daemon would emit
        // ProfileHintApplied carrying the applied field + value
        // + the source proposal id. Verify the event lands in
        // an HmacChainLog and the chain verifies.
        use aivyx_audit::{AuditEvent, AuditLog, AuditWriter, HmacChainLog};
        let chain = HmacChainLog::new(b"phase-119-task7-test-key-32-byte".to_vec());
        chain
            .append(AuditEvent::ProfileHintApplied {
                session_id: aivyx_core::SessionId::new(),
                proposal_id: proposal.id.clone(),
                field: applied.field.clone(),
                applied_value: applied.applied_value.clone(),
            })
            .expect("audit append must succeed");
        chain.verify().expect("chain must verify");
        assert_eq!(AuditLog::len(&chain), 1);

        std::fs::remove_dir_all(&dir).ok();
    }
}
