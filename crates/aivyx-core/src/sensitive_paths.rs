//! Chapter Ward — a default-on guard against reading known-secret files.
//!
//! The [security posture](../../docs/SECURITY_POSTURE.md) names its own
//! sharpest edge: confirm-first gates *writes*, not *reads*, so at `home`/
//! `full` reach the agent can `fs.read` `~/.ssh/id_rsa`, `~/.aws/credentials`,
//! `.env` files — or Aivyx's own encrypted store and the `daemon.env` that
//! holds the master passphrase — and exfiltrate them. For a privacy-first
//! agent that's the gap that most contradicts the promise, and it is **not**
//! mitigated by `confirm_destructive`.
//!
//! This closes it at the source — you cannot exfiltrate what you cannot read.
//! A curated set of secret locations is refused by default (even at `full`),
//! independent of the access-level sandbox root; the operator opts specific
//! paths back in with `[access] allow_sensitive_paths`.
//!
//! ## Matching is location-independent
//!
//! We classify the **canonical** (symlink-resolved) path, so a symlink *to* a
//! secret is caught too. Directory secrets match by path *component* (any
//! path passing through a `.ssh` / `.aws` / … directory), so we don't need to
//! know the user's home dir and it works the same on every host. File secrets
//! match by exact basename (`.env`, `id_rsa`) or extension (`.pem`, `.key`,
//! `.redb`).
//!
//! ## Honest scope
//!
//! This guards **read tools** (`fs.read` and everything that reuses its
//! sandbox — the data readers and the Documents browser). It is
//! defense-in-depth, not a sandbox: `shell.exec` (a separate high-trust tool
//! gated by the access level) can still `cat` a secret. As the posture doc
//! says, the capability model contains the *agent*; OS-level isolation
//! contains the *host*. Blocking the fs-read path removes the largest and most
//! accidental exfiltration surface.

use std::path::{Path, PathBuf};

/// Directory names that make any path passing through them secret by default
/// (SSH/GPG keys, cloud + k8s + docker credentials, browser profiles holding
/// cookies/logins, password managers, OS keyrings).
const SENSITIVE_DIR_SEGMENTS: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".kube",
    ".docker",
    "gcloud",
    ".azure",
    ".mozilla",
    ".password-store",
    "keyrings",
    "Keychains",
];

/// Exact basenames that are secret by default (dotfiles holding tokens/keys,
/// well-known credential files, and — critically — Aivyx's own passphrase
/// env-file).
const SENSITIVE_BASENAMES: &[&str] = &[
    ".env",
    ".netrc",
    ".pgpass",
    ".git-credentials",
    ".npmrc",
    ".dockercfg",
    "credentials",
    "credentials.json",
    "daemon.env",
    // The Studio's automatic sign-in token (first-run coherence A2). The
    // CLI also puts its exact path on the extra-deny list; the basename
    // catches the relative spellings `shell.exec`'s text scan would miss
    // (`cd ~/.aivyx-pa && cat studio-token`).
    "studio-token",
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
];

/// Extensions that are secret by default: private keys / key stores, and the
/// `.redb` extension of Aivyx's own encrypted substrate.
const SENSITIVE_EXTENSIONS: &[&str] = &["pem", "key", "p12", "pfx", "redb"];

/// Directory names a *write* into which plants code that runs later
/// (auto-start, service units, scheduled jobs, git hooks). Chapter Portcullis.
const PERSISTENCE_DIR_SEGMENTS: &[&str] = &[
    "autostart", // ~/.config/autostart/*.desktop
    "systemd",   // ~/.config/systemd/user, /etc/systemd
    "cron.d",
    "cron.daily",
    "cron.hourly",
    "init.d",
    "hooks", // .git/hooks/*
    "LaunchAgents",
    "LaunchDaemons",
];

/// Exact basenames a *write* to which hijacks a login shell / SSH / cron.
const PERSISTENCE_BASENAMES: &[&str] = &[
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".profile",
    ".zshrc",
    ".zprofile",
    ".zshenv",
    ".zlogin",
    "authorized_keys",
    "crontab",
    ".xprofile",
];

/// The read-guard policy: the built-in secret set above, minus operator
/// allow-listed prefixes, plus any operator-added deny prefixes. Cheap to
/// clone (a couple of `Vec<PathBuf>`); the fs tools hold it behind an `Arc`.
#[derive(Debug, Clone, Default)]
pub struct SensitivePolicy {
    /// When false the guard is a no-op (byte-identical to pre-Ward behavior).
    enabled: bool,
    /// Canonical path prefixes the operator explicitly allows despite matching
    /// the built-in set (e.g. a project's own `.env`).
    allow: Vec<PathBuf>,
    /// Extra path prefixes the operator marks secret beyond the built-ins.
    extra_deny: Vec<PathBuf>,
}

