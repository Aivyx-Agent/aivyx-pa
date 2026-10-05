//! Re-export shim + calendar-specific `default_token_path`.
//!
//! Phase 129 Task 2 lift: the implementation of
//! `save_tokens` / `load_tokens` now lives in
//! [`aivyx_google_oauth::storage`].

use std::path::PathBuf;

use aivyx_instance::InstancePaths;

pub use aivyx_google_oauth::storage::{load_tokens, save_tokens, StorageError};

/// Resolves the default token storage path for the
/// Calendar tool process for a given instance.
pub fn default_token_path_for(instance: &InstancePaths) -> Option<PathBuf> {
    instance
        .tool_process_dir("calendar")
        .map(|d| d.join("tokens.json"))
}

/// Resolves the default token storage path for the
/// Calendar tool process:
/// `$HOME/.aivyx-pa/tool-processes/calendar/tokens.json`.
pub fn default_token_path() -> Option<PathBuf> {
    let instance = InstancePaths::current().ok()?;
    default_token_path_for(&instance)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_instance::{BaseDirs, InstanceName};

    #[test]
    fn default_token_path_includes_calendar_segment() {
        let Some(p) = default_token_path() else {
            return;
        };
        let s = p.to_string_lossy();
        assert!(s.contains("tool-processes"), "{s}");
        assert!(s.contains("calendar"), "{s}");
        assert!(s.ends_with("tokens.json"), "{s}");
    }

    #[test]
    fn default_token_path_for_named_instance() {
        let dirs = BaseDirs {
            home: Some(PathBuf::from("/home/user")),
            xdg_config_home: Some(PathBuf::from("/etc/config")),
            xdg_data_home: Some(PathBuf::from("/var/data")),
            xdg_runtime_dir: Some(PathBuf::from("/run")),
        };
        let instance = InstancePaths::new(InstanceName::parse("research").unwrap(), dirs);
        let p = default_token_path_for(&instance).expect("path");
        assert_eq!(
            p,
            PathBuf::from("/home/user/.aivyx-pa/instances/research/tool-processes/calendar/tokens.json")
        );
    }
}
