//! One process at a time on the Matrix store.
//!
//! `matrix-sdk`'s crypto store holds Olm/Megolm session state; two
//! processes writing it at once can desync Olm sessions. Every process that
//! opens the store (`--channel matrix`, `matrix verify`, `connect matrix`)
//! first takes an exclusive advisory lock on `<matrix_dir>/lock` and holds
//! it for as long as the store is open. The daemon never opens the store.

use std::fs::File;
use std::path::Path;

pub const LOCK_FILE: &str = "lock";

/// Shown when another process holds the lock.
pub const BUSY: &str = "the Matrix channel is running in another process \
    (`aivyx-pa --channel matrix`) — stop it first";

/// Held while the Matrix store is open; the lock is released on drop
/// (closing the file releases a `flock`).
#[derive(Debug)]
pub struct StoreLock {
    _file: File,
}

/// Takes the exclusive lock on `<dir>/lock`, creating `dir` (mode 0700) and
/// the lock file if needed. Fails at once with [`BUSY`] if another holder
/// exists.
pub fn acquire(dir: &Path) -> Result<StoreLock, String> {
    {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let path = dir.join(LOCK_FILE);
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path).map_err(|e| format!("open {}: {e}", path.display()))?;
    try_lock_exclusive(&file).map_err(|e| match e {
        LockError::Busy => BUSY.to_string(),
        LockError::Io(e) => format!("lock {}: {e}", path.display()),
    })?;
    Ok(StoreLock { _file: file })
}

enum LockError {
    Busy,
    Io(std::io::Error),
}

#[cfg(unix)]
fn try_lock_exclusive(file: &File) -> Result<(), LockError> {
    use std::os::unix::io::AsRawFd;
    // SAFETY: `flock` on a file descriptor we own for the duration of the
    // call; no memory is passed.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.kind() == std::io::ErrorKind::WouldBlock {
        Err(LockError::Busy)
    } else {
        Err(LockError::Io(err))
    }
}

/// Non-Unix: no advisory lock (the workspace MSRV predates `File::try_lock`).
#[cfg(not(unix))]
fn try_lock_exclusive(_file: &File) -> Result<(), LockError> {
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("aivyx-matrix-lock-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_second_holder_is_refused_with_a_clear_message() {
        let d = scratch("busy");
        let first = acquire(&d).unwrap();
        let err = acquire(&d).unwrap_err();
        assert_eq!(err, BUSY);
        assert!(err.contains("aivyx-pa --channel matrix"));
        drop(first);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_lock_is_released_on_drop() {
        let d = scratch("release");
        drop(acquire(&d).unwrap());
        let again = acquire(&d);
        assert!(again.is_ok(), "{again:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_dir_is_created_private() {
        use std::os::unix::fs::PermissionsExt;
        let d = scratch("mode");
        let _l = acquire(&d).unwrap();
        assert_eq!(std::fs::metadata(&d).unwrap().permissions().mode() & 0o777, 0o700);
        let _ = std::fs::remove_dir_all(&d);
    }
}
