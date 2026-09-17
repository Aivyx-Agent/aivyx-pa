//! Chapter Freight — the signed binary pack-bundle format.
//!
//! A pack bundle (`<name>-<version>-<target>.aivyxpack`) is an **outer
//! plain tar** with exactly three entries:
//!
//! - `payload.tar.gz` — the signed unit: `manifest.toml` + `bin/*`
//!   (tool-process executables) + `config/*` (team-pack TOMLs).
//! - `signature.bin` — a 64-byte Ed25519 signature over the raw
//!   `payload.tar.gz` bytes. Signing the compressed bytes makes
//!   verification a single pass over one artifact — no
//!   canonicalization questions.
//! - `publisher.txt` — base64 of the publisher's verifying key. A
//!   *selector* only: it must match a trusted key the operator (or the
//!   compiled-in set) already holds; it is never trusted itself.
//!
//! The signature proves **authenticity and integrity** — it protects
//! the operator from tampered packs. It is deliberately not DRM.
//!
//! Extraction is path-sanitized: absolute paths, `..` components, and
//! symlink entries are refused outright, so a malicious archive cannot
//! write outside its install directory even if signature checking were
//! misconfigured. See `docs/FREIGHT.md`.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::Deserialize;
use thiserror::Error;

/// Compiled-in Aivyx publisher verifying keys (base64). Empty until the
/// v1.0 web presence establishes the real publisher key ceremony — an
/// empty set means only operator-configured `[pack] trusted_publishers`
/// verify. Deliberately NOT a placeholder key: a placeholder anyone can
/// read is worse than none.
pub const AIVYX_PA_PUBLISHER_KEYS: &[&str] = &[];

/// The target triple this binary was built for (relayed by build.rs) —
/// `pack install` refuses bundles built for a foreign platform.
pub const HOST_TARGET: &str = env!("AIVYX_PA_BUILD_TARGET");

const PAYLOAD_NAME: &str = "payload.tar.gz";
const SIGNATURE_NAME: &str = "signature.bin";
const PUBLISHER_NAME: &str = "publisher.txt";
const MANIFEST_NAME: &str = "manifest.toml";

/// Outer bundle entries larger than this are refused before being read
/// into memory. Generous for a real vertical-pack bundle, small enough to
/// bound a deliberate memory/disk-fill attempt on an operator-supplied
/// file that hasn't been signature-verified yet.
const MAX_BUNDLE_ENTRY_BYTES: u64 = 512 * 1024 * 1024;

/// Payload decompression is refused once it has produced more than this
/// multiple of the compressed input's size — closes a zip-bomb-style
/// expansion attack.
const MAX_DECOMPRESSION_RATIO: u64 = 20;

/// Floor on the decompression budget so a tiny (sub-KB) compressed
/// payload isn't capped down to something that can't even hold a
/// legitimate manifest + small binary.
const MIN_DECOMPRESSED_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Error)]
pub enum PackError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("bundle is missing required entry {0:?}")]
    MissingEntry(&'static str),
    #[error("bundle entry {0:?} is malformed: {1}")]
    BadEntry(&'static str, String),
    #[error(
        "publisher key is not trusted: {hint} — add it to \
         `[pack] trusted_publishers` if you trust this publisher"
    )]
    UntrustedPublisher { hint: String },
    #[error("signature verification FAILED — the bundle is corrupt or tampered")]
    BadSignature,
    #[error("payload has no {MANIFEST_NAME}")]
    MissingManifest,
    #[error("manifest is invalid: {0}")]
    BadManifest(String),
    #[error(
        "unsafe archive entry {0:?} (absolute path, `..`, or symlink) — refusing"
    )]
    UnsafeEntry(String),
    #[error(
        "pack targets {pack} but this host is {host} — install the \
         bundle built for your platform"
    )]
    WrongTarget { pack: String, host: String },
    #[error(
        "pack requires daemon >= {required} but this is {current} — \
         upgrade Aivyx first"
    )]
    DaemonTooOld { required: String, current: String },
}

/// One `[[tool_process]]` a pack wires at install (the Mise pattern).
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PackToolProcess {
    /// Process name (the `[[tool_process]] name` key).
    pub name: String,
    /// Executable path relative to the payload's `bin/` directory.
    pub bin: String,
    /// Optional args passed to the process.
    #[serde(default)]
    pub args: Vec<String>,
}

