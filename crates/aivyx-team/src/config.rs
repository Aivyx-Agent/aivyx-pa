//! The team roster types — `TeamConfig` / `TeamMember` / `DialogueConfig` /
//! `TeamError` (+ `MAX_SPECIALISTS`).
//!
//! Moved to the wasm-clean [`aivyx_team_types`] crate (Chapter M.1) so the
//! browser Mission-Control app can share them with the daemon. Re-exported
//! here so `aivyx-team`'s API and every `crate::config::…` reference are
//! unchanged.
pub use aivyx_team_types::config::*;

use std::path::Path;

/// The local-only guard for a member `base_url`: the `NotLocal` text from
/// `aivyx_route::require_local`.
fn member_url_is_local(url: &str) -> Result<(), String> {
    aivyx_route::require_local(url).map_err(|e| e.to_string())
}

/// Load a team roster, refusing any member `base_url` that is not on this
/// machine or the operator's network. **Every pa load path uses this**, not
/// `TeamConfig::load`, which doesn't check URLs. May do blocking DNS for a
/// domain name.
pub fn load_team(path: impl AsRef<Path>) -> Result<TeamConfig, TeamError> {
    TeamConfig::load_checked(path, &member_url_is_local)
}

/// The `load_team` member-URL check on an in-memory roster, for writers that
/// must refuse before persisting. May do blocking DNS for a domain name.
pub fn check_team_local(team: &TeamConfig) -> Result<(), TeamError> {
    team.check_base_urls(&member_url_is_local)
}

/// [`load_team`] for TOML text.
pub fn team_from_toml(s: &str) -> Result<TeamConfig, TeamError> {
    TeamConfig::from_toml_checked(s, &member_url_is_local)
}

#[cfg(test)]
mod local_tests {
    use super::*;

    fn toml(url: Option<&str>) -> String {
        let u = url.map(|u| format!("base_url = \"{u}\"\n")).unwrap_or_default();
        format!(
            "[team]\nname = \"t\"\nlead = \"lead\"\n\n[[team.member]]\nname = \"lead\"\n\
             role = \"R\"\nsoul = \"s\"\ntrust_ceiling = \"Trusted\"\n{u}"
        )
    }

    #[test]
    fn public_member_url_is_refused() {
        let e = team_from_toml(&toml(Some("http://203.0.113.9:11434"))).unwrap_err();
        let m = e.to_string();
        assert!(m.contains("team member lead base_url: http://203.0.113.9:11434"), "{m}");
        assert!(m.contains("is not local (resolves to a public address 203.0.113.9)"), "{m}");
        assert!(m.contains("aivyx is local-only"), "{m}");
        let e = team_from_toml(&toml(Some("https://api.openai.com/v1"))).unwrap_err();
        assert!(e.to_string().contains("cloud host"), "{e}");
    }

    #[test]
    fn lan_member_url_is_accepted() {
        assert!(team_from_toml(&toml(Some("http://192.168.1.50:11434"))).is_ok());
        assert!(team_from_toml(&toml(Some("http://127.0.0.1:8080"))).is_ok());
    }

    #[test]
    fn missing_member_url_is_fine() {
        assert!(team_from_toml(&toml(None)).is_ok());
    }

    #[test]
    fn load_team_checks_the_file_too() {
        let dir = std::env::temp_dir().join(format!("aivyx-team-local-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("team.toml");
        std::fs::write(&p, toml(Some("http://203.0.113.9:1"))).unwrap();
        assert!(load_team(&p).is_err());
        std::fs::write(&p, toml(Some("http://10.0.0.2:1"))).unwrap();
        assert!(load_team(&p).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
