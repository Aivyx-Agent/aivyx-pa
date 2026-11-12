//! `aivyx-pa tool-programs [list]` and `aivyx-pa tool-programs install [--dir <path>]`: the
//! tool programs (Gmail, Calendar, Drive, Contacts, Notion, Obsidian, n8n,
//! the Toolkit) a `[[tool_process]]` entry runs.
//!
//! The one-line installer ships only `aivyx-pa`; each release also carries
//! `aivyx-pa-tools-<target>.tar.gz` and its `.sha256` (tools-release.yml).
//! `install` downloads the archive matching this binary's version and
//! platform, checks it against the `.sha256`, and puts the programs beside
//! `aivyx-pa`, where `aivyx-pa connect` and the daemon find them
//! (pre-launch audit 2026-11-13, S2). The `.deb` ships them already; the
//! Docker image builds them in.

use std::io::Write as IoWrite;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::connect::FIRST_PARTY_TOOLS;

/// Where release assets are downloaded from.
pub const RELEASES: &str = "https://github.com/Aivyx-Agent/aivyx-pa/releases/download";

/// The release target this binary was built for, as the archives name it.
/// `None` on a platform the release doesn't build tools for.
pub fn release_target() -> Option<&'static str> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("x86_64-unknown-linux-musl")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some("aarch64-unknown-linux-musl")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("aarch64-apple-darwin")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("x86_64-apple-darwin")
    } else {
        None
    }
}

/// `aivyx-pa-tools-<target>`: the archive's stem and its one top folder.
fn stem(target: &str) -> String {
    format!("aivyx-pa-tools-{target}")
}

/// The archive's URL for `version` (no leading `v`).
pub fn archive_url(base: &str, version: &str, target: &str) -> String {
    format!("{}/v{version}/{}.tar.gz", base.trim_end_matches('/'), stem(target))
}

/// The hash in a `sha256sum` line (`<64 hex>  <file>`), lowercased.
pub fn parse_sha256(text: &str) -> Option<String> {
    let hash = text.split_whitespace().next()?;
    (hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| hash.to_ascii_lowercase())
}

/// Whether `bytes` hash to `expected` (lowercase hex).
pub fn verify(bytes: &[u8], expected: &str) -> Result<(), String> {
    let got = hex::encode(Sha256::digest(bytes));
    if got == expected {
        Ok(())
    } else {
        Err(format!(
            "the download doesn't match its checksum (expected {expected}, got {got}); \
             nothing was installed"
        ))
    }
}

/// Unpack the tool programs from a verified `.tar.gz` into `dest`. Only
/// regular files named after a known tool, directly inside the archive's
/// `aivyx-pa-tools-<target>/` folder, are written; any other entry (a
/// link, a path that climbs out, an unknown name) refuses the whole
/// archive before anything is written. Each program is written to a
/// temporary name and renamed into place, executable. Returns the names
/// installed.
pub fn extract(bytes: &[u8], dest: &Path, target: &str) -> Result<Vec<String>, String> {
    let top = stem(target);
    let read = |what: &str, e: std::io::Error| format!("can't read the archive ({what}): {e}");

    // Pass 1: check every entry; collect the programs' contents.
    let mut programs: Vec<(String, Vec<u8>)> = Vec::new();
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    for entry in archive.entries().map_err(|e| read("entries", e))? {
        let mut entry = entry.map_err(|e| read("entry", e))?;
        let path = entry.path().map_err(|e| read("path", e))?.into_owned();
        let parts: Vec<Component> = path.components().collect();
        let in_top = matches!(parts.first(), Some(Component::Normal(p)) if *p == top.as_str());
        let kind = entry.header().entry_type();
        if kind.is_dir() && in_top && parts.len() == 1 {
            continue;
        }
        let name = match (kind.is_file(), in_top, parts.as_slice()) {
            (true, true, [_, Component::Normal(name)]) => name.to_str().unwrap_or_default(),
            _ => "",
        };
        if !FIRST_PARTY_TOOLS.contains(&name) {
            return Err(format!(
                "the archive has an unexpected entry `{}`; nothing was installed",
                path.display()
            ));
        }
        let mut contents = Vec::new();
        std::io::copy(&mut entry, &mut contents).map_err(|e| read(name, e))?;
        programs.push((name.to_string(), contents));
    }
    if programs.is_empty() {
        return Err("the archive holds no tool programs; nothing was installed".into());
    }

    // Pass 2: write them.
    let mut installed = Vec::new();
    for (name, contents) in programs {
        let part = dest.join(format!(".{name}.part"));
        let target_path = dest.join(&name);
        std::fs::write(&part, &contents)
            .map_err(|e| format!("can't write {}: {e}", part.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("can't make {} executable: {e}", part.display()))?;
        }
        std::fs::rename(&part, &target_path)
            .map_err(|e| format!("can't put {} in place: {e}", target_path.display()))?;
        installed.push(name);
    }
    installed.sort();
    Ok(installed)
}