/// The signed manifest at the payload root. The copy INSIDE the signed
/// payload is authoritative — there is deliberately no outer duplicate
/// to drift from it.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PackManifest {
    /// Pack name — kebab-case, also the install directory name.
    pub name: String,
    /// Pack version (semver-ish `x.y.z`; displayed + install dir).
    pub version: String,
    /// Rust target triple the binaries were built for. Install refuses
    /// a foreign host.
    pub target: String,
    /// Minimum daemon version (`x.y.z`) the pack's tool protocol needs.
    pub min_daemon_version: String,
    /// Publisher display label (informational; trust comes from keys).
    pub publisher: String,
    /// Tool processes to wire at install.
    #[serde(default, rename = "tool_process")]
    pub tool_processes: Vec<PackToolProcess>,
    /// Optional team config, relative to the payload's `config/` dir —
    /// wired as `[team] config_path` only if the operator has none
    /// (never clobbered; the Mise/Roster rule).
    #[serde(default)]
    pub team_config: Option<String>,
}

impl PackManifest {
    pub fn parse(text: &str) -> Result<Self, PackError> {
        let m: PackManifest =
            toml::from_str(text).map_err(|e| PackError::BadManifest(e.to_string()))?;
        for field in [&m.name, &m.version, &m.target, &m.min_daemon_version] {
            if field.trim().is_empty() {
                return Err(PackError::BadManifest(
                    "name/version/target/min_daemon_version must be non-empty".into(),
                ));
            }
        }
        // The name becomes an install directory component — keep it boring.
        if !m
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(PackError::BadManifest(format!(
                "pack name {:?} must be alphanumeric/dash/underscore",
                m.name
            )));
        }
        Ok(m)
    }
}

/// Parse `x.y.z` (extra pre-release/build suffixes rejected — pack
/// version gates should be boring).
fn semverish(s: &str) -> Option<(u64, u64, u64)> {
    let mut it = s.trim().split('.');
    let maj = it.next()?.parse().ok()?;
    let min = it.next()?.parse().ok()?;
    let pat = it.next()?.parse().ok()?;
    it.next().is_none().then_some((maj, min, pat))
}

/// `required <= current`, both `x.y.z`.
pub fn daemon_version_ok(required: &str, current: &str) -> Result<(), PackError> {
    let (r, c) = match (semverish(required), semverish(current)) {
        (Some(r), Some(c)) => (r, c),
        _ => {
            return Err(PackError::BadManifest(format!(
                "unparseable version pair ({required:?}, {current:?})"
            )))
        }
    };
    if r <= c {
        Ok(())
    } else {
        Err(PackError::DaemonTooOld {
            required: required.into(),
            current: current.into(),
        })
    }
}

// ---------------------------------------------------------------------------
// Publisher side — keys, payload build, bundle write
// ---------------------------------------------------------------------------

/// Generate a keypair and write the 32-byte secret to `path` (0600 on
/// unix). Returns the base64 verifying key for `trusted_publishers`.
pub fn keygen_to_file(path: &Path) -> Result<String, PackError> {
    let key = SigningKey::generate(&mut rand::rngs::OsRng);
    std::fs::write(path, key.to_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(encode_verifying_key(&key.verifying_key()))
}

/// Load a signing key written by [`keygen_to_file`].
pub fn load_signing_key(path: &Path) -> Result<SigningKey, PackError> {
    let bytes = std::fs::read(path)?;
    let arr: [u8; 32] = bytes.try_into().map_err(|_| {
        PackError::BadEntry("keyfile", "must be exactly 32 secret bytes".into())
    })?;
    Ok(SigningKey::from_bytes(&arr))
}

/// Encode a verifying key for `publisher.txt` / `trusted_publishers`.
pub fn encode_verifying_key(key: &VerifyingKey) -> String {
    base64::engine::general_purpose::STANDARD.encode(key.as_bytes())
}

/// Decode a base64 verifying key (config entries + `publisher.txt`).
pub fn decode_verifying_key(b64: &str) -> Option<VerifyingKey> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .ok()?;
    let arr: [u8; 32] = bytes.try_into().ok()?;
    VerifyingKey::from_bytes(&arr).ok()
}

