//! Chapter Z — read-only filesystem browsing for the Studio's Documents screen.
//!
//! Both `list_dir` and `read_file` run a request through the **same guard the
//! fs/workspace tools use**: lexically resolve `rel` against a *pre-canonicalized*
//! root (`aivyx_core::tools::fs::lexical_resolve` rejects `..`-over-root by
//! construction), then `std::fs::canonicalize` resolves every symlink, then the
//! result must still `starts_with(root)`. `..` and symlink escapes are rejected.
//!
//! Read-only by design: there is no write/delete/rename here. File reads are
//! capped ([`READ_CAP_BYTES`]) and binary-aware (a NUL byte ⇒ `content = None`).
//! The caller passes an **already-canonical** root (the daemon canonicalizes the
//! `fs_root` / workspace root once at startup).

use std::path::{Path, PathBuf};

use aivyx_ipc::protocol::{DocEntry, DocFile};

/// Cap on the bytes returned for a file's text content. Larger files come back
/// with `content = None` + `truncated`/`size_bytes` so the viewer can say
/// "N KB — not shown".
pub const READ_CAP_BYTES: usize = 256 * 1024;

/// Why a browse request was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowseError {
    /// `..` or a symlink resolved outside the root.
    PathEscape,
    /// The path does not exist.
    NotFound,
    /// `list_dir` was asked for a non-directory.
    NotADir,
    /// `read_file` was asked for a directory.
    NotAFile,
    /// Chapter DW — the target already exists (create / rename / non-overwrite
    /// write would clobber).
    Exists,
    /// Chapter DW — a directory delete was asked of a non-empty directory.
    NotEmpty,
    /// Any other I/O failure.
    Io(String),
}

impl std::fmt::Display for BrowseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BrowseError::PathEscape => write!(f, "path escapes the allowed root"),
            BrowseError::NotFound => write!(f, "no such file or directory"),
            BrowseError::NotADir => write!(f, "not a directory"),
            BrowseError::NotAFile => write!(f, "not a file"),
            BrowseError::Exists => write!(f, "already exists"),
            BrowseError::NotEmpty => write!(f, "directory is not empty"),
            BrowseError::Io(e) => write!(f, "{e}"),
        }
    }
}

/// Resolve `rel` against the pre-canonicalized `root`, verifying it stays under
/// `root` after symlink resolution. The shared guard for both list + read.
fn safe_resolve(root: &Path, rel: &str) -> Result<PathBuf, BrowseError> {
    // Empty / "." ⇒ the root itself.
    let rel = if rel.trim().is_empty() { "." } else { rel };
    // Lexical resolution first — rejects more `..` than depth below the root.
    let lexical =
        aivyx_core::tools::fs::lexical_resolve(root, Path::new(rel)).ok_or(BrowseError::PathEscape)?;
    // Canonicalize resolves every symlink; the path must exist.
    let canonical = std::fs::canonicalize(&lexical).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => BrowseError::NotFound,
        _ => BrowseError::Io(e.to_string()),
    })?;
    // The canonical target must still live under the canonical root.
    if !canonical.starts_with(root) {
        return Err(BrowseError::PathEscape);
    }
    Ok(canonical)
}