impl SensitivePolicy {
    /// The guard, enabled, with operator allow / extra-deny prefixes. The
    /// prefixes should already be absolute (the caller canonicalizes/expands
    /// `~` at config load).
    pub fn new(allow: Vec<PathBuf>, extra_deny: Vec<PathBuf>) -> Self {
        SensitivePolicy {
            enabled: true,
            allow,
            extra_deny,
        }
    }

    /// A disabled guard — every path is allowed. Used when the operator sets
    /// `[access] guard_sensitive_paths = false`, and as the `Default`.
    pub fn disabled() -> Self {
        SensitivePolicy {
            enabled: false,
            allow: Vec::new(),
            extra_deny: Vec::new(),
        }
    }

    /// Classify a **canonical** path for READING. `Some(reason)` ⇒ refuse;
    /// `None` ⇒ allowed. The reason is a short operator-facing string (no
    /// secret contents, just why it was blocked).
    pub fn classify(&self, canonical: &Path) -> Option<String> {
        self.gated(canonical, read_sensitive_reason)
    }

    /// Classify a **canonical** path for WRITING (Chapter Portcullis). Refuses
    /// the read-sensitive set (overwriting a credential is as bad as reading
    /// it) AND persistence/exec locations — shell rc files, autostart, systemd
    /// units, cron, `.ssh/authorized_keys`, git hooks — which are how a
    /// (possibly injected) agent would plant a backdoor even when it has no
    /// interest in reading them. Same enabled/allow-list gating as reads.
    pub fn classify_write(&self, canonical: &Path) -> Option<String> {
        self.gated(canonical, |c| {
            read_sensitive_reason(c).or_else(|| persistence_reason(c))
        })
    }

    /// Shared gate: honor `enabled` + the operator allow-list + extra-deny,
    /// then defer to a matcher for the built-in rules.
    fn gated(&self, canonical: &Path, matcher: impl Fn(&Path) -> Option<String>) -> Option<String> {
        if !self.enabled {
            return None;
        }
        // Operator allow-list wins over every built-in / extra-deny rule.
        if self.allow.iter().any(|a| canonical.starts_with(a)) {
            return None;
        }
        if let Some(hit) = self.extra_deny.iter().find(|d| canonical.starts_with(d)) {
            return Some(format!(
                "operator-marked sensitive path ({})",
                hit.display()
            ));
        }
        matcher(canonical)
    }
}

/// The read-sensitive built-in match (secrets). Location-independent.
fn read_sensitive_reason(canonical: &Path) -> Option<String> {
    for comp in canonical.components() {
        if let Some(seg) = comp.as_os_str().to_str() {
            if SENSITIVE_DIR_SEGMENTS.contains(&seg) {
                return Some(format!("under a sensitive directory ({seg})"));
            }
        }
    }
    if let Some(name) = canonical.file_name().and_then(|n| n.to_str()) {
        if SENSITIVE_BASENAMES.contains(&name) {
            return Some(format!("a sensitive file ({name})"));
        }
        // dotenv family beyond the bare `.env`: `.env.local`, `app.env`, …
        if name.starts_with(".env.") || name.ends_with(".env") {
            return Some(format!("a dotenv file ({name})"));
        }
    }
    if let Some(ext) = canonical.extension().and_then(|e| e.to_str()) {
        let ext_lower = ext.to_ascii_lowercase();
        if SENSITIVE_EXTENSIONS.contains(&ext_lower.as_str()) {
            return Some(format!("a sensitive file type (.{ext_lower})"));
        }
    }
    None
}

