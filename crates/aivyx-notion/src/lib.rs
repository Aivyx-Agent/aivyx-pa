//! # aivyx-notion
//!
//! Notion third-party tool process for Aivyx. Chapter F #5
//! — Phase 130. Ships as a separate binary the operator
//! installs and wires into `aivyx-pa.toml` via
//! `[[tool_process]]`. Per PRODUCT.md P10 (substrate is
//! closed at thirteen tools forever; productivity APIs are
//! third-party territory).
//!
//! ## Why no OAuth substrate
//!
//! Notion uses **Integration token (API key) auth** — not
//! the OAuth 2.0 flow Gmail / Calendar / Drive use. The
//! operator creates an "internal integration" in Notion's
//! Integrations dashboard, copies the generated token, and
//! pastes it into `~/.aivyx-pa/tool-processes/notion/config.toml`.
//! No callback flow, no token refresh, no token storage on
//! disk beyond the operator's config file. This crate
//! therefore does NOT depend on `aivyx-google-oauth`.
//!
//! ## Notion's critical UX quirk
//!
//! Integrations don't have implicit access to the
//! operator's content. After creating the integration, the
//! operator MUST explicitly share each page / database
//! they want Aivyx to see by clicking "Share" → "Invite"
//! in Notion's UI and selecting the integration. Without
//! this, `notion.search` returns empty results and
//! `notion.get_page` returns 404. INSTALL.md documents
//! this prominently.
//!
//! ## Layout (Phase 130)
//!
//! - [`notion_client`] — Notion v1 REST API client
//!   (`api.notion.com/v1` with
//!   `Authorization: Bearer <integration-token>` +
//!   `Notion-Version: 2022-06-28` headers).
//! - [`auth_cli`] — minimal CLI surface
//!   (`aivyx-notion auth status / check / help`).
//!   No init/revoke subcommands because there's no token
//!   exchange flow.
//! - [`tools`] — seven `aivyx_core::Tool` impls per
//!   Phase 130 Q1a:
//!   - `notion.search` (Task 3; `notion.read`)
//!   - `notion.get_page` (Task 4; `notion.read`)
//!   - `notion.list_database` (Task 5; `notion.read`)
//!   - `notion.create_page` (Task 6; `notion.write`,
//!     CEILING_TRUSTED)
//!   - `notion.append_blocks` (Task 7; `notion.write`,
//!     CEILING_TRUSTED)
//!   - `notion.update_page_properties` (Task 8;
//!     `notion.write`, CEILING_TRUSTED)
//!   - `notion.archive_page` (Task 9; `notion.write`,
//!     CEILING_TRUSTED)
//!
//! Multi-tool harness consumed from
//! `aivyx_tool::multi_harness` (Phase 128 Task 2 lift).

pub mod auth_cli;
pub mod notion_client;
pub mod tools;

pub use notion_client::{NotionClient, NotionClientError, NotionConfig};

use aivyx_instance::InstancePaths;

// Re-export the lifted multi-tool harness so the binary
// uses the same import surface as gmail / calendar / drive.
pub use aivyx_tool::multi_harness::{run_multi_tool_subprocess, HarnessError};

/// Notion-Version date pin. Notion's REST API requires a
/// version header on every request; pinning a known-good
/// version shields against breaking changes. Bump in a
/// future substrate phase when Notion deprecates this
/// version (typically 12+ month notice).
pub const NOTION_VERSION: &str = "2022-06-28";

/// Base URL for Notion's REST API. All endpoints in this
/// crate's tools are appended to this base.
pub const NOTION_API_BASE: &str = "https://api.notion.com/v1";

/// Default config file path for a given instance:
/// `~/.aivyx-pa[/instances/<n>]/tool-processes/notion/config.toml`.
/// Returns `None` when the instance path cannot be resolved.
pub fn default_config_path_for(instance: &InstancePaths) -> Option<std::path::PathBuf> {
    instance
        .tool_process_dir("notion")
        .map(|d| d.join("config.toml"))
}

/// Default config file path:
/// `$HOME/.aivyx-pa/tool-processes/notion/config.toml`.
/// Returns `None` when the instance cannot be resolved from the environment.
pub fn default_config_path() -> Option<std::path::PathBuf> {
    let instance = InstancePaths::current().ok()?;
    default_config_path_for(&instance)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_instance::{BaseDirs, InstanceName};

    #[test]
    fn default_config_path_for_named_instance() {
        let dirs = BaseDirs {
            home: Some(std::path::PathBuf::from("/home/user")),
            xdg_config_home: Some(std::path::PathBuf::from("/etc/config")),
            xdg_data_home: Some(std::path::PathBuf::from("/var/data")),
            xdg_runtime_dir: Some(std::path::PathBuf::from("/run")),
        };
        let instance = InstancePaths::new(InstanceName::parse("research").unwrap(), dirs);
        let p = default_config_path_for(&instance).expect("path");
        assert_eq!(
            p,
            std::path::PathBuf::from("/home/user/.aivyx-pa/instances/research/tool-processes/notion/config.toml")
        );
    }
}