/// List the directory at `rel` under `root` (read-only). Entries are sorted
/// directories-first, then case-insensitively by name.
pub fn list_dir(root: &Path, rel: &str) -> Result<Vec<DocEntry>, BrowseError> {
    let dir = safe_resolve(root, rel)?;
    let md = std::fs::metadata(&dir).map_err(|e| BrowseError::Io(e.to_string()))?;
    if !md.is_dir() {
        return Err(BrowseError::NotADir);
    }
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&dir)
        .map_err(|e| BrowseError::Io(e.to_string()))?
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        let (kind, size_bytes) = match entry.file_type() {
            Ok(t) if t.is_dir() => ("dir", 0),
            Ok(t) if t.is_file() => ("file", entry.metadata().map(|m| m.len()).unwrap_or(0)),
            Ok(t) if t.is_symlink() => ("symlink", 0),
            _ => ("other", 0),
        };
        entries.push(DocEntry { name, kind: kind.to_string(), size_bytes });
    }
    entries.sort_by(|a, b| {
        let a_dir = a.kind == "dir";
        let b_dir = b.kind == "dir";
        b_dir
            .cmp(&a_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

/// Read the file at `rel` under `root`, capped at [`READ_CAP_BYTES`]. A file
/// containing a NUL byte is reported as binary (`content = None`); otherwise the
/// (possibly truncated) bytes are lossily decoded to UTF-8.
pub fn read_file(root: &Path, rel: &str) -> Result<DocFile, BrowseError> {
    use std::io::Read;
    let path = safe_resolve(root, rel)?;
    let md = std::fs::metadata(&path).map_err(|e| BrowseError::Io(e.to_string()))?;
    if md.is_dir() {
        return Err(BrowseError::NotAFile);
    }
    let size_bytes = md.len();

    // Audit P5: a FIFO in the folder (the agent can `mkfifo`) would block this
    // open forever; open without blocking and refuse anything but a regular file.
    let file = aivyx_core::tools::fs::open_regular_file(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::InvalidInput {
            BrowseError::NotAFile
        } else {
            BrowseError::Io(e.to_string())
        }
    })?;
    let mut bytes = Vec::new();
    file.take(READ_CAP_BYTES as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| BrowseError::Io(e.to_string()))?;

    let truncated = size_bytes > bytes.len() as u64;
    let binary = bytes.contains(&0);
    let content = if binary {
        None
    } else {
        // Lossy decode tolerates a multi-byte char split at the cap boundary.
        Some(String::from_utf8_lossy(&bytes).into_owned())
    };

    Ok(DocFile {
        path: rel.to_string(),
        size_bytes,
        content,
        truncated,
        binary,
    })
}

// ---------------------------------------------------------------------------
// Chapter DW — writes. The same root guard, but for a path that may not exist
// yet we canonicalize the **parent** (which must) then join the leaf — exactly
// the `FsWriteTool` pattern. No op can escape the root.
// ---------------------------------------------------------------------------

/// Resolve a target whose final component may not exist yet (create / write /
/// rename target / mkdir): the parent dir is resolved under the root (must
/// exist + be a directory), the single leaf component is joined on. Rejects an
/// empty / `.` / `..`-ending `rel` (no leaf to create).
fn safe_resolve_parent(root: &Path, rel: &str) -> Result<PathBuf, BrowseError> {
    let rel_path = Path::new(rel.trim());
    // `file_name()` is `None` for "", ".", "..", or a trailing-slash path —
    // none of which name a new leaf, so they're rejected by construction.
    let leaf = rel_path.file_name().ok_or(BrowseError::PathEscape)?;
    let parent_rel = rel_path
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent = safe_resolve(root, &parent_rel)?;
    if !parent.is_dir() {
        return Err(BrowseError::NotADir);
    }
    Ok(parent.join(leaf))
}

/// A unique temp-file name in the same directory (for atomic temp+rename).
fn temp_name() -> String {
    format!(
        ".aivyx-tmp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    )
}

/// Write `content` to `rel` under `root` (create or, with `overwrite`, replace).
/// Atomic (temp-file + rename), 0644. Refuses to clobber an existing path unless
/// `overwrite`, and never replaces a directory with a file.
pub fn write_file(
    root: &Path,
    rel: &str,
    content: &str,
    overwrite: bool,
) -> Result<(), BrowseError> {
    let target = safe_resolve_parent(root, rel)?;
    if target.exists() {
        if !overwrite {
            return Err(BrowseError::Exists);
        }
        if target.is_dir() {
            return Err(BrowseError::NotAFile);
        }
    }
    let parent = target
        .parent()
        .ok_or_else(|| BrowseError::Io("target has no parent".into()))?;
    let tmp = parent.join(temp_name());
    std::fs::write(&tmp, content.as_bytes()).map_err(|e| BrowseError::Io(e.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644));
    }
    std::fs::rename(&tmp, &target).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        BrowseError::Io(e.to_string())
    })?;
    Ok(())
}

/// Delete `rel` under `root` — a **file** or an **empty directory** only. A
/// non-empty directory errors (never recursive); the root itself is refused.
/// The caller (handler) enforces the operator-confirm gate before this runs.
pub fn delete_file(root: &Path, rel: &str) -> Result<(), BrowseError> {
    let path = safe_resolve(root, rel)?;
    if path == root {
        return Err(BrowseError::PathEscape);
    }
    let md = std::fs::metadata(&path).map_err(|e| BrowseError::Io(e.to_string()))?;
    if md.is_dir() {
        let nonempty = std::fs::read_dir(&path)
            .map_err(|e| BrowseError::Io(e.to_string()))?
            .next()
            .is_some();
        if nonempty {
            return Err(BrowseError::NotEmpty);
        }
        std::fs::remove_dir(&path).map_err(|e| BrowseError::Io(e.to_string()))?;
    } else {
        std::fs::remove_file(&path).map_err(|e| BrowseError::Io(e.to_string()))?;
    }
    Ok(())
}