/// The write-only persistence/exec match: locations where a *write* plants
/// startup code, an SSH backdoor, a scheduled job, or a service.
fn persistence_reason(canonical: &Path) -> Option<String> {
    for comp in canonical.components() {
        if let Some(seg) = comp.as_os_str().to_str() {
            if PERSISTENCE_DIR_SEGMENTS.contains(&seg) {
                return Some(format!(
                    "a persistence/startup location ({seg}) — writing here can \
                     plant code that runs later"
                ));
            }
        }
    }
    if let Some(name) = canonical.file_name().and_then(|n| n.to_str()) {
        if PERSISTENCE_BASENAMES.contains(&name) {
            return Some(format!(
                "a startup/persistence file ({name}) — writing here can plant \
                 code that runs later"
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard() -> SensitivePolicy {
        SensitivePolicy::new(Vec::new(), Vec::new())
    }

    #[test]
    fn blocks_the_canonical_secret_locations() {
        let g = guard();
        for p in [
            "/home/alice/.ssh/id_rsa",
            "/home/alice/.ssh/id_ed25519",
            "/home/alice/.aws/credentials",
            "/home/alice/.gnupg/secring.gpg",
            "/home/alice/.config/gcloud/access_tokens.db",
            "/home/alice/project/.env",
            "/home/alice/project/.env.local",
            "/home/alice/project/.env.production",
            "/home/alice/project/app.env",
            "/home/alice/project/prod.env",
            "/home/alice/.netrc",
            "/home/alice/certs/server.pem",
            "/home/alice/certs/tls.key",
            // Aivyx's own crown jewels:
            "/home/alice/.config/aivyx-pa/daemon.env",
            "/home/alice/.local/share/aivyx-pa/store.redb",
            "/home/alice/.aivyx-pa/studio-token",
        ] {
            assert!(g.classify(Path::new(p)).is_some(), "should block {p}");
        }
    }

    #[test]
    fn allows_ordinary_files() {
        let g = guard();
        for p in [
            "/home/alice/project/src/main.rs",
            "/home/alice/notes/todo.md",
            "/home/alice/data/report.csv",
            "/home/alice/.config/app/settings.toml",
            "/home/alice/keys.txt", // "key" in the name, but not a .key file
        ] {
            assert!(g.classify(Path::new(p)).is_none(), "should allow {p}");
        }
    }

    #[test]
    fn blocks_the_whole_ssh_dir_not_just_private_keys() {
        // The `.ssh` directory is sensitive wholesale — the agent has no
        // business reading even known_hosts/config/*.pub there by default.
        let g = guard();
        assert!(
            g.classify(Path::new("/home/alice/.ssh/id_rsa.pub"))
                .is_some()
        );
        assert!(
            g.classify(Path::new("/home/alice/.ssh/known_hosts"))
                .is_some()
        );
        assert!(g.classify(Path::new("/home/alice/.ssh/config")).is_some());
    }

    #[test]
    fn operator_allowlist_overrides_builtins() {
        let g = SensitivePolicy::new(vec![PathBuf::from("/home/alice/project")], Vec::new());
        // The project's own .env is allowed back in…
        assert!(g.classify(Path::new("/home/alice/project/.env")).is_none());
        // …but a secret outside the allowed prefix is still blocked.
        assert!(g.classify(Path::new("/home/alice/.ssh/id_rsa")).is_some());
    }

    #[test]
    fn operator_extra_deny_adds_prefixes() {
        let g = SensitivePolicy::new(Vec::new(), vec![PathBuf::from("/home/alice/secret-vault")]);
        assert!(
            g.classify(Path::new("/home/alice/secret-vault/notes.md"))
                .is_some()
        );
        assert!(
            g.classify(Path::new("/home/alice/other/notes.md"))
                .is_none()
        );
    }

    #[test]
    fn classify_write_blocks_persistence_and_secrets() {
        let g = guard();
        // Persistence / exec locations (write-only concerns).
        for p in [
            "/home/alice/.bashrc",
            "/home/alice/.zshrc",
            "/home/alice/.ssh/authorized_keys",
            "/home/alice/.config/autostart/eve.desktop",
            "/home/alice/.config/systemd/user/x.service",
            "/etc/cron.d/job",
            "/home/alice/project/.git/hooks/pre-commit",
        ] {
            assert!(
                g.classify_write(Path::new(p)).is_some(),
                "write should block {p}"
            );
        }
        // Secrets are blocked for writes too (overwrite a credential).
        assert!(
            g.classify_write(Path::new("/home/alice/.aws/credentials"))
                .is_some()
        );
        // Ordinary writes are fine.
        assert!(
            g.classify_write(Path::new("/home/alice/project/notes.md"))
                .is_none()
        );
        assert!(
            g.classify_write(Path::new("/home/alice/project/src/main.rs"))
                .is_none()
        );
        // Persistence names are a WRITE concern only — reads of them are allowed
        // (e.g. the agent inspecting your .bashrc is fine; rewriting it isn't).
        assert!(g.classify(Path::new("/home/alice/.bashrc")).is_none());
    }

    #[test]
    fn disabled_guard_is_a_noop() {
        let g = SensitivePolicy::disabled();
        assert!(g.classify(Path::new("/home/alice/.ssh/id_rsa")).is_none());
        assert!(
            SensitivePolicy::default()
                .classify(Path::new("/home/alice/.aws/credentials"))
                .is_none()
        );
    }
}
