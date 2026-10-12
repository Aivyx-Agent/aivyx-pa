//! Where the channel left off: the last INBOX UID it handled, so a
//! restart neither repeats nor misses mail. `<proton_dir>/state.json`.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::transport::InboxStatus;

pub const STATE_FILE: &str = "state.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub uid_validity: u32,
    pub last_uid: u32,
}

/// The saved cursor, if it is still valid for this INBOX; otherwise "now"
/// (the next UID), so existing mail is never processed. A changed
/// UIDVALIDITY means old UIDs mean nothing any more.
pub fn resume(saved: Option<Cursor>, status: InboxStatus) -> Cursor {
    match saved {
        Some(c) if c.uid_validity == status.uid_validity => c,
        _ => Cursor {
            uid_validity: status.uid_validity,
            last_uid: status.uid_next.saturating_sub(1),
        },
    }
}

pub fn load(dir: &Path) -> Result<Option<Cursor>, String> {
    let path = dir.join(STATE_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| format!("read {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

/// Written to a temporary file and renamed, so a crash never leaves half a
/// cursor. `dir` already exists (the channel lock creates it).
pub fn save(dir: &Path, cursor: &Cursor) -> Result<(), String> {
    let path = dir.join(STATE_FILE);
    let tmp = dir.join(format!("{STATE_FILE}.tmp"));
    let bytes = serde_json::to_vec(cursor).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(v: u32, next: u32) -> InboxStatus {
        InboxStatus { uid_validity: v, uid_next: next }
    }

    #[test]
    fn first_run_starts_at_now() {
        assert_eq!(resume(None, status(7, 42)), Cursor { uid_validity: 7, last_uid: 41 });
        assert_eq!(resume(None, status(7, 0)).last_uid, 0);
    }

    #[test]
    fn a_saved_cursor_resumes_unless_uidvalidity_changed() {
        let saved = Cursor { uid_validity: 7, last_uid: 12 };
        assert_eq!(resume(Some(saved), status(7, 42)), saved);
        assert_eq!(resume(Some(saved), status(8, 42)), Cursor { uid_validity: 8, last_uid: 41 });
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()).unwrap(), None);
        let c = Cursor { uid_validity: 3, last_uid: 9 };
        save(dir.path(), &c).unwrap();
        assert_eq!(load(dir.path()).unwrap(), Some(c));
    }
}