/// Build the gzip'd payload tar from a staging directory holding
/// `manifest.toml` + `bin/` + `config/`. Deterministic entry order
/// (sorted paths) so rebuilding the same staging tree signs the same
/// bytes. Returns the payload bytes.
pub fn build_payload(staging: &Path) -> Result<Vec<u8>, PackError> {
    // Validate the manifest up front — a bundle that can't install is
    // better refused at build time.
    let manifest_path = staging.join(MANIFEST_NAME);
    let manifest_text = std::fs::read_to_string(&manifest_path)
        .map_err(|_| PackError::MissingManifest)?;
    PackManifest::parse(&manifest_text)?;

    // Collect files: manifest + everything under bin/ and config/.
    let mut files: BTreeMap<String, PathBuf> = BTreeMap::new();
    files.insert(MANIFEST_NAME.to_string(), manifest_path);
    for dir in ["bin", "config"] {
        let root = staging.join(dir);
        if !root.is_dir() {
            continue;
        }
        collect_files(&root, &mut |p| {
            let rel = p.strip_prefix(staging).expect("under staging");
            files.insert(rel.to_string_lossy().replace('\\', "/"), p.to_path_buf());
        })?;
    }

    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tarb = tar::Builder::new(gz);
    for (rel, path) in &files {
        let mut f = std::fs::File::open(path)?;
        let meta = f.metadata()?;
        let mut header = tar::Header::new_gnu();
        header.set_size(meta.len());
        // Executables keep their bit; everything else is 0644. No
        // owner/mtime → deterministic bytes for a given tree.
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o111 != 0 { 0o755 } else { 0o644 }
        };
        #[cfg(not(unix))]
        let mode = 0o644;
        header.set_mode(mode);
        header.set_mtime(0);
        header.set_cksum();
        tarb.append_data(&mut header, rel, &mut f)?;
    }
    let gz = tarb.into_inner()?;
    Ok(gz.finish()?)
}

fn collect_files(dir: &Path, push: &mut dyn FnMut(&Path)) -> Result<(), PackError> {
    let mut entries: Vec<_> =
        std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            return Err(PackError::UnsafeEntry(path.display().to_string()));
        }
        if ft.is_dir() {
            collect_files(&path, push)?;
        } else {
            push(&path);
        }
    }
    Ok(())
}

