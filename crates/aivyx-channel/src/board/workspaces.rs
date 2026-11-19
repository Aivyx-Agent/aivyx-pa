//! A22 — coder workspaces: name and path rules.

use std::path::{Path, PathBuf};

use super::model::BoardError;

pub fn validate_name(name: &str) -> Result<(), BoardError> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(BoardError::NotAllowed(
            "a workspace name is 1-64 letters, digits, - or _",
        ))
    }
}

/// The canonical form of `path`, or why it can't be a workspace. `home` is the
/// operator's home folder; `protected` are pa's own data and config folders.
pub fn validate_path(
    path: &Path,
    home: Option<&Path>,
    protected: &[PathBuf],
) -> Result<PathBuf, BoardError> {
    if !path.is_absolute() {
        return Err(BoardError::NotAllowed(
            "the workspace path must be absolute",
        ));
    }
    let canon = std::fs::canonicalize(path)
        .map_err(|_| BoardError::NotAllowed("the workspace path doesn't exist"))?;
    if !canon.is_dir() {
        return Err(BoardError::NotAllowed("the workspace path isn't a folder"));
    }
    if canon.parent().is_none() {
        return Err(BoardError::NotAllowed(
            "the filesystem root can't be a workspace",
        ));
    }
    if let Some(home) = home.and_then(|h| std::fs::canonicalize(h).ok())
        && canon == home
    {
        return Err(BoardError::NotAllowed(
            "your home folder itself can't be a workspace; pick a project folder inside it",
        ));
    }
    for p in protected {
        if let Ok(p) = std::fs::canonicalize(p)
            && (canon.starts_with(&p) || p.starts_with(&canon))
        {
            return Err(BoardError::NotAllowed(
                "a workspace can't contain or sit inside aivyx-pa's own folders",
            ));
        }
    }
    Ok(canon)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(validate_name("my-proj_1").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name("a b").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name(&"a".repeat(64)).is_ok());
        assert!(validate_name(&"a".repeat(65)).is_err());
    }

    fn msg(r: Result<PathBuf, BoardError>) -> String {
        match r {
            Err(BoardError::NotAllowed(m)) => m.to_string(),
            other => panic!("expected NotAllowed, got {other:?}"),
        }
    }

    #[test]
    fn a_good_folder_is_canonical() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("proj");
        std::fs::create_dir(&p).unwrap();
        assert_eq!(
            validate_path(&p, None, &[]).unwrap(),
            std::fs::canonicalize(&p).unwrap()
        );
    }

    #[test]
    fn bad_paths_are_refused() {
        let d = tempfile::tempdir().unwrap();
        assert!(msg(validate_path(Path::new("rel/x"), None, &[])).contains("absolute"));
        assert!(msg(validate_path(&d.path().join("nope"), None, &[])).contains("exist"));
        let f = d.path().join("f");
        std::fs::write(&f, "x").unwrap();
        assert!(msg(validate_path(&f, None, &[])).contains("folder"));
        assert!(msg(validate_path(Path::new("/"), None, &[])).contains("root"));
    }

    #[test]
    fn home_itself_is_refused_but_a_child_is_fine() {
        let d = tempfile::tempdir().unwrap();
        assert!(msg(validate_path(d.path(), Some(d.path()), &[])).contains("home"));
        let c = d.path().join("c");
        std::fs::create_dir(&c).unwrap();
        assert!(validate_path(&c, Some(d.path()), &[]).is_ok());
    }

    #[test]
    fn protected_folders_are_refused_both_ways() {
        let d = tempfile::tempdir().unwrap();
        let data = d.path().join("data");
        let inside = data.join("sub");
        std::fs::create_dir_all(&inside).unwrap();
        let prot = [data.clone()];
        assert!(msg(validate_path(&inside, None, &prot)).contains("own folders"));
        assert!(msg(validate_path(&data, None, &prot)).contains("own folders"));
        // d contains data
        assert!(msg(validate_path(d.path(), None, &prot)).contains("own folders"));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_canonicalizes_to_its_target() {
        let d = tempfile::tempdir().unwrap();
        let real = d.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = d.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(
            validate_path(&link, None, &[]).unwrap(),
            std::fs::canonicalize(&real).unwrap()
        );
    }
}
