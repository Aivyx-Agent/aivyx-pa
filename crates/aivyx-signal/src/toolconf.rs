//! The instance's signal tool folder (`tool_process_dir("signal")`):
//! signal-cli's socket, and `signal-tool.toml` — the part of `[signal]` the
//! sandboxed `signal.send` tool needs (`send_to`), since it can't read
//! `aivyx-pa.toml`. Written by `aivyx-pa connect signal`.

use std::path::{Path, PathBuf};

use aivyx_instance::InstancePaths;
use serde::{Deserialize, Serialize};

pub const SOCKET_FILE: &str = "signal-cli.sock";
pub const TOOL_CONFIG_FILE: &str = "signal-tool.toml";

/// `<tool_process_dir("signal")>/signal-cli.sock` — where the operator's
/// signal-cli daemon listens (`--socket`).
pub fn socket_path(instance: &InstancePaths) -> Option<PathBuf> {
    instance.tool_process_dir("signal").map(|d| d.join(SOCKET_FILE))
}

/// `<tool_process_dir("signal")>/signal-tool.toml`.
pub fn tool_config_path(instance: &InstancePaths) -> Option<PathBuf> {
    instance
        .tool_process_dir("signal")
        .map(|d| d.join(TOOL_CONFIG_FILE))
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolConfig {
    /// The numbers `signal.send` may message.
    pub send_to: Vec<String>,
}

/// `None` when `connect signal` hasn't run.
pub fn load(path: &Path) -> Result<Option<ToolConfig>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text)
            .map(Some)
            .map_err(|e| format!("read {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

/// Creates the folder (`0700`) and writes the file (`0600`) via an
/// exclusively-created temporary file and a rename.
pub fn save(path: &Path, cfg: &ToolConfig) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no folder", path.display()))?;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .map_err(|e| format!("create {}: {e}", dir.display()))?;
    let text = toml::to_string(cfg).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("toml.tmp");
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("remove {}: {e}", tmp.display())),
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    {
        use std::io::Write;
        let mut f = options
            .open(&tmp)
            .map_err(|e| format!("write {}: {e}", tmp.display()))?;
        f.write_all(text.as_bytes())
            .map_err(|e| format!("write {}: {e}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_live_in_the_instances_signal_tool_folder() {
        let dirs = aivyx_instance::BaseDirs {
            home: Some(PathBuf::from("/h")),
            xdg_config_home: None,
            xdg_data_home: None,
            xdg_runtime_dir: None,
        };
        let work = InstancePaths::new(aivyx_instance::InstanceName::parse("work").unwrap(), dirs);
        assert_eq!(
            socket_path(&work),
            Some(PathBuf::from("/h/.aivyx-pa/instances/work/tool-processes/signal/signal-cli.sock"))
        );
        assert_eq!(
            tool_config_path(&work),
            Some(PathBuf::from("/h/.aivyx-pa/instances/work/tool-processes/signal/signal-tool.toml"))
        );
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("signal").join(TOOL_CONFIG_FILE);
        assert_eq!(load(&path).unwrap(), None);
        let cfg = ToolConfig { send_to: vec!["+15550002222".into()] };
        save(&path, &cfg).unwrap();
        assert_eq!(load(&path).unwrap(), Some(cfg.clone()));
        // A leftover temporary file doesn't block or leak into the next save.
        std::fs::write(path.with_extension("toml.tmp"), "junk").unwrap();
        save(&path, &cfg).unwrap();
        assert_eq!(load(&path).unwrap(), Some(cfg));
    }
}