/// Sign a payload and write the complete `.aivyxpack` bundle.
pub fn write_bundle(
    payload: &[u8],
    signing_key: &SigningKey,
    out: &Path,
) -> Result<(), PackError> {
    let signature = signing_key.sign(payload);
    let publisher = encode_verifying_key(&signing_key.verifying_key());

    let file = std::fs::File::create(out)?;
    let mut tarb = tar::Builder::new(file);
    for (name, bytes) in [
        (PAYLOAD_NAME, payload),
        (SIGNATURE_NAME, signature.to_bytes().as_slice()),
        (PUBLISHER_NAME, publisher.as_bytes()),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_cksum();
        tarb.append_data(&mut header, name, bytes)?;
    }
    tarb.into_inner()?.sync_all()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Operator side — read, verify, unpack
// ---------------------------------------------------------------------------

/// The three parts of a read bundle, pre-verification.
#[derive(Debug)]
pub struct ReadBundle {
    pub payload: Vec<u8>,
    pub signature: [u8; 64],
    /// base64 key hint from `publisher.txt` (NOT trusted).
    pub publisher_hint: String,
}

/// Read the outer tar. Unknown entries are refused (the format is
/// exactly three entries; anything extra is suspicious).
pub fn read_bundle(path: &Path) -> Result<ReadBundle, PackError> {
    let file = std::fs::File::open(path)?;
    let mut archive = tar::Archive::new(file);
    let mut payload = None;
    let mut signature = None;
    let mut publisher = None;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let name = entry.path()?.to_string_lossy().into_owned();
        // `Entry::size()` (not `Header::size()`) is what actually governs
        // how many bytes `entry.read_to_end()` will read: a preceding PAX
        // extended-header record can override the raw header's declared
        // size, and `Entry::size()` is PAX-aware while `Header::size()`
        // reads only the (possibly-lying) raw header field. An attacker
        // fully controlling this not-yet-signature-verified bundle could
        // set a tiny raw header size to slip under a check on
        // `Header::size()` while a PAX record declares the real,
        // arbitrarily large size that `read_to_end` will actually honor.
        let declared_size = entry.size();
        if declared_size > MAX_BUNDLE_ENTRY_BYTES {
            return Err(PackError::BadEntry(
                "bundle",
                format!(
                    "entry {name:?} is {declared_size} bytes, exceeding the \
                     {MAX_BUNDLE_ENTRY_BYTES}-byte limit"
                ),
            ));
        }
        // Belt-and-suspenders: also hard-fence the read itself at the cap.
        // The memory this function allocates should not depend entirely on
        // trusting that `Entry::size()` never disagrees with the actual
        // byte stream `tar` will hand back — if it ever did, `.take()`
        // still bounds the allocation below regardless of what any
        // size-reporting API claims.
        let mut bytes = Vec::with_capacity(declared_size as usize);
        entry
            .by_ref()
            .take(MAX_BUNDLE_ENTRY_BYTES)
            .read_to_end(&mut bytes)?;
        match name.as_str() {
            PAYLOAD_NAME => payload = Some(bytes),
            SIGNATURE_NAME => {
                let arr: [u8; 64] = bytes.try_into().map_err(|_| {
                    PackError::BadEntry(SIGNATURE_NAME, "must be exactly 64 bytes".into())
                })?;
                signature = Some(arr);
            }
            PUBLISHER_NAME => {
                publisher = Some(String::from_utf8(bytes).map_err(|_| {
                    PackError::BadEntry(PUBLISHER_NAME, "not UTF-8".into())
                })?)
            }
            other => {
                return Err(PackError::BadEntry(
                    "bundle",
                    format!("unexpected entry {other:?}"),
                ))
            }
        }
    }
    Ok(ReadBundle {
        payload: payload.ok_or(PackError::MissingEntry(PAYLOAD_NAME))?,
        signature: signature.ok_or(PackError::MissingEntry(SIGNATURE_NAME))?,
        publisher_hint: publisher
            .ok_or(PackError::MissingEntry(PUBLISHER_NAME))?
            .trim()
            .to_string(),
    })
}

/// Verify a bundle against the trusted publisher set (operator config
/// unioned with [`AIVYX_PA_PUBLISHER_KEYS`]). The `publisher.txt` hint
/// selects which trusted key to check — it must decode to a key that is
/// BYTE-IDENTICAL to a trusted entry; an unknown key is refused before
/// any signature math.
pub fn verify_bundle(
    bundle: &ReadBundle,
    trusted_publishers: &[String],
) -> Result<VerifyingKey, PackError> {
    let hint = decode_verifying_key(&bundle.publisher_hint).ok_or_else(|| {
        PackError::BadEntry(PUBLISHER_NAME, "not a valid Ed25519 key".into())
    })?;
    let trusted = trusted_publishers
        .iter()
        .map(String::as_str)
        .chain(AIVYX_PA_PUBLISHER_KEYS.iter().copied())
        .filter_map(decode_verifying_key)
        .any(|k| k == hint);
    if !trusted {
        return Err(PackError::UntrustedPublisher {
            hint: bundle.publisher_hint.clone(),
        });
    }
    let sig = Signature::from_bytes(&bundle.signature);
    hint.verify(&bundle.payload, &sig)
        .map_err(|_| PackError::BadSignature)?;
    Ok(hint)
}

/// Read the manifest out of a (verified) payload without unpacking.
pub fn read_manifest(payload: &[u8]) -> Result<PackManifest, PackError> {
    let gz = flate2::read::GzDecoder::new(payload);
    let mut archive = tar::Archive::new(gz);
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.path()?.to_string_lossy() == MANIFEST_NAME {
            let mut text = String::new();
            entry.read_to_string(&mut text)?;
            return PackManifest::parse(&text);
        }
    }
    Err(PackError::MissingManifest)
}

