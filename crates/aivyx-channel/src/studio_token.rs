//! First-run coherence A2 — the Studio's automatic sign-in token.
//!
//! The Studio is on by default, bound to loopback. Loopback TCP is still
//! reachable by every local user account, so "on by default" must never
//! mean "unauthenticated": when the operator has set neither
//! `[daemon] web_ui_auth_token` nor `web_ui_insecure_no_auth`, the daemon
//! uses a token of its own, kept in a `studio-token` file next to the
//! store (the parent directory of the effective `storage_path`).
//!
//! - **Shape:** 43 alphanumeric characters (256 bits), the same shape the
//!   Harbor appliance entrypoint generates (`tr -dc 'a-zA-Z0-9' <
//!   /dev/urandom | head -c 43`), so it is cookie- and URL-safe.
//! - **Writing:** atomically (a `0600` temp file hard-linked into place, so
//!   a concurrent start can never see a half-written file or clobber a
//!   token another process already published), reused on later starts.
//! - **Damage is an error:** a present but unreadable, empty or corrupt
//!   file is reported, never silently replaced — replacing it would sign
//!   every open Studio tab out without saying why.
//! - **Ward:** the CLI adds [`token_path`] to Ward's extra-deny list, so the
//!   agent's own file and shell tools can't read it.

use std::net::IpAddr;
use std::path::{Path, PathBuf};

/// File name of the automatic token, next to the store.
pub const TOKEN_FILE_NAME: &str = "studio-token";

/// Length of a generated token: 43 characters from a 62-letter alphabet is
/// 43 × log2(62) ≈ 256.03 bits.
pub const TOKEN_LEN: usize = 43;

/// Where the automatic token lives for a given `storage_path`: the
/// `studio-token` file in the store's parent directory.
pub fn token_path(storage_path: &Path) -> PathBuf {
    let dir = storage_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    dir.join(TOKEN_FILE_NAME)
}

/// True when `s` has exactly the shape this module generates.
fn is_valid_token(s: &str) -> bool {
    s.len() == TOKEN_LEN && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// A fresh token: [`TOKEN_LEN`] characters drawn uniformly from
/// `[A-Za-z0-9]` by the OS CSPRNG (`rand`'s `Alphanumeric` rejection-samples,
/// so there is no modulo bias).
fn generate() -> String {
    use rand::Rng;
    rand::rngs::OsRng
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(TOKEN_LEN)
        .map(char::from)
        .collect()
}

/// Read and validate an existing token file. `Ok(None)` when it doesn't
/// exist; `Err` when it exists but can't be read or doesn't hold a token.
fn read_checked(path: &Path) -> Result<Option<String>, String> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(format!(
                "the Studio token file {} exists but can't be read: {e}. \
                 Fix its permissions, or delete it to have a new token \
                 generated (open Studio tabs will need the new sign-in link).",
                path.display()
            ));
        }
    };
    let text = String::from_utf8(bytes).unwrap_or_default();
    let token = text.trim();
    if !is_valid_token(token) {
        return Err(format!(
            "the Studio token file {} is empty or damaged (expected {TOKEN_LEN} \
             letters and digits). Delete it to have a new token generated \
             (open Studio tabs will need the new sign-in link), or set \
             `[daemon] web_ui_auth_token` in aivyx-pa.toml.",
            path.display()
        ));
    }
    Ok(Some(token.to_string()))
}

/// The token in `path`, when the file exists and holds a valid token.
/// Never creates anything — for readers (`doctor`, the REPL banner, the
/// desktop app) running as the same user as the daemon.
pub fn read_existing(path: &Path) -> Option<String> {
    read_checked(path).ok().flatten()
}

/// The token in `path`, creating the file (atomically, mode `0600`) when
/// it doesn't exist yet. A present but unreadable, empty or damaged file
/// is an `Err` with an operator-facing message — never replaced.
pub fn load_or_create(path: &Path) -> Result<String, String> {
    if let Some(token) = read_checked(path)? {
        tighten_mode(path);
        return Ok(token);
    }
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("failed to create {}: {e}", dir.display()))?;
    let token = generate();
    let tmp = dir.join(format!(
        ".{TOKEN_FILE_NAME}.{}.{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let written = write_new_0600(&tmp, token.as_bytes());
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("failed to write {}: {e}", tmp.display()));
    }
    // Publish with create-if-absent semantics: a hard link fails with
    // `AlreadyExists` rather than replacing a token another starting
    // process published first, in which case that one is the token.
    let published = std::fs::hard_link(&tmp, path);
    let result = match published {
        Ok(()) => Ok(token),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            match read_checked(path)? {
                Some(existing) => Ok(existing),
                None => Err(format!("{} vanished while it was created", path.display())),
            }
        }
        // A filesystem without hard links: fall back to a rename, still
        // atomic (the file appears whole or not at all).
        Err(_) => std::fs::rename(&tmp, path)
            .map(|()| token)
            .map_err(|e| format!("failed to write {}: {e}", path.display())),
    };
    let _ = std::fs::remove_file(&tmp);
    result
}

