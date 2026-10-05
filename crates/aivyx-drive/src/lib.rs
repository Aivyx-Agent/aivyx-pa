//! # aivyx-drive
//!
//! Google Drive third-party tool process for Aivyx PA.
//! Chapter F #3 — Phase 129. Ships as a separate binary
//! the operator installs and wires into `aivyx-pa.toml` via
//! `[[tool_process]]`. Per PRODUCT.md P10 (substrate is
//! closed at thirteen tools forever; drive is third-party
//! territory).
//!
//! ## Layout (Phase 129)
//!
//! - **OAuth substrate consumed from
//!   [`aivyx_google_oauth`]** (Phase 129 Task 2 lift) —
//!   no in-tree OAuth copy. Drive is the third Google
//!   integration; the lifted substrate is paid for.
//! - [`auth_cli`] — `aivyx-drive auth init / status /
//!   revoke` operator-facing CLI subcommand surface.
//! - [`drive_client`] — Google Drive v3 REST API client
//!   (token-authenticated reqwest wrapper around
//!   `https://www.googleapis.com/drive/v3/...` +
//!   `https://www.googleapis.com/upload/drive/v3/...`
//!   for uploads).
//! - [`tools`] — seven `aivyx_core::Tool` impls per Phase
//!   129 Q2b (operator-picked richer surface):
//!   - `drive.search` (Task 4; `drive.read`)
//!   - `drive.get_metadata` (Task 5; `drive.read`)
//!   - `drive.list_folder` (Task 6; `drive.read`)
//!   - `drive.create_folder` (Task 7; `drive.write`,
//!     CEILING_TRUSTED)
//!   - `drive.download_file` (Task 8; `drive.read`; 10
//!     MB inline cap)
//!   - `drive.upload_file` (Task 9; `drive.write`,
//!     CEILING_TRUSTED; 10 MB cap)
//!   - `drive.delete_file` (Task 10; `drive.write`,
//!     CEILING_TRUSTED; idempotent)
//!
//! Multi-tool harness consumed from
//! `aivyx_tool::multi_harness` (Phase 128 Task 2 lift).
//!
//! ## Auth model
//!
//! Operator-provided OAuth app — same posture as Gmail
//! and Calendar. Most operators reuse their existing
//! Aivyx PA Gmail / Calendar GCP project + client_id +
//! client_secret; they just enable the Drive API
//! alongside the others, register
//! `https://www.googleapis.com/auth/drive` on the OAuth
//! consent screen, and run `aivyx-drive auth init` to
//! grant the Drive scope independently.

pub mod auth_cli;
pub mod drive_client;
pub mod tools;

pub use drive_client::{DriveClient, DriveClientError};

use aivyx_instance::InstancePaths;

// Re-export the lifted OAuth substrate so consumers
// (main.rs + downstream) can use a single import path.
pub use aivyx_google_oauth::{
    exchange_code, load_tokens, refresh_access_token, save_tokens, ExchangeError,
    OAuthConfig, OAuthError, StorageError, TokenSet, GOOGLE_AUTH_ENDPOINT,
    GOOGLE_TOKEN_ENDPOINT,
};

// Re-export the lifted multi-tool harness so consumers use
// the same import surface as aivyx-gmail / aivyx-calendar /
// aivyx-toolkit.
pub use aivyx_tool::multi_harness::{run_multi_tool_subprocess, HarnessError};

/// Default Drive OAuth scopes covering the Phase 129
/// Task 4-10 tool surface (7 tools — Q2b operator-picked).
///
/// Default is the BROAD `auth/drive` scope (read+write
/// across all files accessible to the user). Per the
/// Phase 129 honest-scope-risk section, the default
/// favors operator-ergonomics (fewer "re-auth with new
/// scope" loops) over least-privilege. INSTALL.md
/// documents the three narrower options:
///
/// - `auth/drive.file` — only files created by Aivyx PA
/// - `auth/drive.readonly` — read-only all-files
/// - `auth/drive.metadata.readonly` — read-only metadata
///
/// Operators wanting a narrower posture supply `scopes`
/// in their config.toml; the write tools fail with a
/// clear "scope not granted" message when narrower
/// scopes are in effect.
pub const DEFAULT_DRIVE_SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/drive",
    // Phase 159 — Drive Activity API
    // (`drive.recent_activity` tool). Separate
    // googleapis host + separate scope; operators
    // upgrading from a pre-Phase-159 install
    // re-run `aivyx-drive auth init` so the new
    // scope is granted.
    "https://www.googleapis.com/auth/drive.activity.readonly",
];

/// Service-specific token storage path
/// (`~/.aivyx-pa[/instances/<n>]/tool-processes/drive/tokens.json`).
/// Returns `None` when the instance path cannot be resolved.
pub fn default_token_path_for(instance: &InstancePaths) -> Option<std::path::PathBuf> {
    instance
        .tool_process_dir("drive")
        .map(|d| d.join("tokens.json"))
}

/// (`$HOME/.aivyx-pa/tool-processes/drive/tokens.json`).
/// Returns `None` when the instance cannot be resolved from the environment.
pub fn default_token_path() -> Option<std::path::PathBuf> {
    let instance = InstancePaths::current().ok()?;
    default_token_path_for(&instance)
}

#[cfg(test)]
mod scope_tests {
    use super::*;
    use aivyx_instance::{BaseDirs, InstanceName};

    #[test]
    fn default_scopes_include_full_drive() {
        assert!(
            DEFAULT_DRIVE_SCOPES.contains(&"https://www.googleapis.com/auth/drive")
        );
    }

    #[test]
    fn default_scopes_include_drive_activity_readonly() {
        // Phase 159 — regression pin so a future
        // narrowing of DEFAULT_DRIVE_SCOPES
        // notices the impact on
        // drive.recent_activity.
        assert!(DEFAULT_DRIVE_SCOPES
            .contains(&"https://www.googleapis.com/auth/drive.activity.readonly"));
    }

    #[test]
    fn default_token_path_for_named_instance() {
        let dirs = BaseDirs {
            home: Some(std::path::PathBuf::from("/home/user")),
            xdg_config_home: Some(std::path::PathBuf::from("/etc/config")),
            xdg_data_home: Some(std::path::PathBuf::from("/var/data")),
            xdg_runtime_dir: Some(std::path::PathBuf::from("/run")),
        };
        let instance = InstancePaths::new(InstanceName::parse("research").unwrap(), dirs);
        let p = default_token_path_for(&instance).expect("path");
        assert_eq!(
            p,
            std::path::PathBuf::from("/home/user/.aivyx-pa/instances/research/tool-processes/drive/tokens.json")
        );
    }
}