/// Rename `rel` → `new_rel`, both under `root`. The source must exist; the
/// target must **not** (never clobbers). The root itself can't be the source.
pub fn rename_path(root: &Path, rel: &str, new_rel: &str) -> Result<(), BrowseError> {
    let src = safe_resolve(root, rel)?;
    if src == root {
        return Err(BrowseError::PathEscape);
    }
    let dst = safe_resolve_parent(root, new_rel)?;
    if dst.exists() {
        return Err(BrowseError::Exists);
    }
    std::fs::rename(&src, &dst).map_err(|e| BrowseError::Io(e.to_string()))?;
    Ok(())
}

/// Create a directory at `rel` under `root`. Errors if the path already exists.
pub fn make_dir(root: &Path, rel: &str) -> Result<(), BrowseError> {
    let target = safe_resolve_parent(root, rel)?;
    if target.exists() {
        return Err(BrowseError::Exists);
    }
    std::fs::create_dir(&target).map_err(|e| BrowseError::Io(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A canonical scratch root with a small seeded tree.
    fn seeded_root() -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "aivyx-docbrowse-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("readme.txt"), b"hello world").unwrap();
        std::fs::write(dir.join("sub").join("nested.md"), b"# nested").unwrap();
        std::fs::write(dir.join("blob.bin"), [0u8, 1, 2, 3, 0]).unwrap();
        // Canonicalize — the primitive expects a canonical root.
        std::fs::canonicalize(&dir).unwrap()
    }

    #[test]
    fn list_dir_sorts_dirs_first_and_reports_kinds() {
        let root = seeded_root();
        let entries = list_dir(&root, "").unwrap();
        // "sub" (dir) sorts before the files.
        assert_eq!(entries[0].name, "sub");
        assert_eq!(entries[0].kind, "dir");
        let readme = entries.iter().find(|e| e.name == "readme.txt").unwrap();
        assert_eq!(readme.kind, "file");
        assert_eq!(readme.size_bytes, 11);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn list_dir_descends_into_subdir() {
        let root = seeded_root();
        let entries = list_dir(&root, "sub").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "nested.md");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn read_file_returns_text() {
        let root = seeded_root();
        let f = read_file(&root, "readme.txt").unwrap();
        assert_eq!(f.content.as_deref(), Some("hello world"));
        assert!(!f.binary);
        assert!(!f.truncated);
        assert_eq!(f.size_bytes, 11);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn read_file_flags_binary() {
        let root = seeded_root();
        let f = read_file(&root, "blob.bin").unwrap();
        assert!(f.binary);
        assert!(f.content.is_none(), "binary content is withheld");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn read_file_caps_and_marks_truncated() {
        let root = seeded_root();
        let big = "a".repeat(READ_CAP_BYTES + 5000);
        std::fs::write(root.join("big.txt"), &big).unwrap();
        let f = read_file(&root, "big.txt").unwrap();
        assert!(f.truncated);
        assert_eq!(f.content.as_ref().unwrap().len(), READ_CAP_BYTES);
        assert_eq!(f.size_bytes, (READ_CAP_BYTES + 5000) as u64);
        std::fs::remove_dir_all(&root).ok();
    }

    /// Audit P5: the Documents browser must refuse a FIFO, not block on it.
    #[test]
    fn audit_reading_a_fifo_does_not_hang_the_documents_browser() {
        let root = seeded_root();
        let fifo = root.join("notes.txt.fifo");
        assert!(std::process::Command::new("mkfifo").arg(&fifo).status().unwrap().success());
        let root2 = root.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(read_file(&root2, "notes.txt.fifo").is_err());
        });
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(3)),
            Ok(true),
            "read_file on a FIFO must refuse it, not block"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn dotdot_escape_is_rejected() {
        let root = seeded_root();
        // Climb above the root.
        let err = list_dir(&root, "../../../etc").unwrap_err();
        assert_eq!(err, BrowseError::PathEscape);
        // And via a file path.
        assert_eq!(read_file(&root, "../secret").unwrap_err(), BrowseError::PathEscape);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn list_on_a_file_and_read_on_a_dir_are_typed_errors() {
        let root = seeded_root();
        assert_eq!(list_dir(&root, "readme.txt").unwrap_err(), BrowseError::NotADir);
        assert_eq!(read_file(&root, "sub").unwrap_err(), BrowseError::NotAFile);
        assert_eq!(list_dir(&root, "nope").unwrap_err(), BrowseError::NotFound);
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_rejected() {
        let root = seeded_root();
        // A symlink inside the root pointing outside it.
        let outside = std::env::temp_dir().join(format!("aivyx-doc-outside-{}", std::process::id()));
        std::fs::write(&outside, b"secret").ok();
        let link = root.join("escape");
        let _ = std::os::unix::fs::symlink(&outside, &link);
        // Reading through the symlink canonicalizes outside the root → rejected.
        assert_eq!(read_file(&root, "escape").unwrap_err(), BrowseError::PathEscape);
        std::fs::remove_file(&outside).ok();
        std::fs::remove_dir_all(&root).ok();
    }

    // ---- Chapter DW — writes -------------------------------------------

    #[test]
    fn write_file_creates_then_refuses_clobber_then_overwrites() {
        let root = seeded_root();
        // Create a new file in a subdir.
        write_file(&root, "sub/new.txt", "v1", false).unwrap();
        assert_eq!(read_file(&root, "sub/new.txt").unwrap().content.as_deref(), Some("v1"));
        // Non-overwrite write to an existing path → Exists.
        assert_eq!(write_file(&root, "sub/new.txt", "v2", false).unwrap_err(), BrowseError::Exists);
        // Overwrite replaces it (atomically — no temp left behind).
        write_file(&root, "sub/new.txt", "v2", true).unwrap();
        assert_eq!(read_file(&root, "sub/new.txt").unwrap().content.as_deref(), Some("v2"));
        let stray: Vec<_> = std::fs::read_dir(root.join("sub"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".aivyx-tmp"))
            .collect();
        assert!(stray.is_empty(), "no temp file left after atomic write");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn write_escape_and_dir_target_are_rejected() {
        let root = seeded_root();
        assert_eq!(write_file(&root, "../evil.txt", "x", false).unwrap_err(), BrowseError::PathEscape);
        // Can't overwrite a directory with a file.
        assert_eq!(write_file(&root, "sub", "x", true).unwrap_err(), BrowseError::NotAFile);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn delete_file_and_empty_dir_but_not_nonempty_or_root() {
        let root = seeded_root();
        // File.
        delete_file(&root, "readme.txt").unwrap();
        assert_eq!(read_file(&root, "readme.txt").unwrap_err(), BrowseError::NotFound);
        // Non-empty dir refused.
        assert_eq!(delete_file(&root, "sub").unwrap_err(), BrowseError::NotEmpty);
        // Empty dir deletes.
        make_dir(&root, "emptydir").unwrap();
        delete_file(&root, "emptydir").unwrap();
        // Root refused; escape refused.
        assert_eq!(delete_file(&root, "").unwrap_err(), BrowseError::PathEscape);
        assert_eq!(delete_file(&root, "../../etc/hosts").unwrap_err(), BrowseError::PathEscape);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rename_moves_but_never_clobbers() {
        let root = seeded_root();
        rename_path(&root, "readme.txt", "renamed.md").unwrap();
        assert!(read_file(&root, "renamed.md").is_ok());
        assert_eq!(read_file(&root, "readme.txt").unwrap_err(), BrowseError::NotFound);
        // Clobber refused (sub/nested.md already exists).
        write_file(&root, "other.txt", "x", false).unwrap();
        assert_eq!(rename_path(&root, "other.txt", "renamed.md").unwrap_err(), BrowseError::Exists);
        // Escape refused.
        assert_eq!(rename_path(&root, "renamed.md", "../out.md").unwrap_err(), BrowseError::PathEscape);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn make_dir_creates_and_refuses_existing() {
        let root = seeded_root();
        make_dir(&root, "newdir").unwrap();
        assert!(list_dir(&root, "newdir").is_ok());
        assert_eq!(make_dir(&root, "newdir").unwrap_err(), BrowseError::Exists);
        assert_eq!(make_dir(&root, "sub").unwrap_err(), BrowseError::Exists);
        std::fs::remove_dir_all(&root).ok();
    }
}
