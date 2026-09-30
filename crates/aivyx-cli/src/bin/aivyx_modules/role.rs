//! Operator-facing `aivyx-pa role` CLI surface — Phase 119 Task 5.
//!
//! Today this module ships exactly one subcommand:
//!
//! - `aivyx-pa role import <proposal-id> [--yes] [--force]` — applies an
//!   operator-approved Phase 118 `RoleDefinitionSuggestion` proposal
//!   to `aivyx-pa.toml`'s `[roles.<name>]` section via the Task 3 atomic
//!   primitive, then records the `AuditEvent::RoleDraftImported`
//!   audit-event via daemon IPC.
//!
//! Future `aivyx-pa role` subcommands (e.g. `list`, `show`, `edit`) land
//! additively under the same [`RoleSubcommand`] enum without
//! fragmenting `CliMode`.


/// Default TOML path — mirrors the Phase 58 `profile.rs` constant.
/// The operator's `aivyx-pa.toml` — see `aivyx_config::resolve_config_path`.
fn config_path() -> std::path::PathBuf {
    aivyx_config::resolve_config_path()
}

/// Entry point for `aivyx-pa role import <proposal-id> [--yes] [--force]`.
/// Phase 119 Task 5 — operator's act-on-approval gesture for a Phase
/// 118 `RoleDefinitionSuggestion` proposal.
///
/// The flow mirrors `aivyx-pa profile apply-hint` (Phase 119 Task 4):
/// 1. Require daemon running.
/// 2. Fetch the proposal via daemon IPC.
/// 3. Validate category + status (Approved only — Q2(a) at Phase 119
///    sign-off, separate approve and apply gestures).
/// 4. Parse the inner `RoleDraft` payload.
/// 5. Confirm with the operator (unless `--yes`).
/// 6. Apply via the Task 3 atomic primitive (refuses overwrite
///    without `--force`).
/// 7. Record the audit event via daemon IPC.
/// 8. Surface a "restart the daemon" reminder.
pub async fn run_role_import(proposal_id: &str, yes: bool, force: bool) -> Result<(), String> {
    use aivyx_channel::daemon_client::{
        daemon_is_running, get_persona_proposal, import_role_draft,
    };
    use aivyx_channel::daemon_ipc::default_socket_path;

    let socket_path = default_socket_path()?;
    if !daemon_is_running(&socket_path).await {
        return Err(format!(
            "aivyx-pa role import: daemon must be running (socket {}). \
             Start it with `aivyx-pa`.",
            socket_path.display(),
        ));
    }

    let proposal = get_persona_proposal(&socket_path, proposal_id)
        .await
        .map_err(|e| format!("failed to fetch proposal: {e}"))?
        .ok_or_else(|| format!("no proposal with id `{proposal_id}`"))?;

    let draft = parse_proposal_as_role_draft(&proposal)?;

    if !yes {
        let parent_label = draft
            .parent
            .as_deref()
            .map(|p| format!(" inheriting from `{p}`"))
            .unwrap_or_default();
        eprintln!(
            "Import role `{name}`{parent_label} into {path}?",
            name = draft.name,
            path = config_path().display(),
        );
        if force {
            eprintln!(
                "  (--force: will overwrite any existing `[roles.{}]` \
                 section)",
                draft.name,
            );
        }
        eprintln!("[y/N] (re-run with --yes to skip this prompt)");
        let mut answer = String::new();
        std::io::stdin()
            .read_line(&mut answer)
            .map_err(|e| format!("failed to read confirmation: {e}"))?;
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            return Err("import cancelled by operator".to_string());
        }
    }

    let applied =
        crate::toml_edit_apply::apply_role_draft_to_path(&config_path(), &draft, force)
            .map_err(|e| format!("failed to import role to {}: {e}", config_path().display()))?;

    // Same posture as profile apply-hint: audit-event record is
    // forensic, not load-bearing. Soft-warn if it fails after the
    // file mutation lands.
    let audit_result = import_role_draft(
        &socket_path,
        proposal_id,
        &applied.role_name,
        applied.parent.as_deref(),
    )
    .await;

    eprintln!();
    eprintln!(
        "Imported role `{name}` into {path}.",
        name = applied.role_name,
        path = config_path().display(),
    );
    match audit_result {
        Ok(()) => {
            eprintln!(
                "Audit event `RoleDraftImported` recorded for proposal \
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
        "Restart the daemon for the new role to take effect: \
         `aivyx-pa daemon stop && aivyx-pa`."
    );
    Ok(())
}

/// Pure validator: parse the proposal's wire shape into a
/// [`RoleDraft`]. Fails closed on every distinct error mode so the
/// CLI surface gives the operator an actionable hint:
/// - Wrong category → cross-reference `aivyx-pa profile apply-hint`.
/// - Wrong status → cross-reference `aivyx-pa persona proposals approve`.
/// - Malformed payload → operator-readable parse error.
///
/// Extracted as a pure function so the validation logic is testable
/// without IPC scaffolding. Mirrors
/// [`crate::profile::parse_proposal_as_profile_hint`] from Task 4.
fn parse_proposal_as_role_draft(
    proposal: &aivyx_channel::daemon_ipc::PersonaProposalSummary,
) -> Result<aivyx_core::skill_proposer::RoleDraft, String> {
    if proposal.category != "RoleDefinitionSuggestion" {
        return Err(format!(
            "proposal `{}` has category `{}`, not `RoleDefinitionSuggestion`. \
             Use `aivyx-pa profile apply-hint` for ProfileHint.",
            proposal.id, proposal.category
        ));
    }
    if proposal.status != "Approved" {
        return Err(format!(
            "proposal `{}` has status `{}`; only Approved proposals can be \
             imported. Run `aivyx-pa persona proposals approve {}` first.",
            proposal.id, proposal.status, proposal.id
        ));
    }
    let op = proposal
        .applied_op
        .as_ref()
        .unwrap_or(&proposal.proposed_op);
    let value = op.get("value").and_then(|v| v.as_str()).ok_or_else(|| {
        format!(
            "proposal `{}` op shape is unexpected (no AppendList.value); \
                 cannot decode RoleDraft payload.",
            proposal.id
        )
    })?;
    let draft: aivyx_core::skill_proposer::RoleDraft =
        serde_json::from_str(value).map_err(|e| {
            format!(
                "proposal `{}` value is not a valid RoleDraft payload: {e}",
                proposal.id
            )
        })?;
    Ok(draft)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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

    fn role_draft_payload(name: &str, parent: Option<&str>) -> serde_json::Value {
        let inner = serde_json::json!({
            "name": name,
            "parent": parent,
            "system_prompt_addendum": "Test addendum.",
            "tool_allowlist_additions": ["git.commit"],
            "rationale": "operator pattern observed",
        });
        serde_json::json!({
            "kind": "AppendList",
            "value": inner.to_string(),
        })
    }

    #[test]
    fn parse_proposal_accepts_approved_role_definition_suggestion() {
        let proposal = proposal_fixture(
            "pp-r1",
            "RoleDefinitionSuggestion",
            "Approved",
            Some(role_draft_payload("research-deploy", Some("research"))),
        );
        let draft = parse_proposal_as_role_draft(&proposal).unwrap();
        assert_eq!(draft.name, "research-deploy");
        assert_eq!(draft.parent.as_deref(), Some("research"));
        assert!(draft.system_prompt_addendum.contains("Test addendum"));
        assert_eq!(draft.tool_allowlist_additions.len(), 1);
    }

    #[test]
    fn parse_proposal_accepts_top_level_role_with_no_parent() {
        let proposal = proposal_fixture(
            "pp-r2",
            "RoleDefinitionSuggestion",
            "Approved",
            Some(role_draft_payload("operator-mode", None)),
        );
        let draft = parse_proposal_as_role_draft(&proposal).unwrap();
        assert_eq!(draft.parent, None);
    }

    #[test]
    fn parse_proposal_refuses_non_role_definition_category() {
        // Operator picked a `ProfileHint` id by mistake.
        let proposal = proposal_fixture(
            "pp-x",
            "ProfileHint",
            "Approved",
            Some(serde_json::json!({"kind":"AppendList","value":"{}"})),
        );
        let err = parse_proposal_as_role_draft(&proposal).unwrap_err();
        assert!(err.contains("not `RoleDefinitionSuggestion`"));
        assert!(err.contains("aivyx-pa profile apply-hint"));
    }

    #[test]
    fn parse_proposal_refuses_pending_status() {
        let proposal = proposal_fixture("pp-y", "RoleDefinitionSuggestion", "Pending", None);
        let err = parse_proposal_as_role_draft(&proposal).unwrap_err();
        assert!(err.contains("only Approved"));
        assert!(err.contains("aivyx-pa persona proposals approve"));
    }

    #[test]
    fn parse_proposal_refuses_malformed_payload() {
        let proposal = proposal_fixture(
            "pp-bad",
            "RoleDefinitionSuggestion",
            "Approved",
            Some(serde_json::json!({"kind":"AppendList","value":"not json"})),
        );
        let err = parse_proposal_as_role_draft(&proposal).unwrap_err();
        assert!(err.contains("RoleDraft"));
    }

    #[test]
    fn parse_proposal_falls_back_to_proposed_op_when_applied_op_absent() {
        let mut proposal =
            proposal_fixture("pp-fallback", "RoleDefinitionSuggestion", "Approved", None);
        proposal.proposed_op = role_draft_payload("inline-role", None);
        let draft = parse_proposal_as_role_draft(&proposal).unwrap();
        assert_eq!(draft.name, "inline-role");
    }

    // ----- Phase 119 Task 7 — scripted e2e (role import pipeline) -----

    fn e2e_tempdir(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "aivyx-phase119-task7-role-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn e2e_role_import_pipeline_from_approved_proposal_to_aivyx_toml() {
        // Mirrors the ProfileHint e2e in profile.rs for the second
        // Phase 118 category. Steps:
        //   1. Build the wire-shape PersonaProposalSummary.
        //   2. Validate via the pure parser.
        //   3. Apply via the Task 3 atomic primitive.
        //   4. Read back the file; assert the [roles.<name>] section
        //      landed with the right addendum + parent + allowlist.
        //   5. Append the audit event the daemon would have written,
        //      assert chain HMAC verifies.
        use crate::toml_edit_apply::apply_role_draft_to_path;
        let dir = e2e_tempdir("import-pipeline");
        let aivyx_toml = dir.join("aivyx-pa.toml");
        // Pre-existing config has a profile + an unrelated role.
        // Import must add the new role WITHOUT touching either.
        std::fs::write(
            &aivyx_toml,
            "[profile]\n\
             assistant_name = \"Aivyx PA\"\n\
             \n\
             [roles.coder]\n\
             tool_allowlist = [\"fs.read\"]\n",
        )
        .unwrap();

        // Step 1 — wire-shape proposal.
        let proposal = proposal_fixture(
            "pp-e2e-role-1",
            "RoleDefinitionSuggestion",
            "Approved",
            Some(role_draft_payload("research-deploy", Some("research"))),
        );

        // Step 2 — pure parse.
        let draft = parse_proposal_as_role_draft(&proposal)
            .expect("Phase 118 proposal must validate as RoleDraft");
        assert_eq!(draft.name, "research-deploy");
        assert_eq!(draft.parent.as_deref(), Some("research"));

        // Step 3 — Task 3 atomic apply (force=false; no conflict
        // since `research-deploy` isn't in the pre-existing toml).
        let applied =
            apply_role_draft_to_path(&aivyx_toml, &draft, false).expect("apply must succeed");
        assert_eq!(applied.role_name, "research-deploy");
        assert_eq!(applied.parent.as_deref(), Some("research"));

        // Step 4 — file contents reflect the apply.
        let post = std::fs::read_to_string(&aivyx_toml).unwrap();
        // New section landed.
        assert!(post.contains("[roles.research-deploy]"));
        assert!(post.contains("inherits_from = \"research\""));
        assert!(post.contains("system_prompt = \"Test addendum.\""));
        assert!(post.contains("\"git.commit\""));
        // Pre-existing state untouched.
        assert!(post.contains("[profile]"));
        assert!(post.contains("\"Aivyx PA\""));
        assert!(post.contains("[roles.coder]"));
        assert!(post.contains("\"fs.read\""));

        // Step 5 — audit event linkage.
        use aivyx_audit::{AuditEvent, AuditLog, AuditWriter, HmacChainLog};
        let chain = HmacChainLog::new(b"phase-119-task7-test-key-32-byte".to_vec());
        chain
            .append(AuditEvent::RoleDraftImported {
                session_id: aivyx_core::SessionId::new(),
                proposal_id: proposal.id.clone(),
                role_name: applied.role_name.clone(),
                parent: applied.parent.clone(),
            })
            .expect("audit append must succeed");
        chain.verify().expect("chain must verify");
        assert_eq!(AuditLog::len(&chain), 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn e2e_role_import_refuses_overwrite_then_force_succeeds() {
        // The --force semantics end-to-end: same-name role already
        // present → RoleExists without --force; succeeds with --force
        // and replaces the section.
        use crate::toml_edit_apply::{TomlApplyError, apply_role_draft_to_path};
        let dir = e2e_tempdir("force-overwrite");
        let aivyx_toml = dir.join("aivyx-pa.toml");
        std::fs::write(
            &aivyx_toml,
            "[roles.research-deploy]\n\
             tool_allowlist = [\"git.status\"]\n",
        )
        .unwrap();

        let proposal = proposal_fixture(
            "pp-force-1",
            "RoleDefinitionSuggestion",
            "Approved",
            Some(role_draft_payload("research-deploy", None)),
        );
        let draft = parse_proposal_as_role_draft(&proposal).unwrap();

        // Without --force: refuses.
        let err = apply_role_draft_to_path(&aivyx_toml, &draft, false).unwrap_err();
        assert!(matches!(err, TomlApplyError::RoleExists { .. }));
        // File untouched.
        let mid = std::fs::read_to_string(&aivyx_toml).unwrap();
        assert!(mid.contains("\"git.status\""));

        // With --force: succeeds and replaces.
        let applied = apply_role_draft_to_path(&aivyx_toml, &draft, true)
            .expect("apply with force must succeed");
        assert_eq!(applied.role_name, "research-deploy");
        let post = std::fs::read_to_string(&aivyx_toml).unwrap();
        // Old allowlist gone (full section replacement).
        assert!(!post.contains("\"git.status\""));
        // New section's allowlist present.
        assert!(post.contains("\"git.commit\""));

        std::fs::remove_dir_all(&dir).ok();
    }
}