/// The folder holding the running `aivyx-pa`: where the tools go by default.
pub fn exe_dir() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("can't find aivyx-pa itself: {e}"))?;
    let exe = exe.canonicalize().unwrap_or(exe);
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "can't find aivyx-pa's folder".to_string())
}

/// The program a `[[tool_process]] command` runs: a bare name found beside
/// `aivyx-pa` (`exe_dir`) becomes that path, since `tool-programs install`
/// puts the programs there and a background service's `PATH` (systemd's
/// default, a macOS app's) may not include that folder. Anything else —
/// a path, or a name not found there — is left for `PATH` as before.
pub fn resolve_command(command: &str, exe_dir: Option<&Path>) -> String {
    if command.contains('/') {
        return command.to_string();
    }
    exe_dir
        .map(|dir| dir.join(command))
        .filter(|p| p.is_file())
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| command.to_string())
}

/// Whether we can create files in `dir` (a system folder such as /usr/bin
/// usually isn't, for an ordinary user).
fn writable(dir: &Path) -> bool {
    let probe = dir.join(".aivyx-pa-tools-probe");
    let ok = std::fs::File::create(&probe).is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Download, check and unpack the tool programs for `version` into `dest`.
pub async fn install(
    base: &str,
    version: &str,
    target: &str,
    dest: &Path,
    writer: &mut dyn IoWrite,
) -> Result<Vec<String>, String> {
    if !writable(dest) {
        return Err(format!(
            "can't write to {} (aivyx-pa's folder). Choose another folder on your PATH with \
             `aivyx-pa tool-programs install --dir <folder>`, or use the .deb or the Docker image, \
             which include the tools",
            dest.display()
        ));
    }
    let url = archive_url(base, version, target);
    let w = |writer: &mut dyn IoWrite, s: &str| writeln!(writer, "{s}").map_err(|e| format!("write error: {e}"));
    w(writer, &format!("Downloading the tool programs for aivyx-pa {version} ({target})…"))?;
    let client = reqwest::Client::builder()
        .user_agent(concat!("aivyx-pa/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("can't set up the download: {e}"))?;
    let fetch = |url: String| {
        let client = client.clone();
        async move {
            let response = client
                .get(&url)
                .send()
                .await
                .map_err(|e| format!("can't download {url}: {e}"))?;
            if !response.status().is_success() {
                return Err(format!(
                    "can't download {url}: the server answered {} (this release may not have \
                     the tool programs)",
                    response.status()
                ));
            }
            response
                .bytes()
                .await
                .map_err(|e| format!("can't download {url}: {e}"))
        }
    };
    let archive = fetch(url.clone()).await?;
    let sums = fetch(format!("{url}.sha256")).await?;
    let expected = parse_sha256(&String::from_utf8_lossy(&sums))
        .ok_or_else(|| format!("{url}.sha256 holds no checksum; nothing was installed"))?;
    verify(&archive, &expected)?;
    let installed = extract(&archive, dest, target)?;
    w(writer, &format!("Installed in {}: {}", dest.display(), installed.join(", ")))?;
    Ok(installed)
}

/// Each tool program and where it was found (beside `aivyx-pa` first, then
/// on `PATH`), for `aivyx-pa tool-programs list`.
pub fn status(exe_dir: Option<&Path>) -> Vec<(&'static str, Option<PathBuf>)> {
    FIRST_PARTY_TOOLS
        .iter()
        .map(|name| (*name, crate::connect::find_tool_program(name, exe_dir)))
        .collect()
}

/// `aivyx-pa tool-programs [list]` / `aivyx-pa tool-programs install [--dir <folder>]`.
pub async fn run(args: &[String]) -> Result<(), String> {
    let mut out = std::io::stdout();
    match args.first().map(String::as_str) {
        None | Some("list") => {
            let here = exe_dir().ok();
            let found = status(here.as_deref());
            for (name, at) in &found {
                match at {
                    Some(p) => println!("  {name:<16} {}", p.display()),
                    None => println!("  {name:<16} not installed"),
                }
            }
            if found.iter().any(|(_, at)| at.is_none()) {
                println!("\n`aivyx-pa tool-programs install` installs the missing ones.");
            }
            Ok(())
        }
        Some("install") => {
            let dest = match args.get(1).map(String::as_str) {
                None => exe_dir()?,
                Some("--dir") => PathBuf::from(
                    args.get(2).ok_or("`--dir` needs a folder")?,
                ),
                Some(other) => {
                    return Err(format!("unknown `aivyx-pa tool-programs install` argument: `{other}`"));
                }
            };
            let target = release_target().ok_or(
                "there are no prebuilt tool programs for this platform; build them from \
                 source (see docs/INSTALL.md, \"The tool programs\")",
            )?;
            install(RELEASES, env!("CARGO_PKG_VERSION"), target, &dest, &mut out).await?;
            println!(
                "\nNext: `aivyx-pa connect gmail` (or calendar, drive, contacts) signs in to \
                 Google; the others are set up in aivyx-pa.toml (docs/INSTALL.md)."
            );
            Ok(())
        }
        Some(other) => Err(format!(
            "unknown `aivyx-pa tool-programs` subcommand: `{other}`. Use `aivyx-pa tool-programs list` or \
             `aivyx-pa tool-programs install`."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = "x86_64-unknown-linux-musl";

    /// A gzipped tar with the given (path, contents, is_symlink) entries.
    fn archive(entries: &[(&str, &[u8], bool)]) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut b = tar::Builder::new(gz);
        for (path, contents, link) in entries {
            let mut h = tar::Header::new_gnu();
            if *link {
                h.set_entry_type(tar::EntryType::Symlink);
                h.set_size(0);
                h.set_mode(0o777);
                b.append_link(&mut h, path, "/etc/passwd").unwrap();
            } else {
                h.set_size(contents.len() as u64);
                h.set_mode(0o755);
                h.set_cksum();
                // `append_data` refuses `..`; write the raw name to test it.
                h.as_gnu_mut().unwrap().name[..path.len()].copy_from_slice(path.as_bytes());
                h.set_cksum();
                b.append(&h, *contents).unwrap();
            }
        }
        b.into_inner().unwrap().finish().unwrap()
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aivyx-tools-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_bare_tool_name_beside_aivyx_pa_runs_from_there() {
        let dir = tmp("resolve");
        std::fs::write(dir.join("aivyx-gmail"), b"x").unwrap();
        let got = resolve_command("aivyx-gmail", Some(&dir));
        assert_eq!(got, dir.join("aivyx-gmail").display().to_string());
        // Not there, a path, or no folder: unchanged (PATH decides).
        assert_eq!(resolve_command("aivyx-notion", Some(&dir)), "aivyx-notion");
        assert_eq!(resolve_command("/opt/x/aivyx-gmail", Some(&dir)), "/opt/x/aivyx-gmail");
        assert_eq!(resolve_command("aivyx-gmail", None), "aivyx-gmail");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_url_and_checksum_line_are_read_as_published() {
        assert_eq!(
            archive_url(RELEASES, "0.21.3", T),
            "https://github.com/Aivyx-Agent/aivyx-pa/releases/download/v0.21.3/\
             aivyx-pa-tools-x86_64-unknown-linux-musl.tar.gz"
        );
        let h = "A".repeat(64);
        assert_eq!(parse_sha256(&format!("{h}  aivyx-pa-tools.tar.gz\n")), Some("a".repeat(64)));
        assert_eq!(parse_sha256("nothing"), None);
        assert!(verify(b"x", &hex::encode(Sha256::digest(b"x"))).is_ok());
        assert!(verify(b"x", &"0".repeat(64)).unwrap_err().contains("nothing was installed"));
    }

    #[test]
    fn known_programs_in_the_top_folder_are_installed_executable() {
        let dest = tmp("good");
        let bytes = archive(&[
            ("aivyx-pa-tools-x86_64-unknown-linux-musl/aivyx-gmail", b"gmail", false),
            ("aivyx-pa-tools-x86_64-unknown-linux-musl/aivyx-toolkit", b"toolkit", false),
        ]);
        let got = extract(&bytes, &dest, T).unwrap();
        assert_eq!(got, vec!["aivyx-gmail", "aivyx-toolkit"]);
        assert_eq!(std::fs::read(dest.join("aivyx-gmail")).unwrap(), b"gmail");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dest.join("aivyx-toolkit")).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111);
        }
        std::fs::remove_dir_all(&dest).unwrap();
    }

    #[test]
    fn anything_else_in_the_archive_refuses_it_before_writing() {
        for bad in [
            archive(&[("aivyx-pa-tools-x86_64-unknown-linux-musl/../aivyx-gmail", b"x", false)]),
            archive(&[("aivyx-pa-tools-x86_64-unknown-linux-musl/sh", b"x", false)]),
            archive(&[("aivyx-gmail", b"x", false)]),
            archive(&[("aivyx-pa-tools-x86_64-unknown-linux-musl/aivyx-gmail", b"", true)]),
            archive(&[
                ("aivyx-pa-tools-x86_64-unknown-linux-musl/aivyx-gmail", b"ok", false),
                ("aivyx-pa-tools-x86_64-unknown-linux-musl/evil", b"x", false),
            ]),
        ] {
            let dest = tmp("bad");
            let err = extract(&bad, &dest, T).unwrap_err();
            assert!(err.contains("nothing was installed"), "{err}");
            assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 0, "nothing written");
            std::fs::remove_dir_all(&dest).unwrap();
        }
    }

    /// An archive made the way tools-release.yml makes it (the system's
    /// `tar -czf <folder>.tar.gz <folder>`) unpacks.
    #[test]
    fn an_archive_made_by_system_tar_unpacks() {
        let work = tmp("systar");
        let top = work.join(stem(T));
        std::fs::create_dir_all(&top).unwrap();
        for name in FIRST_PARTY_TOOLS {
            std::fs::write(top.join(name), name.as_bytes()).unwrap();
        }
        let status = std::process::Command::new("tar")
            .env("COPYFILE_DISABLE", "1")
            .current_dir(&work)
            .args(["-czf", "tools.tar.gz", &stem(T)])
            .status();
        if !status.is_ok_and(|s| s.success()) {
            return; // no system tar here
        }
        let dest = tmp("systar-dest");
        let got = extract(&std::fs::read(work.join("tools.tar.gz")).unwrap(), &dest, T).unwrap();
        assert_eq!(got.len(), FIRST_PARTY_TOOLS.len());
        assert_eq!(std::fs::read(dest.join("aivyx-n8n")).unwrap(), b"aivyx-n8n");
        std::fs::remove_dir_all(&work).unwrap();
        std::fs::remove_dir_all(&dest).unwrap();
    }

    /// The whole path against a local server: download, checksum, unpack;
    /// and a wrong checksum installs nothing.
    #[tokio::test]
    async fn install_downloads_checks_and_unpacks() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let bytes = archive(&[(
            "aivyx-pa-tools-x86_64-unknown-linux-musl/aivyx-gmail",
            b"gmail",
            false,
        )]);
        let good = format!("{}  aivyx-pa-tools.tar.gz\n", hex::encode(Sha256::digest(&bytes)));
        for (sum, ok) in [(good, true), (format!("{}  x\n", "0".repeat(64)), false)] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let (bytes, sum) = (bytes.clone(), sum.clone());
            tokio::spawn(async move {
                for _ in 0..2 {
                    let (mut sock, _) = listener.accept().await.unwrap();
                    let mut buf = [0u8; 2048];
                    let n = sock.read(&mut buf).await.unwrap();
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let body: Vec<u8> = if req.contains(".sha256 ") {
                        sum.clone().into_bytes()
                    } else {
                        bytes.clone()
                    };
                    let head = format!(
                        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    );
                    sock.write_all(head.as_bytes()).await.unwrap();
                    sock.write_all(&body).await.unwrap();
                }
            });
            let dest = tmp(if ok { "e2e-ok" } else { "e2e-bad" });
            let mut out = Vec::new();
            let result = install(&base, "0.21.3", T, &dest, &mut out).await;
            assert_eq!(result.is_ok(), ok, "{result:?}");
            assert_eq!(dest.join("aivyx-gmail").exists(), ok);
            std::fs::remove_dir_all(&dest).unwrap();
        }
    }
}
