//! Chapter Roster (RO.2) — the team-config file writer.
//!
//! The Studio's Teams screen (and the `SetTeamRoster` IPC) persists a whole
//! `[team]`-rooted file. Unlike the Chapter U section writers (which patch one
//! section of `aivyx-pa.toml` with `toml_edit`), the team file is owned end-to-end
//! by the team config, so the writer is just **validate → serialize → write**:
//! [`TeamConfig::validate`] (names / scopes / lead-is-a-member / ≤9 specialists),
//! [`TeamConfig::to_toml`], then [`aivyx_config::write_toml_0600`] (the shared
//! permission-pinning path). The daemon reads it back at the next start via
//! `team::resolve_daemon_team_config` (RO.1) — one definition, no drift.

use std::path::Path;

use aivyx_team::TeamConfig;

/// Why a team-config write failed — mapped to a typed IPC `QueryError` by the
/// `SetTeamRoster` handler.
#[derive(Debug)]
pub enum TeamConfigWriteError {
    /// The roster failed [`TeamConfig::validate`] (bad name, unknown scope base,
    /// lead-not-a-member, more than 9 specialists, …). Carries the validator's
    /// message; nothing was written.
    Invalid(String),
    /// Serialization or the filesystem write failed (after validation passed).
    Write(String),
}

/// Validate then write `roster` to `path` as a `[team]`-rooted TOML file, pinned
/// `0600`. **Validation runs first**, so an invalid roster never touches disk.
/// A missing parent directory is created.
pub fn write_team_config(path: &Path, roster: &TeamConfig) -> Result<(), TeamConfigWriteError> {
    roster
        .validate()
        .map_err(|e| TeamConfigWriteError::Invalid(e.to_string()))?;
    // 2.0 (local-only): `load_team` refuses a non-local member `base_url`, so
    // never persist one. May do a blocking DNS lookup; the daemon calls this
    // off the async runtime.
    aivyx_team::check_team_local(roster).map_err(|e| TeamConfigWriteError::Invalid(e.to_string()))?;
    let toml = roster
        .to_toml()
        .map_err(|e| TeamConfigWriteError::Write(e.to_string()))?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| {
                TeamConfigWriteError::Write(format!("failed to create {}: {e}", parent.display()))
            })?;
        }
    }
    aivyx_config::write_toml_0600(path, &toml).map_err(|e| TeamConfigWriteError::Write(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_capability::TrustTier;
    use aivyx_team::config::{DialogueConfig, TeamMember};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn scratch(tag: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!(
            "aivyx-roster-write-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn member(name: &str) -> TeamMember {
        TeamMember {
            name: name.into(),
            role: "R".into(),
            soul: "s".into(),
            tool_allowlist: vec![],
            capability_scopes: vec![],
            trust_ceiling: TrustTier::Trusted,
            model: None,
            base_url: None,
        }
    }

    fn team(name: &str, lead: &str, members: Vec<TeamMember>) -> TeamConfig {
        TeamConfig {
            name: name.into(),
            description: String::new(),
            lead: lead.into(),
            members,
            dialogue: DialogueConfig::default(),
        }
    }

    #[test]
    fn write_then_load_round_trips() {
        let path = scratch("roundtrip").join("team.toml");
        let roster = team("my-team", "boss", vec![member("boss"), member("helper")]);
        write_team_config(&path, &roster).unwrap();
        let back = TeamConfig::load(&path).unwrap();
        assert_eq!(back, roster);
    }

    #[test]
    fn write_creates_a_missing_parent_dir() {
        let path = scratch("mkparent").join("nested/teams/team.toml");
        let roster = team("nested-team", "lead", vec![member("lead"), member("m")]);
        write_team_config(&path, &roster).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn a_non_local_member_base_url_is_refused_and_nothing_is_written() {
        let path = scratch("nonlocal").join("team.toml");
        let mut m = member("helper");
        m.base_url = Some("http://203.0.113.9:11434".into());
        let roster = team("t", "boss", vec![member("boss"), m]);
        let err = write_team_config(&path, &roster).unwrap_err();
        match err {
            TeamConfigWriteError::Invalid(msg) => {
                assert!(msg.contains("is not local") && msg.contains("helper"), "{msg}")
            }
            other => panic!("got {other:?}"),
        }
        assert!(!path.exists(), "a non-local roster must not touch disk");
        // A LAN member is fine.
        let mut lan = member("helper");
        lan.base_url = Some("http://192.168.1.50:11434".into());
        write_team_config(&path, &team("t", "boss", vec![member("boss"), lan])).unwrap();
    }

    #[test]
    fn invalid_roster_is_rejected_and_nothing_is_written() {
        let path = scratch("invalid").join("team.toml");
        // Lead is not one of the members → validate() fails.
        let roster = team("bad", "ghost", vec![member("a"), member("b")]);
        let err = write_team_config(&path, &roster).unwrap_err();
        assert!(matches!(err, TeamConfigWriteError::Invalid(_)), "got {err:?}");
        assert!(!path.exists(), "an invalid roster must not touch disk");
    }

    #[test]
    fn unknown_scope_base_is_rejected() {
        let path = scratch("scope").join("team.toml");
        let mut m = member("worker");
        m.capability_scopes = vec!["not.a.real.base".into()];
        let roster = team("scoped", "lead", vec![member("lead"), m]);
        let err = write_team_config(&path, &roster).unwrap_err();
        assert!(matches!(err, TeamConfigWriteError::Invalid(_)), "got {err:?}");
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn written_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let path = scratch("perms").join("team.toml");
        let roster = team("perm-team", "lead", vec![member("lead"), member("m")]);
        write_team_config(&path, &roster).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