/// True when `rel` is a safe archive-relative path: no absolute roots,
/// no `..`, no prefix components. `pub` so callers outside this crate
/// (e.g. `aivyx-cli`'s pack installer) can apply the same check to
/// manifest-declared paths (`bin`, `team_config`) before joining them
/// onto an install directory.
pub fn safe_relative(rel: &Path) -> bool {
    rel.components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
        && !rel.as_os_str().is_empty()
}

/// Wraps a decompressing reader and refuses to yield more than a fixed
/// byte budget total, erroring rather than silently truncating — used to
/// bound gzip expansion to a fixed multiple of the compressed input size
/// (a zip-bomb-style attack otherwise has no ceiling since the archive
/// entries stream straight to disk without ever being buffered whole).
struct RatioCappedReader<R> {
    inner: R,
    remaining: u64,
}

impl<R: Read> Read for RatioCappedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n as u64 > self.remaining {
            return Err(std::io::Error::other(
                "pack payload exceeds the maximum allowed decompression ratio",
            ));
        }
        self.remaining -= n as u64;
        Ok(n)
    }
}

/// Unpack a (verified) payload into `dest`. Every entry is sanitized;
/// symlinks and unsafe paths abort the whole unpack. Total decompressed
/// output across all entries is capped at `MAX_DECOMPRESSION_RATIO` times
/// the compressed payload size, closing a zip-bomb-style expansion attack.
pub fn unpack_payload(payload: &[u8], dest: &Path) -> Result<(), PackError> {
    let max_decompressed = (payload.len() as u64)
        .saturating_mul(MAX_DECOMPRESSION_RATIO)
        .max(MIN_DECOMPRESSED_BYTES);
    let gz = flate2::read::GzDecoder::new(payload);
    let capped = RatioCappedReader {
        inner: gz,
        remaining: max_decompressed,
    };
    let mut archive = tar::Archive::new(capped);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let rel = entry.path()?.into_owned();
        if !safe_relative(&rel)
            || entry.header().entry_type().is_symlink()
            || entry.header().entry_type().is_hard_link()
        {
            return Err(PackError::UnsafeEntry(rel.display().to_string()));
        }
        let target = dest.join(&rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&target)?;
        std::io::copy(&mut entry, &mut out)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(mode) = entry.header().mode() {
                let _ = out.set_permissions(std::fs::Permissions::from_mode(
                    if mode & 0o111 != 0 { 0o755 } else { 0o644 },
                ));
            }
        }
        out.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::OsRng;

    fn keypair() -> SigningKey {
        SigningKey::generate(&mut OsRng)
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let base = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
        let dir = PathBuf::from(base).join(format!(
            "aivyx-pack-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const MANIFEST: &str = r#"
name = "kitchen"
version = "1.0.0"
target = "x86_64-unknown-linux-gnu"
min_daemon_version = "0.8.0"
publisher = "Aivyx Test"
# NB: root keys must precede [[tool_process]] — a root key after a table
# array belongs to that table (the TOML trap deny_unknown_fields catches).
team_config = "kitchen-boh.toml"

[[tool_process]]
name = "kitchen-toolkit"
bin = "aivyx-kitchen-toolkit"
"#;

    fn stage(tag: &str) -> PathBuf {
        let dir = tmpdir(tag);
        std::fs::write(dir.join("manifest.toml"), MANIFEST).unwrap();
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(dir.join("bin/aivyx-kitchen-toolkit"), b"#!fake-elf").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                dir.join("bin/aivyx-kitchen-toolkit"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        std::fs::write(dir.join("config/kitchen-boh.toml"), "[team]\n").unwrap();
        dir
    }

    #[test]
    fn round_trip_build_sign_verify_unpack() {
        let staging = stage("roundtrip");
        let key = keypair();
        let payload = build_payload(&staging).unwrap();
        let bundle_path = tmpdir("roundtrip-out").join("kitchen.aivyxpack");
        write_bundle(&payload, &key, &bundle_path).unwrap();

        let bundle = read_bundle(&bundle_path).unwrap();
        let trusted = vec![encode_verifying_key(&key.verifying_key())];
        verify_bundle(&bundle, &trusted).expect("trusted key verifies");

        let manifest = read_manifest(&bundle.payload).unwrap();
        assert_eq!(manifest.name, "kitchen");
        assert_eq!(manifest.tool_processes.len(), 1);
        assert_eq!(manifest.team_config.as_deref(), Some("kitchen-boh.toml"));

        let dest = tmpdir("roundtrip-dest");
        unpack_payload(&bundle.payload, &dest).unwrap();
        assert!(dest.join("bin/aivyx-kitchen-toolkit").is_file());
        assert!(dest.join("config/kitchen-boh.toml").is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dest.join("bin/aivyx-kitchen-toolkit"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "executable bit survives");
        }
    }

    #[test]
    fn untrusted_publisher_is_refused_before_signature_math() {
        let staging = stage("untrusted");
        let key = keypair();
        let payload = build_payload(&staging).unwrap();
        let path = tmpdir("untrusted-out").join("p.aivyxpack");
        write_bundle(&payload, &key, &path).unwrap();
        let bundle = read_bundle(&path).unwrap();
        // Empty trusted set (and the compiled-in set is empty until v1.0).
        let err = verify_bundle(&bundle, &[]).unwrap_err();
        assert!(matches!(err, PackError::UntrustedPublisher { .. }));
        // A DIFFERENT trusted key doesn't admit this publisher either.
        let other = keypair();
        let err = verify_bundle(
            &bundle,
            &[encode_verifying_key(&other.verifying_key())],
        )
        .unwrap_err();
        assert!(matches!(err, PackError::UntrustedPublisher { .. }));
    }

    #[test]
    fn tampered_payload_fails_signature() {
        let staging = stage("tamper");
        let key = keypair();
        let payload = build_payload(&staging).unwrap();
        let path = tmpdir("tamper-out").join("p.aivyxpack");
        write_bundle(&payload, &key, &path).unwrap();
        let mut bundle = read_bundle(&path).unwrap();
        // Flip one payload byte post-signing.
        let mid = bundle.payload.len() / 2;
        bundle.payload[mid] ^= 0xFF;
        let trusted = vec![encode_verifying_key(&key.verifying_key())];
        let err = verify_bundle(&bundle, &trusted).unwrap_err();
        assert!(matches!(err, PackError::BadSignature));
    }

    #[test]
    fn unsafe_paths_are_refused_on_unpack() {
        // Hand-build a payload with a traversal entry.
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut tarb = tar::Builder::new(gz);
        let evil = b"pwned";
        let mut header = tar::Header::new_gnu();
        header.set_size(evil.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tarb.append_data(&mut header, "../escape.txt", evil.as_slice())
            .unwrap_or(()); // some tar impls refuse here already — fine
        let payload = tarb.into_inner().unwrap().finish().unwrap();
        let dest = tmpdir("unsafe-dest");
        // Either the builder refused (empty archive → unpack succeeds
        // trivially and nothing escapes) or unpack must refuse.
        match unpack_payload(&payload, &dest) {
            Ok(()) => assert!(!dest.parent().unwrap().join("escape.txt").exists()),
            Err(PackError::UnsafeEntry(_)) => {}
            Err(other) => panic!("unexpected error: {other}"),
        }
        assert!(safe_relative(Path::new("bin/tool")));
        assert!(!safe_relative(Path::new("../up")));
        assert!(!safe_relative(Path::new("/abs")));
    }

    #[test]
    fn daemon_version_gate() {
        assert!(daemon_version_ok("0.8.0", "0.8.0").is_ok());
        assert!(daemon_version_ok("0.8.0", "0.9.2").is_ok());
        assert!(matches!(
            daemon_version_ok("1.0.0", "0.8.0"),
            Err(PackError::DaemonTooOld { .. })
        ));
        assert!(daemon_version_ok("0.8", "0.8.0").is_err(), "x.y.z only");
    }

    #[test]
    fn manifest_rejects_sneaky_names() {
        let bad = MANIFEST.replace("\"kitchen\"", "\"../kitchen\"");
        assert!(PackManifest::parse(&bad).is_err());
    }

    #[test]
    fn deterministic_payload_for_same_tree() {
        let staging = stage("determinism");
        let a = build_payload(&staging).unwrap();
        let b = build_payload(&staging).unwrap();
        assert_eq!(a, b, "same staging tree must sign the same bytes");
    }

    #[test]
    fn read_bundle_refuses_an_oversized_entry() {
        // Hand-build an outer tar (bypassing write_bundle) with one entry
        // whose header claims a size far larger than MAX_BUNDLE_ENTRY_BYTES.
        // The actual bytes written are tiny — read_bundle must refuse
        // based on the declared size alone, before ever trying to read
        // (nonexistent) gigabytes off disk.
        let path = tmpdir("oversized-entry").join("evil.aivyxpack");
        let file = std::fs::File::create(&path).unwrap();
        let mut tarb = tar::Builder::new(file);
        let claimed_size = MAX_BUNDLE_ENTRY_BYTES + 1;
        let mut header = tar::Header::new_gnu();
        header.set_size(claimed_size);
        header.set_mode(0o644);
        header.set_cksum();
        tarb.append_data(&mut header, PAYLOAD_NAME, b"tiny".as_slice())
            .unwrap();
        tarb.into_inner().unwrap().sync_all().unwrap();

        let err = read_bundle(&path).unwrap_err();
        match &err {
            PackError::BadEntry(_, msg) => {
                assert!(
                    msg.contains(&MAX_BUNDLE_ENTRY_BYTES.to_string()),
                    "error should mention the cap: {msg}"
                );
            }
            other => panic!("expected BadEntry, got {other:?}"),
        }
    }

    #[test]
    fn read_bundle_refuses_an_entry_whose_pax_header_overrides_its_declared_size() {
        // A PAX extended-header record can override an entry's declared
        // size independently of the raw tar header field. Craft an entry
        // whose raw header claims a tiny size (5 bytes — small enough that
        // a check on `Header::size()` alone would wrongly pass it) but is
        // preceded by a PAX extended header declaring the real size to be
        // far larger than MAX_BUNDLE_ENTRY_BYTES. `Entry::size()` is
        // PAX-aware and is what actually governs how much `tar` will let
        // a reader pull from this entry — read_bundle must refuse based on
        // that value, not the raw header field.
        let path = tmpdir("pax-override-entry").join("evil.aivyxpack");
        let file = std::fs::File::create(&path).unwrap();
        let mut tarb = tar::Builder::new(file);

        let pax_size = MAX_BUNDLE_ENTRY_BYTES + 1;
        tarb.append_pax_extensions([("size", pax_size.to_string().as_bytes())])
            .unwrap();

        let mut header = tar::Header::new_gnu();
        header.set_size(5); // raw header claims a tiny size
        header.set_mode(0o644);
        header.set_cksum();
        tarb.append_data(&mut header, PAYLOAD_NAME, b"tiny!".as_slice())
            .unwrap();
        tarb.into_inner().unwrap().sync_all().unwrap();

        let err = read_bundle(&path).unwrap_err();
        match &err {
            PackError::BadEntry(_, msg) => {
                assert!(
                    msg.contains(&pax_size.to_string()),
                    "error should mention the PAX-overridden size, not the tiny raw \
                     header size: {msg}"
                );
            }
            other => panic!("expected BadEntry, got {other:?}"),
        }
    }

    #[test]
    fn unpack_payload_refuses_a_decompression_bomb() {
        // A tar containing one large all-zero entry compresses enormously
        // under gzip — a real, small zip-bomb-style fixture built
        // deterministically rather than a committed binary file.
        let big_zeroes = vec![0u8; 20 * 1024 * 1024];
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        let mut tarb = tar::Builder::new(gz);
        let mut header = tar::Header::new_gnu();
        header.set_size(big_zeroes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tarb.append_data(&mut header, "bin/huge", big_zeroes.as_slice())
            .unwrap();
        let payload = tarb.into_inner().unwrap().finish().unwrap();
        assert!(
            (payload.len() as u64) * MAX_DECOMPRESSION_RATIO < big_zeroes.len() as u64,
            "fixture must actually exceed the ratio cap to be a meaningful test"
        );

        let dest = tmpdir("bomb-dest");
        let err = unpack_payload(&payload, &dest).unwrap_err();
        assert!(
            matches!(err, PackError::Io(_)),
            "expected an io error from the capped reader, got {err:?}"
        );
        assert!(
            err.to_string().contains("decompression ratio"),
            "error should explain the refusal: {err}"
        );
    }
}
