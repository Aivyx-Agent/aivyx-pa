//! Re-export shim + gmail-specific `default_token_path`.
//!
//! Phase 129 Task 2 lift: the implementation of
//! `save_tokens` / `load_tokens` now lives in
//! [`aivyx_google_oauth::storage`]. The service-specific
//! token-file path resolution
//! (`~/.aivyx-pa/tool-processes/gmail/tokens.json`) stays
//! here because each Google integration has its own
//! service-name segment.

use std::path::PathBuf;

use aivyx_instance::InstancePaths;

pub use aivyx_google_oauth::storage::{load_tokens, save_tokens, StorageError};

/// Resolves the default token storage path for the Gmail
/// tool process for a given instance.
/// Returns `None` when the instance path cannot be resolved.
pub fn default_token_path_for(instance: &InstancePaths) -> Option<PathBuf> {
    instance
        .tool_process_dir("gmail")
        .map(|d| d.join("tokens.json"))
}

/// Resolves the default token storage path for the Gmail
/// tool process: `$HOME/.aivyx-pa/tool-processes/gmail/tokens.json`.
/// Returns `None` when the instance cannot be resolved from the environment
/// (CI or other non-interactive contexts).
pub fn default_token_path() -> Option<PathBuf> {
    let instance = InstancePaths::current().ok()?;
    default_token_path_for(&instance)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_instance::{BaseDirs, InstanceName};

    #[test]
    fn default_token_path_includes_gmail_segment() {
        // Test only runs when HOME is set (always true on
        // dev/CI Linux + macOS); skipped otherwise.
        let Some(p) = default_token_path() else {
            return;
        };
        let s = p.to_string_lossy();
        assert!(s.contains("tool-processes"), "{s}");
        assert!(s.contains("gmail"), "{s}");
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
            PathBuf::from("/home/user/.aivyx-pa/instances/research/tool-processes/gmail/tokens.json")
        );
    }
}
