//! `aivyx-pa workspace` — operator-facing visibility into the agent's personal
//! workspace (Chapter O). The workspace is the agent's own space, but it is a
//! real directory the operator can always inspect: `ls` lists it, `cat` prints
//! a file, `path` prints where it is. Read-only — no daemon, no passphrase.

use std::path::{Component, Path, PathBuf};

use aivyx_config::{AivyxConfig, LoadOptions};

/// The operator's `aivyx-pa.toml` — see `aivyx_config::resolve_config_path`.
fn config_path() -> std::path::PathBuf {
    aivyx_config::resolve_config_path()
}

/// `aivyx-pa workspace path` — print the resolved workspace directory.
pub fn run_workspace_path() -> Result<(), String> {
    let root = workspace_root()?;
    println!("{}", root.display());
    Ok(())
}

/// `aivyx-pa workspace ls [path]` — list the workspace, or a sub-path within it.
pub fn run_workspace_ls(rel: Option<&str>) -> Result<(), String> {
    let root = workspace_root()?;
    let target = resolve_within(&root, rel.unwrap_or("."))?;
    let mut entries: Vec<(String, bool)> = std::fs::read_dir(&target)
        .map_err(|e| format!("cannot list {}: {e}", display_rel(&root, &target)))?
        .filter_map(|e| e.ok())
        .map(|e| {
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            (e.file_name().to_string_lossy().into_owned(), is_dir)
        })
        .collect();
    entries.sort();
    if entries.is_empty() {
        println!("(empty)");
    }
    for (name, is_dir) in entries {
        println!("{}{}", name, if is_dir { "/" } else { "" });
    }
    Ok(())
}

/// `aivyx-pa workspace cat <path>` — print a file from the workspace.
pub fn run_workspace_cat(rel: &str) -> Result<(), String> {
    let root = workspace_root()?;
    let target = resolve_within(&root, rel)?;
    let body = std::fs::read_to_string(&target)
        .map_err(|e| format!("cannot read {}: {e}", display_rel(&root, &target)))?;
    print!("{body}");
    Ok(())
}

/// Resolve the workspace root from config (honours `[workspace] path` /
/// `AIVYX_PA_WORKSPACE`). Errors if the workspace is disabled or absent.
fn workspace_root() -> Result<PathBuf, String> {
    let cfg = load_config_for_inspection()?;
    if !cfg.workspace_enabled.value {
        return Err("the agent workspace is disabled (`[workspace] enabled = false`).".into());
    }
    let root = cfg.workspace_path.value;
    if !root.exists() {
        return Err(format!(
            "workspace directory {} does not exist yet — start the daemon once \
             to provision it.",
            root.display()
        ));
    }
    Ok(root)
}

/// Join `rel` under `root` and reject any path that escapes (`..` / absolute).
/// Operator-run, but still fenced so a stray `../../etc/passwd` can't read out.
fn resolve_within(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let mut joined = PathBuf::from(root);
    joined.push(rel);
    let root_n = root.components().count();
    let mut stack: Vec<Component<'_>> = Vec::new();
    for c in joined.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if stack.len() <= root_n {
                    return Err(format!("path {rel:?} escapes the workspace"));
                }
                stack.pop();
            }
            other => stack.push(other),
        }
    }
    let resolved: PathBuf = stack.into_iter().collect();
    if !resolved.starts_with(root) {
        return Err(format!("path {rel:?} escapes the workspace"));
    }
    Ok(resolved)
}

fn display_rel(root: &Path, target: &Path) -> String {
    target
        .strip_prefix(root)
        .map(|p| {
            if p.as_os_str().is_empty() {
                ".".into()
            } else {
                p.display().to_string()
            }
        })
        .unwrap_or_else(|_| target.display().to_string())
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_within_accepts_in_workspace_paths() {
        let root = Path::new("/ws");
        assert_eq!(
            resolve_within(root, "plans/x.md").unwrap(),
            PathBuf::from("/ws/plans/x.md")
        );
        assert_eq!(resolve_within(root, ".").unwrap(), PathBuf::from("/ws"));
    }

    #[test]
    fn resolve_within_rejects_escapes() {
        let root = Path::new("/ws");
        assert!(resolve_within(root, "../etc/passwd").is_err());
        assert!(resolve_within(root, "../../etc").is_err());
        assert!(resolve_within(root, "/etc/passwd").is_err());
    }
}
