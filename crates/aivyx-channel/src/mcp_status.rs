//! Shared MCP status snapshot — Chapter Conduit (CD.3) + Lantern (LN.1).
//!
//! The daemon writes a per-server health snapshot at the end of its MCP
//! startup loop; three readers consume it from one definition:
//! - `aivyx-pa mcp status` (the CLI, Chapter Conduit),
//! - the `GetMcpStatus` IPC handler (the Studio MCP screen, Chapter Lantern),
//! - the startup writer itself.
//!
//! The per-server row is the wasm-clean [`McpServerStatusView`] (in
//! `aivyx-ipc`, so the Studio renders it directly); this module owns the
//! native filesystem half — path resolution + read/write — which never
//! reaches the wasm graph. Snapshot semantics are "as of the last daemon
//! start" (a file, not live daemon memory; see Conduit OQ-4).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// Re-exported so consumers that already depend on `aivyx-channel` (the
// daemon binary's CLI) reach the wasm-clean view without a direct
// `aivyx-ipc` dependency.
pub use aivyx_ipc::protocol::McpServerStatusView;

/// On-disk snapshot: the captured time + each server's last-start health.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpStatusSnapshot {
    /// Unix seconds at which the daemon wrote this snapshot.
    pub captured_unix: u64,
    pub servers: Vec<McpServerStatusView>,
}

/// Current unix time in seconds (saturating to 0 before the epoch).
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Shared snapshot path for a given instance: `$XDG_DATA_HOME/aivyx-pa/mcp-status.json` (or
/// `$HOME/.local/share/aivyx-pa/…`), beside the store. `None` if neither
/// env var is set (no writable home — callers skip silently).
/// Task 3: path now comes from InstancePaths.
pub fn snapshot_path_for(paths: &aivyx_instance::InstancePaths) -> Option<PathBuf> {
    paths.data_dir().map(|d| d.join("mcp-status.json"))
}

/// Shared snapshot path: `$XDG_DATA_HOME/aivyx-pa/mcp-status.json` (or
/// `$HOME/.local/share/aivyx-pa/…`), beside the store. `None` if neither
/// env var is set (no writable home — callers skip silently).
pub fn snapshot_path() -> Option<PathBuf> {
    let paths = aivyx_instance::InstancePaths::current().ok()?;
    snapshot_path_for(&paths)
}

/// Write the snapshot (called by the daemon at the end of MCP startup).
/// Best-effort: `Ok(())` when there is no writable home.
pub fn write_snapshot(servers: &[McpServerStatusView]) -> std::io::Result<()> {
    let Some(path) = snapshot_path() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let snapshot = McpStatusSnapshot {
        captured_unix: unix_now(),
        servers: servers.to_vec(),
    };
    let json = serde_json::to_string_pretty(&snapshot).map_err(std::io::Error::other)?;
    std::fs::write(&path, json)
}

/// Read the snapshot. `Ok(None)` when no snapshot exists yet (the daemon
/// hasn't started with any `[[mcp_server]]` configured); `Err` only on a
/// present-but-unreadable/unparseable file.
pub fn read_snapshot() -> std::io::Result<Option<McpStatusSnapshot>> {
    let Some(path) = snapshot_path() else {
        return Ok(None);
    };
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let snapshot: McpStatusSnapshot =
        serde_json::from_str(&raw).map_err(std::io::Error::other)?;
    Ok(Some(snapshot))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Point `$XDG_DATA_HOME` at a fresh temp dir for the duration of a
    /// test so reads/writes are isolated. Not thread-safe across tests
    /// that share the env, so this module's tests run serially via a
    /// single combined test.
    fn with_temp_home<R>(f: impl FnOnce() -> R) -> R {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "aivyx-mcp-status-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let prev = std::env::var_os("XDG_DATA_HOME");
        // SAFETY: tests in this module are combined into one #[test] so
        // they don't race on the process environment.
        unsafe { std::env::set_var("XDG_DATA_HOME", &dir) };
        let out = f();
        match prev {
            Some(p) => unsafe { std::env::set_var("XDG_DATA_HOME", p) },
            None => unsafe { std::env::remove_var("XDG_DATA_HOME") },
        }
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    #[test]
    fn snapshot_write_read_and_absent() {
        with_temp_home(|| {
            // Absent → Ok(None).
            assert!(read_snapshot().unwrap().is_none());

            let servers = vec![
                McpServerStatusView::connected("github", "stdio", 26),
                McpServerStatusView::failed(
                    "broken",
                    "stdio",
                    "failed to start: not found".to_string(),
                    vec!["npm ERR! boom".to_string()],
                ),
            ];
            write_snapshot(&servers).unwrap();

            let back = read_snapshot().unwrap().expect("snapshot present");
            assert_eq!(back.servers.len(), 2);
            assert!(back.servers[0].connected && back.servers[0].tool_count == 26);
            assert!(!back.servers[1].connected);
            assert_eq!(back.servers[1].stderr_tail, vec!["npm ERR! boom".to_string()]);
            assert!(back.captured_unix > 0);
        });
    }
}