/// Create `path` (which must not exist) with mode `0600` from the start,
/// write `bytes`, and flush them to disk.
fn write_new_0600(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// A reused token file that is readable by other accounts is pulled back
/// to `0600`, with a warning — the token may already have been read, and
/// deleting the file rotates it.
fn tighten_mode(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Ok(meta) = std::fs::metadata(path) else {
            return;
        };
        if meta.permissions().mode() & 0o077 != 0 {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
            eprintln!(
                "aivyx-pa: the Studio token file {} was readable by other \
                 accounts; it is now 0600. Delete it to rotate the token.",
                path.display()
            );
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// The token the daemon protects the Studio with:
///
/// - the operator's `[daemon] web_ui_auth_token`, when set (no file is
///   created);
/// - none, when `web_ui_insecure_no_auth` is set;
/// - otherwise the automatic token at `path` ([`load_or_create`]).
pub fn effective_token(
    configured: Option<String>,
    insecure_no_auth: bool,
    path: &Path,
) -> Result<Option<String>, String> {
    if let Some(token) = configured {
        return Ok(Some(token));
    }
    if insecure_no_auth {
        return Ok(None);
    }
    load_or_create(path).map(Some)
}

/// The one-time sign-in link: `http://<host>:<port>/?token=<token>`. A
/// wildcard bind (`0.0.0.0` / `::`) is shown as loopback, since that's
/// the address that works from this machine.
pub fn sign_in_url(host: IpAddr, port: u16, token: &str) -> String {
    format!("{}?token={token}", studio_url(host, port))
}

/// The `daemon run` banner line for a Studio bound at `addr`: the sign-in
/// link when it has a token, the bare URL when it has none
/// (`web_ui_insecure_no_auth`).
pub fn banner_line(addr: std::net::SocketAddr, token: Option<&str>) -> String {
    match token {
        Some(t) => format!("Studio: {}", sign_in_url(addr.ip(), addr.port(), t)),
        None => format!("Studio: {}", studio_url(addr.ip(), addr.port())),
    }
}

/// The Studio's base URL, `http://<host>:<port>/`, with the same wildcard
/// handling as [`sign_in_url`].
pub fn studio_url(host: IpAddr, port: u16) -> String {
    let host = match host {
        IpAddr::V4(v4) if v4.is_unspecified() => IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        IpAddr::V6(v6) if v6.is_unspecified() => IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
        other => other,
    };
    match host {
        IpAddr::V4(v4) => format!("http://{v4}:{port}/"),
        IpAddr::V6(v6) => format!("http://[{v6}]:{port}/"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A per-test directory, removed again on drop (even after a panic).
    struct Scratch(PathBuf);

    impl std::ops::Deref for Scratch {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(tag: &str) -> Scratch {
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!(
            "aivyx-studio-token-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed),
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Scratch(d)
    }

    #[test]
    fn token_path_sits_next_to_the_store() {
        assert_eq!(
            token_path(Path::new("/home/u/.aivyx-pa/store.redb")),
            PathBuf::from("/home/u/.aivyx-pa/studio-token")
        );
        assert_eq!(token_path(Path::new("store.redb")), PathBuf::from("./studio-token"));
    }

    #[test]
    fn create_gives_43_alphanumerics_in_a_0600_file() {
        let dir = scratch("create");
        let path = dir.join("studio-token");
        let token = load_or_create(&path).expect("create");
        assert_eq!(token.len(), 43);
        assert!(token.bytes().all(|b| b.is_ascii_alphanumeric()), "{token}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), token);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "mode was {mode:o}");
        }
        // No temp files are left behind.
        let names: Vec<_> = std::fs::read_dir(&*dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("studio-token")]);
        // Two fresh tokens differ.
        let dir2 = scratch("create2");
        let other = load_or_create(&dir2.join("studio-token")).unwrap();
        assert_ne!(token, other);
    }

    #[test]
    fn a_second_call_returns_the_same_token() {
        let dir = scratch("reuse");
        let path = dir.join("studio-token");
        let first = load_or_create(&path).unwrap();
        let second = load_or_create(&path).unwrap();
        assert_eq!(first, second);
        assert_eq!(read_existing(&path), Some(first));
    }

    #[test]
    fn creates_a_missing_parent_directory() {
        let dir = scratch("parent");
        let path = dir.join("nested").join("studio-token");
        assert!(load_or_create(&path).is_ok());
        assert!(path.exists());
    }

    #[test]
    fn a_corrupt_or_empty_file_is_an_error_and_is_left_alone() {
        for (tag, body) in [
            ("empty", &b""[..]),
            ("short", &b"abc"[..]),
            ("bad-chars", &b"!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!"[..]),
            ("binary", &[0xff, 0xfe, 0x00][..]),
        ] {
            let dir = scratch(tag);
            let path = dir.join("studio-token");
            std::fs::write(&path, body).unwrap();
            let err = load_or_create(&path).expect_err(tag);
            assert!(err.contains("empty or damaged"), "{tag}: {err}");
            assert_eq!(std::fs::read(&path).unwrap(), body, "{tag}: file replaced");
            assert_eq!(read_existing(&path), None, "{tag}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_file_is_an_error() {
        // A directory where the file should be can't be read as a file,
        // even by root (unlike a chmod-000 file).
        let dir = scratch("unreadable");
        let path = dir.join("studio-token");
        std::fs::create_dir(&path).unwrap();
        let err = load_or_create(&path).expect_err("unreadable");
        assert!(err.contains("can't be read"), "{err}");
        assert!(path.is_dir(), "must not be replaced");
    }

    #[test]
    fn surrounding_whitespace_is_tolerated() {
        let dir = scratch("ws");
        let path = dir.join("studio-token");
        let token = "A".repeat(43);
        std::fs::write(&path, format!("{token}\n")).unwrap();
        assert_eq!(load_or_create(&path).unwrap(), token);
    }

    #[cfg(unix)]
    #[test]
    fn a_reused_world_readable_file_is_tightened() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("tighten");
        let path = dir.join("studio-token");
        let token = load_or_create(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(load_or_create(&path).unwrap(), token);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn read_existing_on_a_missing_file_is_none() {
        let dir = scratch("missing");
        let path = dir.join("studio-token");
        assert_eq!(read_existing(&path), None);
        assert!(!path.exists(), "read_existing must never create the file");
    }

    #[test]
    fn sign_in_url_shape() {
        assert_eq!(
            sign_in_url(IpAddr::V4(Ipv4Addr::LOCALHOST), 7843, "abc"),
            "http://127.0.0.1:7843/?token=abc"
        );
        assert_eq!(
            sign_in_url(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 9000, "abc"),
            "http://127.0.0.1:9000/?token=abc"
        );
        assert_eq!(
            sign_in_url(IpAddr::V6(Ipv6Addr::LOCALHOST), 7843, "abc"),
            "http://[::1]:7843/?token=abc"
        );
    }

    #[test]
    fn banner_line_shows_the_sign_in_link() {
        let addr = std::net::SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7843);
        assert_eq!(
            banner_line(addr, Some("abc")),
            "Studio: http://127.0.0.1:7843/?token=abc"
        );
        assert_eq!(banner_line(addr, None), "Studio: http://127.0.0.1:7843/");
    }

    #[test]
    fn a_configured_token_wins_and_no_file_is_created() {
        let dir = scratch("configured");
        let path = dir.join("studio-token");
        let got = effective_token(Some("operator-tok".into()), false, &path).unwrap();
        assert_eq!(got.as_deref(), Some("operator-tok"));
        // Even with the escape hatch set, an operator token still applies.
        let got = effective_token(Some("operator-tok".into()), true, &path).unwrap();
        assert_eq!(got.as_deref(), Some("operator-tok"));
        assert!(!path.exists());
    }

    #[test]
    fn insecure_no_auth_means_no_token_and_no_file() {
        let dir = scratch("insecure");
        let path = dir.join("studio-token");
        assert_eq!(effective_token(None, true, &path).unwrap(), None);
        assert!(!path.exists());
    }

    #[test]
    fn otherwise_the_automatic_token_is_used() {
        let dir = scratch("auto");
        let path = dir.join("studio-token");
        let got = effective_token(None, false, &path).unwrap().expect("a token");
        assert_eq!(read_existing(&path), Some(got.clone()));
        assert_eq!(effective_token(None, false, &path).unwrap(), Some(got));
        // A damaged file surfaces as an error, not a fresh token.
        std::fs::write(&path, "x").unwrap();
        assert!(effective_token(None, false, &path).is_err());
    }
}
