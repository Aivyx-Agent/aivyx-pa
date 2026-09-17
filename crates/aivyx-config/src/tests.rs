//! Unit tests for [`crate::AivyxConfig`].
//!
//! Process-environment tests are the dangerous part: `std::env` is
//! process-wide, `cargo test` runs tests in parallel by default, and
//! two tests that both `set_var("FOO", ...)` will race. The
//! [`env_guard`] module below serializes every env-touching test
//! through one `Mutex` and restores the prior state on drop, so tests
//! are race-free but still express one-var-at-a-time mutations.
//!
//! Tests are structured around the three phases of
//! [`AivyxConfig::load_from_env_and_toml`] + `hydrate_secrets_from_store`
//! + `validate`:
//!
//! 1. **Env-only precedence.** Set env vars, no TOML, assert the
//!    resulting `Sourced<T>::source == FieldSource::Env`.
//! 2. **TOML-only precedence.** Clear env vars, write TOML, assert
//!    `source == FieldSource::Toml`.
//! 3. **Env-over-TOML.** Both sources hold the same field; assert the
//!    env value wins and `source == FieldSource::Env`.
//! 4. **Invalid parsing.** Set `AIVYX_PA_MEMORY_MAX_PER_TOPIC=not-a-num`
//!    and `AIVYX_PA_TELEGRAM_CHAT_ID=oops`; assert typed `Invalid`.
//! 5. **Missing required field.** Ask for `require_api_key = true`
//!    with no source supplying one; assert `Missing { field:
//!    "anthropic_api_key" }`.
//! 6. **Encrypted-store hydration.** Seed `KeyDomain::Secrets`
//!    manually, call `hydrate_secrets_from_store`, assert the api_key
//!    now reports `FieldSource::EncryptedStore`.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

use secrecy::ExposeSecret;

use crate::{
    AccessLevel, AivyxConfig, AutonomyLevel, AutonomyPosture, ConfigError, FieldSource,
    LoadOptions, McpTransportKind, NotifyTargetKind, NotifyWhen, ProviderKind, Role, TlsMode,
    ToolAllowlist, DEFAULT_ASSISTANT_NAME, DEFAULT_MEMORY_MAX_PER_TOPIC, DEFAULT_MODEL,
    DEFAULT_ROLE_NAME, DEFAULT_SYSTEM_PROMPT,
};

// ------------------------------------------------------------------
// env_guard — serialize env mutations across parallel tests
// ------------------------------------------------------------------

/// All env-touching tests hold this `Mutex` for their full duration so
/// `cargo test`'s parallel runner cannot cross-contaminate them. The
/// guard is RAII: drop = unlock.
fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

thread_local! {
    /// `true` while an [`EnvScope`] is live on this thread. The loader
    /// helpers assert on it so a test that reads ambient env (HOME,
    /// `AIVYX_PA_*`) without holding the env-guard fails *deterministically*
    /// here instead of flaking when it races a parallel env-mutating test.
    static ENV_SCOPE_ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Assert an [`EnvScope`] is live on this thread. Called by the loader
/// helpers — anything that resolves config from the ambient environment
/// must serialize behind the env-guard.
fn assert_env_guarded() {
    assert!(
        ENV_SCOPE_ACTIVE.with(std::cell::Cell::get),
        "config loaded without a live EnvScope — wrap the test in \
         `let env = EnvScope::new();` so ambient env reads can't race \
         parallel env-mutating tests",
    );
}

/// Snapshot + clear every env var this test crate might mutate, then
/// restore on drop. Prevents a flaky test from polluting the process
/// environment for any later test or for the rest of the cargo run.
///
/// `#[allow(unsafe_code)]` scoped to this impl is the whole reason
/// `lib.rs` uses `#![deny(unsafe_code)]` instead of `forbid`. See the
/// comment on that attribute.
struct EnvScope {
    saved: Vec<(&'static str, Option<String>)>,
    _guard: MutexGuard<'static, ()>,
}

impl EnvScope {
    fn new() -> Self {
        let guard = env_lock();
        ENV_SCOPE_ACTIVE.with(|f| f.set(true));
        let vars = [
            "ANTHROPIC_API_KEY",
            "AIVYX_PA_MODEL",
            "AIVYX_PA_SYSTEM_PROMPT",
            "AIVYX_PA_FS_ROOT",
            "AIVYX_PA_WORKSPACE",
            "AIVYX_PA_STORAGE_PATH",
            "XDG_DATA_HOME",
            "HOME",
            "AIVYX_PA_MEMORY_MAX_PER_TOPIC",
            "AIVYX_PA_PASSPHRASE",
            "AIVYX_PA_TELEGRAM_TOKEN",
            "AIVYX_PA_TELEGRAM_CHAT_ID",
            // Security-audit fix (Task 10, 2026-09-16): scope the new
            // Discord/Slack channel_filter env vars into the guard so
            // their round-trip tests don't leak state across the rest
            // of this file's tests (same reasoning as
            // AIVYX_PA_TELEGRAM_CHAT_ID above).
            "AIVYX_PA_DISCORD_CHANNEL_ID",
            "AIVYX_PA_SLACK_CHANNEL_ID",
            // Phase 11 Task 1: scope the role override env var into
            // the env-guard so role-tests don't leak state across
            // parallel cargo-test runs.
            "AIVYX_PA_ROLE",
            "AIVYX_PA_OPENAI_API_KEY",
            "AIVYX_PA_OPENAI_BASE_URL",
            "AIVYX_PA_PROVIDER",
        ];
        let saved: Vec<_> = vars
            .iter()
            .map(|&v| (v, std::env::var(v).ok()))
            .collect();
        // Start every test with a clean slate — no env vars set,
        // except HOME which we always keep pointed at a safe
        // directory so the path-resolving helpers don't trip a
        // spurious NoHome error.
        for (var, _) in &saved {
            // Safety: scoped env mutation under a process-wide mutex,
            // restored on drop. Test-only.
            #[allow(unsafe_code)]
            unsafe {
                std::env::remove_var(var);
            }
        }
        // HOME default: under $TMPDIR, which always exists and is a
        // directory. Tests that need a specific HOME override this.
        let tmp_home = std::env::temp_dir();
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("HOME", &tmp_home);
        }
        EnvScope {
            saved,
            _guard: guard,
        }
    }

    fn set(&self, var: &str, value: &str) {
        // Safety: scoped env mutation under a process-wide mutex.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var(var, value);
        }
    }

    fn clear(&self, var: &str) {
        // Safety: scoped env mutation under a process-wide mutex.
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var(var);
        }
    }
}

impl Drop for EnvScope {
    fn drop(&mut self) {
        ENV_SCOPE_ACTIVE.with(|f| f.set(false));
        for (var, prior) in self.saved.drain(..) {
            match prior {
                Some(value) => {
                    // Safety: restoring prior state under the same mutex.
                    #[allow(unsafe_code)]
                    unsafe {
                        std::env::set_var(var, value);
                    }
                }
                None => {
                    // Safety: restoring prior state under the same mutex.
                    #[allow(unsafe_code)]
                    unsafe {
                        std::env::remove_var(var);
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------------
// temp-dir RAII — matches aivyx-storage's `$TMPDIR` convention
// ------------------------------------------------------------------

/// Bare-bones unique dir under `$TMPDIR`, removed on drop. Same shape
/// as the helper in `crates/aivyx-storage/src/tests.rs` so a reader
/// grepping for temp-dir patterns finds one convention.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let unique = format!(
            "aivyx-config-test-{tag}-{}",
            uuid_like(),
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Cheap unique id — nanos since epoch + a counter. We deliberately
/// do not pull `uuid` into this test file; the existing convention
/// in `aivyx-storage`'s tests is the same.
fn uuid_like() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let c = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{nanos}-{c}")
}

// ------------------------------------------------------------------
// Phase 1: env-only precedence
// ------------------------------------------------------------------

#[test]
fn env_only_populates_every_field_with_env_source() {
    let env = EnvScope::new();
    env.set("ANTHROPIC_API_KEY", "sk-test-env-key");
    env.set("AIVYX_PA_MODEL", "claude-from-env");
    env.set("AIVYX_PA_SYSTEM_PROMPT", "env prompt");
    env.set("AIVYX_PA_FS_ROOT", "/tmp/env-fs-root");
    env.set("AIVYX_PA_STORAGE_PATH", "/tmp/env-store.redb");
    env.set("AIVYX_PA_MEMORY_MAX_PER_TOPIC", "1234");
    env.set("AIVYX_PA_PASSPHRASE", "env-passphrase");
    env.set("AIVYX_PA_TELEGRAM_TOKEN", "123:env-token");
    env.set("AIVYX_PA_TELEGRAM_CHAT_ID", "42");

    let cfg = AivyxConfig::load_from_env_and_toml(&LoadOptions::test_env_only())
        .expect("load should succeed");

    let api_key = cfg.anthropic_api_key.as_ref().expect("api key set");
    assert_eq!(api_key.source, FieldSource::Env);
    assert_eq!(api_key.value.expose_secret(), "sk-test-env-key");

    assert_eq!(cfg.model.source, FieldSource::Env);
    assert_eq!(cfg.model.value, "claude-from-env");
    assert_eq!(cfg.system_prompt.source, FieldSource::Env);
    assert_eq!(cfg.system_prompt.value, "env prompt");
    assert_eq!(cfg.fs_root.source, FieldSource::Env);
    assert_eq!(cfg.fs_root.value, PathBuf::from("/tmp/env-fs-root"));
    assert_eq!(cfg.storage_path.source, FieldSource::Env);
    assert_eq!(cfg.memory_max_per_topic.source, FieldSource::Env);
    assert_eq!(cfg.memory_max_per_topic.value, 1234);

    let pw = cfg.passphrase.as_ref().expect("passphrase set");
    assert_eq!(pw.source, FieldSource::Env);
    assert_eq!(pw.value.expose_secret(), "env-passphrase");

    let tg = cfg.telegram.as_ref().expect("telegram set");
    let tok = tg.token.as_ref().expect("token set");
    assert_eq!(tok.source, FieldSource::Env);
    assert_eq!(tok.value.expose_secret(), "123:env-token");
    let chat = tg.chat_filter.as_ref().expect("chat set");
    assert_eq!(chat.source, FieldSource::Env);
    assert_eq!(chat.value, 42);

    // Phase 11 Task 1: with no `[[role]]` entries and no AIVYX_PA_ROLE
    // override, the backwards-compat bridge must synthesize an
    // implicit `default` role whose system_prompt mirrors the
    // legacy env-sourced value. The test does not set AIVYX_PA_ROLE, so
    // the active role falls through to DEFAULT_ROLE_NAME.
    assert_eq!(cfg.roles.len(), 1, "exactly one synthesized role");
    let default_role = cfg
        .roles
        .get(DEFAULT_ROLE_NAME)
        .expect("default role synthesized");
    // The synthesized role's prompt carries the original env source,
    // not `FieldSource::Default` — pre-Phase-11 provenance preserved.
    assert_eq!(default_role.system_prompt.source, FieldSource::Env);
    assert_eq!(default_role.system_prompt.value, "env prompt");
    assert_eq!(cfg.active_role.value, DEFAULT_ROLE_NAME);
    assert_eq!(cfg.active_role.source, FieldSource::Default);
    assert!(
        cfg.warnings.is_empty(),
        "no warning when config has no explicit roles: {:?}",
        cfg.warnings
    );

    drop(env);
}

#[test]
fn env_empty_string_is_treated_as_unset() {
    // Phase 8 behavior preserved: `export AIVYX_PA_PASSPHRASE=` with no
    // value should not populate the passphrase field. Tests the
    // `env_string` helper's empty-is-unset rule.
    let env = EnvScope::new();
    env.set("AIVYX_PA_PASSPHRASE", "");

    let cfg = AivyxConfig::load_from_env_and_toml(&LoadOptions::test_env_only())
        .expect("load should succeed");
    assert!(cfg.passphrase.is_none(), "empty env var treated as unset");

    drop(env);
}

// ------------------------------------------------------------------
// Phase 2: TOML-only precedence
// ------------------------------------------------------------------

#[test]
fn toml_only_populates_fields_with_toml_source() {
    let env = EnvScope::new();
    let tmp = TempDir::new("toml-only");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-from-toml"

[agent]
model = "claude-toml-model"
system_prompt = "toml prompt"

[fs]
root = "/tmp/toml-fs"

[storage]
path = "/tmp/toml-store.redb"

[memory]
max_per_topic = 777

[telegram]
token = "toml:123:abc"
chat_id = 99

[aivyx_pa]
passphrase = "toml-passphrase"
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path.clone()),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");

    assert_eq!(
        cfg.anthropic_api_key.as_ref().unwrap().source,
        FieldSource::Toml
    );
    assert_eq!(cfg.model.source, FieldSource::Toml);
    assert_eq!(cfg.model.value, "claude-toml-model");
    assert_eq!(cfg.system_prompt.source, FieldSource::Toml);
    assert_eq!(cfg.fs_root.source, FieldSource::Toml);
    assert_eq!(cfg.fs_root.value, PathBuf::from("/tmp/toml-fs"));
    assert_eq!(cfg.storage_path.source, FieldSource::Toml);
    assert_eq!(cfg.memory_max_per_topic.source, FieldSource::Toml);
    assert_eq!(cfg.memory_max_per_topic.value, 777);
    assert_eq!(cfg.passphrase.as_ref().unwrap().source, FieldSource::Toml);
    let tg = cfg.telegram.as_ref().unwrap();
    assert_eq!(tg.token.as_ref().unwrap().source, FieldSource::Toml);
    assert_eq!(tg.chat_filter.as_ref().unwrap().value, 99);

    drop(env);
}

// ------------------------------------------------------------------
// Task 10 fix round 3 (2026-09-16) — `chat_filter` as a TOML alias
// for `chat_id`. An earlier `docs/INSTALL.md` example incorrectly
// documented the Telegram allowlist key as `chat_filter` (the
// internal Rust field name); `RawTelegram` has no
// `deny_unknown_fields`, so that typo would silently discard the
// operator's allowlist. The real key is `chat_id`; `chat_filter`
// must keep working as a backward-compatible alias so operators who
// already wrote it (from the old docs, or a natural guess) don't
// silently lose their allowlist — see THREAT_MODEL.md: since Task 10
// round 2, no allowlist configured means `Untrusted` for everyone.
// ------------------------------------------------------------------

#[test]
fn telegram_chat_id_and_chat_filter_alias_deserialize_to_the_same_value() {
    let env = EnvScope::new();

    let via_chat_id = load_with_toml(
        r#"
        [telegram]
        token = "t"
        chat_id = 99
    "#,
        "telegram-chat-id-key",
    );
    let via_chat_filter = load_with_toml(
        r#"
        [telegram]
        token = "t"
        chat_filter = 99
    "#,
        "telegram-chat-filter-alias-key",
    );

    let tg_chat_id = via_chat_id.telegram.expect("telegram section present");
    let tg_chat_filter = via_chat_filter
        .telegram
        .expect("telegram section present");

    assert_eq!(
        tg_chat_id.chat_filter.as_ref().expect("chat set").value,
        99
    );
    assert_eq!(
        tg_chat_filter.chat_filter.as_ref().expect("chat set").value,
        99
    );
    assert_eq!(
        tg_chat_id.chat_filter.as_ref().unwrap().value,
        tg_chat_filter.chat_filter.as_ref().unwrap().value,
    );

    drop(env);
}

// ------------------------------------------------------------------
// Piece C Task 2 — team_run_channel / team_trigger_rate_limit
// ------------------------------------------------------------------

#[test]
fn telegram_team_run_channel_defaults_to_false_and_unset_rate_limit() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        r#"
        [telegram]
        token = "t"
    "#,
        "team-run-telegram-defaults",
    );
    let tg = cfg.telegram.expect("telegram section present");
    assert!(!tg.team_run_channel);
    assert_eq!(tg.team_trigger_rate_limit, None);
    drop(env);
}

#[test]
fn telegram_team_run_channel_and_rate_limit_round_trip_from_toml() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        r#"
        [telegram]
        token = "t"
        team_run_channel = true
        team_trigger_rate_limit = 5
    "#,
        "team-run-telegram-round-trip",
    );
    let tg = cfg.telegram.expect("telegram section present");
    assert!(tg.team_run_channel);
    assert_eq!(tg.team_trigger_rate_limit, Some(5));
    drop(env);
}

#[test]
fn discord_and_slack_team_run_channel_round_trip_from_toml() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        r#"
        [discord]
        token = "t"
        team_run_channel = true
        team_trigger_rate_limit = 3

        [slack]
        bot_token = "b"
        app_token = "a"
        team_run_channel = true
        team_trigger_rate_limit = 7
    "#,
        "team-run-discord-slack-round-trip",
    );
    let discord = cfg.discord.expect("discord section present");
    assert!(discord.team_run_channel);
    assert_eq!(discord.team_trigger_rate_limit, Some(3));
    let slack = cfg.slack.expect("slack section present");
    assert!(slack.team_run_channel);
    assert_eq!(slack.team_trigger_rate_limit, Some(7));
    drop(env);
}

// ------------------------------------------------------------------
// Security-audit fix (Task 10, 2026-09-16) — Discord/Slack
// `channel_filter`, mirroring Telegram's `chat_filter`. Consumed by
// `DiscordChannel`/`SlackChannel::trust_tier()` to distinguish an
// allowlisted channel (SemiTrusted) from everything else
// (Untrusted) — see THREAT_MODEL.md.
// ------------------------------------------------------------------

#[test]
fn discord_channel_filter_round_trips_from_toml() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        r#"
        [discord]
        token = "t"
        channel_filter = 123456789
    "#,
        "discord-channel-filter-round-trip",
    );
    let discord = cfg.discord.expect("discord section present");
    let filter = discord.channel_filter.expect("channel_filter set");
    assert_eq!(filter.source, FieldSource::Toml);
    assert_eq!(filter.value, 123456789);
    drop(env);
}

#[test]
fn discord_channel_filter_env_beats_toml() {
    let env = EnvScope::new();
    env.set("AIVYX_PA_DISCORD_CHANNEL_ID", "42");
    let cfg = load_with_toml(
        r#"
        [discord]
        token = "t"
        channel_filter = 123456789
    "#,
        "discord-channel-filter-env-wins",
    );
    let discord = cfg.discord.expect("discord section present");
    let filter = discord.channel_filter.expect("channel_filter set");
    assert_eq!(filter.source, FieldSource::Env);
    assert_eq!(filter.value, 42);
    drop(env);
}

#[test]
fn discord_channel_filter_absent_is_none() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        r#"
        [discord]
        token = "t"
    "#,
        "discord-channel-filter-absent",
    );
    let discord = cfg.discord.expect("discord section present");
    assert!(discord.channel_filter.is_none());
    drop(env);
}

#[test]
fn unparseable_discord_channel_id_is_typed_invalid_error() {
    let env = EnvScope::new();
    env.set("AIVYX_PA_DISCORD_CHANNEL_ID", "oops");

    let err = AivyxConfig::load_from_env_and_toml(&LoadOptions::test_env_only())
        .expect_err("should fail parsing");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(field, "discord.channel_filter");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }

    drop(env);
}

#[test]
fn slack_channel_filter_round_trips_from_toml() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        r#"
        [slack]
        bot_token = "b"
        app_token = "a"
        channel_filter = "C0123456789"
    "#,
        "slack-channel-filter-round-trip",
    );
    let slack = cfg.slack.expect("slack section present");
    let filter = slack.channel_filter.expect("channel_filter set");
    assert_eq!(filter.source, FieldSource::Toml);
    assert_eq!(filter.value, "C0123456789");
    drop(env);
}

#[test]
fn slack_channel_filter_env_beats_toml() {
    let env = EnvScope::new();
    env.set("AIVYX_PA_SLACK_CHANNEL_ID", "C_ENV");
    let cfg = load_with_toml(
        r#"
        [slack]
        bot_token = "b"
        app_token = "a"
        channel_filter = "C_TOML"
    "#,
        "slack-channel-filter-env-wins",
    );
    let slack = cfg.slack.expect("slack section present");
    let filter = slack.channel_filter.expect("channel_filter set");
    assert_eq!(filter.source, FieldSource::Env);
    assert_eq!(filter.value, "C_ENV");
    drop(env);
}

#[test]
fn slack_channel_filter_absent_is_none() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        r#"
        [slack]
        bot_token = "b"
        app_token = "a"
    "#,
        "slack-channel-filter-absent",
    );
    let slack = cfg.slack.expect("slack section present");
    assert!(slack.channel_filter.is_none());
    drop(env);
}

// ------------------------------------------------------------------
// Sender Allowlist Task 1 — team_command_allowed_senders
// ------------------------------------------------------------------

#[test]
fn telegram_team_command_allowed_senders_defaults_to_empty() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        r#"
        [telegram]
        token = "t"
    "#,
        "telegram-allowlist-default",
    );
    let tg = cfg.telegram.expect("telegram section present");
    assert!(tg.team_command_allowed_senders.is_empty());
    drop(env);
}

#[test]
fn telegram_team_command_allowed_senders_round_trips_from_toml() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        r#"
        [telegram]
        token = "t"
        team_command_allowed_senders = [123456789, 987654321]
    "#,
        "telegram-allowlist-round-trip",
    );
    let tg = cfg.telegram.expect("telegram section present");
    assert_eq!(tg.team_command_allowed_senders, vec![123456789, 987654321]);
    drop(env);
}

#[test]
fn discord_and_slack_team_command_allowed_senders_round_trip_from_toml() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        r#"
        [discord]
        token = "t"
        team_command_allowed_senders = [111111111, 222222222]

        [slack]
        bot_token = "b"
        app_token = "a"
        team_command_allowed_senders = ["U012ABCDEF", "U098ZYXWVU"]
    "#,
        "discord-slack-allowlist-round-trip",
    );
    let discord = cfg.discord.expect("discord section present");
    assert_eq!(
        discord.team_command_allowed_senders,
        vec![111111111u64, 222222222u64]
    );
    let slack = cfg.slack.expect("slack section present");
    assert_eq!(
        slack.team_command_allowed_senders,
        vec!["U012ABCDEF".to_string(), "U098ZYXWVU".to_string()]
    );
    drop(env);
}

// ------------------------------------------------------------------
// Phase 3: env-over-TOML precedence
// ------------------------------------------------------------------

#[test]
fn env_beats_toml_when_both_set() {
    let env = EnvScope::new();
    let tmp = TempDir::new("precedence");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[agent]
model = "toml-loses"
[telegram]
token = "toml-token"
chat_id = 1
"#,
    )
    .unwrap();

    env.set("AIVYX_PA_MODEL", "env-wins");
    env.set("AIVYX_PA_TELEGRAM_TOKEN", "env-token");
    env.set("AIVYX_PA_TELEGRAM_CHAT_ID", "2");

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");

    assert_eq!(cfg.model.source, FieldSource::Env);
    assert_eq!(cfg.model.value, "env-wins");
    let tg = cfg.telegram.as_ref().unwrap();
    assert_eq!(tg.token.as_ref().unwrap().source, FieldSource::Env);
    assert_eq!(
        tg.token.as_ref().unwrap().value.expose_secret(),
        "env-token"
    );
    assert_eq!(tg.chat_filter.as_ref().unwrap().source, FieldSource::Env);
    assert_eq!(tg.chat_filter.as_ref().unwrap().value, 2);

    drop(env);
}

// ------------------------------------------------------------------
// Phase 4: defaults
// ------------------------------------------------------------------

#[test]
fn defaults_win_when_no_source_supplies_value() {
    let env = EnvScope::new();
    // HOME is set by EnvScope to $TMPDIR, so fs_root and
    // storage_path's default branches both succeed.
    let cfg = AivyxConfig::load_from_env_and_toml(&LoadOptions::test_env_only())
        .expect("load should succeed with defaults");

    assert!(cfg.anthropic_api_key.is_none());
    assert_eq!(cfg.model.source, FieldSource::Default);
    assert_eq!(cfg.model.value, DEFAULT_MODEL);
    assert_eq!(cfg.system_prompt.source, FieldSource::Default);
    assert_eq!(cfg.system_prompt.value, DEFAULT_SYSTEM_PROMPT);
    assert_eq!(cfg.fs_root.source, FieldSource::Default);
    assert!(cfg.fs_root.value.ends_with("aivyx-pa-sandbox"));
    assert_eq!(cfg.storage_path.source, FieldSource::Default);
    // Default path without XDG_DATA_HOME should end in .local/share/aivyx-pa/store.redb
    assert!(cfg
        .storage_path
        .value
        .to_string_lossy()
        .ends_with(".local/share/aivyx-pa/store.redb"));
    assert_eq!(
        cfg.memory_max_per_topic.source,
        FieldSource::Default
    );
    assert_eq!(
        cfg.memory_max_per_topic.value,
        DEFAULT_MEMORY_MAX_PER_TOPIC
    );
    assert!(cfg.passphrase.is_none());
    assert!(cfg.telegram.is_none());

    // Phase 11 Task 1: the synthesized `default` role's system_prompt
    // should inherit `FieldSource::Default` from the legacy field —
    // a brand-new config with no prompt source should fall through
    // to DEFAULT_SYSTEM_PROMPT wrapped inside the synthesized role.
    let default_role = cfg
        .roles
        .get(DEFAULT_ROLE_NAME)
        .expect("default role synthesized");
    assert_eq!(default_role.system_prompt.source, FieldSource::Default);
    assert_eq!(default_role.system_prompt.value, DEFAULT_SYSTEM_PROMPT);
    // The synthesized `default` role uses AllowAll so pre-Phase-11
    // behavior is preserved: every registered tool is available.
    assert!(matches!(
        default_role.tool_allowlist.value,
        crate::ToolAllowlist::AllowAll
    ));
    // No prefix in the backwards-compat path → identical memory
    // layout to Phase 8–10.
    assert!(default_role.memory_topic_prefix.value.is_none());
    assert_eq!(cfg.active_role.value, DEFAULT_ROLE_NAME);
    assert!(cfg.warnings.is_empty());

    drop(env);
}

/// Chapter Keel — the default system prompt is the operating *charter*,
/// not the old one-line stub. This is a drift guard: it asserts the four
/// invariant pillars (identity, work habits, safety posture, turn
/// discipline) survive future edits to the constant, by keyword presence
/// rather than exact text so the prose can still be reworded freely. It
/// also pins the compactness ceiling so the charter cannot quietly grow
/// large enough to starve a small local model's context.
#[test]
fn default_charter_carries_its_invariant_pillars() {
    let charter = DEFAULT_SYSTEM_PROMPT.to_lowercase();
    let has = |needle: &str| {
        assert!(
            charter.contains(needle),
            "charter missing invariant keyword {needle:?}"
        );
    };

    // Identity + local framing.
    has("aivyx");
    has("locally");

    // Work habits: terse/honest, tool-first, memory, respond-don't-loop.
    has("terse");
    has("tools");
    has("memory");
    has("workspace");
    assert!(
        charter.contains("loop") || charter.contains("repeat a tool call"),
        "charter should discourage looping / repeated tool calls"
    );

    // Safety posture, stated in prose (mirrors docs/SECURITY_POSTURE.md):
    // confirm-first on irreversible/outbound, no self-escalation, audited.
    has("confirm");
    assert!(
        charter.contains("irreversible") || charter.contains("destructive"),
        "charter should name the irreversible/destructive boundary"
    );
    assert!(
        charter.contains("widen") && charter.contains("authority"),
        "charter must state the agent cannot widen its own authority"
    );
    assert!(
        charter.contains("recorded") || charter.contains("tamper-evident"),
        "charter should state actions are recorded"
    );
    // Chapter Bulwark — the prompt-injection pillar: tool/fetched content is
    // untrusted data, not instructions.
    assert!(
        charter.contains("untrusted") && charter.contains("instructions"),
        "charter must state fetched/tool content is untrusted, not instructions"
    );

    // POLISH_WAVES.md sub-project 4, item F — source-currency instinct.
    assert!(
        charter.contains("current") || charter.contains("outdated"),
        "charter should instruct treating undated sources as unverified for currency"
    );

    // Compactness ceiling: keep the always-on base layer small. Raised to
    // 2000 for Chapter Bulwark's prompt-injection pillar ("tool output is
    // untrusted data, not instructions") — a deliberate safety addition, not
    // drift. Raised again to 2150 for POLISH_WAVES.md sub-project 4's
    // one-sentence source-currency addition (measured 2091 bytes) — still a
    // single added sentence, not renewed drift.
    assert!(
        DEFAULT_SYSTEM_PROMPT.len() < 2150,
        "charter grew to {} bytes — keep the always-on base layer compact",
        DEFAULT_SYSTEM_PROMPT.len()
    );
}

#[test]
fn xdg_data_home_wins_over_home_for_storage_default() {
    let env = EnvScope::new();
    env.set("XDG_DATA_HOME", "/tmp/xdg");

    let cfg = AivyxConfig::load_from_env_and_toml(&LoadOptions::test_env_only()).unwrap();
    assert_eq!(
        cfg.storage_path.value,
        PathBuf::from("/tmp/xdg/aivyx-pa/store.redb")
    );
    assert_eq!(cfg.storage_path.source, FieldSource::Default);

    drop(env);
}

#[test]
fn no_home_and_no_fs_root_override_is_typed_error() {
    let env = EnvScope::new();
    env.clear("HOME");
    // fs_root's default needs HOME; unset → NoHome error.
    let err = AivyxConfig::load_from_env_and_toml(&LoadOptions::test_env_only())
        .expect_err("should fail without HOME");
    match err {
        ConfigError::NoHome { field } => assert_eq!(field, "fs_root"),
        other => panic!("expected NoHome, got {other:?}"),
    }

    drop(env);
}

// ------------------------------------------------------------------
// Phase 5: invalid parsing
// ------------------------------------------------------------------

#[test]
fn unparseable_memory_cap_is_typed_invalid_error() {
    let env = EnvScope::new();
    env.set("AIVYX_PA_MEMORY_MAX_PER_TOPIC", "not-a-number");

    let err = AivyxConfig::load_from_env_and_toml(&LoadOptions::test_env_only())
        .expect_err("should fail parsing");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "memory_max_per_topic");
            assert!(
                reason.contains("not a valid usize"),
                "reason should mention usize parse: {reason}"
            );
        }
        other => panic!("expected Invalid, got {other:?}"),
    }

    drop(env);
}

#[test]
fn unparseable_telegram_chat_id_is_typed_invalid_error() {
    let env = EnvScope::new();
    env.set("AIVYX_PA_TELEGRAM_CHAT_ID", "oops");

    let err = AivyxConfig::load_from_env_and_toml(&LoadOptions::test_env_only())
        .expect_err("should fail parsing");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(field, "telegram.chat_id");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }

    drop(env);
}

#[test]
fn malformed_toml_is_typed_parse_error() {
    let env = EnvScope::new();
    let tmp = TempDir::new("malformed");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(&toml_path, "this is : not : valid TOML ][").unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path.clone()),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("should fail");
    match err {
        ConfigError::TomlParse { path, .. } => assert_eq!(path, toml_path),
        other => panic!("expected TomlParse, got {other:?}"),
    }

    drop(env);
}

#[test]
fn missing_toml_file_is_not_an_error() {
    let env = EnvScope::new();
    let tmp = TempDir::new("missing");
    let toml_path = tmp.path().join("does-not-exist.toml");

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts)
        .expect("missing TOML file should load cleanly");
    assert_eq!(cfg.model.source, FieldSource::Default);

    drop(env);
}

// ------------------------------------------------------------------
// Phase 6: validation
// ------------------------------------------------------------------

#[test]
fn validate_errors_when_required_api_key_missing() {
    let env = EnvScope::new();
    let cfg = AivyxConfig::load_from_env_and_toml(&LoadOptions::test_env_only()).unwrap();
    let opts = LoadOptions {
        toml_path: None,
        require_api_key: true,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = cfg.validate(&opts).expect_err("should require api key");
    match err {
        ConfigError::Missing { field } => assert_eq!(field, "anthropic_api_key"),
        other => panic!("expected Missing, got {other:?}"),
    }

    drop(env);
}

#[test]
fn validate_errors_when_required_telegram_token_missing() {
    let env = EnvScope::new();
    // Set only the chat_id — token missing everywhere.
    env.set("AIVYX_PA_TELEGRAM_CHAT_ID", "1");
    let cfg = AivyxConfig::load_from_env_and_toml(&LoadOptions::test_env_only()).unwrap();
    let opts = LoadOptions {
        toml_path: None,
        require_api_key: false,
        require_telegram_token: true,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = cfg.validate(&opts).expect_err("should require token");
    match err {
        ConfigError::Missing { field } => assert_eq!(field, "telegram.token"),
        other => panic!("expected Missing, got {other:?}"),
    }

    drop(env);
}

#[test]
fn validate_succeeds_when_everything_required_is_set() {
    let env = EnvScope::new();
    env.set("ANTHROPIC_API_KEY", "sk-x");
    env.set("AIVYX_PA_TELEGRAM_TOKEN", "t-x");
    let cfg = AivyxConfig::load_from_env_and_toml(&LoadOptions::test_env_only()).unwrap();
    let opts = LoadOptions {
        toml_path: None,
        require_api_key: true,
        require_telegram_token: true,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    cfg.validate(&opts).expect("should validate cleanly");

    drop(env);
}

// ------------------------------------------------------------------
// Phase 7: encrypted-store hydration
// ------------------------------------------------------------------

/// End-to-end: env + TOML do not supply the api key, but the
/// encrypted store has a row for `secret_keys::ANTHROPIC_API_KEY`.
/// After `hydrate_secrets_from_store`, the api key is populated with
/// `FieldSource::EncryptedStore`.
#[tokio::test]
async fn encrypted_store_hydrates_missing_api_key() {
    use aivyx_crypto::MasterKey;
    use aivyx_storage::{KeyDomain, RedbStorage, StorageConfig};

    let env = EnvScope::new();
    // Make sure no env source competes with the store.
    env.clear("ANTHROPIC_API_KEY");

    let tmp = TempDir::new("store-hydrate");
    let store_path = tmp.path().join("store.redb");

    // `MasterKey::from_raw` is the documented test-only fast path
    // (see `aivyx-crypto` lib.rs doc on `MasterKey::from_raw`). Using
    // a hard-coded 32-byte array keeps tests in milliseconds instead
    // of seconds — Argon2id even with `weak_for_tests()` is ~10ms per
    // derivation and we do not need any of its security properties
    // to round-trip a byte row through `KeyDomain::Secrets`.
    let master = MasterKey::from_raw([7u8; 32]);

    let storage = RedbStorage::open(StorageConfig::new(store_path), master)
        .await
        .expect("open store");

    // Seed the secrets domain with a known api key.
    let secrets = storage.domain(KeyDomain::Secrets);
    secrets
        .put(crate::secret_keys::ANTHROPIC_API_KEY, b"sk-from-store")
        .await
        .expect("put api key");

    // Load env+TOML → api key is None.
    let mut cfg =
        AivyxConfig::load_from_env_and_toml(&LoadOptions::test_env_only()).expect("load");
    assert!(cfg.anthropic_api_key.is_none());

    // Hydrate from the store → api key is now populated.
    cfg.hydrate_secrets_from_store(&storage)
        .await
        .expect("hydrate");
    let api_key = cfg
        .anthropic_api_key
        .as_ref()
        .expect("hydrated from store");
    assert_eq!(api_key.source, FieldSource::EncryptedStore);
    assert_eq!(api_key.value.expose_secret(), "sk-from-store");

    // Validate with required_api_key = true now succeeds.
    let opts = LoadOptions {
        toml_path: None,
        require_api_key: true,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    cfg.validate(&opts).expect("api key present after hydration");

    drop(env);
}

/// Env already supplies the api key → hydrate_secrets_from_store must
/// not overwrite it even if the store has a different row. Precedence
/// is env > toml > store.
#[tokio::test]
async fn env_wins_over_encrypted_store_on_hydrate() {
    use aivyx_crypto::MasterKey;
    use aivyx_storage::{KeyDomain, RedbStorage, StorageConfig};

    let env = EnvScope::new();
    env.set("ANTHROPIC_API_KEY", "sk-from-env");

    let tmp = TempDir::new("store-vs-env");
    let store_path = tmp.path().join("store.redb");
    let master = MasterKey::from_raw([8u8; 32]);
    let storage = RedbStorage::open(StorageConfig::new(store_path), master)
        .await
        .unwrap();
    let secrets = storage.domain(KeyDomain::Secrets);
    secrets
        .put(crate::secret_keys::ANTHROPIC_API_KEY, b"sk-store-loses")
        .await
        .unwrap();

    let mut cfg = AivyxConfig::load_from_env_and_toml(&LoadOptions::test_env_only()).unwrap();
    cfg.hydrate_secrets_from_store(&storage).await.unwrap();

    let api_key = cfg.anthropic_api_key.as_ref().unwrap();
    assert_eq!(api_key.source, FieldSource::Env);
    assert_eq!(api_key.value.expose_secret(), "sk-from-env");

    drop(env);
}

/// A non-UTF-8 secret row surfaces as typed `NonUtf8Secret`.
#[tokio::test]
async fn non_utf8_secret_in_store_is_typed_error() {
    use aivyx_crypto::MasterKey;
    use aivyx_storage::{KeyDomain, RedbStorage, StorageConfig};

    let env = EnvScope::new();
    let tmp = TempDir::new("store-nonutf8");
    let store_path = tmp.path().join("store.redb");
    let master = MasterKey::from_raw([9u8; 32]);
    let storage = RedbStorage::open(StorageConfig::new(store_path), master)
        .await
        .unwrap();
    let secrets = storage.domain(KeyDomain::Secrets);
    // invalid UTF-8: a stray high byte without a valid continuation
    secrets
        .put(crate::secret_keys::ANTHROPIC_API_KEY, &[0xff, 0xfe, 0xfd])
        .await
        .unwrap();

    let mut cfg = AivyxConfig::load_from_env_and_toml(&LoadOptions::test_env_only()).unwrap();
    let err = cfg
        .hydrate_secrets_from_store(&storage)
        .await
        .expect_err("should reject non-UTF-8");
    match err {
        ConfigError::NonUtf8Secret { field } => assert_eq!(field, "anthropic_api_key"),
        other => panic!("expected NonUtf8Secret, got {other:?}"),
    }

    drop(env);
}

// ------------------------------------------------------------------
// Phase 11 Task 1 — Role primitive
// ------------------------------------------------------------------
//
// These tests cover the new `Role` / `ToolAllowlist` types, the
// `[[role]]` TOML schema, the implicit-`default`-role backwards-
// compatibility bridge, active-role resolution priority, and the
// `UnknownRole` / Q4 warning error-handling paths.
//
// The decisions pinned by these tests:
//   - Q3 resolution: absent `tool_allowlist` → `AllowAll`,
//     explicit empty `tool_allowlist = []` → `Only(vec![])` (deny all).
//   - Q4 resolution (Option B): a config with both legacy
//     `system_prompt` *and* explicit `[[role]]` entries loads
//     successfully with a non-fatal warning on `AivyxConfig::warnings`.
//   - Active-role priority: `LoadOptions::role_override` > `AIVYX_PA_ROLE`
//     env var > `DEFAULT_ROLE_NAME`.
//   - Backwards compat: zero `[[role]]` entries synthesize an implicit
//     `default` role that inherits the legacy `system_prompt`'s
//     original `FieldSource` so provenance rendering is preserved.

/// Two explicit `[[role]]` entries in the TOML file load into the
/// `roles` map, keyed by name. Each field is parsed into the runtime
/// types with `FieldSource::Toml` wrappers; absent fields fall back
/// to defaults. Also checks the deterministic `BTreeMap` ordering
/// that the module docstring promises.
#[test]
fn explicit_roles_from_toml_parse_into_role_map() {
    let env = EnvScope::new();
    let tmp = TempDir::new("roles-toml");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[[role]]
name = "coder"
system_prompt = "You are a pair-programmer."
tool_allowlist = ["fs.read", "fs.write", "memory.read", "memory.write", "shell.exec"]
memory_topic_prefix = "coder/"

[[role]]
name = "researcher"
system_prompt = "You are a careful note-taker."
tool_allowlist = ["fs.read", "memory.read", "memory.write"]
memory_topic_prefix = "researcher/"
"#,
    )
    .unwrap();

    // Select `coder` explicitly via env var so the load succeeds;
    // neither `coder` nor `researcher` is the default role name so
    // omitting the selector would fail `UnknownRole`.
    env.set("AIVYX_PA_ROLE", "coder");

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");

    assert_eq!(cfg.roles.len(), 2);
    let coder = cfg.roles.get("coder").expect("coder role present");
    assert_eq!(coder.name.value, "coder");
    assert_eq!(coder.name.source, FieldSource::Toml);
    assert_eq!(coder.system_prompt.source, FieldSource::Toml);
    assert_eq!(coder.system_prompt.value, "You are a pair-programmer.");
    match &coder.tool_allowlist.value {
        ToolAllowlist::Only(tools) => {
            assert_eq!(
                tools,
                &vec![
                    "fs.read".to_string(),
                    "fs.write".to_string(),
                    "memory.read".to_string(),
                    "memory.write".to_string(),
                    "shell.exec".to_string(),
                ]
            );
        }
        other => panic!("expected Only(_), got {other:?}"),
    }
    assert_eq!(coder.memory_topic_prefix.value.as_deref(), Some("coder/"));

    let researcher = cfg
        .roles
        .get("researcher")
        .expect("researcher role present");
    assert_eq!(researcher.name.value, "researcher");
    match &researcher.tool_allowlist.value {
        ToolAllowlist::Only(tools) => assert_eq!(tools.len(), 3),
        other => panic!("expected Only(_), got {other:?}"),
    }
    assert_eq!(
        researcher.memory_topic_prefix.value.as_deref(),
        Some("researcher/")
    );

    // BTreeMap ordering — iteration is alphabetical by key.
    let names: Vec<&str> = cfg.roles.keys().map(String::as_str).collect();
    assert_eq!(names, vec!["coder", "researcher"]);

    // Active role reflects the AIVYX_PA_ROLE env var.
    assert_eq!(cfg.active_role.value, "coder");
    assert_eq!(cfg.active_role.source, FieldSource::Env);

    drop(env);
}

/// Q3 resolution — a `[[role]]` entry with no `tool_allowlist` key
/// at all maps to `ToolAllowlist::AllowAll` (no filter, every
/// registered tool available).
#[test]
fn role_without_tool_allowlist_key_is_allow_all() {
    let env = EnvScope::new();
    let tmp = TempDir::new("role-allow-all");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[[role]]
name = "open"
system_prompt = "no allowlist key at all"
"#,
    )
    .unwrap();
    env.set("AIVYX_PA_ROLE", "open");

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    let role = cfg.roles.get("open").unwrap();
    assert_eq!(role.tool_allowlist.value, ToolAllowlist::AllowAll);
    // `AllowAll` came from the default branch (field absent), not
    // from TOML-supplied source.
    assert_eq!(role.tool_allowlist.source, FieldSource::Default);

    drop(env);
}

/// Q3 resolution — a `[[role]]` entry with `tool_allowlist = []`
/// (explicit empty list) maps to `ToolAllowlist::Only(vec![])`,
/// meaning "deny every tool." This is a legal configuration
/// distinct from an absent field.
#[test]
fn role_with_empty_tool_allowlist_is_deny_all() {
    let env = EnvScope::new();
    let tmp = TempDir::new("role-deny-all");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[[role]]
name = "locked"
system_prompt = "deny-all role"
tool_allowlist = []
"#,
    )
    .unwrap();
    env.set("AIVYX_PA_ROLE", "locked");

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    let role = cfg.roles.get("locked").unwrap();
    match &role.tool_allowlist.value {
        ToolAllowlist::Only(v) => assert!(v.is_empty(), "explicit empty list"),
        other => panic!("expected Only(empty), got {other:?}"),
    }
    // An explicit empty list is `Toml`-sourced, not `Default`.
    assert_eq!(role.tool_allowlist.source, FieldSource::Toml);

    drop(env);
}

/// Active-role priority: `LoadOptions::role_override` beats the
/// `AIVYX_PA_ROLE` env var.
#[test]
fn role_override_beats_env_var() {
    let env = EnvScope::new();
    let tmp = TempDir::new("role-override");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[[role]]
name = "first"
system_prompt = "first"

[[role]]
name = "second"
system_prompt = "second"
"#,
    )
    .unwrap();
    env.set("AIVYX_PA_ROLE", "first");

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        // Override wins even though the env var says "first".
        role_override: Some("second".to_string()),
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.active_role.value, "second");

    drop(env);
}

/// Active-role priority: when `LoadOptions::role_override` is `None`
/// the `AIVYX_PA_ROLE` env var is honored.
#[test]
fn env_var_sets_active_role_when_no_override() {
    let env = EnvScope::new();
    let tmp = TempDir::new("role-env");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[[role]]
name = "primary"
system_prompt = "primary"

[[role]]
name = "secondary"
system_prompt = "secondary"
"#,
    )
    .unwrap();
    env.set("AIVYX_PA_ROLE", "secondary");

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.active_role.value, "secondary");
    assert_eq!(cfg.active_role.source, FieldSource::Env);

    drop(env);
}

/// Selecting an active role that doesn't exist in the loaded config
/// is a typed `UnknownRole` error; the error includes the list of
/// known role names so operators can diagnose the typo.
#[test]
fn active_role_not_in_config_is_typed_unknown_role_error() {
    let env = EnvScope::new();
    let tmp = TempDir::new("role-unknown");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[[role]]
name = "real-role"
system_prompt = "real"
"#,
    )
    .unwrap();
    env.set("AIVYX_PA_ROLE", "ghost-role");

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("ghost-role should fail active-role validation");
    match err {
        ConfigError::UnknownRole { name, known } => {
            assert_eq!(name, "ghost-role");
            assert_eq!(known, vec!["real-role".to_string()]);
        }
        other => panic!("expected UnknownRole, got {other:?}"),
    }

    drop(env);
}

/// Q4 resolution — a config with both a legacy env-sourced
/// `system_prompt` *and* an explicit `[[role]]` entry loads
/// successfully. `AivyxConfig::warnings` accumulates a clear
/// message; the runtime behavior prefers the explicit role.
#[test]
fn legacy_system_prompt_with_explicit_roles_warns_but_loads() {
    let env = EnvScope::new();
    let tmp = TempDir::new("role-legacy-conflict");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[[role]]
name = "explicit"
system_prompt = "from the explicit role"
"#,
    )
    .unwrap();
    // Legacy env-level prompt — user forgot to remove it when
    // adopting roles.
    env.set("AIVYX_PA_SYSTEM_PROMPT", "legacy value that will be shadowed");
    env.set("AIVYX_PA_ROLE", "explicit");

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load with conflict");
    // Legacy field is still populated for the banner's sake.
    assert_eq!(
        cfg.system_prompt.value,
        "legacy value that will be shadowed"
    );
    // The explicit role's prompt is what Task 4 will consume.
    let role = cfg.roles.get("explicit").unwrap();
    assert_eq!(role.system_prompt.value, "from the explicit role");
    // Exactly one warning was emitted, and it mentions the legacy
    // field so the operator can find it.
    assert_eq!(
        cfg.warnings.len(),
        1,
        "one warning about the legacy field: got {:?}",
        cfg.warnings
    );
    assert!(cfg.warnings[0].contains("system_prompt"));

    drop(env);
}

/// Q4 refinement — when the legacy `system_prompt` is sourced from
/// `FieldSource::Default` (no user source supplied it), adding an
/// explicit `[[role]]` entry must **not** fire the warning. A brand-
/// new role-using config should be silent.
#[test]
fn explicit_role_with_default_legacy_prompt_emits_no_warning() {
    let env = EnvScope::new();
    let tmp = TempDir::new("role-silent");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[[role]]
name = "alpha"
system_prompt = "brand new role, no legacy baggage"
"#,
    )
    .unwrap();
    // Deliberately DO NOT set AIVYX_PA_SYSTEM_PROMPT. The legacy field
    // falls through to DEFAULT_SYSTEM_PROMPT with source `Default`,
    // which must not trip the warning.
    env.set("AIVYX_PA_ROLE", "alpha");

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.system_prompt.source, FieldSource::Default);
    assert!(
        cfg.warnings.is_empty(),
        "no warning when legacy prompt source is Default: {:?}",
        cfg.warnings
    );

    drop(env);
}

/// Rename clean-break — a config carrying the pre-rename `[aivyx]`
/// section (with no `[aivyx_pa]` present) still loads successfully
/// (TOML happily parses an unrecognized table), but must accumulate a
/// loud, specific warning naming the exact problem: the old section's
/// passphrase was NOT read. Silent loss of a security-relevant field
/// is exactly what Finding 2 exists to prevent.
#[test]
fn legacy_aivyx_section_warns_that_passphrase_was_not_read() {
    let env = EnvScope::new();
    let tmp = TempDir::new("legacy-aivyx-section");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[aivyx]
passphrase = "old-section-passphrase-never-read"
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts)
        .expect("legacy [aivyx] section is structurally legal TOML, load must succeed");

    // The old section's passphrase must NOT have been read.
    assert!(
        cfg.passphrase.is_none(),
        "the legacy [aivyx] section's passphrase must not be read into \
         the runtime config"
    );

    // Exactly one warning fires, and it names both the old and new
    // section names plus the fact that the passphrase was dropped.
    assert_eq!(
        cfg.warnings.len(),
        1,
        "expected exactly one warning about the legacy [aivyx] section: got {:?}",
        cfg.warnings
    );
    assert!(cfg.warnings[0].contains("[aivyx]"), "{:?}", cfg.warnings);
    assert!(cfg.warnings[0].contains("[aivyx_pa]"), "{:?}", cfg.warnings);
    assert!(
        cfg.warnings[0].contains("NOT read"),
        "{:?}",
        cfg.warnings
    );

    drop(env);
}

/// The `[aivyx_pa]` section (the current, correct name) must never
/// trip the legacy-section warning on its own.
#[test]
fn current_aivyx_pa_section_emits_no_legacy_warning() {
    let env = EnvScope::new();
    let tmp = TempDir::new("current-aivyx-pa-section");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[aivyx_pa]
passphrase = "correctly-named-section"
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert!(
        cfg.passphrase.is_some(),
        "the [aivyx_pa] section's passphrase must be read"
    );
    assert!(
        cfg.warnings.is_empty(),
        "no legacy-section warning expected: {:?}",
        cfg.warnings
    );

    drop(env);
}

/// A `Role` parsed from TOML that omits both `system_prompt` and
/// `memory_topic_prefix` still loads. The omitted fields fall
/// through to `Default`-sourced values on the runtime `Role`.
#[test]
fn role_with_only_name_populates_defaults_for_optional_fields() {
    let env = EnvScope::new();
    let tmp = TempDir::new("role-minimal");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[[role]]
name = "bare"
"#,
    )
    .unwrap();
    env.set("AIVYX_PA_ROLE", "bare");

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    let role = cfg.roles.get("bare").unwrap();
    assert_eq!(role.system_prompt.source, FieldSource::Default);
    assert_eq!(role.system_prompt.value, DEFAULT_SYSTEM_PROMPT);
    assert!(matches!(
        role.tool_allowlist.value,
        ToolAllowlist::AllowAll
    ));
    assert_eq!(role.tool_allowlist.source, FieldSource::Default);
    assert!(role.memory_topic_prefix.value.is_none());
    assert_eq!(role.memory_topic_prefix.source, FieldSource::Default);

    drop(env);
}

/// Public-API smoke test: the `Role` struct's fields are all public
/// and the `ToolAllowlist` variants are all constructible from
/// outside `aivyx-config`. Task 4 will consume these via the
/// `aivyx-channel` binary; this test proves the API surface supports
/// that consumption pattern without any unexposed internals.
///
/// Phase 13 Task 1 extended `Role` with three more fields
/// (`capability_scopes`, `trust_ceiling`, `parent_role`). This test
/// now constructs them inline as well, asserting the whole struct
/// remains exhaustively literal-constructible from outside the crate.
#[test]
fn role_struct_is_constructible_and_matchable_from_outside() {
    use aivyx_capability::TrustTier;

    let role = Role {
        name: crate::Sourced::new("test".to_string(), FieldSource::Default),
        system_prompt: crate::Sourced::new("sp".to_string(), FieldSource::Default),
        tool_allowlist: crate::Sourced::new(
            ToolAllowlist::Only(vec!["fs.read".to_string()]),
            FieldSource::Default,
        ),
        memory_topic_prefix: crate::Sourced::new(Some("x/".to_string()), FieldSource::Default),
        capability_scopes: crate::Sourced::new(Vec::new(), FieldSource::Default),
        trust_ceiling: crate::Sourced::new(TrustTier::Trusted, FieldSource::Default),
        parent_role: crate::Sourced::new(None, FieldSource::Default),
    };
    // The match is exhaustive against the public enum — if Task 3 or
    // a later task ever adds a variant, this test exists to catch
    // the surface change at the `_ => unreachable!()` alternative
    // missing.
    let behavior = match &role.tool_allowlist.value {
        ToolAllowlist::AllowAll => "no filter",
        ToolAllowlist::Only(_) => "filtered",
    };
    assert_eq!(behavior, "filtered");
    assert_eq!(role.name.value, "test");
    assert!(role.capability_scopes.value.is_empty());
    assert_eq!(role.trust_ceiling.value, TrustTier::Trusted);
    assert!(role.parent_role.value.is_none());
}

// ====================================================================
// Phase 13 Task 1 — per-role capability envelope fields
// ====================================================================
//
// These tests exercise the three fields added to `Role` in Phase 13
// Task 1 (`capability_scopes`, `trust_ceiling`, `parent_role`) plus
// the single-inheritance tree validator that runs at config-load
// time. Each test names the invariant it locks in so a future
// refactor that breaks one knows which contract it just violated.

/// Phase 11 backcompat — a TOML file with one explicit `[[role]]`
/// entry that touches none of the three new Phase 13 fields still
/// loads. The new fields populate from their absent-key defaults:
/// empty `capability_scopes`, `Trusted` ceiling, and `parent_role =
/// None` (because no `default` role exists in the same file to
/// implicit-parent against — Q4's "implicit-from-default only when
/// default is declared" rule).
#[test]
fn legacy_role_loads_with_default_capability_envelope() {
    use aivyx_capability::TrustTier;

    let env = EnvScope::new();
    let tmp = TempDir::new("phase13-legacy-role");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[[role]]
name = "coder"
system_prompt = "You are a pair-programmer."
"#,
    )
    .unwrap();
    env.set("AIVYX_PA_ROLE", "coder");

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");

    let role = cfg.roles.get("coder").expect("coder role present");
    assert!(
        role.capability_scopes.value.is_empty(),
        "absent capability_scopes key → empty Vec"
    );
    assert_eq!(role.capability_scopes.source, FieldSource::Default);
    assert_eq!(role.trust_ceiling.value, TrustTier::Trusted);
    assert_eq!(role.trust_ceiling.source, FieldSource::Default);
    assert!(
        role.parent_role.value.is_none(),
        "no `default` role declared → this role is its own tree root"
    );
    assert_eq!(role.parent_role.source, FieldSource::Default);

    drop(env);
}

/// An explicit `capability_scopes = ["fs.read", "shell.exec:git"]`
/// list parses through `Scope::parse` and lands as
/// `Sourced::new(Vec<Scope>, FieldSource::Toml)`. Locks in (a) the
/// scope-string-parsing-at-config-load-time decision from Q2,
/// (b) the `FieldSource::Toml` provenance for explicit lists, and
/// (c) the round-trip through `Scope::as_str` so the in-memory
/// `Scope` holds the original string verbatim.
#[test]
fn explicit_capability_scopes_parse_at_load_time() {
    let env = EnvScope::new();
    let tmp = TempDir::new("phase13-scopes");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[[role]]
name = "shellrunner"
system_prompt = "shell role"
capability_scopes = ["fs.read", "shell.exec:git", "memory.write"]
"#,
    )
    .unwrap();
    env.set("AIVYX_PA_ROLE", "shellrunner");

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");

    let role = cfg.roles.get("shellrunner").unwrap();
    assert_eq!(role.capability_scopes.source, FieldSource::Toml);
    let scope_strings: Vec<&str> = role
        .capability_scopes
        .value
        .iter()
        .map(|s| s.as_str())
        .collect();
    assert_eq!(scope_strings, vec!["fs.read", "shell.exec:git", "memory.write"]);

    drop(env);
}

/// An unknown scope base in `capability_scopes` (not in
/// `KNOWN_BASES`) fails loudly at config-load time, not at
/// capability-check time later. The error message names the role
/// and the bad scope string so the operator can grep their TOML.
#[test]
fn unknown_capability_scope_fails_loudly_at_load_time() {
    let env = EnvScope::new();
    let tmp = TempDir::new("phase13-bad-scope");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[[role]]
name = "broken"
system_prompt = "broken"
capability_scopes = ["fs.read", "this.is.not.a.real.base"]
"#,
    )
    .unwrap();
    env.set("AIVYX_PA_ROLE", "broken");

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("unknown scope should fail load");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "role.capability_scopes");
            assert!(reason.contains("broken"), "mentions role name: {reason}");
            assert!(
                reason.contains("this.is.not.a.real.base"),
                "mentions bad scope: {reason}"
            );
        }
        other => panic!("expected Invalid, got {other:?}"),
    }

    drop(env);
}

/// All four `TrustTier` variants parse from TOML strings (via
/// `serde::Deserialize` derived on `TrustTier` in
/// `aivyx-capability`), and an unknown variant fails with a
/// `TomlParse` error pointing at the offending file. Locks in that
/// trust-tier validation is a TOML-parse-time concern, not a
/// post-parse loader concern — typos surface with file+line context.
#[test]
fn trust_ceiling_parses_all_four_tiers_and_rejects_garbage() {
    use aivyx_capability::TrustTier;

    let env = EnvScope::new();
    for (tier_str, expected) in [
        ("Kernel", TrustTier::Kernel),
        ("Trusted", TrustTier::Trusted),
        ("SemiTrusted", TrustTier::SemiTrusted),
        ("Untrusted", TrustTier::Untrusted),
    ] {
        let tmp = TempDir::new(&format!("phase13-tier-{tier_str}"));
        let toml_path = tmp.path().join("aivyx-pa.toml");
        std::fs::write(
            &toml_path,
            format!(
                r#"
[[role]]
name = "tiered"
system_prompt = "tier check"
trust_ceiling = "{tier_str}"
"#
            ),
        )
        .unwrap();
        env.set("AIVYX_PA_ROLE", "tiered");

        let opts = LoadOptions {
            toml_path: Some(toml_path),
            require_api_key: false,
            require_telegram_token: false,
            require_discord_token: false,
            require_slack_tokens: false,
            role_override: None,
        };
        let cfg = AivyxConfig::load_from_env_and_toml(&opts)
            .unwrap_or_else(|e| panic!("load {tier_str}: {e:?}"));
        let role = cfg.roles.get("tiered").unwrap();
        assert_eq!(role.trust_ceiling.value, expected);
        assert_eq!(role.trust_ceiling.source, FieldSource::Toml);
    }

    // Garbage tier name surfaces as a TomlParse error (serde
    // rejects the unknown variant during `toml::from_str`).
    let tmp = TempDir::new("phase13-tier-garbage");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[[role]]
name = "tiered"
system_prompt = "tier check"
trust_ceiling = "Goat"
"#,
    )
    .unwrap();
    env.set("AIVYX_PA_ROLE", "tiered");
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("garbage tier should fail");
    assert!(
        matches!(err, ConfigError::TomlParse { .. }),
        "expected TomlParse, got {err:?}"
    );

    drop(env);
}

/// `parent_role` must name an existing role. A typo (or a renamed
/// role that some other entry still points at) fails loudly with
/// `RoleInheritance`, naming the offending role *and* the missing
/// parent string.
#[test]
fn parent_role_pointing_at_unknown_role_fails_loudly() {
    let env = EnvScope::new();
    let tmp = TempDir::new("phase13-parent-typo");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[[role]]
name = "default"
system_prompt = "root"

[[role]]
name = "child"
system_prompt = "child"
parent_role = "no-such-role"
"#,
    )
    .unwrap();
    env.set("AIVYX_PA_ROLE", "child");

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("unknown parent should fail");
    match err {
        ConfigError::RoleInheritance { reason } => {
            assert!(reason.contains("child"), "mentions child: {reason}");
            assert!(
                reason.contains("no-such-role"),
                "mentions missing parent: {reason}"
            );
        }
        other => panic!("expected RoleInheritance, got {other:?}"),
    }

    drop(env);
}

/// A `parent_role` cycle (A → B → A) is detected at load time and
/// surfaces as `RoleInheritance`. Locks in invariant 3 of the
/// single-inheritance tree validator. Includes a self-cycle as a
/// sub-case because self-cycles are the degenerate path through
/// the same code.
#[test]
fn parent_role_cycle_is_detected_at_load_time() {
    // --- self-cycle (A → A) ---
    {
        let env = EnvScope::new();
        let tmp = TempDir::new("phase13-self-cycle");
        let toml_path = tmp.path().join("aivyx-pa.toml");
        std::fs::write(
            &toml_path,
            r#"
[[role]]
name = "selfish"
system_prompt = "self-loop"
parent_role = "selfish"
"#,
        )
        .unwrap();
        env.set("AIVYX_PA_ROLE", "selfish");
        let opts = LoadOptions {
            toml_path: Some(toml_path),
            require_api_key: false,
            require_telegram_token: false,
            require_discord_token: false,
            require_slack_tokens: false,
            role_override: None,
        };
        let err = AivyxConfig::load_from_env_and_toml(&opts)
            .expect_err("self-cycle should fail");
        match err {
            ConfigError::RoleInheritance { reason } => {
                assert!(reason.contains("selfish"), "mentions role: {reason}");
                assert!(
                    reason.contains("self-cycle") || reason.contains("itself"),
                    "names the failure mode: {reason}"
                );
            }
            other => panic!("expected RoleInheritance, got {other:?}"),
        }
        drop(env);
    }

    // --- two-hop cycle (A → B → A) ---
    {
        let env = EnvScope::new();
        let tmp = TempDir::new("phase13-two-hop-cycle");
        let toml_path = tmp.path().join("aivyx-pa.toml");
        std::fs::write(
            &toml_path,
            r#"
[[role]]
name = "a"
system_prompt = "a"
parent_role = "b"

[[role]]
name = "b"
system_prompt = "b"
parent_role = "a"
"#,
        )
        .unwrap();
        env.set("AIVYX_PA_ROLE", "a");
        let opts = LoadOptions {
            toml_path: Some(toml_path),
            require_api_key: false,
            require_telegram_token: false,
            require_discord_token: false,
            require_slack_tokens: false,
            role_override: None,
        };
        let err = AivyxConfig::load_from_env_and_toml(&opts)
            .expect_err("two-hop cycle should fail");
        match err {
            ConfigError::RoleInheritance { reason } => {
                assert!(reason.contains("cycle"), "mentions cycle: {reason}");
            }
            other => panic!("expected RoleInheritance, got {other:?}"),
        }
        drop(env);
    }
}

// ====================================================================
// Phase 13 Task 2 — child-parent attenuation invariant (invariant 5)
// ====================================================================

/// PRODUCT.md P7's "child can attenuate, never widen" rule fires
/// at config-load time when a child role declares a
/// `capability_scopes` entry that its constraining ancestor does
/// not grant. The error message names both the child role, the
/// offending scope string, and the constraining ancestor whose
/// declared scopes failed to grant it.
#[test]
fn child_role_widening_parent_envelope_fails_at_load_time() {
    let env = EnvScope::new();
    let tmp = TempDir::new("phase13t2-widen");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[[role]]
name = "default"
system_prompt = "root"
capability_scopes = ["fs.read"]

[[role]]
name = "rogue"
system_prompt = "tries to widen"
parent_role = "default"
capability_scopes = ["fs.read", "shell.exec"]
"#,
    )
    .unwrap();
    env.set("AIVYX_PA_ROLE", "rogue");

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("widening child should fail");
    match err {
        ConfigError::RoleInheritance { reason } => {
            assert!(reason.contains("rogue"), "names child: {reason}");
            assert!(
                reason.contains("shell.exec"),
                "names offending scope: {reason}"
            );
            assert!(
                reason.contains("default"),
                "names constraining ancestor: {reason}"
            );
        }
        other => panic!("expected RoleInheritance, got {other:?}"),
    }

    drop(env);
}

/// Empty `capability_scopes` is the unconstrained sentinel: a
/// role with no declared scopes adds no constraint, and the
/// attenuation walk skips through it to find the next non-empty
/// ancestor. This pins that a `grandparent → empty parent →
/// child` chain validates the child against the **grandparent's**
/// scopes, not the parent's empty set (which would otherwise
/// either pass everything or fail everything depending on edge
/// behavior).
#[test]
fn attenuation_walk_skips_empty_parent_to_grandparent() {
    let env = EnvScope::new();
    let tmp = TempDir::new("phase13t2-skip-empty");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    // grandparent: fs.read only
    // parent: empty (sentinel — adds nothing)
    // child: tries to declare net.fetch
    // Expected: fail, because grandparent doesn't grant net.fetch
    // and the walk skips through the empty parent.
    std::fs::write(
        &toml_path,
        r#"
[[role]]
name = "grandparent"
system_prompt = "gp"
capability_scopes = ["fs.read"]

[[role]]
name = "parent"
system_prompt = "p"
parent_role = "grandparent"

[[role]]
name = "child"
system_prompt = "c"
parent_role = "parent"
capability_scopes = ["net.fetch"]
"#,
    )
    .unwrap();
    env.set("AIVYX_PA_ROLE", "child");

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("widening through empty parent should fail");
    match err {
        ConfigError::RoleInheritance { reason } => {
            assert!(reason.contains("child"), "names child: {reason}");
            assert!(reason.contains("net.fetch"), "names scope: {reason}");
            assert!(
                reason.contains("grandparent"),
                "constraining ancestor is grandparent, not parent: {reason}"
            );
        }
        other => panic!("expected RoleInheritance, got {other:?}"),
    }

    drop(env);
}

/// D4 prefix-attenuation under inheritance: a child declaring
/// `fs.read:/tmp/**` under a parent declaring unqualified
/// `fs.read` loads cleanly, because Rule 2 ("unqualified held
/// grants any qualified needed with the same base") makes the
/// parent's unqualified scope grant the child's narrow one. This
/// is the load-time analog of the runtime intersection behavior
/// — both routes (config validation, runtime envelope assembly)
/// agree on what counts as "child can attenuate."
#[test]
fn child_qualifier_under_unqualified_parent_loads_cleanly() {
    let env = EnvScope::new();
    let tmp = TempDir::new("phase13t2-qualifier-attenuation");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[[role]]
name = "default"
system_prompt = "broad parent"
capability_scopes = ["fs.read", "fs.write"]

[[role]]
name = "narrow"
system_prompt = "narrowed child"
parent_role = "default"
capability_scopes = ["fs.read:/etc/**"]
"#,
    )
    .unwrap();
    env.set("AIVYX_PA_ROLE", "narrow");

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts)
        .expect("qualifier attenuation should load");
    let role = cfg.roles.get("narrow").unwrap();
    assert_eq!(role.capability_scopes.value.len(), 1);
    assert_eq!(
        role.capability_scopes.value[0].as_str(),
        "fs.read:/etc/**"
    );
}

/// Q4 ergonomic — when an explicit `default` role is declared
/// alongside other roles, those other roles implicitly inherit
/// from `default` (with `FieldSource::Default` provenance, since
/// no operator wrote `parent_role = "default"` literally).
/// This locks in the "implicit-from-default *when default exists*"
/// half of Q4 — the other half (no-default → root) is locked in
/// by `legacy_role_loads_with_default_capability_envelope`.
#[test]
fn implicit_parent_default_kicks_in_when_default_role_is_declared() {
    let env = EnvScope::new();
    let tmp = TempDir::new("phase13-implicit-parent");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[[role]]
name = "default"
system_prompt = "root prompt"

[[role]]
name = "coder"
system_prompt = "coder prompt"
"#,
    )
    .unwrap();
    env.set("AIVYX_PA_ROLE", "coder");

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");

    let default_role = cfg.roles.get("default").unwrap();
    assert!(
        default_role.parent_role.value.is_none(),
        "default role is its own root"
    );

    let coder = cfg.roles.get("coder").unwrap();
    assert_eq!(
        coder.parent_role.value.as_deref(),
        Some("default"),
        "coder implicitly inherits from default"
    );
    assert_eq!(
        coder.parent_role.source,
        FieldSource::Default,
        "implicit parent has Default source — no operator typed it"
    );

    drop(env);
}

// ------------------------------------------------------------------
// Phase 24: [[mcp_server]] config entries
// ------------------------------------------------------------------

#[test]
fn mcp_server_entries_parse_from_toml() {
    let env = EnvScope::new();
    let tmp = TempDir::new("mcp-cfg");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[mcp_server]]
name = "github"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]

[[mcp_server]]
name = "disabled-one"
command = "echo"
enabled = false

[[mcp_server]]
name = "bare"
command = "/usr/bin/my-server"
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");

    assert_eq!(cfg.mcp_servers.len(), 2, "disabled server filtered out");

    let gh = &cfg.mcp_servers[0];
    assert_eq!(gh.name, "github");
    assert_eq!(gh.transport, McpTransportKind::Stdio);
    assert_eq!(gh.command.as_deref(), Some("npx"));
    assert_eq!(gh.args, vec!["-y", "@modelcontextprotocol/server-github"]);
    assert!(gh.enabled);

    let bare = &cfg.mcp_servers[1];
    assert_eq!(bare.name, "bare");
    assert_eq!(bare.transport, McpTransportKind::Stdio);
    assert_eq!(bare.command.as_deref(), Some("/usr/bin/my-server"));
    assert!(bare.args.is_empty(), "absent args default to empty vec");
    assert!(bare.enabled);

    drop(env);
}

// Chapter Conduit (CD.1) — `[[mcp_server]] env` + `${VAR}` interpolation.

#[test]
fn mcp_server_env_literals_and_interpolation() {
    let env = EnvScope::new();
    env.set("CONDUIT_TEST_TOKEN", "ghp_secret123");
    let tmp = TempDir::new("mcp-env");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[mcp_server]]
name = "github"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_PERSONAL_ACCESS_TOKEN = "${CONDUIT_TEST_TOKEN}", LOG = "debug" }
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    let gh = &cfg.mcp_servers[0];
    // Sorted by key: GITHUB_PERSONAL_ACCESS_TOKEN before LOG.
    assert_eq!(
        gh.env,
        vec![
            ("GITHUB_PERSONAL_ACCESS_TOKEN".to_string(), "ghp_secret123".to_string()),
            ("LOG".to_string(), "debug".to_string()),
        ],
        "${{VAR}} resolved from the daemon env; literal kept; sorted by key",
    );
    drop(env);
}

#[test]
fn mcp_server_env_unset_var_is_a_config_error() {
    let env = EnvScope::new();
    let tmp = TempDir::new("mcp-env-unset");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[mcp_server]]
name = "github"
command = "npx"
env = { TOKEN = "${CONDUIT_DEFINITELY_UNSET_VAR}" }
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("unset var must fail");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "mcp_server.env");
            assert!(reason.contains("unset"), "reason names the unset var: {reason}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn mcp_server_env_dollar_escape_is_literal() {
    let env = EnvScope::new();
    let tmp = TempDir::new("mcp-env-escape");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[mcp_server]]
name = "lit"
command = "x"
env = { PRICE = "$${NOT_A_VAR}" }
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.mcp_servers[0].env[0].1, "${NOT_A_VAR}", "$$ escapes to literal $");
    drop(env);
}

// Chapter Conduit (CD.2) — `[[mcp_server]] headers` for remote transports.

#[test]
fn mcp_server_headers_interpolate_for_http() {
    let env = EnvScope::new();
    env.set("CONDUIT_BEARER", "tok-xyz");
    let tmp = TempDir::new("mcp-headers");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[mcp_server]]
name = "remote"
transport = "http"
url = "https://mcp.example.com/mcp"
headers = { Authorization = "Bearer ${CONDUIT_BEARER}", X-Trace = "on" }
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    let s = &cfg.mcp_servers[0];
    assert_eq!(s.transport, McpTransportKind::Http);
    // Sorted by key: Authorization before X-Trace.
    assert_eq!(
        s.headers,
        vec![
            ("Authorization".to_string(), "Bearer tok-xyz".to_string()),
            ("X-Trace".to_string(), "on".to_string()),
        ],
    );
    drop(env);
}

#[test]
fn mcp_server_headers_rejected_on_stdio() {
    let env = EnvScope::new();
    let tmp = TempDir::new("mcp-headers-stdio");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[mcp_server]]
name = "local"
command = "npx"
headers = { Authorization = "Bearer x" }
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("headers on stdio must fail");
    match err {
        ConfigError::Invalid { field, .. } => assert_eq!(field, "mcp_server.headers"),
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn no_mcp_server_section_gives_empty_vec() {
    let env = EnvScope::new();
    let tmp = TempDir::new("no-mcp");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert!(cfg.mcp_servers.is_empty());

    drop(env);
}

#[test]
fn mcp_server_sse_transport_parses() {
    let env = EnvScope::new();
    let tmp = TempDir::new("mcp-sse");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[mcp_server]]
name = "remote"
transport = "sse"
url = "http://example.com:8080/sse"

[[mcp_server]]
name = "local"
command = "npx"
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");

    assert_eq!(cfg.mcp_servers.len(), 2);

    let remote = &cfg.mcp_servers[0];
    assert_eq!(remote.name, "remote");
    assert_eq!(remote.transport, McpTransportKind::Sse);
    assert_eq!(remote.url.as_deref(), Some("http://example.com:8080/sse"));
    assert!(remote.command.is_none());

    let local = &cfg.mcp_servers[1];
    assert_eq!(local.name, "local");
    assert_eq!(local.transport, McpTransportKind::Stdio);
    assert_eq!(local.command.as_deref(), Some("npx"));
    assert!(local.url.is_none());

    drop(env);
}

#[test]
fn mcp_server_http_transport_parses() {
    let env = EnvScope::new();
    let tmp = TempDir::new("mcp-http");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[mcp_server]]
name = "modern"
transport = "http"
url = "https://example.com/mcp"

[[mcp_server]]
name = "alias"
transport = "streamable-http"
url = "https://example.com/mcp2"
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");

    assert_eq!(cfg.mcp_servers.len(), 2);
    assert_eq!(cfg.mcp_servers[0].transport, McpTransportKind::Http);
    assert_eq!(cfg.mcp_servers[0].url.as_deref(), Some("https://example.com/mcp"));
    // The `streamable-http` alias parses to the same kind.
    assert_eq!(cfg.mcp_servers[1].transport, McpTransportKind::Http);

    drop(env);
}

#[test]
fn mcp_server_http_missing_url_is_error() {
    let env = EnvScope::new();
    let tmp = TempDir::new("mcp-http-nourl");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "[anthropic]\napi_key = \"sk-test\"\n\n[[mcp_server]]\nname = \"x\"\ntransport = \"http\"\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("http needs url");
    assert!(format!("{err:?}").contains("url"));
    drop(env);
}

#[test]
fn mcp_server_sse_missing_url_is_error() {
    let env = EnvScope::new();
    let tmp = TempDir::new("mcp-sse-no-url");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[mcp_server]]
name = "broken"
transport = "sse"
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("sse without url must fail");
    let msg = err.to_string();
    assert!(msg.contains("url"), "error must mention url: {msg}");

    drop(env);
}

// ------------------------------------------------------------------
// Phase 49 — [[tool_process]] config
// ------------------------------------------------------------------

#[test]
fn tool_process_basic_entry_loads() {
    let env = EnvScope::new();
    let tmp = TempDir::new("tool-process-basic");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[tool_process]]
name = "wordcount"
command = "python3"
args = ["/path/to/tool.py"]

[tool_process.env]
LOG_LEVEL = "info"

[tool_process.scope_overrides]
wordcount = "memory.read:topic:wc/**"
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.tool_processes.len(), 1);
    let t = &cfg.tool_processes[0];
    assert_eq!(t.name, "wordcount");
    assert_eq!(t.command, "python3");
    assert_eq!(t.args, vec!["/path/to/tool.py"]);
    assert!(t.enabled);
    assert_eq!(t.env.len(), 1);
    assert_eq!(t.env[0].0, "LOG_LEVEL");
    assert_eq!(t.env[0].1, "info");
    assert_eq!(t.scope_overrides.len(), 1);
    assert_eq!(
        t.scope_overrides.get("wordcount").map(String::as_str),
        Some("memory.read:topic:wc/**"),
    );
    drop(env);
}

#[test]
fn deckhand_applications_opt_in_synthesizes_unsandboxed_tool_process() {
    // Chapter Deckhand — `[applications] enabled = true` adds the aivyx-apps
    // tool process, unsandboxed (GUI control needs the host display).
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[applications]\nenabled = true\n",
        "deckhand-on",
    );
    let app = cfg
        .tool_processes
        .iter()
        .find(|t| t.name == "applications")
        .expect("applications tool process synthesized");
    assert_eq!(app.command, "aivyx-apps");
    assert!(app.enabled);
    assert!(app.disable_sandbox, "GUI control must run unsandboxed");

    // Absent / disabled → nothing synthesized (byte-identical).
    let off = load_with_toml("\n[applications]\nenabled = false\n", "deckhand-off");
    assert!(!off.tool_processes.iter().any(|t| t.name == "applications"));
    let absent = load_with_toml("\n", "deckhand-absent");
    assert!(!absent.tool_processes.iter().any(|t| t.name == "applications"));
    drop(env);
}

#[test]
fn deckhand_applications_binary_path_override() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[applications]\nenabled = true\nbinary_path = \"/opt/aivyx-apps\"\n",
        "deckhand-path",
    );
    let app = cfg
        .tool_processes
        .iter()
        .find(|t| t.name == "applications")
        .expect("synthesized");
    assert_eq!(app.command, "/opt/aivyx-apps");
    drop(env);
}

#[test]
fn tool_process_disabled_entries_filtered() {
    let env = EnvScope::new();
    let tmp = TempDir::new("tool-process-disabled");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[tool_process]]
name = "active"
command = "python3"

[[tool_process]]
name = "skipped"
command = "python3"
enabled = false
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.tool_processes.len(), 1);
    assert_eq!(cfg.tool_processes[0].name, "active");
    drop(env);
}

#[test]
fn tool_process_sandbox_block_loads() {
    let env = EnvScope::new();
    let tmp = TempDir::new("tool-process-sandbox");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[tool_process]]
name = "sandboxed"
command = "python3"
args = ["/path/to/tool.py"]

[tool_process.sandbox]
wrapper = "bwrap"
args = ["--ro-bind", "/", "/", "--proc", "/proc", "--unshare-all", "--die-with-parent", "--"]
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.tool_processes.len(), 1);
    let sandbox = cfg.tool_processes[0]
        .sandbox
        .as_ref()
        .expect("sandbox block must be Some");
    assert_eq!(sandbox.wrapper, "bwrap");
    assert!(sandbox.args.iter().any(|a| a == "--unshare-all"));
    drop(env);
}

#[test]
fn tool_process_sandbox_empty_wrapper_is_error() {
    let env = EnvScope::new();
    let tmp = TempDir::new("tool-process-sandbox-empty");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[tool_process]]
name = "broken-sandbox"
command = "python3"

[tool_process.sandbox]
wrapper = "   "
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("empty wrapper must fail");
    let msg = err.to_string();
    assert!(msg.contains("sandbox.wrapper"), "error must name field: {msg}");
    drop(env);
}

#[test]
fn tool_process_without_sandbox_is_none() {
    let env = EnvScope::new();
    let tmp = TempDir::new("tool-process-no-sandbox");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[tool_process]]
name = "plain"
command = "python3"
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert!(
        cfg.tool_processes[0].sandbox.is_none(),
        "omitting [tool_process.sandbox] must yield None",
    );
    drop(env);
}

#[test]
fn tool_process_empty_command_is_error() {
    let env = EnvScope::new();
    let tmp = TempDir::new("tool-process-empty-cmd");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[tool_process]]
name = "broken"
command = "   "
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("empty command must fail");
    let msg = err.to_string();
    assert!(msg.contains("command"), "error must mention command: {msg}");
    drop(env);
}

// ------------------------------------------------------------------
// Phase 55 — [mcp_server.sandbox] schema
// ------------------------------------------------------------------

#[test]
fn mcp_server_sandbox_block_loads() {
    let env = EnvScope::new();
    let tmp = TempDir::new("mcp-sandbox-basic");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[mcp_server]]
name = "external-thing"
command = "/usr/local/bin/external-mcp"

[mcp_server.sandbox]
wrapper = "bwrap"
args = ["--ro-bind", "/", "/", "--proc", "/proc", "--unshare-all", "--die-with-parent", "--"]
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.mcp_servers.len(), 1);
    let sandbox = cfg.mcp_servers[0]
        .sandbox
        .as_ref()
        .expect("sandbox block must be Some");
    assert_eq!(sandbox.wrapper, "bwrap");
    assert!(sandbox.args.iter().any(|a| a == "--unshare-all"));
    drop(env);
}

#[test]
fn mcp_server_sandbox_empty_wrapper_is_error() {
    let env = EnvScope::new();
    let tmp = TempDir::new("mcp-sandbox-empty-wrapper");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[mcp_server]]
name = "broken"
command = "/usr/local/bin/mcp"

[mcp_server.sandbox]
wrapper = "   "
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("empty wrapper must fail");
    let msg = err.to_string();
    assert!(msg.contains("sandbox.wrapper"), "error must name field: {msg}");
    drop(env);
}

#[test]
fn mcp_server_sandbox_on_sse_transport_is_error() {
    // SSE has no local child to wrap; declaring a sandbox on it
    // is operator confusion the loader should call out.
    let env = EnvScope::new();
    let tmp = TempDir::new("mcp-sandbox-sse");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[mcp_server]]
name = "remote"
transport = "sse"
url = "http://localhost:9000"

[mcp_server.sandbox]
wrapper = "bwrap"
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("sandbox on SSE transport must fail");
    let msg = err.to_string();
    assert!(
        msg.contains("stdio-only") || msg.contains("sandbox"),
        "error must explain the stdio-only constraint: {msg}",
    );
    drop(env);
}

#[test]
fn mcp_server_without_sandbox_is_none() {
    let env = EnvScope::new();
    let tmp = TempDir::new("mcp-no-sandbox");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[mcp_server]]
name = "plain"
command = "/usr/local/bin/mcp"
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert!(
        cfg.mcp_servers[0].sandbox.is_none(),
        "omitting [mcp_server.sandbox] must yield None",
    );
    drop(env);
}

#[test]
fn mcp_server_stdio_missing_command_is_error() {
    let env = EnvScope::new();
    let tmp = TempDir::new("mcp-stdio-no-cmd");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[mcp_server]]
name = "broken"
transport = "stdio"
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("stdio without command must fail");
    let msg = err.to_string();
    assert!(msg.contains("command"), "error must mention command: {msg}");

    drop(env);
}

// ------------------------------------------------------------------
// Phase 25 Task 3 — provider selection + OpenAI config
// ------------------------------------------------------------------

#[test]
fn provider_defaults_to_anthropic() {
    let env = EnvScope::new();
    let opts = LoadOptions::test_env_only();
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.provider.value, ProviderKind::Anthropic);
    assert_eq!(cfg.provider.source, FieldSource::Default);
    drop(env);
}

#[test]
fn provider_from_env_var() {
    let env = EnvScope::new();
    env.set("AIVYX_PA_PROVIDER", "openai");
    let opts = LoadOptions::test_env_only();
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.provider.value, ProviderKind::OpenAi);
    assert_eq!(cfg.provider.source, FieldSource::Env);
    drop(env);
}

#[test]
fn provider_invalid_env_var_is_error() {
    let env = EnvScope::new();
    env.set("AIVYX_PA_PROVIDER", "gemini");
    let opts = LoadOptions::test_env_only();
    let err = AivyxConfig::load_from_env_and_toml(&opts).unwrap_err();
    assert!(matches!(err, ConfigError::Invalid { field: "provider", .. }));
    drop(env);
}

#[test]
fn provider_from_toml() {
    let env = EnvScope::new();
    let tmp = TempDir::new("provider-toml");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[agent]
provider = "openai"

[openai]
api_key = "sk-openai-test"
base_url = "http://localhost:11434/v1"
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.provider.value, ProviderKind::OpenAi);
    assert_eq!(cfg.provider.source, FieldSource::Toml);
    assert!(cfg.openai_api_key.is_some());
    assert_eq!(cfg.openai_api_key.as_ref().unwrap().source, FieldSource::Toml);
    let base_url = cfg.openai_base_url.as_ref().expect("base_url set");
    assert_eq!(base_url.value, "http://localhost:11434/v1");
    assert_eq!(base_url.source, FieldSource::Toml);
    drop(env);
}

#[test]
fn openai_constrain_tool_calls_round_trips_and_defaults_off() {
    // Chapter Emboss (EB.2) — `[openai] constrain_tool_calls` is the
    // opt-in for grammar-constrained tool-calling on llama.cpp-family
    // servers. Omitted → false (byte-identical passthrough); set → true.
    let env = EnvScope::new();

    // Omitted → false.
    let tmp = TempDir::new("emboss-constrain-default");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[agent]
provider = "llamacpp"

[openai]
base_url = "http://localhost:8080"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert!(
        !cfg.openai_constrain_tool_calls,
        "omitted constrain_tool_calls must default to false"
    );

    // Set → true.
    let tmp2 = TempDir::new("emboss-constrain-on");
    let toml_path2 = tmp2.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path2,
        r#"
[agent]
provider = "llamacpp"

[openai]
base_url = "http://localhost:8080"
constrain_tool_calls = true
"#,
    )
    .unwrap();
    let opts2 = LoadOptions {
        toml_path: Some(toml_path2),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg2 = AivyxConfig::load_from_env_and_toml(&opts2).expect("load");
    assert!(cfg2.openai_constrain_tool_calls);
    drop(env);
}

#[test]
fn openai_api_key_from_env_overrides_toml() {
    let env = EnvScope::new();
    env.set("AIVYX_PA_OPENAI_API_KEY", "sk-env-wins");
    let tmp = TempDir::new("openai-env-over-toml");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[openai]
api_key = "sk-toml-loses"
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.openai_api_key.as_ref().unwrap().source, FieldSource::Env);
    drop(env);
}

#[test]
fn validate_requires_openai_key_when_provider_is_openai() {
    let env = EnvScope::new();
    env.set("AIVYX_PA_PROVIDER", "openai");
    let opts = LoadOptions {
        toml_path: None,
        require_api_key: true,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    let err = cfg.validate(&opts).unwrap_err();
    assert!(matches!(err, ConfigError::Missing { field: "openai_api_key" }));
    drop(env);
}

#[test]
fn validate_does_not_require_anthropic_key_when_provider_is_openai() {
    let env = EnvScope::new();
    env.set("AIVYX_PA_PROVIDER", "openai");
    env.set("AIVYX_PA_OPENAI_API_KEY", "sk-test");
    let opts = LoadOptions {
        toml_path: None,
        require_api_key: true,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    cfg.validate(&opts).expect("should pass — openai key present");
    drop(env);
}

// ---- Ollama provider tests ----

#[test]
fn ollama_provider_from_env() {
    let env = EnvScope::new();
    env.set("AIVYX_PA_PROVIDER", "ollama");
    let opts = LoadOptions {
        toml_path: None,
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.provider.value, ProviderKind::Ollama);
    assert_eq!(cfg.provider.source, FieldSource::Env);
    drop(env);
}

#[test]
fn ollama_provider_from_toml() {
    let env = EnvScope::new();
    let tmp = TempDir::new("ollama-toml");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[agent]
provider = "ollama"
model = "llama3.1"
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.provider.value, ProviderKind::Ollama);
    assert_eq!(cfg.model.value, "llama3.1");
    drop(env);
}

#[test]
fn ollama_validate_does_not_require_api_key() {
    let env = EnvScope::new();
    env.set("AIVYX_PA_PROVIDER", "ollama");
    let opts = LoadOptions {
        toml_path: None,
        require_api_key: true,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    cfg.validate(&opts)
        .expect("ollama must not require an API key even with require_api_key=true");
    drop(env);
}

#[test]
fn ollama_accepts_optional_api_key() {
    let env = EnvScope::new();
    env.set("AIVYX_PA_PROVIDER", "ollama");
    env.set("AIVYX_PA_OPENAI_API_KEY", "sk-optional");
    let opts = LoadOptions {
        toml_path: None,
        require_api_key: true,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    cfg.validate(&opts).expect("ollama with optional key must pass");
    assert!(cfg.openai_api_key.is_some());
    drop(env);
}

#[test]
fn provider_kind_is_openai_compatible() {
    assert!(!ProviderKind::Anthropic.is_openai_compatible());
    assert!(ProviderKind::OpenAi.is_openai_compatible());
    assert!(ProviderKind::Ollama.is_openai_compatible());
    // Phase 133 — llama-server and Jan are both OpenAI-compat
    // local-LLM providers; they speak the same /v1/chat/completions
    // surface as the existing Ollama / OpenAI variants.
    assert!(ProviderKind::LlamaCpp.is_openai_compatible());
    assert!(ProviderKind::Jan.is_openai_compatible());
}

#[test]
fn provider_kind_display() {
    assert_eq!(ProviderKind::Anthropic.to_string(), "anthropic");
    assert_eq!(ProviderKind::OpenAi.to_string(), "openai");
    assert_eq!(ProviderKind::Ollama.to_string(), "ollama");
    // Phase 133 — lowercase, hyphen-free for parity with the
    // existing variants. The serde deserialize layer accepts
    // hyphen and underscore aliases for `llamacpp`; Display uses
    // the canonical lowercase form.
    assert_eq!(ProviderKind::LlamaCpp.to_string(), "llamacpp");
    assert_eq!(ProviderKind::Jan.to_string(), "jan");
}

#[test]
fn provider_kind_default_context_window_groups_local_llm_providers() {
    // Phase 133 audit — every local-LLM provider gets the same
    // conservative 8000-token default. Models vary widely and
    // operators routinely override via config.
    assert_eq!(ProviderKind::Ollama.default_context_window(), 8_000);
    assert_eq!(ProviderKind::LlamaCpp.default_context_window(), 8_000);
    assert_eq!(ProviderKind::Jan.default_context_window(), 8_000);
    // Cloud providers keep their existing defaults — regression
    // guard against accidentally folding them into the local-LLM
    // group.
    assert_eq!(ProviderKind::Anthropic.default_context_window(), 200_000);
    assert_eq!(ProviderKind::OpenAi.default_context_window(), 128_000);
}

#[test]
fn llamacpp_provider_from_env() {
    let env = EnvScope::new();
    env.set("AIVYX_PA_PROVIDER", "llamacpp");
    let opts = LoadOptions {
        toml_path: None,
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.provider.value, ProviderKind::LlamaCpp);
    assert_eq!(cfg.provider.source, FieldSource::Env);
    drop(env);
}

#[test]
fn llamacpp_provider_serde_aliases_parse() {
    // The serde alias attribute on the enum accepts three
    // common spellings; verify each round-trips through TOML.
    for alias in ["llamacpp", "llama-cpp", "llama_cpp"] {
        let env = EnvScope::new();
        let tmp = TempDir::new(&format!("llamacpp-toml-{alias}"));
        let toml_path = tmp.path().join("aivyx-pa.toml");
        std::fs::write(
            &toml_path,
            format!(
                r#"
[agent]
provider = "{alias}"
model = "qwen3:32b"
"#
            ),
        )
        .unwrap();
        let opts = LoadOptions {
            toml_path: Some(toml_path),
            require_api_key: false,
            require_telegram_token: false,
            require_discord_token: false,
            require_slack_tokens: false,
            role_override: None,
        };
        let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
        assert_eq!(
            cfg.provider.value,
            ProviderKind::LlamaCpp,
            "alias {alias:?} must deserialize to LlamaCpp"
        );
        drop(env);
    }
}

#[test]
fn llamacpp_validate_does_not_require_api_key() {
    let env = EnvScope::new();
    env.set("AIVYX_PA_PROVIDER", "llamacpp");
    let opts = LoadOptions {
        toml_path: None,
        require_api_key: true,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    cfg.validate(&opts)
        .expect("llamacpp must not require an API key even with require_api_key=true");
    drop(env);
}

#[test]
fn jan_provider_from_env() {
    let env = EnvScope::new();
    env.set("AIVYX_PA_PROVIDER", "jan");
    let opts = LoadOptions {
        toml_path: None,
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.provider.value, ProviderKind::Jan);
    assert_eq!(cfg.provider.source, FieldSource::Env);
    drop(env);
}

#[test]
fn jan_provider_from_toml() {
    let env = EnvScope::new();
    let tmp = TempDir::new("jan-toml");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[agent]
provider = "jan"
model = "qwen2.5-7b-instruct"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.provider.value, ProviderKind::Jan);
    assert_eq!(cfg.model.value, "qwen2.5-7b-instruct");
    drop(env);
}

#[test]
fn jan_validate_does_not_require_api_key() {
    let env = EnvScope::new();
    env.set("AIVYX_PA_PROVIDER", "jan");
    let opts = LoadOptions {
        toml_path: None,
        require_api_key: true,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    cfg.validate(&opts)
        .expect("jan must not require an API key even with require_api_key=true");
    drop(env);
}

// ------------------------------------------------------------------
// Phase 134 — ProviderKind::MistralRs regression tests
// ------------------------------------------------------------------

#[test]
fn mistralrs_provider_from_env() {
    let env = EnvScope::new();
    env.set("AIVYX_PA_PROVIDER", "mistralrs");
    let opts = LoadOptions {
        toml_path: None,
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.provider.value, ProviderKind::MistralRs);
    assert_eq!(cfg.provider.source, FieldSource::Env);
    drop(env);
}

#[test]
fn mistralrs_provider_serde_aliases_parse() {
    for alias in ["mistralrs", "mistral-rs", "mistral_rs"] {
        let env = EnvScope::new();
        let tmp = TempDir::new(&format!("mistralrs-toml-{alias}"));
        let toml_path = tmp.path().join("aivyx-pa.toml");
        std::fs::write(
            &toml_path,
            format!(
                r#"
[agent]
provider = "{alias}"
model = "qwen3-4b-q4_k_m.gguf"
"#
            ),
        )
        .unwrap();
        let opts = LoadOptions {
            toml_path: Some(toml_path),
            require_api_key: false,
            require_telegram_token: false,
            require_discord_token: false,
            require_slack_tokens: false,
            role_override: None,
        };
        let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
        assert_eq!(
            cfg.provider.value,
            ProviderKind::MistralRs,
            "alias {alias:?} must deserialize to MistralRs",
        );
        drop(env);
    }
}

#[test]
fn mistralrs_validate_requires_model_path() {
    let env = EnvScope::new();
    env.set("AIVYX_PA_PROVIDER", "mistralrs");
    let opts = LoadOptions {
        toml_path: None,
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    let err = cfg.validate(&opts).expect_err("missing model_path must fail");
    match err {
        ConfigError::Missing { field } => {
            assert_eq!(field, "mistralrs.model_path");
        }
        other => panic!("expected Missing(mistralrs.model_path), got {other:?}"),
    }
    drop(env);
}

#[test]
fn mistralrs_validate_passes_with_model_path() {
    let env = EnvScope::new();
    let tmp = TempDir::new("mistralrs-passes");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[agent]
provider = "mistralrs"
model = "qwen3-4b"

[mistralrs]
model_path = "/models/qwen3-4b-q4_k_m.gguf"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: true,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    cfg.validate(&opts).expect("mistralrs + model_path must pass");
    assert_eq!(
        cfg.mistralrs_options.model_path.as_deref(),
        Some(std::path::Path::new("/models/qwen3-4b-q4_k_m.gguf")),
    );
    drop(env);
}

#[test]
fn mistralrs_options_full_section_round_trip() {
    let env = EnvScope::new();
    let tmp = TempDir::new("mistralrs-full");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[agent]
provider = "mistralrs"
model = "qwen3"

[mistralrs]
model_path = "/models/qwen3-dir"
model_file = "qwen3-4b-q4_k_m.gguf"
chat_template_path = "/templates/qwen3.json"
max_seq_len = 32768
constrain_tool_calls = true
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    let mr = &cfg.mistralrs_options;
    assert_eq!(
        mr.model_path.as_deref(),
        Some(std::path::Path::new("/models/qwen3-dir"))
    );
    assert_eq!(mr.model_file.as_deref(), Some("qwen3-4b-q4_k_m.gguf"));
    assert_eq!(
        mr.chat_template_path.as_deref(),
        Some(std::path::Path::new("/templates/qwen3.json"))
    );
    assert_eq!(mr.max_seq_len, Some(32768));
    // Chapter Stencil (ST.2) — grammar-constrained tool-calling opt-in.
    assert!(mr.constrain_tool_calls);
    drop(env);
}

#[test]
fn mistralrs_constrain_tool_calls_defaults_off() {
    // Chapter Stencil (ST.2) — the opt-in must default to `false`
    // so the unconstrained code path stays byte-identical when an
    // operator doesn't set it. Omitting the key (and the whole
    // section) both leave it off.
    let env = EnvScope::new();
    let tmp = TempDir::new("mistralrs-constrain-default");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[agent]
provider = "mistralrs"
model = "qwen3"

[mistralrs]
model_path = "/models/qwen3-4b-q4_k_m.gguf"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert!(
        !cfg.mistralrs_options.constrain_tool_calls,
        "constrain_tool_calls must default to false"
    );
    drop(env);
}

#[test]
fn agent_turn_timeout_secs_round_trips_and_defaults_none() {
    // Chapter Bridle (BR.4) — `[agent] turn_timeout_secs` is an opt-in
    // override; unset (the common case) → `None` so the agent keeps the
    // built-in 120s default. When set, it round-trips as seconds.
    let env = EnvScope::new();

    // Unset → None.
    let tmp = TempDir::new("bridle-timeout-default");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[agent]
model = "qwen3"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(
        cfg.turn_timeout_secs, None,
        "unset turn_timeout_secs must be None (→ built-in default)"
    );

    // Set → that value.
    let tmp2 = TempDir::new("bridle-timeout-set");
    let toml_path2 = tmp2.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path2,
        r#"
[agent]
model = "qwen3"
turn_timeout_secs = 1800
"#,
    )
    .unwrap();
    let opts2 = LoadOptions {
        toml_path: Some(toml_path2),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg2 = AivyxConfig::load_from_env_and_toml(&opts2).expect("load");
    assert_eq!(cfg2.turn_timeout_secs, Some(1800));
    drop(env);
}

#[test]
fn provider_kind_mistralrs_is_in_process_and_not_openai_compat() {
    // Phase 134 distinct posture: in-process, not
    // OpenAI-compatible at the wire level.
    assert!(ProviderKind::MistralRs.is_in_process());
    assert!(!ProviderKind::MistralRs.is_openai_compatible());
    // Other providers are not in-process.
    assert!(!ProviderKind::Ollama.is_in_process());
    assert!(!ProviderKind::LlamaCpp.is_in_process());
    assert!(!ProviderKind::Jan.is_in_process());
    assert!(!ProviderKind::Anthropic.is_in_process());
    assert!(!ProviderKind::OpenAi.is_in_process());
}

#[test]
fn mistralrs_default_context_window_matches_other_local_providers() {
    assert_eq!(ProviderKind::MistralRs.default_context_window(), 8_000);
}

#[test]
fn mistralrs_display_canonical_form() {
    assert_eq!(ProviderKind::MistralRs.to_string(), "mistralrs");
}

// ------------------------------------------------------------------
// GPU-slot broker coordination — ProviderKind::Broker regression tests
// ------------------------------------------------------------------

#[test]
fn broker_provider_from_env() {
    let env = EnvScope::new();
    env.set("AIVYX_PA_PROVIDER", "broker");
    let opts = LoadOptions {
        toml_path: None,
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.provider.value, ProviderKind::Broker);
    assert_eq!(cfg.provider.source, FieldSource::Env);
    drop(env);
}

#[test]
fn broker_provider_serde_aliases_parse() {
    for alias in ["broker", "aivyx-broker", "aivyx_broker"] {
        let env = EnvScope::new();
        let tmp = TempDir::new(&format!("broker-toml-{alias}"));
        let toml_path = tmp.path().join("aivyx-pa.toml");
        std::fs::write(
            &toml_path,
            format!(
                r#"
[agent]
provider = "{alias}"
model = "qwen3-32b"
"#
            ),
        )
        .unwrap();
        let opts = LoadOptions {
            toml_path: Some(toml_path),
            require_api_key: false,
            require_telegram_token: false,
            require_discord_token: false,
            require_slack_tokens: false,
            role_override: None,
        };
        let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
        assert_eq!(
            cfg.provider.value,
            ProviderKind::Broker,
            "alias {alias:?} must deserialize to Broker",
        );
        drop(env);
    }
}

#[test]
fn broker_validate_does_not_require_api_key() {
    let env = EnvScope::new();
    env.set("AIVYX_PA_PROVIDER", "broker");
    let opts = LoadOptions {
        toml_path: None,
        require_api_key: true,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    cfg.validate(&opts)
        .expect("broker must not require an API key even with require_api_key=true");
    drop(env);
}

#[test]
fn broker_base_url_defaults_to_none() {
    let env = EnvScope::new();
    env.set("AIVYX_PA_PROVIDER", "broker");
    let opts = LoadOptions {
        toml_path: None,
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(
        cfg.broker_base_url, None,
        "absent [broker] section must leave broker_base_url None -- the binary's \
         dispatch arm falls back to aivyx-broker's own http://127.0.0.1:8899 default"
    );
    drop(env);
}

#[test]
fn broker_base_url_round_trips_from_toml() {
    let env = EnvScope::new();
    let tmp = TempDir::new("broker-base-url");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[agent]
provider = "broker"
model = "qwen3-32b"

[broker]
base_url = "http://127.0.0.1:9999"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(
        cfg.broker_base_url.as_deref(),
        Some("http://127.0.0.1:9999"),
    );
    drop(env);
}

#[test]
fn provider_kind_broker_is_openai_compatible_and_not_in_process() {
    // GPU-slot broker coordination — unlike MistralRs, Broker speaks the identical
    // OpenAI-compatible wire protocol as LlamaCpp (plus one additive
    // optional field) and is not an in-process provider.
    assert!(ProviderKind::Broker.is_openai_compatible());
    assert!(!ProviderKind::Broker.is_in_process());
}

#[test]
fn broker_default_context_window_matches_other_local_providers() {
    assert_eq!(ProviderKind::Broker.default_context_window(), 8_000);
}

#[test]
fn broker_display_canonical_form() {
    assert_eq!(ProviderKind::Broker.to_string(), "broker");
}

// ------------------------------------------------------------------
// [daemon] web_ui / web_ui_port — Phase 39
// ------------------------------------------------------------------

#[test]
fn daemon_web_ui_true_yields_default_port() {
    let env = EnvScope::new();
    let tmp = TempDir::new("web-ui-true");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[daemon]
web_ui = true
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.web_ui_port, Some(7843));
    drop(env);
}

#[test]
fn daemon_web_ui_port_overrides_default() {
    let env = EnvScope::new();
    let tmp = TempDir::new("web-ui-port");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[daemon]
web_ui_port = 9999
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.web_ui_port, Some(9999));
    drop(env);
}

#[test]
fn daemon_web_ui_false_disables() {
    let env = EnvScope::new();
    let tmp = TempDir::new("web-ui-false");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[daemon]
web_ui = false
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.web_ui_port, None);
    drop(env);
}

#[test]
fn daemon_web_ui_absent_means_none() {
    let env = EnvScope::new();
    let tmp = TempDir::new("web-ui-absent");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(&toml_path, "").unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.web_ui_port, None);
    drop(env);
}

#[test]
fn daemon_web_ui_host_absent_defaults_to_none() {
    // Chapter Harbor — no web_ui_host → None (the daemon binds 127.0.0.1,
    // the localhost-only default every native install keeps).
    let env = EnvScope::new();
    let cfg = load_with_toml("\n[daemon]\nweb_ui = true\n", "web-ui-host-absent");
    assert_eq!(cfg.web_ui_host, None);
    drop(env);
}

#[test]
fn daemon_web_ui_host_parses_bind_all() {
    // Chapter Harbor — `0.0.0.0` for containerized deployment. (With a
    // token: bare off-host binds are refused by the Gatehouse interlock,
    // tested separately below.)
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[daemon]\nweb_ui = true\nweb_ui_host = \"0.0.0.0\"\n\
         web_ui_auth_token = \"t0ken-t0ken\"\n",
        "web-ui-host-all",
    );
    assert_eq!(
        cfg.web_ui_host,
        Some(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
    );
    drop(env);
}

// ---- Chapter Gatehouse — the exposure interlock ------------------------

#[test]
fn pack_trusted_publishers_validates_key_shape() {
    // Chapter Freight — entries must be base64 of exactly 32 bytes.
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[pack]\ntrusted_publishers = [\"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\"]\n",
        "pack-trust-ok",
    );
    assert_eq!(cfg.pack_trusted_publishers.len(), 1);
    drop(env);

    let env = EnvScope::new();
    let tmp = TempDir::new("pack-trust-bad");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(&toml_path, "\n[pack]\ntrusted_publishers = [\"not-base64!\"]\n").unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must reject");
    assert!(err.to_string().contains("trusted_publishers"), "error: {err}");
    drop(env);
}

#[test]
fn gatehouse_off_host_without_token_is_refused() {
    // The two-key launch: off-host + no token + no explicit escape hatch
    // must fail AT CONFIG LOAD, naming both remedies.
    let env = EnvScope::new();
    let tmp = TempDir::new("gatehouse-refuse");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[daemon]\nweb_ui = true\nweb_ui_host = \"0.0.0.0\"\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must refuse");
    let msg = err.to_string();
    assert!(msg.contains("UNAUTHENTICATED"), "names the risk: {msg}");
    assert!(msg.contains("web_ui_auth_token"), "names remedy 1: {msg}");
    assert!(msg.contains("web_ui_insecure_no_auth"), "names remedy 2: {msg}");
    drop(env);
}

#[test]
fn gatehouse_escape_hatch_permits_bare_off_host_bind() {
    // The behind-my-own-reverse-proxy case: explicit acknowledgement
    // makes the bare off-host bind legal (Postern's runtime warnings
    // still fire — that path is web_ui.rs's).
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[daemon]\nweb_ui = true\nweb_ui_host = \"0.0.0.0\"\n\
         web_ui_insecure_no_auth = true\n",
        "gatehouse-hatch",
    );
    assert!(cfg.web_ui_insecure_no_auth);
    assert_eq!(cfg.web_ui_auth_token, None);
    drop(env);
}

#[test]
fn gatehouse_loopback_without_token_is_untouched() {
    // The desktop local-first posture: loopback (default or explicit)
    // needs no token and no hatch — byte-identical to pre-Gatehouse.
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[daemon]\nweb_ui = true\nweb_ui_host = \"127.0.0.1\"\n",
        "gatehouse-loopback",
    );
    assert_eq!(cfg.web_ui_auth_token, None);
    assert!(!cfg.web_ui_insecure_no_auth);
    drop(env);
}

#[test]
fn daemon_web_ui_host_rejects_non_ip() {
    // A non-IP value is a hard config error, not a silent fallback.
    let env = EnvScope::new();
    let tmp = TempDir::new("web-ui-host-bad");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(&toml_path, "\n[daemon]\nweb_ui_host = \"not-an-ip\"\n").unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must reject");
    assert!(
        err.to_string().contains("web_ui_host"),
        "error should name the field: {err}"
    );
    drop(env);
}

#[test]
fn daemon_web_ui_allowed_origins_absent_is_empty() {
    let env = EnvScope::new();
    let cfg = load_with_toml("\n[daemon]\nweb_ui = true\n", "origins-absent");
    assert!(cfg.web_ui_allowed_origins.is_empty());
    drop(env);
}

#[test]
fn daemon_web_ui_auth_token_absent_is_none() {
    // Chapter Postern — no token → no auth (byte-identical default).
    let env = EnvScope::new();
    let cfg = load_with_toml("\n[daemon]\nweb_ui = true\n", "token-absent");
    assert_eq!(cfg.web_ui_auth_token, None);
    drop(env);
}

#[test]
fn daemon_web_ui_auth_token_is_read() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[daemon]\nweb_ui = true\nweb_ui_auth_token = \"s3cret-TOKEN_9.~\"\n",
        "token-set",
    );
    assert_eq!(cfg.web_ui_auth_token.as_deref(), Some("s3cret-TOKEN_9.~"));
    drop(env);
}

#[test]
fn daemon_web_ui_auth_token_rejects_empty() {
    let env = EnvScope::new();
    let err = load_with_toml_result(
        "\n[daemon]\nweb_ui_auth_token = \"   \"\n",
        "token-empty",
    )
    .expect_err("blank token must be rejected");
    assert!(
        err.to_string().contains("web_ui_auth_token"),
        "error should name the field: {err}"
    );
    drop(env);
}

#[test]
fn daemon_web_ui_auth_token_rejects_unsafe_chars() {
    // A token with cookie-breaking chars (space, `;`) is rejected up front.
    let env = EnvScope::new();
    let err = load_with_toml_result(
        "\n[daemon]\nweb_ui_auth_token = \"bad token;drop\"\n",
        "token-unsafe",
    )
    .expect_err("unsafe token must be rejected");
    assert!(
        err.to_string().contains("web_ui_auth_token"),
        "error should name the field: {err}"
    );
    drop(env);
}

#[test]
fn daemon_web_ui_allowed_origins_parses_bare_origins() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[daemon]\nweb_ui = true\nweb_ui_allowed_origins = [\"https://studio.example\", \"http://box.lan:7843\"]\n",
        "origins-ok",
    );
    assert_eq!(
        cfg.web_ui_allowed_origins,
        vec!["https://studio.example".to_string(), "http://box.lan:7843".to_string()]
    );
    drop(env);
}

#[test]
fn daemon_web_ui_allowed_origins_rejects_path() {
    // An entry with a path is not a bare origin → hard config error.
    let env = EnvScope::new();
    let tmp = TempDir::new("origins-bad");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[daemon]\nweb_ui_allowed_origins = [\"https://studio.example/app\"]\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must reject");
    assert!(
        err.to_string().contains("web_ui_allowed_origins"),
        "error should name the field: {err}"
    );
    drop(env);
}

// ------------------------------------------------------------------
// Phase 46: `bundled` flag on [[mcp_server]]
// ------------------------------------------------------------------

#[test]
fn bundled_flag_parses() {
    let env = EnvScope::new();
    let tmp = TempDir::new("mcp-bundled");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[[mcp_server]]
name = "web-search"
command = "aivyx-pa"
args = ["mcp-server", "web-search"]
bundled = true
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.mcp_servers.len(), 1);
    assert!(cfg.mcp_servers[0].bundled);
    assert_eq!(cfg.mcp_servers[0].name, "web-search");
    drop(env);
}

#[test]
fn bundled_default_false() {
    let env = EnvScope::new();
    let tmp = TempDir::new("mcp-no-bundled");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[[mcp_server]]
name = "github"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.mcp_servers.len(), 1);
    assert!(!cfg.mcp_servers[0].bundled);
    drop(env);
}

// ------------------------------------------------------------------
// Profile (Phase 57 — PRODUCT.md P13)
// ------------------------------------------------------------------

#[test]
fn profile_section_populates_all_fields_with_toml_source() {
    let env = EnvScope::new();
    let tmp = TempDir::new("profile-full");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[profile]
assistant_name = "Codex"
operator_profile = "Senior Rust engineer focused on systems and AI agents."
communication_style = "terse, conclusion-first, three-bullet lists"
primary_use_cases = ["Rust systems programming", "AI agent design"]
behavioral_preferences = [
    "prefer integration tests over mocks",
    "always cite sources when summarizing",
]
behavioral_constraints = [
    "never autonomously commit code",
    "always confirm destructive shell commands",
]
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");

    assert_eq!(cfg.profile.assistant_name.value, "Codex");
    assert_eq!(cfg.profile.assistant_name.source, FieldSource::Toml);
    assert_eq!(
        cfg.profile.operator_profile.as_deref(),
        Some("Senior Rust engineer focused on systems and AI agents."),
    );
    assert_eq!(
        cfg.profile.communication_style.as_deref(),
        Some("terse, conclusion-first, three-bullet lists"),
    );
    assert_eq!(
        cfg.profile.primary_use_cases,
        vec![
            "Rust systems programming".to_string(),
            "AI agent design".to_string(),
        ],
    );
    assert_eq!(cfg.profile.behavioral_preferences.len(), 2);
    assert!(cfg
        .profile
        .behavioral_preferences
        .iter()
        .any(|s| s.contains("integration tests")));
    assert_eq!(cfg.profile.behavioral_constraints.len(), 2);
    assert!(cfg
        .profile
        .behavioral_constraints
        .iter()
        .any(|s| s.contains("autonomously commit code")));

    drop(env);
}

#[test]
fn profile_section_absent_synthesizes_default_with_assistant_name() {
    let env = EnvScope::new();
    let tmp = TempDir::new("profile-absent");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    // No [profile] section at all — legacy aivyx-pa.toml shape.
    std::fs::write(
        &toml_path,
        r#"
[agent]
provider = "anthropic"
model = "claude-haiku-4-5-20251001"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");

    assert_eq!(cfg.profile.assistant_name.value, DEFAULT_ASSISTANT_NAME);
    assert_eq!(cfg.profile.assistant_name.source, FieldSource::Default);
    assert!(cfg.profile.operator_profile.is_none());
    assert!(cfg.profile.communication_style.is_none());
    assert!(cfg.profile.primary_use_cases.is_empty());
    assert!(cfg.profile.behavioral_preferences.is_empty());
    assert!(cfg.profile.behavioral_constraints.is_empty());

    drop(env);
}

#[test]
fn profile_section_partial_provides_some_defaults_some_toml() {
    let env = EnvScope::new();
    let tmp = TempDir::new("profile-partial");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    // Only assistant_name + primary_use_cases declared. The other
    // four fields must remain at their unset defaults.
    std::fs::write(
        &toml_path,
        r#"
[profile]
assistant_name = "Mira"
primary_use_cases = ["personal-finance analysis"]
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");

    // Declared fields carry FieldSource::Toml.
    assert_eq!(cfg.profile.assistant_name.value, "Mira");
    assert_eq!(cfg.profile.assistant_name.source, FieldSource::Toml);
    assert_eq!(
        cfg.profile.primary_use_cases,
        vec!["personal-finance analysis".to_string()],
    );

    // Undeclared fields stay at default — Option::None for the
    // two free-text fields, empty Vec for the two list fields.
    assert!(cfg.profile.operator_profile.is_none());
    assert!(cfg.profile.communication_style.is_none());
    assert!(cfg.profile.behavioral_preferences.is_empty());
    assert!(cfg.profile.behavioral_constraints.is_empty());

    drop(env);
}

// ==============================================================
// Phase 62 Task 3 — `[[notify_target]]` entries
// ==============================================================

#[test]
fn notify_target_entries_parse_telegram_and_webhook() {
    let env = EnvScope::new();
    let tmp = TempDir::new("notify-cfg");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[notify_target]]
name = "phone"
kind = "telegram"
chat_id = "123456789"

[[notify_target]]
name = "ops-alerts"
kind = "webhook"
url = "https://ntfy.sh/aivyx-personal-2026"

[[notify_target]]
name = "disabled-one"
kind = "telegram"
chat_id = "987654321"
enabled = false
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");

    assert_eq!(cfg.notify_targets.len(), 2, "disabled target filtered out");

    let phone = &cfg.notify_targets[0];
    assert_eq!(phone.name, "phone");
    assert!(phone.enabled);
    match &phone.kind {
        NotifyTargetKind::Telegram { chat_id } => assert_eq!(chat_id, "123456789"),
        other => panic!("expected Telegram, got {other:?}"),
    }

    let webhook = &cfg.notify_targets[1];
    assert_eq!(webhook.name, "ops-alerts");
    match &webhook.kind {
        NotifyTargetKind::Webhook { url } => {
            assert_eq!(url, "https://ntfy.sh/aivyx-personal-2026");
        }
        other => panic!("expected Webhook, got {other:?}"),
    }

    drop(env);
}

#[test]
fn no_notify_target_section_gives_empty_vec() {
    let env = EnvScope::new();
    let tmp = TempDir::new("no-notify");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert!(cfg.notify_targets.is_empty());
    drop(env);
}

#[test]
fn notify_target_telegram_missing_chat_id_is_error() {
    let env = EnvScope::new();
    let tmp = TempDir::new("notify-bad-telegram");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[notify_target]]
name = "phone"
kind = "telegram"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "notify_target.chat_id");
            assert!(
                reason.contains("requires `chat_id`"),
                "reason was: {reason}"
            );
            assert!(reason.contains("phone"), "reason should name target: {reason}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn notify_target_webhook_missing_url_is_error() {
    let env = EnvScope::new();
    let tmp = TempDir::new("notify-bad-webhook");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[notify_target]]
name = "alerts"
kind = "webhook"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    assert!(matches!(err, ConfigError::Invalid { field, .. } if field == "notify_target.url"));
    drop(env);
}

#[test]
fn notify_target_webhook_rejects_non_http_url() {
    let env = EnvScope::new();
    let tmp = TempDir::new("notify-bad-scheme");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[notify_target]]
name = "weird"
kind = "webhook"
url = "ftp://example.com/notify"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "notify_target.url");
            assert!(
                reason.contains("must start with http:// or https://"),
                "reason was: {reason}"
            );
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn notify_target_unknown_kind_is_error() {
    let env = EnvScope::new();
    let tmp = TempDir::new("notify-bad-kind");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[notify_target]]
name = "phone"
kind = "signal"
chat_id = "x"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "notify_target.kind");
            assert!(
                reason.contains("unknown notify_target kind"),
                "reason was: {reason}"
            );
            assert!(reason.contains("signal"));
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn notify_target_duplicate_names_are_error() {
    let env = EnvScope::new();
    let tmp = TempDir::new("notify-dup");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[notify_target]]
name = "phone"
kind = "telegram"
chat_id = "1"

[[notify_target]]
name = "phone"
kind = "webhook"
url = "https://example.com/x"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "notify_target.name");
            assert!(
                reason.contains("duplicate"),
                "reason was: {reason}"
            );
            assert!(reason.contains("phone"));
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn notify_target_empty_name_is_error() {
    let env = EnvScope::new();
    let tmp = TempDir::new("notify-empty-name");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[notify_target]]
name = ""
kind = "webhook"
url = "https://example.com/x"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    assert!(matches!(err, ConfigError::Invalid { field, .. } if field == "notify_target.name"));
    drop(env);
}

// ==============================================================
// Phase 63 Task 2 — trigger.notify_target field + cross-validation
// ==============================================================

#[test]
fn schedule_notify_target_loads_when_role_has_capability() {
    let env = EnvScope::new();
    let tmp = TempDir::new("schedule-notify-ok");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[role]]
name = "default"
capability_scopes = ["notify.send"]
trust_ceiling = "Trusted"

[[notify_target]]
name = "phone"
kind = "webhook"
url = "https://example.com/x"

[[schedule]]
name = "morning-summary"
cron = "0 0 9 * * * *"
prompt = "Summarize my day"
notify_target = "phone"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.schedules.len(), 1);
    assert_eq!(cfg.schedules[0].notify_target.as_deref(), Some("phone"));
    drop(env);
}

#[test]
fn ledger_schedule_report_kind_parses() {
    // Chapter Ledger — `report_kind = "digest"` parses; absent → None.
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[[schedule]]\nname = \"weekly-digest\"\ncron = \"0 0 8 * * 1\"\n\
         prompt = \"unused for a report\"\nreport_kind = \"digest\"\n\
         \n[[schedule]]\nname = \"env\"\ncron = \"0 0 7 * * *\"\nprompt = \"go\"\n",
        "ledger-report-kind",
    );
    let digest = cfg.schedules.iter().find(|s| s.name == "weekly-digest").unwrap();
    assert_eq!(digest.report_kind.as_deref(), Some("digest"));
    let env_sched = cfg.schedules.iter().find(|s| s.name == "env").unwrap();
    assert_eq!(env_sched.report_kind, None);
    drop(env);
}

#[test]
fn schedule_team_mission_loads_with_no_prompt() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        r#"
[[schedule]]
name = "nightly-boh-close"
cron = "0 0 2 * * * *"
[schedule.team_mission]
goal = "run the overnight close"
pack_config = "crates/verticals/aivyx-kitchen/assets/kitchen-boh.toml"
"#,
        "team-mission-loads",
    );
    assert_eq!(cfg.schedules.len(), 1);
    let tm = cfg.schedules[0]
        .team_mission
        .as_ref()
        .expect("team_mission set");
    assert_eq!(tm.goal, "run the overnight close");
    assert_eq!(
        tm.pack_config.as_deref(),
        Some("crates/verticals/aivyx-kitchen/assets/kitchen-boh.toml")
    );
    drop(env);
}

#[test]
fn schedule_rejects_both_prompt_and_team_mission() {
    let env = EnvScope::new();
    let err = load_with_toml_result(
        r#"
[[schedule]]
name = "bad"
cron = "0 0 2 * * * *"
prompt = "do a thing"
[schedule.team_mission]
goal = "run the overnight close"
"#,
        "team-mission-both",
    )
    .unwrap_err();
    assert!(matches!(
        err,
        ConfigError::Invalid {
            field: "schedule.team_mission",
            ..
        }
    ));
    drop(env);
}

#[test]
fn schedule_rejects_neither_prompt_nor_team_mission() {
    let env = EnvScope::new();
    let err = load_with_toml_result(
        r#"
[[schedule]]
name = "bad"
cron = "0 0 2 * * * *"
"#,
        "team-mission-neither",
    )
    .unwrap_err();
    assert!(matches!(
        err,
        ConfigError::Invalid {
            field: "schedule.prompt",
            ..
        }
    ));
    drop(env);
}

#[test]
fn schedule_notify_target_unknown_target_is_error() {
    let env = EnvScope::new();
    let tmp = TempDir::new("schedule-notify-unknown");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[role]]
name = "default"
capability_scopes = ["notify.send"]
trust_ceiling = "Trusted"

[[schedule]]
name = "morning-summary"
cron = "0 0 9 * * * *"
prompt = "Summarize my day"
notify_target = "phone"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "schedule.notify_targets");
            assert!(reason.contains("unknown notify_target"), "reason: {reason}");
            assert!(reason.contains("phone"), "reason: {reason}");
            assert!(reason.contains("morning-summary"), "reason: {reason}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn schedule_notify_target_role_lacks_capability_is_error() {
    let env = EnvScope::new();
    let tmp = TempDir::new("schedule-notify-noscope");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[role]]
name = "default"
capability_scopes = ["memory.read"]
trust_ceiling = "Trusted"

[[notify_target]]
name = "phone"
kind = "webhook"
url = "https://example.com/x"

[[schedule]]
name = "morning-summary"
cron = "0 0 9 * * * *"
prompt = "Summarize my day"
notify_target = "phone"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "schedule.notify_targets");
            assert!(reason.contains("lacks `notify.send`"), "reason: {reason}");
            assert!(reason.contains("default"), "reason: {reason}");
            assert!(reason.contains("morning-summary"), "reason: {reason}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn schedule_notify_target_qualified_scope_grants_named_target() {
    // Role declares `notify.send:phone` (qualified). The
    // schedule with notify_target = "phone" passes; if the
    // schedule named a different target it would fail.
    let env = EnvScope::new();
    let tmp = TempDir::new("schedule-notify-qualified");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[role]]
name = "default"
capability_scopes = ["notify.send:phone"]
trust_ceiling = "Trusted"

[[notify_target]]
name = "phone"
kind = "webhook"
url = "https://example.com/x"

[[schedule]]
name = "morning-summary"
cron = "0 0 9 * * * *"
prompt = "Summarize my day"
notify_target = "phone"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.schedules[0].notify_target.as_deref(), Some("phone"));
    drop(env);
}

#[test]
fn schedule_notify_target_semitrusted_role_is_error() {
    // notify.send is in CEILING_TRUSTED only; a SemiTrusted
    // role declaring notify.send loses it after intersection.
    let env = EnvScope::new();
    let tmp = TempDir::new("schedule-notify-semi");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[role]]
name = "default"
capability_scopes = ["notify.send"]
trust_ceiling = "SemiTrusted"

[[notify_target]]
name = "phone"
kind = "webhook"
url = "https://example.com/x"

[[schedule]]
name = "morning-summary"
cron = "0 0 9 * * * *"
prompt = "Summarize my day"
notify_target = "phone"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    assert!(matches!(err, ConfigError::Invalid { field, .. } if field == "schedule.notify_targets"));
    drop(env);
}

#[test]
fn webhook_notify_target_validated_the_same_way() {
    let env = EnvScope::new();
    let tmp = TempDir::new("webhook-notify");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[role]]
name = "default"
capability_scopes = ["notify.send"]
trust_ceiling = "Trusted"

[[notify_target]]
name = "ops"
kind = "webhook"
url = "https://example.com/x"

[[webhook]]
name = "ci-events"
prompt = "Process CI event"
notify_target = "ops"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.webhooks[0].notify_target.as_deref(), Some("ops"));
    drop(env);
}

#[test]
fn file_watch_notify_target_validated_the_same_way() {
    let env = EnvScope::new();
    let tmp = TempDir::new("filewatch-notify");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[role]]
name = "default"
capability_scopes = ["notify.send"]
trust_ceiling = "Trusted"

[[notify_target]]
name = "alerts"
kind = "webhook"
url = "https://example.com/x"

[[file_watch]]
name = "notes-dir"
path = "/tmp/notes"
prompt = "React to note change"
notify_target = "alerts"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.file_watches[0].notify_target.as_deref(), Some("alerts"));
    drop(env);
}

#[test]
fn trigger_without_notify_target_loads_normally() {
    // Phase 63 doesn't change behavior for triggers that don't
    // opt in to notify_target. Regression test.
    let env = EnvScope::new();
    let tmp = TempDir::new("no-notify-target");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[schedule]]
name = "plain-old-schedule"
cron = "0 0 9 * * * *"
prompt = "Do the thing"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.schedules[0].notify_target, None);
    drop(env);
}

#[test]
fn notify_send_via_parent_role_grants_inherited_capability() {
    // Inheritance: child role doesn't declare notify.send, but
    // its parent does. Should be granted via the parent chain.
    let env = EnvScope::new();
    let tmp = TempDir::new("notify-inherited");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[role]]
name = "default"
capability_scopes = ["notify.send"]
trust_ceiling = "Trusted"

[[role]]
name = "child"
capability_scopes = []
trust_ceiling = "Trusted"
parent_role = "default"

[[notify_target]]
name = "phone"
kind = "webhook"
url = "https://example.com/x"

[[schedule]]
name = "morning-summary"
cron = "0 0 9 * * * *"
role = "child"
prompt = "Summarize my day"
notify_target = "phone"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.schedules[0].role, "child");
    assert_eq!(cfg.schedules[0].notify_target.as_deref(), Some("phone"));
    drop(env);
}

// ==============================================================
// Phase 68 — [email] section + kind = "email" notify_target
// ==============================================================

#[test]
fn email_section_with_kind_email_target_loads_cleanly() {
    use secrecy::ExposeSecret;
    let env = EnvScope::new();
    let tmp = TempDir::new("email-happy");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[email]
host = "smtp.fastmail.com"
username = "alice@example.com"
password = "app-password-xyz"
from = "aivyx@example.com"

[[notify_target]]
name = "self"
kind = "email"
to = "alice@example.com"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    let email = cfg.email.expect("[email] populated");
    assert_eq!(email.host, "smtp.fastmail.com");
    assert_eq!(email.port, 587);
    assert_eq!(email.tls_mode, TlsMode::Starttls);
    assert_eq!(email.from, "aivyx@example.com");
    assert_eq!(email.password.value.expose_secret(), "app-password-xyz");
    assert_eq!(cfg.notify_targets.len(), 1);
    match &cfg.notify_targets[0].kind {
        NotifyTargetKind::Email { to } => assert_eq!(to, "alice@example.com"),
        other => panic!("expected Email, got {other:?}"),
    }
    drop(env);
}

#[test]
fn email_kind_target_without_email_section_is_error() {
    let env = EnvScope::new();
    let tmp = TempDir::new("email-no-section");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[notify_target]]
name = "self"
kind = "email"
to = "alice@example.com"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "notify_target.to");
            assert!(
                reason.contains("[email] section"),
                "reason: {reason}"
            );
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn email_tls_mode_none_is_rejected() {
    let env = EnvScope::new();
    let tmp = TempDir::new("email-tls-none");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[email]
host = "smtp.example.com"
tls_mode = "none"
username = "u"
password = "p"
from = "a@b.c"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "email.tls_mode");
            assert!(reason.contains("cleartext"), "reason: {reason}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn email_implicit_tls_picks_port_465_default() {
    let env = EnvScope::new();
    let tmp = TempDir::new("email-implicit");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[email]
host = "smtp.example.com"
tls_mode = "implicit"
username = "u"
password = "p"
from = "a@example.com"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    let email = cfg.email.expect("[email] populated");
    assert_eq!(email.port, 465);
    assert_eq!(email.tls_mode, TlsMode::Implicit);
    drop(env);
}

#[test]
fn email_explicit_port_override_wins() {
    let env = EnvScope::new();
    let tmp = TempDir::new("email-explicit-port");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[email]
host = "smtp.example.com"
port = 2525
username = "u"
password = "p"
from = "a@example.com"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.email.unwrap().port, 2525);
    drop(env);
}

#[test]
fn email_section_partial_config_is_error() {
    // [email] declared with host but no password.
    let env = EnvScope::new();
    let tmp = TempDir::new("email-partial");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[email]
host = "smtp.example.com"
username = "u"
from = "a@example.com"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    assert!(matches!(err, ConfigError::Invalid { field, .. } if field == "email.password"));
    drop(env);
}

#[test]
fn email_from_without_at_sign_is_error() {
    let env = EnvScope::new();
    let tmp = TempDir::new("email-bad-from");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[email]
host = "smtp.example.com"
username = "u"
password = "p"
from = "notanemail"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    assert!(matches!(err, ConfigError::Invalid { field, .. } if field == "email.from"));
    drop(env);
}

#[test]
fn email_to_without_at_sign_is_error() {
    let env = EnvScope::new();
    let tmp = TempDir::new("email-bad-to");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[email]
host = "smtp.example.com"
username = "u"
password = "p"
from = "a@example.com"

[[notify_target]]
name = "broken"
kind = "email"
to = "no-at-sign"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    assert!(matches!(err, ConfigError::Invalid { field, .. } if field == "notify_target.to"));
    drop(env);
}

#[test]
fn email_unknown_tls_mode_is_error() {
    let env = EnvScope::new();
    let tmp = TempDir::new("email-unknown-tls");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[email]
host = "smtp.example.com"
tls_mode = "bogus"
username = "u"
password = "p"
from = "a@example.com"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "email.tls_mode");
            assert!(reason.contains("bogus"), "reason: {reason}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

// ==============================================================
// Phase 69 — kind = "web-ui" notify_target
// ==============================================================

#[test]
fn web_ui_notify_target_parses_with_no_extra_fields() {
    let env = EnvScope::new();
    let tmp = TempDir::new("webui-target");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[notify_target]]
name = "desktop"
kind = "web-ui"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.notify_targets.len(), 1);
    assert_eq!(cfg.notify_targets[0].name, "desktop");
    assert!(matches!(
        cfg.notify_targets[0].kind,
        NotifyTargetKind::WebUi
    ));
    drop(env);
}

#[test]
fn unknown_notify_target_kind_error_mentions_web_ui() {
    // Regression: the helpful "supported kinds" list in the
    // unknown-kind error message must include web-ui.
    let env = EnvScope::new();
    let tmp = TempDir::new("notify-unknown-kind");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[notify_target]]
name = "bogus"
kind = "carrier-pigeon"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { reason, .. } => {
            assert!(reason.contains("web-ui"), "reason: {reason}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

// ==============================================================
// Phase 70 — [[reflection_schedule]] config block
// ==============================================================

#[test]
fn reflection_schedule_with_defaults_parses() {
    let env = EnvScope::new();
    let tmp = TempDir::new("reflection-default");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[reflection_schedule]]
name = "nightly"
cron = "0 0 23 * * *"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.reflection_schedules.len(), 1);
    let s = &cfg.reflection_schedules[0];
    assert_eq!(s.name, "nightly");
    assert_eq!(s.cron, "0 0 23 * * *");
    assert_eq!(s.lookback_window_secs, 86400); // 24h default
    assert!(s.role_override.is_none());
    assert!(s.enabled);
    // Phase 95 — defaults: skip_when_idle off, threshold 1.
    assert!(!s.skip_when_idle);
    assert_eq!(s.min_audit_entries_to_fire, 1);
    drop(env);
}

/// Phase 95 — explicit `skip_when_idle = true` +
/// `min_audit_entries_to_fire = 10` round-trips through the
/// loader.
#[test]
fn reflection_schedule_skip_when_idle_explicit_values_win() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[anthropic]\napi_key = \"sk-test\"\n\n\
         [[reflection_schedule]]\nname = \"hourly\"\n\
         cron = \"0 0 * * * *\"\n\
         skip_when_idle = true\n\
         min_audit_entries_to_fire = 10\n",
        "refl-cadence-explicit",
    );
    assert_eq!(cfg.reflection_schedules.len(), 1);
    let s = &cfg.reflection_schedules[0];
    assert!(s.skip_when_idle);
    assert_eq!(s.min_audit_entries_to_fire, 10);
    drop(env);
}

/// Phase 95 — staged config: `min_audit_entries_to_fire =
/// 5` set but `skip_when_idle` not declared (defaults to
/// `false`) is honored without validation. The operator
/// pre-stages the threshold for later flip without it
/// having to be valid.
#[test]
fn reflection_schedule_skip_when_idle_staged_threshold_unvalidated() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[anthropic]\napi_key = \"sk-test\"\n\n\
         [[reflection_schedule]]\nname = \"staged\"\n\
         cron = \"0 0 23 * * *\"\n\
         min_audit_entries_to_fire = 5\n",
        "refl-cadence-staged",
    );
    let s = &cfg.reflection_schedules[0];
    assert!(!s.skip_when_idle);
    assert_eq!(s.min_audit_entries_to_fire, 5);
    drop(env);
}

/// Phase 95 — `skip_when_idle = true` +
/// `min_audit_entries_to_fire = 0` is rejected at load time.
/// Zero would skip every cycle unconditionally; the loader
/// defends.
#[test]
fn reflection_schedule_skip_when_idle_with_zero_threshold_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("refl-cadence-zero");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[anthropic]\napi_key = \"sk-test\"\n\n\
         [[reflection_schedule]]\nname = \"zero\"\n\
         cron = \"0 0 23 * * *\"\n\
         skip_when_idle = true\n\
         min_audit_entries_to_fire = 0\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(
                field,
                "reflection_schedule.min_audit_entries_to_fire",
            );
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn reflection_schedule_disabled_entries_are_skipped() {
    let env = EnvScope::new();
    let tmp = TempDir::new("reflection-disabled");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[reflection_schedule]]
name = "nightly"
cron = "0 0 23 * * *"
enabled = false
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert!(cfg.reflection_schedules.is_empty());
    drop(env);
}

#[test]
fn reflection_schedule_empty_cron_is_rejected() {
    let env = EnvScope::new();
    let tmp = TempDir::new("reflection-empty-cron");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[reflection_schedule]]
name = "nightly"
cron = ""
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "reflection_schedule.cron");
            assert!(reason.contains("nightly"), "{reason}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn reflection_schedule_lookback_below_min_is_rejected() {
    let env = EnvScope::new();
    let tmp = TempDir::new("reflection-lookback-low");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[reflection_schedule]]
name = "fast"
cron = "* * * * * *"
lookback_window_secs = 30
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "reflection_schedule.lookback_window_secs");
            assert!(reason.contains("fast"), "{reason}");
            assert!(reason.contains("60"), "{reason}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn reflection_schedule_lookback_above_max_is_rejected() {
    let env = EnvScope::new();
    let tmp = TempDir::new("reflection-lookback-high");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[reflection_schedule]]
name = "slow"
cron = "0 0 * * * *"
lookback_window_secs = 999999999
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(field, "reflection_schedule.lookback_window_secs");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn reflection_schedule_duplicate_name_is_rejected() {
    let env = EnvScope::new();
    let tmp = TempDir::new("reflection-dup");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[reflection_schedule]]
name = "nightly"
cron = "0 0 23 * * *"

[[reflection_schedule]]
name = "nightly"
cron = "0 0 1 * * *"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "reflection_schedule.name");
            assert!(reason.contains("duplicate"), "{reason}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn reflection_schedule_collision_with_schedule_is_rejected() {
    let env = EnvScope::new();
    let tmp = TempDir::new("reflection-vs-schedule");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[schedule]]
name = "nightly"
cron = "0 0 23 * * *"
prompt = "do stuff"

[[reflection_schedule]]
name = "nightly"
cron = "0 0 1 * * *"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "reflection_schedule.name");
            assert!(reason.contains("collides"), "{reason}");
            assert!(reason.contains("[[schedule]]"), "{reason}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn reflection_schedule_unknown_role_override_is_rejected() {
    let env = EnvScope::new();
    let tmp = TempDir::new("reflection-bad-role");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[reflection_schedule]]
name = "nightly"
cron = "0 0 23 * * *"
role_override = "ghost-role"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "reflection_schedule.role_override");
            assert!(reason.contains("ghost-role"), "{reason}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

// ==============================================================
// Phase 72 — multi-target, default-target, conditional notify
// ==============================================================

#[test]
fn singular_notify_target_bridges_into_plural() {
    let env = EnvScope::new();
    let tmp = TempDir::new("singular-alias");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[role]]
name = "default"
capability_scopes = ["notify.send"]
trust_ceiling = "Trusted"

[[notify_target]]
name = "phone"
kind = "webhook"
url = "https://example.com/x"

[[schedule]]
name = "daily"
cron = "0 0 9 * * *"
prompt = "morning summary"
notify_target = "phone"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    let sched = cfg
        .schedules
        .iter()
        .find(|s| s.name == "daily")
        .expect("schedule loaded");
    assert_eq!(sched.notify_targets, vec!["phone".to_string()]);
    assert_eq!(sched.notify_target.as_deref(), Some("phone"));
    assert_eq!(sched.notify_when, NotifyWhen::Always);
    drop(env);
}

#[test]
fn plural_notify_targets_loads_full_list() {
    let env = EnvScope::new();
    let tmp = TempDir::new("plural-list");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[role]]
name = "default"
capability_scopes = ["notify.send"]
trust_ceiling = "Trusted"

[[notify_target]]
name = "phone"
kind = "webhook"
url = "https://example.com/p"

[[notify_target]]
name = "desktop"
kind = "web-ui"

[[schedule]]
name = "daily"
cron = "0 0 9 * * *"
prompt = "morning summary"
notify_targets = ["phone", "desktop"]
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    let sched = &cfg.schedules[0];
    assert_eq!(
        sched.notify_targets,
        vec!["phone".to_string(), "desktop".to_string()]
    );
    drop(env);
}

#[test]
fn both_singular_and_plural_declared_is_rejected() {
    let env = EnvScope::new();
    let tmp = TempDir::new("both-forms");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[role]]
name = "default"
capability_scopes = ["notify.send"]
trust_ceiling = "Trusted"

[[notify_target]]
name = "phone"
kind = "webhook"
url = "https://example.com/x"

[[schedule]]
name = "daily"
cron = "0 0 9 * * *"
prompt = "morning summary"
notify_target = "phone"
notify_targets = ["phone"]
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "trigger.notify_targets");
            assert!(reason.contains("both"), "{reason}");
            assert!(reason.contains("daily"), "{reason}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn default_target_resolves_into_empty_trigger_list() {
    let env = EnvScope::new();
    let tmp = TempDir::new("default-resolve");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[role]]
name = "default"
capability_scopes = ["notify.send"]
trust_ceiling = "Trusted"

[[notify_target]]
name = "phone"
kind = "webhook"
url = "https://example.com/x"
default = true

[[schedule]]
name = "daily"
cron = "0 0 9 * * *"
prompt = "morning summary"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    // The default flag flows through to NotifyTargetConfig.
    let phone = &cfg.notify_targets[0];
    assert!(phone.is_default);
    // The schedule's empty notify_targets gets filled with the
    // default at load time.
    let sched = &cfg.schedules[0];
    assert_eq!(sched.notify_targets, vec!["phone".to_string()]);
    drop(env);
}

#[test]
fn default_target_does_not_overwrite_explicit_list() {
    let env = EnvScope::new();
    let tmp = TempDir::new("default-no-overwrite");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[role]]
name = "default"
capability_scopes = ["notify.send"]
trust_ceiling = "Trusted"

[[notify_target]]
name = "phone"
kind = "webhook"
url = "https://example.com/p"
default = true

[[notify_target]]
name = "desktop"
kind = "web-ui"

[[schedule]]
name = "daily"
cron = "0 0 9 * * *"
prompt = "morning summary"
notify_targets = ["desktop"]
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    let sched = &cfg.schedules[0];
    // The explicit list survives unchanged — default doesn't merge.
    assert_eq!(sched.notify_targets, vec!["desktop".to_string()]);
    drop(env);
}

#[test]
fn multiple_default_targets_is_rejected() {
    let env = EnvScope::new();
    let tmp = TempDir::new("dup-default");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[role]]
name = "default"
capability_scopes = ["notify.send"]
trust_ceiling = "Trusted"

[[notify_target]]
name = "phone"
kind = "webhook"
url = "https://example.com/p"
default = true

[[notify_target]]
name = "desktop"
kind = "web-ui"
default = true
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "notify_target.default");
            assert!(reason.contains("multiple"), "{reason}");
            assert!(reason.contains("phone"), "{reason}");
            assert!(reason.contains("desktop"), "{reason}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn notify_when_variants_parse() {
    for (input, expected) in [
        ("always", NotifyWhen::Always),
        ("on_failed", NotifyWhen::OnFailed),
        ("on_completed_non_empty", NotifyWhen::OnCompletedNonEmpty),
        ("on_completed_grounded", NotifyWhen::OnCompletedGrounded),
    ] {
        let env = EnvScope::new();
        let tmp = TempDir::new(&format!("notify-when-{input}"));
        let toml_path = tmp.path().join("aivyx-pa.toml");
        std::fs::write(
            &toml_path,
            format!(
                r#"
[anthropic]
api_key = "sk-test"

[[role]]
name = "default"
capability_scopes = ["notify.send"]
trust_ceiling = "Trusted"

[[notify_target]]
name = "phone"
kind = "webhook"
url = "https://example.com/x"

[[schedule]]
name = "daily"
cron = "0 0 9 * * *"
prompt = "morning"
notify_targets = ["phone"]
notify_when = "{input}"
"#,
            ),
        )
        .unwrap();
        let opts = LoadOptions {
            toml_path: Some(toml_path),
            require_api_key: false,
            require_telegram_token: false,
            require_discord_token: false,
            require_slack_tokens: false,
            role_override: None,
        };
        let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
        assert_eq!(cfg.schedules[0].notify_when, expected);
        drop(env);
    }
}

#[test]
fn notify_when_unknown_value_is_rejected() {
    let env = EnvScope::new();
    let tmp = TempDir::new("notify-when-bad");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[role]]
name = "default"
capability_scopes = ["notify.send"]
trust_ceiling = "Trusted"

[[notify_target]]
name = "phone"
kind = "webhook"
url = "https://example.com/x"

[[schedule]]
name = "daily"
cron = "0 0 9 * * *"
prompt = "morning"
notify_targets = ["phone"]
notify_when = "if_blue_moon"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "trigger.notify_when");
            assert!(reason.contains("if_blue_moon"), "{reason}");
            assert!(reason.contains("always"), "{reason}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn multi_target_with_one_unknown_name_is_rejected() {
    let env = EnvScope::new();
    let tmp = TempDir::new("multi-unknown");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[role]]
name = "default"
capability_scopes = ["notify.send"]
trust_ceiling = "Trusted"

[[notify_target]]
name = "phone"
kind = "webhook"
url = "https://example.com/x"

[[schedule]]
name = "daily"
cron = "0 0 9 * * *"
prompt = "morning"
notify_targets = ["phone", "ghost"]
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "schedule.notify_targets");
            assert!(reason.contains("ghost"), "{reason}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

// ==============================================================
// Phase 73 — retry + rate-limit fields on notify_target
// ==============================================================

#[test]
fn retry_fields_default_to_zero_count_and_default_backoff() {
    let env = EnvScope::new();
    let tmp = TempDir::new("retry-defaults");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[notify_target]]
name = "phone"
kind = "webhook"
url = "https://example.com/x"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    let t = &cfg.notify_targets[0];
    assert_eq!(t.retry_count, 0);
    assert_eq!(t.retry_backoff_ms_start, 500); // DEFAULT_RETRY_BACKOFF_MS_START
    assert!(t.rate_limit_max.is_none());
    assert!(t.rate_limit_window_secs.is_none());
    drop(env);
}

#[test]
fn retry_count_above_cap_is_rejected() {
    let env = EnvScope::new();
    let tmp = TempDir::new("retry-too-high");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[notify_target]]
name = "phone"
kind = "webhook"
url = "https://example.com/x"
retry_count = 100
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "notify_target.retry_count");
            assert!(reason.contains("100"), "{reason}");
            assert!(reason.contains("hard cap"), "{reason}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn retry_backoff_below_floor_is_rejected() {
    let env = EnvScope::new();
    let tmp = TempDir::new("backoff-too-low");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[notify_target]]
name = "phone"
kind = "webhook"
url = "https://example.com/x"
retry_count = 3
retry_backoff_ms_start = 50
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "notify_target.retry_backoff_ms_start");
            assert!(reason.contains("50"), "{reason}");
            assert!(reason.contains("100"), "{reason}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn retry_explicit_values_parse() {
    let env = EnvScope::new();
    let tmp = TempDir::new("retry-explicit");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[notify_target]]
name = "phone"
kind = "webhook"
url = "https://example.com/x"
retry_count = 5
retry_backoff_ms_start = 200
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    let t = &cfg.notify_targets[0];
    assert_eq!(t.retry_count, 5);
    assert_eq!(t.retry_backoff_ms_start, 200);
    drop(env);
}

#[test]
fn rate_limit_partial_max_without_window_is_rejected() {
    let env = EnvScope::new();
    let tmp = TempDir::new("rate-no-window");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[notify_target]]
name = "phone"
kind = "webhook"
url = "https://example.com/x"
rate_limit_max = 10
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "notify_target.rate_limit_window_secs");
            assert!(reason.contains("both"), "{reason}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn rate_limit_partial_window_without_max_is_rejected() {
    let env = EnvScope::new();
    let tmp = TempDir::new("rate-no-max");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[notify_target]]
name = "phone"
kind = "webhook"
url = "https://example.com/x"
rate_limit_window_secs = 3600
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "notify_target.rate_limit_max");
            assert!(reason.contains("both"), "{reason}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn rate_limit_zero_max_is_rejected() {
    let env = EnvScope::new();
    let tmp = TempDir::new("rate-zero-max");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[notify_target]]
name = "phone"
kind = "webhook"
url = "https://example.com/x"
rate_limit_max = 0
rate_limit_window_secs = 60
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(field, "notify_target.rate_limit_max");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn rate_limit_both_set_parses() {
    let env = EnvScope::new();
    let tmp = TempDir::new("rate-both");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[notify_target]]
name = "phone"
kind = "webhook"
url = "https://example.com/x"
rate_limit_max = 10
rate_limit_window_secs = 3600
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    let t = &cfg.notify_targets[0];
    assert_eq!(t.rate_limit_max, Some(10));
    assert_eq!(t.rate_limit_window_secs, Some(3600));
    drop(env);
}

// ==============================================================
// Phase 74 — [[memory.retention]] config blocks
// ==============================================================

#[test]
fn memory_retention_forever_parses() {
    let env = EnvScope::new();
    let tmp = TempDir::new("retention-forever");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[memory.retention]]
topic_glob = "project/*"
retention = "forever"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.memory_retention.len(), 1);
    let rule = &cfg.memory_retention[0];
    assert_eq!(rule.topic_glob, "project/*");
    assert!(matches!(rule.retention, crate::RetentionPolicy::Forever));
    // Glob matcher works as expected.
    assert!(rule.matcher.is_match("project/x"));
    assert!(!rule.matcher.is_match("notes/today"));
    drop(env);
}

#[test]
fn memory_retention_days_parses() {
    let env = EnvScope::new();
    let tmp = TempDir::new("retention-days");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[memory.retention]]
topic_glob = "notes/*"
retention_days = 30
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.memory_retention.len(), 1);
    assert!(matches!(
        cfg.memory_retention[0].retention,
        crate::RetentionPolicy::ForDays(30)
    ));
    drop(env);
}

#[test]
fn memory_retention_multiple_rules_preserve_first_match_order() {
    let env = EnvScope::new();
    let tmp = TempDir::new("retention-multi");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[memory.retention]]
topic_glob = "project/critical/*"
retention = "forever"

[[memory.retention]]
topic_glob = "project/*"
retention_days = 90

[[memory.retention]]
topic_glob = "notes/*"
retention_days = 30
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert_eq!(cfg.memory_retention.len(), 3);
    // Order preserved — operator put narrower glob first.
    assert_eq!(cfg.memory_retention[0].topic_glob, "project/critical/*");
    assert_eq!(cfg.memory_retention[1].topic_glob, "project/*");
    assert_eq!(cfg.memory_retention[2].topic_glob, "notes/*");
    drop(env);
}

#[test]
fn memory_retention_empty_glob_rejects() {
    let env = EnvScope::new();
    let tmp = TempDir::new("retention-empty-glob");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[memory.retention]]
topic_glob = ""
retention = "forever"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(field, "memory.retention.topic_glob");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn memory_retention_unknown_retention_value_rejects() {
    let env = EnvScope::new();
    let tmp = TempDir::new("retention-bad-value");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[memory.retention]]
topic_glob = "project/*"
retention = "until-summer"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "memory.retention.retention");
            assert!(reason.contains("until-summer"), "{reason}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn memory_retention_zero_days_rejects() {
    let env = EnvScope::new();
    let tmp = TempDir::new("retention-zero-days");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[memory.retention]]
topic_glob = "notes/*"
retention_days = 0
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(field, "memory.retention.retention_days");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn memory_retention_neither_form_rejects() {
    let env = EnvScope::new();
    let tmp = TempDir::new("retention-no-policy");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[memory.retention]]
topic_glob = "notes/*"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "memory.retention");
            assert!(reason.contains("must declare"), "{reason}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn memory_retention_both_forms_rejects() {
    let env = EnvScope::new();
    let tmp = TempDir::new("retention-both-forms");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[memory.retention]]
topic_glob = "notes/*"
retention = "forever"
retention_days = 30
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "memory.retention");
            assert!(reason.contains("both"), "{reason}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn memory_retention_invalid_glob_rejects() {
    let env = EnvScope::new();
    let tmp = TempDir::new("retention-bad-glob");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[anthropic]
api_key = "sk-test"

[[memory.retention]]
topic_glob = "[unclosed"
retention = "forever"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "memory.retention.topic_glob");
            assert!(reason.contains("not a valid glob"), "{reason}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

// ------------------------------------------------------------------
// Phase 75 — [embedding] section
// ------------------------------------------------------------------

/// No `[embedding]` section → `embedding: None`. Semantic search
/// is disabled; pre-Phase-75 configs are unaffected.
#[test]
fn embedding_absent_section_is_none() {
    let env = EnvScope::new();
    let cfg = AivyxConfig::load_from_env_and_toml(&LoadOptions::test_env_only())
        .expect("load");
    assert!(cfg.embedding.is_none());
    drop(env);
}

/// A `[embedding]` section with only the api_key set: the three
/// non-secret fields fall back to the `DEFAULT_EMBEDDING_*`
/// constants, the key is `FieldSource::Toml`.
#[test]
fn embedding_partial_section_applies_defaults() {
    let env = EnvScope::new();
    env.clear("AIVYX_PA_EMBEDDING_API_KEY");
    let tmp = TempDir::new("embedding-defaults");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[embedding]
api_key = "sk-emb-toml"
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    let emb = cfg.embedding.expect("section present");
    assert_eq!(emb.base_url, crate::DEFAULT_EMBEDDING_BASE_URL);
    assert_eq!(emb.model, crate::DEFAULT_EMBEDDING_MODEL);
    assert_eq!(emb.dimensions, crate::DEFAULT_EMBEDDING_DIMENSIONS);
    // Phase 76 — RAG knobs default when unspecified.
    assert_eq!(emb.rag_top_k, crate::DEFAULT_RAG_TOP_K);
    assert_eq!(
        emb.rag_min_similarity,
        crate::DEFAULT_RAG_MIN_SIMILARITY
    );
    // Phase 86 — recall-window default is 1 (= byte-identical
    // to pre-Phase-86 single-message behaviour).
    assert_eq!(
        emb.recall_window_turns,
        crate::DEFAULT_RECALL_WINDOW_TURNS
    );
    assert_eq!(emb.recall_window_turns, 1);
    let key = emb.api_key.expect("key from toml");
    assert_eq!(key.source, FieldSource::Toml);
    assert_eq!(key.value.expose_secret(), "sk-emb-toml");
    drop(env);
}

/// Explicit values override every default; a local base_url
/// keeps embedding on-device.
#[test]
fn embedding_explicit_fields_win() {
    let env = EnvScope::new();
    env.clear("AIVYX_PA_EMBEDDING_API_KEY");
    let tmp = TempDir::new("embedding-explicit");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[embedding]
base_url = "http://localhost:11434"
model = "nomic-embed-text"
dimensions = 768
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    let emb = cfg.embedding.expect("section present");
    assert_eq!(emb.base_url, "http://localhost:11434");
    assert_eq!(emb.model, "nomic-embed-text");
    assert_eq!(emb.dimensions, 768);
    // No key set anywhere — a local server needs none.
    assert!(emb.api_key.is_none());
    drop(env);
}

/// `AIVYX_PA_EMBEDDING_API_KEY` beats the TOML `api_key` (env >
/// TOML), matching the anthropic / openai key precedence.
#[test]
fn embedding_env_key_beats_toml_key() {
    let env = EnvScope::new();
    env.set("AIVYX_PA_EMBEDDING_API_KEY", "sk-emb-env");
    let tmp = TempDir::new("embedding-env-wins");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[embedding]
api_key = "sk-emb-toml-loses"
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    let key = cfg.embedding.unwrap().api_key.expect("key present");
    assert_eq!(key.source, FieldSource::Env);
    assert_eq!(key.value.expose_secret(), "sk-emb-env");
    drop(env);
}

/// A blank `base_url` is a load-time `Invalid`.
#[test]
fn embedding_blank_base_url_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("embedding-blank-url");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[embedding]
base_url = "   "
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(field, "embedding.base_url");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

/// `dimensions = 0` is a load-time `Invalid`.
#[test]
fn embedding_zero_dimensions_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("embedding-zero-dims");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[embedding]
dimensions = 0
"#,
    )
    .unwrap();

    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(field, "embedding.dimensions");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

/// Phase 76 — explicit RAG knobs override the defaults.
#[test]
fn embedding_rag_knobs_explicit_win() {
    let env = EnvScope::new();
    env.clear("AIVYX_PA_EMBEDDING_API_KEY");
    let tmp = TempDir::new("embedding-rag-explicit");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[embedding]
base_url = "http://localhost:11434"
rag_top_k = 12
rag_min_similarity = 0.55
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    let emb = cfg.embedding.expect("section present");
    assert_eq!(emb.rag_top_k, 12);
    assert!((emb.rag_min_similarity - 0.55).abs() < 1e-6);
}

/// Phase 76 — `rag_top_k = 0` is a load-time `Invalid`.
#[test]
fn embedding_rag_top_k_zero_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("embedding-rag-topk-zero");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[embedding]
rag_top_k = 0
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(field, "embedding.rag_top_k");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

/// Phase 76 — `rag_min_similarity` outside `[0.0, 1.0]` is a
/// load-time `Invalid`.
#[test]
fn embedding_rag_min_similarity_out_of_range_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("embedding-rag-sim-oor");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[embedding]
rag_min_similarity = 1.5
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(field, "embedding.rag_min_similarity");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

/// Phase 86 — explicit `recall_window_turns` wins; the
/// default-when-absent is asserted in
/// `embedding_partial_section_applies_defaults`.
#[test]
fn embedding_recall_window_turns_explicit_wins() {
    let env = EnvScope::new();
    let tmp = TempDir::new("embedding-window-explicit");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[embedding]
recall_window_turns = 5
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts)
        .expect("load");
    assert_eq!(
        cfg.embedding.expect("section").recall_window_turns,
        5
    );
    drop(env);
}

/// Phase 86 — `recall_window_turns = 0` is a load-time
/// `Invalid` (1 is the byte-identical-to-pre-Phase-86 floor).
#[test]
fn embedding_recall_window_turns_zero_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("embedding-window-zero");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[embedding]
recall_window_turns = 0
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(
                field,
                "embedding.recall_window_turns"
            );
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

/// Phase 90 — `recall_gate_min_chars` defaults to `0`
/// (gate disabled = byte-identical to pre-Phase-90).
#[test]
fn embedding_recall_gate_min_chars_default_is_zero() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[embedding]\nmodel = \"any\"\n",
        "embed-gate-default",
    );
    let emb = cfg.embedding.expect("section present");
    assert_eq!(
        emb.recall_gate_min_chars,
        crate::DEFAULT_RECALL_GATE_MIN_CHARS,
    );
    assert_eq!(emb.recall_gate_min_chars, 0);
    drop(env);
}

/// Phase 90 — explicit `recall_gate_min_chars` wins.
#[test]
fn embedding_recall_gate_min_chars_explicit_wins() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[embedding]\nrecall_gate_min_chars = 6\n",
        "embed-gate-explicit",
    );
    let emb = cfg.embedding.expect("section present");
    assert_eq!(emb.recall_gate_min_chars, 6);
    drop(env);
}

/// Phase 90 — explicit `recall_gate_min_chars = 0` is
/// honored (the operator can express the default explicitly
/// without changing behaviour). Any value is legal —
/// large thresholds gate aggressively, the operator's call.
#[test]
fn embedding_recall_gate_min_chars_explicit_zero_honored() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[embedding]\nrecall_gate_min_chars = 0\n",
        "embed-gate-explicit-zero",
    );
    let emb = cfg.embedding.expect("section present");
    assert_eq!(emb.recall_gate_min_chars, 0);
    drop(env);
}

/// Phase 96 — defaults pinned. With no ANN knobs set, the
/// embedding section builds with `ann_index = false` and
/// `ann_rebuild_threshold = 100`. Pre-Phase-96 behaviour
/// is byte-identical for every operator.
#[test]
fn embedding_ann_index_defaults() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[embedding]\nmodel = \"text-embedding-3-small\"\n",
        "embed-ann-defaults",
    );
    let emb = cfg.embedding.expect("section present");
    assert!(!emb.ann_index);
    assert_eq!(
        emb.ann_rebuild_threshold,
        crate::DEFAULT_ANN_REBUILD_THRESHOLD,
    );
    drop(env);
}

/// Phase 96 — explicit `ann_index = true` +
/// `ann_rebuild_threshold = 50` round-trips through the
/// loader.
#[test]
fn embedding_ann_index_explicit_values_win() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[embedding]\nann_index = true\n\
         ann_rebuild_threshold = 50\n",
        "embed-ann-explicit",
    );
    let emb = cfg.embedding.expect("section present");
    assert!(emb.ann_index);
    assert_eq!(emb.ann_rebuild_threshold, 50);
    drop(env);
}

/// Phase 96 — staged config: `ann_rebuild_threshold = 25`
/// set but `ann_index = false` (the default) is honored
/// unvalidated. Mirrors the established staged-config
/// posture (Phase 85 / 87 / 91 / 92 / 95).
#[test]
fn embedding_ann_index_staged_threshold_unvalidated() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[embedding]\nann_rebuild_threshold = 25\n",
        "embed-ann-staged",
    );
    let emb = cfg.embedding.expect("section present");
    assert!(!emb.ann_index);
    assert_eq!(emb.ann_rebuild_threshold, 25);
    drop(env);
}

/// Phase 97 — default `recall_token_budget = 0` means
/// budget enforcement is disabled. Pre-Phase-97
/// behaviour byte-identical for every operator.
#[test]
fn embedding_recall_token_budget_default_is_zero() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[embedding]\nmodel = \"text-embedding-3-small\"\n",
        "embed-budget-default",
    );
    let emb = cfg.embedding.expect("section present");
    assert_eq!(emb.recall_token_budget, 0);
    drop(env);
}

/// Phase 97 — explicit `recall_token_budget = 2000`
/// round-trips through the loader.
#[test]
fn embedding_recall_token_budget_explicit_wins() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[embedding]\nrecall_token_budget = 2000\n",
        "embed-budget-explicit",
    );
    let emb = cfg.embedding.expect("section present");
    assert_eq!(emb.recall_token_budget, 2000);
    drop(env);
}

/// Phase 97 — explicit `recall_token_budget = 0` honored
/// (operator can declare the default explicitly without
/// changing behaviour). No bounds-rejection — any value
/// is legal.
#[test]
fn embedding_recall_token_budget_explicit_zero_honored() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[embedding]\nrecall_token_budget = 0\n",
        "embed-budget-zero",
    );
    let emb = cfg.embedding.expect("section present");
    assert_eq!(emb.recall_token_budget, 0);
    drop(env);
}

/// Phase 98 — default `recall_hybrid = false` means
/// hybrid fusion is disabled. Pre-Phase-98 behaviour
/// byte-identical for every operator.
#[test]
fn embedding_recall_hybrid_default_is_false() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[embedding]\nmodel = \"text-embedding-3-small\"\n",
        "embed-hybrid-default",
    );
    let emb = cfg.embedding.expect("section present");
    assert!(!emb.recall_hybrid);
    drop(env);
}

/// Phase 98 — explicit `recall_hybrid = true` round-trips.
#[test]
fn embedding_recall_hybrid_explicit_true_wins() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[embedding]\nrecall_hybrid = true\n",
        "embed-hybrid-true",
    );
    let emb = cfg.embedding.expect("section present");
    assert!(emb.recall_hybrid);
    drop(env);
}

/// Phase 98 — explicit `recall_hybrid = false` honored
/// (operator can declare the default explicitly).
#[test]
fn embedding_recall_hybrid_explicit_false_honored() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[embedding]\nrecall_hybrid = false\n",
        "embed-hybrid-false",
    );
    let emb = cfg.embedding.expect("section present");
    assert!(!emb.recall_hybrid);
    drop(env);
}

/// Phase 96 — `ann_index = true` +
/// `ann_rebuild_threshold = 0` is rejected. Zero would
/// force a rebuild every recall and defeat the perf win.
#[test]
fn embedding_ann_index_zero_threshold_when_armed_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("embed-ann-zero");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[embedding]\nann_index = true\n\
         ann_rebuild_threshold = 0\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(
                field,
                "embedding.ann_rebuild_threshold",
            );
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

/// Env + TOML do not supply the embedding key, but the
/// encrypted store has a `secret_keys::EMBEDDING_API_KEY` row.
/// After hydration the key is populated with
/// `FieldSource::EncryptedStore`.
#[tokio::test]
async fn embedding_key_hydrates_from_store() {
    use aivyx_crypto::MasterKey;
    use aivyx_storage::{KeyDomain, RedbStorage, StorageConfig};

    let env = EnvScope::new();
    env.clear("AIVYX_PA_EMBEDDING_API_KEY");

    let tmp = TempDir::new("embedding-store-hydrate");
    let store_path = tmp.path().join("store.redb");
    let master = MasterKey::from_raw([11u8; 32]);
    let storage = RedbStorage::open(StorageConfig::new(store_path), master)
        .await
        .expect("open store");
    let secrets = storage.domain(KeyDomain::Secrets);
    secrets
        .put(crate::secret_keys::EMBEDDING_API_KEY, b"sk-emb-from-store")
        .await
        .expect("put embedding key");

    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        r#"
[embedding]
model = "text-embedding-3-small"
"#,
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let mut cfg =
        AivyxConfig::load_from_env_and_toml(&opts).expect("load");
    assert!(cfg.embedding.as_ref().unwrap().api_key.is_none());

    cfg.hydrate_secrets_from_store(&storage)
        .await
        .expect("hydrate");
    let key = cfg
        .embedding
        .unwrap()
        .api_key
        .expect("hydrated from store");
    assert_eq!(key.source, FieldSource::EncryptedStore);
    assert_eq!(key.value.expose_secret(), "sk-emb-from-store");
    drop(env);
}

/// Hydration must not materialize an `EmbeddingConfig` when the
/// `[embedding]` section was absent, even if the store holds a
/// key row (mirrors the telegram-token rule).
#[tokio::test]
async fn embedding_store_key_without_section_stays_none() {
    use aivyx_crypto::MasterKey;
    use aivyx_storage::{KeyDomain, RedbStorage, StorageConfig};

    let env = EnvScope::new();
    env.clear("AIVYX_PA_EMBEDDING_API_KEY");
    let tmp = TempDir::new("embedding-no-section");
    let store_path = tmp.path().join("store.redb");
    let master = MasterKey::from_raw([12u8; 32]);
    let storage = RedbStorage::open(StorageConfig::new(store_path), master)
        .await
        .unwrap();
    storage
        .domain(KeyDomain::Secrets)
        .put(crate::secret_keys::EMBEDDING_API_KEY, b"orphan-key")
        .await
        .unwrap();

    let mut cfg =
        AivyxConfig::load_from_env_and_toml(&LoadOptions::test_env_only())
            .expect("load");
    cfg.hydrate_secrets_from_store(&storage).await.unwrap();
    assert!(cfg.embedding.is_none());
    drop(env);
}

// ------------------------------------------------------------------
// Phase 80 — [proactive] section
// ------------------------------------------------------------------

fn load_with_toml(body: &str, tag: &str) -> AivyxConfig {
    assert_env_guarded();
    let tmp = TempDir::new(tag);
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(&toml_path, body).unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    AivyxConfig::load_from_env_and_toml(&opts).expect("load")
}

/// Like [`load_with_toml`] but returns the `Result` so error-path tests can
/// assert the typed `ConfigError` instead of panicking on load.
fn load_with_toml_result(body: &str, tag: &str) -> Result<AivyxConfig, ConfigError> {
    assert_env_guarded();
    let tmp = TempDir::new(tag);
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(&toml_path, body).unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    AivyxConfig::load_from_env_and_toml(&opts)
}

/// No `[proactive]` section → `proactive: None` (off; the
/// assistant never reaches out unprompted, pre-Phase-80).
#[test]
fn proactive_absent_section_is_none() {
    let env = EnvScope::new();
    let cfg = AivyxConfig::load_from_env_and_toml(
        &LoadOptions::test_env_only(),
    )
    .expect("load");
    assert!(cfg.proactive.is_none());
    drop(env);
}

/// A present-but-disabled section may be partial (staged
/// config): it builds with `enabled = false`, defaults
/// elsewhere, and is NOT validated.
#[test]
fn proactive_present_disabled_is_allowed_partial() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[proactive]\nenabled = false\n",
        "proactive-staged",
    );
    let p = cfg.proactive.expect("section present");
    assert!(!p.enabled);
    assert_eq!(p.target, "");
    assert_eq!(
        p.max_per_window,
        crate::DEFAULT_PROACTIVE_MAX_PER_WINDOW
    );
    assert_eq!(
        p.window_secs,
        crate::DEFAULT_PROACTIVE_WINDOW_SECS
    );
    // Signals default on.
    assert!(p.signals.ttl_expiry);
    assert!(p.signals.recall_cluster);
    assert!(p.signals.due_reminder);
    drop(env);
}

/// Enabled + valid: explicit fields win; an explicitly-off
/// signal is respected while the others default on.
#[test]
fn proactive_enabled_valid_with_signal_toggle() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[proactive]\nenabled = true\ntarget = \"ops\"\n\
         max_per_window = 5\nwindow_secs = 3600\n\
         signal_due_reminder = false\n",
        "proactive-valid",
    );
    let p = cfg.proactive.expect("section present");
    assert!(p.enabled);
    assert_eq!(p.target, "ops");
    assert_eq!(p.max_per_window, 5);
    assert_eq!(p.window_secs, 3600);
    assert!(p.signals.ttl_expiry);
    assert!(p.signals.recall_cluster);
    assert!(!p.signals.due_reminder);
    drop(env);
}

/// Enabled without a target → load-time `Invalid`.
#[test]
fn proactive_enabled_requires_target() {
    let env = EnvScope::new();
    let tmp = TempDir::new("proactive-no-target");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(&toml_path, "\n[proactive]\nenabled = true\n")
        .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(field, "proactive.target");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

/// Enabled with `max_per_window = 0` → `Invalid`.
#[test]
fn proactive_enabled_zero_cap_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("proactive-zero-cap");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[proactive]\nenabled = true\ntarget = \"ops\"\n\
         max_per_window = 0\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(field, "proactive.max_per_window");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

/// Enabled with every signal class off → `Invalid`.
#[test]
fn proactive_enabled_all_signals_off_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("proactive-no-signals");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[proactive]\nenabled = true\ntarget = \"ops\"\n\
         signal_ttl_expiry = false\n\
         signal_recall_cluster = false\n\
         signal_due_reminder = false\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(field, "proactive.signals");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

// ------------------------------------------------------------------
// Chapter W — [persona_seed] section
// ------------------------------------------------------------------

/// No `[persona_seed]` section → `persona_seed: None` (no onboarding seed).
/// `[skills] starter = false` isolates this from Chapter Outfit's default
/// starter-skill merge (tested separately).
#[test]
fn persona_seed_absent_section_is_none() {
    let env = EnvScope::new();
    let cfg = load_with_toml("\n[skills]\nstarter = false\n", "seed-absent");
    assert!(cfg.persona_seed.is_none());
    drop(env);
}

/// A populated `[persona_seed]` parses every facet list + the
/// `[[persona_seed.skill]]` array-of-tables.
#[test]
fn persona_seed_parses_facets_and_skills() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[persona_seed]\n\
         learned_context = [\"operator builds Aivyx\"]\n\
         communication_adaptations = [\"leads with code\"]\n\
         character_traits = [\"pragmatic\", \"precise\"]\n\
         relationship_milestones = [\"genesis: first launch\"]\n\
         \n[[persona_seed.skill]]\n\
         name = \"rust-review\"\n\
         trigger = \"when asked to review Rust\"\n\
         procedure = \"check unwraps + lifetimes; cite file:line\"\n\
         \n[skills]\nstarter = false\n",
        "seed-full",
    );
    let s = cfg.persona_seed.expect("section present");
    assert_eq!(s.learned_context, vec!["operator builds Aivyx"]);
    assert_eq!(s.communication_adaptations, vec!["leads with code"]);
    assert_eq!(s.character_traits, vec!["pragmatic", "precise"]);
    assert_eq!(s.relationship_milestones, vec!["genesis: first launch"]);
    assert_eq!(s.skills.len(), 1);
    assert_eq!(s.skills[0].name, "rust-review");
    assert_eq!(s.skills[0].trigger, "when asked to review Rust");
    assert!(s.skills[0].procedure.contains("unwraps"));
    drop(env);
}

/// Normalization: blank/whitespace facets are dropped, and a skill with no
/// `name` (its identifier) is dropped.
#[test]
fn persona_seed_normalizes_blanks_and_drops_nameless_skills() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[persona_seed]\n\
         character_traits = [\"  pragmatic  \", \"\", \"   \"]\n\
         \n[[persona_seed.skill]]\n\
         name = \"\"\n\
         procedure = \"orphan — no name, dropped\"\n\
         \n[[persona_seed.skill]]\n\
         name = \"kept\"\n\
         trigger = \"t\"\n\
         procedure = \"p\"\n\
         \n[skills]\nstarter = false\n",
        "seed-norm",
    );
    let s = cfg.persona_seed.expect("section present");
    assert_eq!(s.character_traits, vec!["pragmatic"], "blanks trimmed/dropped");
    assert_eq!(s.skills.len(), 1, "nameless skill dropped");
    assert_eq!(s.skills[0].name, "kept");
    drop(env);
}

/// An all-empty section (only blank entries) collapses to `None` — nothing to
/// seed.
#[test]
fn persona_seed_all_blank_is_none() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[persona_seed]\nlearned_context = [\"\", \"  \"]\ncharacter_traits = []\n\
         \n[skills]\nstarter = false\n",
        "seed-empty",
    );
    assert!(cfg.persona_seed.is_none());
    drop(env);
}

// ------------------------------------------------------------------
// Chapter Outfit — default starter skills
// ------------------------------------------------------------------

/// Drift guard on the compiled-in starter repertoire: a fixed count of
/// well-formed `{name, trigger, procedure}` recipes with unique kebab-case
/// names, and a compactness ceiling so the standing per-turn cost (the
/// `name: trigger` line each renders) can't quietly balloon.
#[test]
fn default_starter_skills_carry_valid_recipes() {
    let skills = crate::default_starter_skills();
    assert_eq!(skills.len(), 5, "the curated starter set is five skills");

    let mut seen = std::collections::HashSet::new();
    for sk in &skills {
        assert!(!sk.name.trim().is_empty(), "skill has a name");
        assert!(!sk.trigger.trim().is_empty(), "{} has a trigger", sk.name);
        assert!(!sk.procedure.trim().is_empty(), "{} has a procedure", sk.name);
        // Kebab-case identifier: lowercase, no whitespace.
        assert!(
            sk.name.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
            "skill name {:?} is kebab-case",
            sk.name
        );
        assert!(seen.insert(&sk.name), "skill name {:?} is unique", sk.name);
        // Compactness: a recipe, not an essay.
        assert!(
            sk.trigger.len() < 240 && sk.procedure.len() < 600,
            "skill {:?} stays compact (trigger {}, procedure {})",
            sk.name,
            sk.trigger.len(),
            sk.procedure.len()
        );
    }
}

/// A bare config (no `[persona_seed]`, no `[skills]`) gets the full starter
/// repertoire merged in — the default-on behavior that equips a fresh agent.
#[test]
fn bare_config_merges_default_starter_skills() {
    let env = EnvScope::new();
    let cfg = AivyxConfig::load_from_env_and_toml(&LoadOptions::test_env_only()).expect("load");
    let seed = cfg.persona_seed.expect("starter skills make the seed Some");
    let names: Vec<&str> = seed.skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(seed.skills.len(), 5);
    assert!(names.contains(&"summarize-document"));
    assert!(names.contains(&"daily-briefing"));
    assert!(names.contains(&"capture-note"));
    // Only skills are seeded by default — no facets get invented.
    assert!(seed.learned_context.is_empty());
    assert!(seed.character_traits.is_empty());
    drop(env);
}

/// Operator-declared skills win on a name collision: the operator's
/// `daily-briefing` replaces the default, and the other four defaults are still
/// appended (no duplicate, no loss).
#[test]
fn operator_declared_skill_wins_over_starter_default() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[[persona_seed.skill]]\n\
         name = \"daily-briefing\"\n\
         trigger = \"my own trigger\"\n\
         procedure = \"my own procedure\"\n",
        "outfit-collision",
    );
    let seed = cfg.persona_seed.expect("seed present");
    // 1 operator skill + 4 non-colliding defaults = 5 (no duplicate).
    assert_eq!(seed.skills.len(), 5);
    let briefings: Vec<&crate::SeedSkill> =
        seed.skills.iter().filter(|s| s.name == "daily-briefing").collect();
    assert_eq!(briefings.len(), 1, "no duplicate daily-briefing");
    assert_eq!(
        briefings[0].procedure, "my own procedure",
        "operator's version wins, not the default"
    );
    drop(env);
}

/// `[skills] starter = false` suppresses the defaults entirely — byte-identical
/// to a pre-Outfit build. A bare config stays `None`; an operator seed keeps
/// only the operator's own skills.
#[test]
fn skills_starter_false_suppresses_defaults() {
    let env = EnvScope::new();
    // Bare + opt-out → no seed at all.
    let bare = load_with_toml("\n[skills]\nstarter = false\n", "outfit-off-bare");
    assert!(bare.persona_seed.is_none());

    // Operator seed + opt-out → exactly the operator's skills, no defaults.
    let with_op = load_with_toml(
        "\n[[persona_seed.skill]]\n\
         name = \"rust-review\"\n\
         trigger = \"t\"\n\
         procedure = \"p\"\n\
         \n[skills]\nstarter = false\n",
        "outfit-off-op",
    );
    let seed = with_op.persona_seed.expect("operator seed present");
    assert_eq!(seed.skills.len(), 1);
    assert_eq!(seed.skills[0].name, "rust-review");
    drop(env);
}

// Phase 81 — [persona_lifecycle] section
// ------------------------------------------------------------------

/// No `[persona_lifecycle]` section → `persona_lifecycle: None`
/// (off; the Persona only ever grows, pre-Phase-81).
#[test]
fn persona_lifecycle_absent_section_is_none() {
    let env = EnvScope::new();
    let cfg = AivyxConfig::load_from_env_and_toml(
        &LoadOptions::test_env_only(),
    )
    .expect("load");
    assert!(cfg.persona_lifecycle.is_none());
    drop(env);
}

/// A present-but-disabled section may be partial (staged
/// config): it builds with `enabled = false`, defaults
/// elsewhere, and is NOT validated.
#[test]
fn persona_lifecycle_present_disabled_is_allowed_partial() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[persona_lifecycle]\nenabled = false\n",
        "pl-staged",
    );
    let p = cfg.persona_lifecycle.expect("section present");
    assert!(!p.enabled);
    assert!(
        (p.consolidation_similarity
            - crate::DEFAULT_PL_CONSOLIDATION_SIMILARITY)
            .abs()
            < 1e-6
    );
    assert_eq!(
        p.decay_max_age_secs,
        crate::DEFAULT_PL_DECAY_MAX_AGE_SECS
    );
    assert_eq!(
        p.min_soft_facets,
        crate::DEFAULT_PL_MIN_SOFT_FACETS
    );
    // Phase 85 — helpfulness-decay knobs default.
    assert!(
        (p.decay_unhelpful_threshold
            - crate::DEFAULT_PL_DECAY_UNHELPFUL_THRESHOLD)
            .abs()
            < 1e-6
    );
    assert_eq!(
        p.decay_min_samples,
        crate::DEFAULT_PL_DECAY_MIN_SAMPLES
    );
    // Phase 88 — pair-affinity decay floor default.
    assert!(
        (p.decay_pair_below_affinity
            - crate::DEFAULT_PL_DECAY_PAIR_BELOW_AFFINITY)
            .abs()
            < 1e-6
    );
    // Signals default on.
    assert!(p.signals.consolidate);
    assert!(p.signals.decay);
    drop(env);
}

/// Phase 85 — explicit helpfulness-decay knobs win; the two
/// validation cases fire only when decay is armed.
#[test]
fn wiki_section_parses_defaults_and_overrides() {
    let _env = EnvScope::new();
    // Absent section → None (no synthesis).
    let cfg = load_with_toml("\n", "wiki-absent");
    assert!(cfg.wiki.is_none());

    // Present + enabled with defaults filled in.
    let cfg = load_with_toml("\n[wiki]\nenabled = true\n", "wiki-default");
    let w = cfg.wiki.expect("section present");
    assert!(w.enabled);
    assert_eq!(w.max_pages_per_sweep, crate::DEFAULT_WIKI_MAX_PAGES_PER_SWEEP);
    assert_eq!(w.interval_secs, crate::DEFAULT_WIKI_INTERVAL_SECS);

    // Overrides honored.
    let cfg = load_with_toml(
        "\n[wiki]\nenabled = true\nmax_pages_per_sweep = 5\ninterval_secs = 900\n",
        "wiki-override",
    );
    let w = cfg.wiki.unwrap();
    assert_eq!(w.max_pages_per_sweep, 5);
    assert_eq!(w.interval_secs, 900);

    // Present but disabled (staged) → Some, no validation of zero knobs.
    let cfg = load_with_toml(
        "\n[wiki]\nenabled = false\nmax_pages_per_sweep = 0\n",
        "wiki-staged",
    );
    assert!(!cfg.wiki.unwrap().enabled);
}

#[test]
fn graph_vocabulary_subtable_parses() {
    let _env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[graph]\nenabled = true\n\
         [graph.vocabulary]\ndepends-on = [\"builds on\", \"sits atop\"]\nrivals = [\"competes with\"]\n",
        "graph-vocab",
    );
    let g = cfg.graph.expect("graph section");
    assert!(g.enabled);
    // BTreeMap → deterministic order: depends-on, rivals.
    assert_eq!(g.vocabulary.len(), 2);
    let deps = g.vocabulary.iter().find(|(c, _)| c == "depends-on").unwrap();
    assert_eq!(deps.1, vec!["builds on".to_string(), "sits atop".to_string()]);
    // The section is `Some` even when only [graph.vocabulary] is present.
    let only_vocab = load_with_toml(
        "\n[graph.vocabulary]\nowns = [\"stewards\"]\n",
        "graph-vocab-only",
    );
    assert!(only_vocab.graph.is_some());
}

#[test]
fn skill_authoring_section_parses_and_defaults() {
    let _env = EnvScope::new();
    let off = load_with_toml("\n[agent]\nprovider = \"ollama\"\n", "ska-off");
    assert!(off.skill_authoring.is_none());
    let cfg = load_with_toml(
        "\n[skill_authoring]\nenabled = true\nmin_edges = 3\n",
        "ska-on",
    );
    let s = cfg.skill_authoring.expect("section present");
    assert!(s.enabled);
    assert_eq!(s.min_edges, 3);
    assert_eq!(s.min_summary_chars, crate::DEFAULT_AUTHOR_MIN_SUMMARY_CHARS);
    assert_eq!(s.max_per_cycle, crate::DEFAULT_AUTHOR_MAX_PER_CYCLE);
}

#[test]
fn skill_refinement_section_parses_and_defaults() {
    let _env = EnvScope::new();
    // Absent → None.
    let off = load_with_toml("\n[agent]\nprovider = \"ollama\"\n", "skr-off");
    assert!(off.skill_refinement.is_none());
    // Present → Some; unset fields take defaults; enabled honored.
    let cfg = load_with_toml(
        "\n[skill_refinement]\nenabled = true\nmin_samples = 6\n",
        "skr-on",
    );
    let s = cfg.skill_refinement.expect("section present");
    assert!(s.enabled);
    assert_eq!(s.min_samples, 6);
    assert_eq!(s.floor, crate::DEFAULT_REFINE_FLOOR);
    assert_eq!(s.max_per_cycle, crate::DEFAULT_REFINE_MAX_PER_CYCLE);
}

#[test]
fn memory_profile_off_is_byte_identical() {
    let _env = EnvScope::new();
    // No [memory] + a present [embedding] → profile Off, nothing armed.
    let cfg = load_with_toml("\n[embedding]\nmodel = \"m\"\n", "mp-off");
    assert_eq!(cfg.memory_profile, crate::MemoryProfile::Off);
    let e = cfg.embedding.expect("embedding present");
    assert!(!e.recall_hybrid);
    assert_eq!(e.recall_graph_hops, 0);
    assert_eq!(e.recall_wiki_weight, 0.0);
    assert_eq!(e.recall_graph_typed_weight, 0.0);
    assert!(cfg.wiki.is_none());
    assert!(cfg.graph.is_none());
    assert!(cfg.recall_cluster.is_none());
}

#[test]
fn memory_profile_lite_arms_cheap_fusion_only() {
    let _env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[memory]\nprofile = \"lite\"\n[embedding]\nmodel = \"m\"\n",
        "mp-lite",
    );
    assert_eq!(cfg.memory_profile, crate::MemoryProfile::Lite);
    let e = cfg.embedding.expect("embedding present");
    // Cheap recall fusion over existing data: armed.
    assert!(e.recall_hybrid, "lite arms hybrid");
    assert_eq!(e.recall_graph_hops, 1, "lite arms the co-occurrence walk");
    assert!(cfg.recall_cluster.unwrap().enabled, "lite arms co-occurrence siblings");
    // Paid generation: NOT armed — no sweeps, no wiki/typed-graph weights.
    assert_eq!(e.recall_wiki_weight, 0.0, "lite leaves the wiki source silent");
    assert_eq!(e.recall_graph_typed_weight, 0.0, "lite leaves the typed-graph source silent");
    assert!(cfg.wiki.is_none(), "lite does not arm the wiki sweep");
    assert!(cfg.graph.is_none(), "lite does not arm the graph sweep");
}

#[test]
fn memory_profile_smart_arms_the_bundle() {
    let _env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[memory]\nprofile = \"smart\"\n[embedding]\nmodel = \"m\"\n",
        "mp-smart",
    );
    assert_eq!(cfg.memory_profile, crate::MemoryProfile::Smart);
    let e = cfg.embedding.expect("embedding present");
    assert!(e.recall_hybrid, "smart arms hybrid");
    assert_eq!(e.recall_graph_hops, 1, "smart arms the co-occurrence walk");
    assert_eq!(e.recall_wiki_weight, 1.0, "smart arms the wiki source");
    assert_eq!(e.recall_graph_typed_weight, 1.0, "smart arms the typed-graph source");
    // The extraction sweeps + cluster expansion are synthesized enabled.
    assert!(cfg.wiki.unwrap().enabled);
    assert!(cfg.graph.unwrap().enabled);
    assert!(cfg.recall_cluster.unwrap().enabled);
}

#[test]
fn memory_profile_smart_explicit_knobs_win() {
    let _env = EnvScope::new();
    // smart, but the operator explicitly disables hybrid + the wiki sweep.
    let cfg = load_with_toml(
        "\n[memory]\nprofile = \"smart\"\n\
         [embedding]\nmodel = \"m\"\nrecall_hybrid = false\n\
         [wiki]\nenabled = false\n",
        "mp-override",
    );
    let e = cfg.embedding.unwrap();
    assert!(!e.recall_hybrid, "explicit recall_hybrid=false beats smart");
    // ...but the unset weights still get the smart defaults.
    assert_eq!(e.recall_wiki_weight, 1.0);
    // The explicitly-present [wiki] section wins (stays disabled).
    assert!(!cfg.wiki.unwrap().enabled, "explicit [wiki] enabled=false beats smart");
    // The unset [graph] section is still synthesized enabled.
    assert!(cfg.graph.unwrap().enabled);
}

#[test]
fn graph_section_parses_and_validates() {
    let _env = EnvScope::new();
    assert!(load_with_toml("\n", "graph-absent").graph.is_none());

    let cfg = load_with_toml("\n[graph]\nenabled = true\n", "graph-default");
    let g = cfg.graph.expect("section present");
    assert!(g.enabled);
    assert_eq!(g.max_topics_per_sweep, crate::DEFAULT_GRAPH_MAX_TOPICS_PER_SWEEP);
    assert_eq!(g.interval_secs, crate::DEFAULT_GRAPH_INTERVAL_SECS);

    let cfg = load_with_toml(
        "\n[graph]\nenabled = true\nmax_topics_per_sweep = 4\ninterval_secs = 600\n",
        "graph-override",
    );
    let g = cfg.graph.unwrap();
    assert_eq!(g.max_topics_per_sweep, 4);
    assert_eq!(g.interval_secs, 600);

    // Enabled with a zero knob → Invalid.
    let tmp = TempDir::new("graph-bad");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(&toml_path, "\n[graph]\nenabled = true\ninterval_secs = 0\n").unwrap();
    let opts = LoadOptions { toml_path: Some(toml_path), ..LoadOptions::test_env_only() };
    assert!(AivyxConfig::load_from_env_and_toml(&opts).is_err());
}

#[test]
fn wiki_enabled_rejects_zero_knobs() {
    let _env = EnvScope::new();
    let tmp = TempDir::new("wiki-bad");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(&toml_path, "\n[wiki]\nenabled = true\nmax_pages_per_sweep = 0\n").unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        ..LoadOptions::test_env_only()
    };
    assert!(AivyxConfig::load_from_env_and_toml(&opts).is_err());
}

#[test]
fn persona_lifecycle_helpfulness_decay_knobs() {
    let env = EnvScope::new();

    // Valid override.
    let cfg = load_with_toml(
        "\n[persona_lifecycle]\nenabled = true\n\
         decay_unhelpful_threshold = -5.0\n\
         decay_min_samples = 8\n",
        "pl-help-valid",
    );
    let p = cfg.persona_lifecycle.expect("section present");
    assert!(
        (p.decay_unhelpful_threshold - (-5.0)).abs() < 1e-6
    );
    assert_eq!(p.decay_min_samples, 8);

    // Non-negative threshold (armed) → Invalid.
    let tmp = TempDir::new("pl-help-bad-threshold");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[persona_lifecycle]\nenabled = true\n\
         decay_unhelpful_threshold = 1.0\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    match AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("must error")
    {
        ConfigError::Invalid { field, .. } => assert_eq!(
            field,
            "persona_lifecycle.decay_unhelpful_threshold"
        ),
        other => panic!("expected Invalid, got {other:?}"),
    }

    // Zero min-samples (armed) → Invalid.
    let tmp2 = TempDir::new("pl-help-zero-samples");
    let toml2 = tmp2.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml2,
        "\n[persona_lifecycle]\nenabled = true\n\
         decay_min_samples = 0\n",
    )
    .unwrap();
    let opts2 = LoadOptions {
        toml_path: Some(toml2),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    match AivyxConfig::load_from_env_and_toml(&opts2)
        .expect_err("must error")
    {
        ConfigError::Invalid { field, .. } => assert_eq!(
            field,
            "persona_lifecycle.decay_min_samples"
        ),
        other => panic!("expected Invalid, got {other:?}"),
    }

    // Decay disarmed → the knobs are NOT validated even if
    // nonsensical (staged config).
    let cfg2 = load_with_toml(
        "\n[persona_lifecycle]\nenabled = true\n\
         signal_consolidate = true\nsignal_decay = false\n\
         decay_unhelpful_threshold = 9.0\n\
         decay_min_samples = 0\n",
        "pl-help-disarmed",
    );
    assert!(cfg2.persona_lifecycle.is_some());

    drop(env);
}

/// Phase 88 — explicit `decay_pair_below_affinity` wins;
/// validation fires only when decay is armed; non-finite +
/// negative are rejects.
#[test]
fn persona_lifecycle_pair_affinity_decay_knob() {
    let env = EnvScope::new();

    // Valid override.
    let cfg = load_with_toml(
        "\n[persona_lifecycle]\nenabled = true\n\
         decay_pair_below_affinity = 0.4\n",
        "pl-pair-valid",
    );
    let p = cfg.persona_lifecycle.expect("section present");
    assert!(
        (p.decay_pair_below_affinity - 0.4).abs() < 1e-6
    );

    // Negative pair floor (armed) → Invalid.
    let tmp = TempDir::new("pl-pair-neg");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[persona_lifecycle]\nenabled = true\n\
         decay_pair_below_affinity = -1.0\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    match AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("must error")
    {
        ConfigError::Invalid { field, .. } => assert_eq!(
            field,
            "persona_lifecycle.decay_pair_below_affinity"
        ),
        other => panic!("expected Invalid, got {other:?}"),
    }

    // Non-finite pair floor (armed) → Invalid.
    let tmp2 = TempDir::new("pl-pair-nan");
    let toml2 = tmp2.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml2,
        "\n[persona_lifecycle]\nenabled = true\n\
         decay_pair_below_affinity = nan\n",
    )
    .unwrap();
    let opts2 = LoadOptions {
        toml_path: Some(toml2),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    match AivyxConfig::load_from_env_and_toml(&opts2)
        .expect_err("must error")
    {
        ConfigError::Invalid { field, .. } => assert_eq!(
            field,
            "persona_lifecycle.decay_pair_below_affinity"
        ),
        other => panic!("expected Invalid, got {other:?}"),
    }

    // Decay disarmed → the knob is NOT validated even if
    // nonsensical (staged config — same Phase 85 posture).
    let cfg2 = load_with_toml(
        "\n[persona_lifecycle]\nenabled = true\n\
         signal_consolidate = true\nsignal_decay = false\n\
         decay_pair_below_affinity = -42.0\n",
        "pl-pair-disarmed",
    );
    assert!(cfg2.persona_lifecycle.is_some());

    drop(env);
}

/// Enabled + valid: explicit fields win; an explicitly-off
/// signal is respected while the other defaults on.
#[test]
fn persona_lifecycle_enabled_valid_with_signal_toggle() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[persona_lifecycle]\nenabled = true\n\
         consolidation_similarity = 0.85\n\
         decay_max_age_secs = 1209600\nmin_soft_facets = 4\n\
         signal_decay = false\n",
        "pl-valid",
    );
    let p = cfg.persona_lifecycle.expect("section present");
    assert!(p.enabled);
    assert!((p.consolidation_similarity - 0.85).abs() < 1e-6);
    assert_eq!(p.decay_max_age_secs, 1_209_600);
    assert_eq!(p.min_soft_facets, 4);
    assert!(p.signals.consolidate);
    assert!(!p.signals.decay);
    drop(env);
}

/// Enabled with an out-of-range similarity → load-time
/// `Invalid` (the `(0.0, 1.0]` bound; `0.0` is excluded).
#[test]
fn persona_lifecycle_enabled_bad_similarity_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("pl-bad-sim");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[persona_lifecycle]\nenabled = true\n\
         consolidation_similarity = 1.5\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(
                field,
                "persona_lifecycle.consolidation_similarity"
            );
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

/// Enabled with `min_soft_facets = 0` → `Invalid`.
#[test]
fn persona_lifecycle_enabled_zero_min_facets_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("pl-zero-floor");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[persona_lifecycle]\nenabled = true\n\
         min_soft_facets = 0\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(field, "persona_lifecycle.min_soft_facets");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

/// Enabled with every signal class off → `Invalid`.
#[test]
fn persona_lifecycle_enabled_all_signals_off_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("pl-no-signals");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[persona_lifecycle]\nenabled = true\n\
         signal_consolidate = false\nsignal_decay = false\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(field, "persona_lifecycle.signals");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

/// No `[recall_cluster]` section → `recall_cluster: None`
/// (off; Phase 76 recall is unchanged, pre-Phase-84).
#[test]
fn recall_cluster_absent_section_is_none() {
    let env = EnvScope::new();
    let cfg = AivyxConfig::load_from_env_and_toml(
        &LoadOptions::test_env_only(),
    )
    .expect("load");
    assert!(cfg.recall_cluster.is_none());
    drop(env);
}

/// A present-but-disabled section may be partial (staged
/// config): builds with `enabled = false`, defaults
/// elsewhere, and is NOT validated.
#[test]
fn recall_cluster_present_disabled_is_allowed_partial() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[recall_cluster]\nenabled = false\n",
        "rc-staged",
    );
    let r = cfg.recall_cluster.expect("section present");
    assert!(!r.enabled);
    assert_eq!(
        r.max_siblings,
        crate::DEFAULT_RC_MAX_SIBLINGS
    );
    assert!(
        (r.min_affinity - crate::DEFAULT_RC_MIN_AFFINITY)
            .abs()
            < 1e-6
    );
    drop(env);
}

/// Enabled + valid: explicit fields win.
#[test]
fn recall_cluster_enabled_valid() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[recall_cluster]\nenabled = true\n\
         max_siblings = 5\nmin_affinity = 2.5\n",
        "rc-valid",
    );
    let r = cfg.recall_cluster.expect("section present");
    assert!(r.enabled);
    assert_eq!(r.max_siblings, 5);
    assert!((r.min_affinity - 2.5).abs() < 1e-6);
    drop(env);
}

/// Enabled with `max_siblings = 0` → load-time `Invalid`.
#[test]
fn recall_cluster_enabled_zero_siblings_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("rc-zero-siblings");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[recall_cluster]\nenabled = true\n\
         max_siblings = 0\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(field, "recall_cluster.max_siblings");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

/// Enabled with non-positive `min_affinity` → `Invalid`.
#[test]
fn recall_cluster_enabled_nonpositive_affinity_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("rc-bad-affinity");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[recall_cluster]\nenabled = true\n\
         min_affinity = 0.0\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(field, "recall_cluster.min_affinity");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

// ---- Phase 87 — [persona_consolidation] ---------------------

/// No `[persona_consolidation]` section →
/// `persona_consolidation: None` (off; the Persona proposal
/// pipeline is byte-identical to pre-Phase-87).
#[test]
fn persona_consolidation_absent_section_is_none() {
    let env = EnvScope::new();
    let cfg = AivyxConfig::load_from_env_and_toml(
        &LoadOptions::test_env_only(),
    )
    .expect("load");
    assert!(cfg.persona_consolidation.is_none());
    drop(env);
}

/// A present-but-disabled section may be partial (staged
/// config): builds with `enabled = false`, defaults elsewhere,
/// and is NOT validated.
#[test]
fn persona_consolidation_present_disabled_is_allowed_partial() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[persona_consolidation]\nenabled = false\n",
        "pc-staged",
    );
    let p = cfg.persona_consolidation.expect("section present");
    assert!(!p.enabled);
    assert!(
        (p.min_affinity - crate::DEFAULT_PC_MIN_AFFINITY).abs()
            < 1e-6
    );
    assert_eq!(
        p.min_samples,
        crate::DEFAULT_PC_MIN_SAMPLES
    );
    assert!(
        (p.min_topic_helpfulness
            - crate::DEFAULT_PC_MIN_TOPIC_HELPFULNESS)
            .abs()
            < 1e-6
    );
    assert_eq!(
        p.max_proposals_per_cycle,
        crate::DEFAULT_PC_MAX_PROPOSALS_PER_CYCLE
    );
    drop(env);
}

/// Enabled + valid: explicit fields win.
#[test]
fn persona_consolidation_enabled_valid() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[persona_consolidation]\nenabled = true\n\
         min_affinity = 2.5\nmin_samples = 7\n\
         min_topic_helpfulness = 0.5\n\
         max_proposals_per_cycle = 5\n",
        "pc-valid",
    );
    let p = cfg.persona_consolidation.expect("section present");
    assert!(p.enabled);
    assert!((p.min_affinity - 2.5).abs() < 1e-6);
    assert_eq!(p.min_samples, 7);
    assert!((p.min_topic_helpfulness - 0.5).abs() < 1e-6);
    assert_eq!(p.max_proposals_per_cycle, 5);
    drop(env);
}

/// Enabled with non-positive `min_affinity` → `Invalid`.
#[test]
fn persona_consolidation_enabled_nonpositive_affinity_is_invalid()
{
    let env = EnvScope::new();
    let tmp = TempDir::new("pc-bad-affinity");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[persona_consolidation]\nenabled = true\n\
         min_affinity = 0.0\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(
                field,
                "persona_consolidation.min_affinity"
            );
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

/// Enabled with `min_samples = 0` → `Invalid`.
#[test]
fn persona_consolidation_enabled_zero_samples_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("pc-zero-samples");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[persona_consolidation]\nenabled = true\n\
         min_samples = 0\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(
                field,
                "persona_consolidation.min_samples"
            );
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

/// Enabled with `max_proposals_per_cycle = 0` → `Invalid`.
#[test]
fn persona_consolidation_enabled_zero_cap_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("pc-zero-cap");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[persona_consolidation]\nenabled = true\n\
         max_proposals_per_cycle = 0\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(
                field,
                "persona_consolidation.max_proposals_per_cycle"
            );
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

/// Phase 92 — `enable_supersession` defaults to `false` even
/// when the block is present and `enabled = true`. Operators
/// running Phase 87 consolidation must explicitly opt into
/// supersession.
#[test]
fn persona_consolidation_supersession_defaults_false() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[persona_consolidation]\nenabled = true\n",
        "pc-supersede-default",
    );
    let p = cfg.persona_consolidation.expect("section present");
    assert!(p.enabled);
    assert!(
        !p.enable_supersession,
        "supersession is opt-in; default false even when \
         consolidation itself is enabled"
    );
    drop(env);
}

/// Phase 92 — explicit `enable_supersession = true` wins.
#[test]
fn persona_consolidation_supersession_explicit_true_wins() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[persona_consolidation]\nenabled = true\n\
         enable_supersession = true\n",
        "pc-supersede-on",
    );
    let p = cfg.persona_consolidation.expect("section present");
    assert!(p.enable_supersession);
    drop(env);
}

/// Phase 92 — a staged-disabled section can carry the
/// supersession key partially (the staged-config flexibility
/// from Phase 85 / 87). Setting only the supersession key (no
/// other persona_consolidation fields) still builds Some(...)
/// because the "any field set" predicate fires.
#[test]
fn persona_consolidation_supersession_only_builds_some() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[persona_consolidation]\nenable_supersession = true\n",
        "pc-supersede-only",
    );
    let p = cfg.persona_consolidation.expect("section present");
    assert!(!p.enabled, "enabled defaults to false");
    assert!(p.enable_supersession);
    drop(env);
}

// ---- Phase 172 — [correction_consolidation] -----------------

/// No `[correction_consolidation]` section →
/// `correction_consolidation: None` (off; the correction ledger
/// still accumulates passively but files no proposals).
#[test]
fn correction_consolidation_absent_section_is_none() {
    let env = EnvScope::new();
    let cfg = AivyxConfig::load_from_env_and_toml(
        &LoadOptions::test_env_only(),
    )
    .expect("load");
    assert!(cfg.correction_consolidation.is_none());
    drop(env);
}

/// A present-but-disabled section may be partial (staged
/// config): builds with `enabled = false`, defaults elsewhere,
/// and is NOT validated.
#[test]
fn correction_consolidation_present_disabled_is_allowed_partial() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[correction_consolidation]\nenabled = false\n",
        "cc-staged",
    );
    let c =
        cfg.correction_consolidation.expect("section present");
    assert!(!c.enabled);
    assert!(
        (c.min_corrections - crate::DEFAULT_CC_MIN_CORRECTIONS)
            .abs()
            < 1e-6
    );
    assert_eq!(c.min_samples, crate::DEFAULT_CC_MIN_SAMPLES);
    assert_eq!(
        c.max_proposals_per_cycle,
        crate::DEFAULT_CC_MAX_PROPOSALS_PER_CYCLE
    );
    drop(env);
}

/// Enabled + valid: explicit fields win.
#[test]
fn correction_consolidation_enabled_valid() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[correction_consolidation]\nenabled = true\n\
         min_corrections = 5.0\nmin_samples = 4\n\
         max_proposals_per_cycle = 2\n",
        "cc-valid",
    );
    let c =
        cfg.correction_consolidation.expect("section present");
    assert!(c.enabled);
    assert!((c.min_corrections - 5.0).abs() < 1e-6);
    assert_eq!(c.min_samples, 4);
    assert_eq!(c.max_proposals_per_cycle, 2);
    drop(env);
}

/// Enabled with non-positive `min_corrections` → `Invalid`.
#[test]
fn correction_consolidation_enabled_nonpositive_corrections_is_invalid()
{
    let env = EnvScope::new();
    let tmp = TempDir::new("cc-bad-corrections");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[correction_consolidation]\nenabled = true\n\
         min_corrections = 0.0\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(
                field,
                "correction_consolidation.min_corrections"
            );
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

// ---- Phase 173 — [loop] -------------------------------------

/// No `[loop]` section → `loop_config: None` (the driver is not
/// spawned; the backlog can still be stocked).
#[test]
fn loop_absent_section_is_none() {
    let env = EnvScope::new();
    let cfg = AivyxConfig::load_from_env_and_toml(
        &LoadOptions::test_env_only(),
    )
    .expect("load");
    assert!(cfg.loop_config.is_none());
    drop(env);
}

/// A present-but-disabled section may be partial (staged
/// config): builds with `enabled = false`, defaults elsewhere.
#[test]
fn loop_present_disabled_is_allowed_partial() {
    let env = EnvScope::new();
    let cfg = load_with_toml("\n[loop]\nenabled = false\n", "loop-staged");
    let l = cfg.loop_config.expect("section present");
    assert!(!l.enabled);
    assert_eq!(l.max_iterations, crate::DEFAULT_LOOP_MAX_ITERATIONS);
    assert_eq!(l.default_priority, crate::DEFAULT_LOOP_PRIORITY);
    // Chapter Helm — resume_on_boot defaults off.
    assert!(!l.resume_on_boot);
    drop(env);
}

/// Chapter Foreman — `delegate_above` parses; absent ⇒ None (off); 0 ⇒ None.
#[test]
fn loop_delegate_above_parses() {
    let env = EnvScope::new();
    let on = load_with_toml(
        "\n[loop]\nenabled = true\ndelegate_above = 5\n",
        "loop-deleg",
    );
    assert_eq!(on.loop_config.expect("present").delegate_above, Some(5));
    // Absent → off.
    let off = load_with_toml("\n[loop]\nenabled = true\n", "loop-nodeleg");
    assert_eq!(off.loop_config.expect("present").delegate_above, None);
    // 0 is treated as off (no story scores below 0; explicit "never").
    let zero = load_with_toml(
        "\n[loop]\nenabled = true\ndelegate_above = 0\n",
        "loop-zerodeleg",
    );
    assert_eq!(zero.loop_config.expect("present").delegate_above, None);
    drop(env);
}

/// Chapter Helm — `resume_on_boot` parses from `[loop]`.
#[test]
fn loop_resume_on_boot_parses() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[loop]\nenabled = true\nresume_on_boot = true\n",
        "loop-resume",
    );
    assert!(cfg.loop_config.expect("section present").resume_on_boot);
    drop(env);
}

/// Enabled + valid: explicit fields win.
#[test]
fn loop_enabled_valid() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[loop]\nenabled = true\nmax_iterations = 8\n\
         default_priority = 50\n",
        "loop-valid",
    );
    let l = cfg.loop_config.expect("section present");
    assert!(l.enabled);
    assert_eq!(l.max_iterations, 8);
    assert_eq!(l.default_priority, 50);
    drop(env);
}

/// Enabled with `max_iterations = 0` → `Invalid` (the cap is the
/// primary guardrail; it must be at least 1).
#[test]
fn loop_enabled_zero_max_iterations_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("loop-bad-cap");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[loop]\nenabled = true\nmax_iterations = 0\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(field, "loop.max_iterations");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

/// Phase 174 — gate knobs parse + default; an empty/whitespace
/// `gate_command` collapses to `None` (no driver verification).
#[test]
fn loop_gate_knobs_parse() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[loop]\nenabled = true\nmax_iterations = 10\n\
         gate_command = \"cargo test\"\ngate_timeout_secs = 120\n\
         working_dir = \"/repo\"\nmax_run_secs = 3600\n",
        "loop-gate",
    );
    let l = cfg.loop_config.expect("section present");
    assert_eq!(l.gate_command.as_deref(), Some("cargo test"));
    assert_eq!(l.gate_timeout_secs, 120);
    assert_eq!(l.working_dir.as_deref(), Some("/repo"));
    assert_eq!(l.max_run_secs, Some(3600));
    drop(env);
}

/// Gate defaults: no gate_command → `None`; timeout defaults;
/// `max_run_secs = 0` collapses to `None` (disabled).
#[test]
fn loop_gate_defaults() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[loop]\nenabled = true\nmax_run_secs = 0\n",
        "loop-gate-default",
    );
    let l = cfg.loop_config.expect("section present");
    assert!(l.gate_command.is_none());
    assert_eq!(
        l.gate_timeout_secs,
        crate::DEFAULT_LOOP_GATE_TIMEOUT_SECS
    );
    assert!(l.max_run_secs.is_none());
    drop(env);
}

/// Enabled with a gate but `gate_timeout_secs = 0` → `Invalid`.
#[test]
fn loop_enabled_zero_gate_timeout_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("loop-bad-gate-timeout");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[loop]\nenabled = true\ngate_command = \"cargo test\"\n\
         gate_timeout_secs = 0\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(field, "loop.gate_timeout_secs");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

/// Phase 175 — progress_inject_count parses + defaults; `0`
/// (disable injection) is allowed even on an armed section.
#[test]
fn loop_progress_inject_count_parse_and_default() {
    let env = EnvScope::new();
    // Explicit value wins.
    let cfg = load_with_toml(
        "\n[loop]\nenabled = true\nprogress_inject_count = 5\n",
        "loop-progress",
    );
    assert_eq!(
        cfg.loop_config.expect("present").progress_inject_count,
        5
    );
    // Absent → default.
    let cfg2 =
        load_with_toml("\n[loop]\nenabled = true\n", "loop-progress-def");
    assert_eq!(
        cfg2.loop_config.expect("present").progress_inject_count,
        crate::DEFAULT_LOOP_PROGRESS_INJECT_COUNT
    );
    // 0 disables injection — valid on an armed section.
    let cfg3 = load_with_toml(
        "\n[loop]\nenabled = true\nprogress_inject_count = 0\n",
        "loop-progress-off",
    );
    assert_eq!(
        cfg3.loop_config.expect("present").progress_inject_count,
        0
    );
    drop(env);
}

/// Chapter Circuit (CI.1) — max_idle_iterations (the stall breaker)
/// parses, defaults to the constant, and `0` disables it. Also
/// arms a bare `[loop]` section on its own (so an operator can set
/// only the stall threshold).
#[test]
fn loop_max_idle_iterations_parse_and_default() {
    let env = EnvScope::new();
    // Explicit value wins.
    let cfg = load_with_toml(
        "\n[loop]\nenabled = true\nmax_idle_iterations = 5\n",
        "loop-idle",
    );
    assert_eq!(
        cfg.loop_config.expect("present").max_idle_iterations,
        5
    );
    // Absent → default.
    let cfg2 =
        load_with_toml("\n[loop]\nenabled = true\n", "loop-idle-def");
    assert_eq!(
        cfg2.loop_config.expect("present").max_idle_iterations,
        crate::DEFAULT_LOOP_MAX_IDLE_ITERATIONS
    );
    // 0 disables the breaker — valid on an armed section.
    let cfg3 = load_with_toml(
        "\n[loop]\nenabled = true\nmax_idle_iterations = 0\n",
        "loop-idle-off",
    );
    assert_eq!(
        cfg3.loop_config.expect("present").max_idle_iterations,
        0
    );
    // The key on its own arms the section (any_set).
    let cfg4 =
        load_with_toml("\n[loop]\nmax_idle_iterations = 2\n", "loop-idle-arms");
    let lc = cfg4.loop_config.expect("section present via max_idle_iterations");
    assert_eq!(lc.max_idle_iterations, 2);
    assert!(!lc.enabled, "enabled still defaults to false");
    drop(env);
}

/// Phase 176 — max_run_tokens parses; absent → None; `0`
/// collapses to None (disabled), like max_run_secs.
#[test]
fn loop_max_run_tokens_parse_and_disable() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[loop]\nenabled = true\nmax_run_tokens = 500000\n",
        "loop-tokens",
    );
    assert_eq!(
        cfg.loop_config.expect("present").max_run_tokens,
        Some(500_000)
    );
    // Absent → None.
    let cfg2 =
        load_with_toml("\n[loop]\nenabled = true\n", "loop-tokens-none");
    assert!(cfg2.loop_config.expect("present").max_run_tokens.is_none());
    // 0 disables.
    let cfg3 = load_with_toml(
        "\n[loop]\nenabled = true\nmax_run_tokens = 0\n",
        "loop-tokens-zero",
    );
    assert!(cfg3.loop_config.expect("present").max_run_tokens.is_none());
    drop(env);
}

/// Phase 178 — `[correction_judgment]` parses; absent → None;
/// enabled with a 0 cap → Invalid.
#[test]
fn correction_judgment_parse_and_validate() {
    let env = EnvScope::new();
    // Absent.
    let cfg = AivyxConfig::load_from_env_and_toml(
        &LoadOptions::test_env_only(),
    )
    .expect("load");
    assert!(cfg.correction_judgment.is_none());
    // Enabled + valid.
    let cfg2 = load_with_toml(
        "\n[correction_judgment]\nenabled = true\n\
         max_corrections_per_cycle = 12\n",
        "cj-valid",
    );
    let c = cfg2.correction_judgment.expect("present");
    assert!(c.enabled);
    assert_eq!(c.max_corrections_per_cycle, 12);
    // Default cap when omitted.
    let cfg3 =
        load_with_toml("\n[correction_judgment]\nenabled = true\n", "cj-def");
    assert_eq!(
        cfg3.correction_judgment.expect("present").max_corrections_per_cycle,
        crate::DEFAULT_CJ_MAX_CORRECTIONS_PER_CYCLE
    );
    // Enabled + 0 cap → Invalid.
    let tmp = TempDir::new("cj-bad");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[correction_judgment]\nenabled = true\n\
         max_corrections_per_cycle = 0\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    match AivyxConfig::load_from_env_and_toml(&opts).expect_err("err") {
        ConfigError::Invalid { field, .. } => assert_eq!(
            field,
            "correction_judgment.max_corrections_per_cycle"
        ),
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

/// Phase 183 — `[reminders].check_interval_secs` parses; absent
/// → None (driver default).
#[test]
fn reminders_check_interval_parses() {
    let env = EnvScope::new();
    let cfg = AivyxConfig::load_from_env_and_toml(
        &LoadOptions::test_env_only(),
    )
    .expect("load");
    assert!(cfg.reminders_check_interval_secs.is_none());
    let on = load_with_toml(
        "\n[reminders]\ncheck_interval_secs = 15\n",
        "rem-on",
    );
    assert_eq!(on.reminders_check_interval_secs, Some(15));
    drop(env);
}

/// Phase 180 — `[sandbox].default_backend` parses; absent →
/// None; unknown → Invalid.
#[test]
fn sandbox_default_backend_parse_and_validate() {
    let env = EnvScope::new();
    // Absent → None.
    let cfg = AivyxConfig::load_from_env_and_toml(
        &LoadOptions::test_env_only(),
    )
    .expect("load");
    assert_eq!(
        cfg.sandbox_default_backend,
        crate::SandboxDefaultBackend::None
    );
    // auto / bubblewrap / firejail / none.
    for (s, want) in [
        ("auto", crate::SandboxDefaultBackend::Auto),
        ("bubblewrap", crate::SandboxDefaultBackend::Bubblewrap),
        ("firejail", crate::SandboxDefaultBackend::Firejail),
        ("none", crate::SandboxDefaultBackend::None),
    ] {
        let cfg = load_with_toml(
            &format!("\n[sandbox]\ndefault_backend = \"{s}\"\n"),
            &format!("sb-{s}"),
        );
        assert_eq!(cfg.sandbox_default_backend, want, "for {s}");
    }
    // Unknown → Invalid.
    let tmp = TempDir::new("sb-bad");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[sandbox]\ndefault_backend = \"docker\"\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    match AivyxConfig::load_from_env_and_toml(&opts).expect_err("err") {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(field, "sandbox.default_backend")
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

/// Phase 179 — `[correction_signal]` parses; absent → None.
#[test]
fn correction_signal_parse() {
    let env = EnvScope::new();
    let cfg = AivyxConfig::load_from_env_and_toml(
        &LoadOptions::test_env_only(),
    )
    .expect("load");
    assert!(cfg.correction_signal.is_none());

    let on = load_with_toml(
        "\n[correction_signal]\nattribute_tools = true\n",
        "cs-on",
    );
    assert!(on.correction_signal.expect("present").attribute_tools);

    let off = load_with_toml(
        "\n[correction_signal]\nattribute_tools = false\n",
        "cs-off",
    );
    assert!(!off.correction_signal.expect("present").attribute_tools);
    drop(env);
}

// ---- Phase 89 — [memory].canonicalize_topics ----------------

/// No `[memory]` block (or no `canonicalize_topics` key) →
/// `memory_canonicalize_topics = false` (the memory layer is
/// byte-identical to pre-Phase-89).
#[test]
fn memory_canonicalize_topics_absent_defaults_false() {
    let env = EnvScope::new();
    let cfg = AivyxConfig::load_from_env_and_toml(
        &LoadOptions::test_env_only(),
    )
    .expect("load");
    assert!(!cfg.memory_canonicalize_topics.value);
    assert_eq!(
        cfg.memory_canonicalize_topics.source,
        crate::FieldSource::Default,
    );
    drop(env);
}

/// Explicit `canonicalize_topics = true` wins; source records
/// it came from the TOML.
#[test]
fn memory_canonicalize_topics_explicit_true_wins() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[memory]\ncanonicalize_topics = true\n",
        "mem-canon-on",
    );
    assert!(cfg.memory_canonicalize_topics.value);
    assert_eq!(
        cfg.memory_canonicalize_topics.source,
        crate::FieldSource::Toml,
    );
    drop(env);
}

/// Explicit `canonicalize_topics = false` is honored (the
/// operator can express the default explicitly without
/// changing behaviour).
#[test]
fn memory_canonicalize_topics_explicit_false_wins() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[memory]\ncanonicalize_topics = false\n",
        "mem-canon-off",
    );
    assert!(!cfg.memory_canonicalize_topics.value);
    assert_eq!(
        cfg.memory_canonicalize_topics.source,
        crate::FieldSource::Toml,
    );
    drop(env);
}

// ---- Phase 91 — [recall_judgment] -----------------------------

/// No `[recall_judgment]` section → `recall_judgment: None`
/// (off; the Phase 77 structural recall-feedback signal is
/// the only signal — byte-identical to pre-Phase-91).
#[test]
fn recall_judgment_absent_section_is_none() {
    let env = EnvScope::new();
    let cfg = AivyxConfig::load_from_env_and_toml(
        &LoadOptions::test_env_only(),
    )
    .expect("load");
    assert!(cfg.recall_judgment.is_none());
    drop(env);
}

/// A present-but-disabled section may be partial (staged
/// config). It builds with `enabled = false`, defaults
/// elsewhere, and is NOT validated.
#[test]
fn recall_judgment_present_disabled_is_allowed_partial() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[recall_judgment]\nenabled = false\n",
        "rj-staged",
    );
    let rj = cfg.recall_judgment.expect("section present");
    assert!(!rj.enabled);
    assert_eq!(
        rj.max_recalls_per_cycle,
        crate::DEFAULT_RJ_MAX_RECALLS_PER_CYCLE,
    );
    drop(env);
}

/// Enabled + valid: explicit field wins.
#[test]
fn recall_judgment_enabled_valid() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[recall_judgment]\nenabled = true\n\
         max_recalls_per_cycle = 7\n",
        "rj-valid",
    );
    let rj = cfg.recall_judgment.expect("section present");
    assert!(rj.enabled);
    assert_eq!(rj.max_recalls_per_cycle, 7);
    drop(env);
}

/// Enabled with `max_recalls_per_cycle = 0` → `Invalid`.
#[test]
fn recall_judgment_enabled_zero_cap_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("rj-zero-cap");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[recall_judgment]\nenabled = true\n\
         max_recalls_per_cycle = 0\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(
                field,
                "recall_judgment.max_recalls_per_cycle",
            );
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

/// Decay disarmed (the `enabled = false` staged-config
/// posture) → the cap knob is NOT validated even if
/// nonsensical (mirrors the Phase 85 / Phase 87 staged-
/// config behaviour).
#[test]
fn recall_judgment_disabled_partial_allows_nonsense() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[recall_judgment]\nenabled = false\n\
         max_recalls_per_cycle = 0\n",
        "rj-disabled-nonsense",
    );
    let rj = cfg.recall_judgment.expect("section present");
    assert!(!rj.enabled);
    assert_eq!(rj.max_recalls_per_cycle, 0);
    drop(env);
}

// ---- Phase 93 — [recall_feedback] -----------------------------

/// No `[recall_feedback]` section → `recall_feedback: None`
/// (off; `correlate_detailed` keeps the Phase 77 structural
/// turn-level proxy — byte-identical to pre-Phase-93).
#[test]
fn recall_feedback_absent_section_is_none() {
    let env = EnvScope::new();
    let cfg = AivyxConfig::load_from_env_and_toml(
        &LoadOptions::test_env_only(),
    )
    .expect("load");
    assert!(cfg.recall_feedback.is_none());
    drop(env);
}

/// Present-but-default: `use_judgment_signal` not set →
/// section is treated as absent (no field set means no
/// section in build terms). Mirrors the established
/// `any_set` guard.
#[test]
fn recall_feedback_present_empty_is_none() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[recall_feedback]\n",
        "rf-empty",
    );
    assert!(cfg.recall_feedback.is_none());
    drop(env);
}

/// Explicit `use_judgment_signal = true` → `Some(.. {
/// use_judgment_signal: true })`.
#[test]
fn recall_feedback_explicit_true_wins() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[recall_feedback]\nuse_judgment_signal = true\n",
        "rf-true",
    );
    let rf = cfg.recall_feedback.expect("section present");
    assert!(rf.use_judgment_signal);
    drop(env);
}

/// Explicit `use_judgment_signal = false` is honored — the
/// operator may want the section present (for documentation
/// or staged rollout) with the augment off. The build path
/// returns `Some(.. { use_judgment_signal: false })`, which
/// is equivalent to `None` for the correlator's behaviour
/// but distinguishable in the config surface.
#[test]
fn recall_feedback_explicit_false_honored() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[recall_feedback]\nuse_judgment_signal = false\n",
        "rf-false",
    );
    let rf = cfg.recall_feedback.expect("section present");
    assert!(!rf.use_judgment_signal);
    drop(env);
}

// ---- Phase 113 — [skills.auto_propose] -------------------------

/// No `[skills.auto_propose]` section → `skill_auto_propose:
/// None` (the daemon wires `DaemonConfig::skill_auto_proposer
/// = None`; auto-proposer is disabled).
#[test]
fn skills_auto_propose_absent_section_is_none() {
    let env = EnvScope::new();
    let cfg = AivyxConfig::load_from_env_and_toml(
        &LoadOptions::test_env_only(),
    )
    .expect("load");
    assert!(cfg.skill_auto_propose.is_none());
    drop(env);
}

/// `[skills.auto_propose]` with only `enabled = true` → all
/// other fields filled from the `DEFAULT_SKILLS_AUTO_PROPOSE_*`
/// constants. Defaults match Phase 112's runtime
/// `SkillAutoProposeConfig::default()`.
#[test]
fn skills_auto_propose_minimal_section_uses_defaults() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[skills.auto_propose]\nenabled = true\n",
        "sap-minimal",
    );
    let sap = cfg.skill_auto_propose.expect("section present");
    assert!(sap.enabled);
    // Unset judge_model stays None — the daemon wiring resolves
    // it to the planner's configured model (Vitrine §6).
    assert_eq!(sap.judge_model, None);
    assert_eq!(
        sap.judge_max_tokens,
        crate::DEFAULT_SKILLS_AUTO_PROPOSE_JUDGE_MAX_TOKENS
    );
    assert!(
        (sap.auto_accept_confidence_threshold - 0.85).abs() < 1e-6
    );
    assert!(
        (sap.fuzzy_match_threshold - 0.80).abs() < 1e-6
    );
    assert_eq!(sap.heuristic.tool_call_count_min, 3);
    assert_eq!(sap.heuristic.distinct_tool_id_min, 2);
    assert_eq!(sap.heuristic.duration_ms_min, 5000);
    assert!(!sap.heuristic.require_gate_resolve);
    assert_eq!(
        sap.heuristic.mode,
        crate::SkillsAutoProposeMatchMode::Any
    );
    drop(env);
}

/// Full section → every field parsed; nothing comes from
/// defaults.
#[test]
fn skills_auto_propose_full_section_parses_every_field() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[skills.auto_propose]\n\
         enabled = true\n\
         judge_model = \"claude-opus-4-7\"\n\
         judge_max_tokens = 1200\n\
         auto_accept_confidence_threshold = 0.92\n\
         fuzzy_match_threshold = 0.70\n\
         \n[skills.auto_propose.heuristic]\n\
         tool_call_count_min = 5\n\
         distinct_tool_id_min = 3\n\
         duration_ms_min = 12000\n\
         require_gate_resolve = true\n\
         mode = \"all\"\n",
        "sap-full",
    );
    let sap = cfg.skill_auto_propose.expect("section present");
    assert!(sap.enabled);
    assert_eq!(sap.judge_model.as_deref(), Some("claude-opus-4-7"));
    assert_eq!(sap.judge_max_tokens, 1200);
    assert!(
        (sap.auto_accept_confidence_threshold - 0.92).abs() < 1e-6
    );
    assert!((sap.fuzzy_match_threshold - 0.70).abs() < 1e-6);
    assert_eq!(sap.heuristic.tool_call_count_min, 5);
    assert_eq!(sap.heuristic.distinct_tool_id_min, 3);
    assert_eq!(sap.heuristic.duration_ms_min, 12000);
    assert!(sap.heuristic.require_gate_resolve);
    assert_eq!(
        sap.heuristic.mode,
        crate::SkillsAutoProposeMatchMode::All
    );
    drop(env);
}

/// Empty `judge_model` → `Invalid` with the field name
/// pointing at `skills.auto_propose.judge_model`.
#[test]
fn skills_auto_propose_empty_judge_model_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("sap-empty-model");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[skills.auto_propose]\nenabled = true\njudge_model = \"\"\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(field, "skills.auto_propose.judge_model");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

/// `judge_max_tokens = 0` → `Invalid`.
#[test]
fn skills_auto_propose_zero_max_tokens_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("sap-zero-max");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[skills.auto_propose]\nenabled = true\njudge_max_tokens = 0\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(field, "skills.auto_propose.judge_max_tokens");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

/// `auto_accept_confidence_threshold` out of `[0.0, 1.0]` →
/// `Invalid`. Tests the upper-bound failure case (above 1.0).
#[test]
fn skills_auto_propose_threshold_above_one_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("sap-thresh-high");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[skills.auto_propose]\nenabled = true\n\
         auto_accept_confidence_threshold = 1.5\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(
                field,
                "skills.auto_propose.auto_accept_confidence_threshold"
            );
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

/// Negative `fuzzy_match_threshold` → `Invalid` (lower-bound
/// failure case).
#[test]
fn skills_auto_propose_fuzzy_below_zero_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("sap-fuzzy-neg");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[skills.auto_propose]\nenabled = true\n\
         fuzzy_match_threshold = -0.1\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(field, "skills.auto_propose.fuzzy_match_threshold");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

/// Unknown `mode` value → `Invalid` (Phase-91 precedent for
/// string-discriminator validation).
#[test]
fn skills_auto_propose_unknown_mode_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("sap-bad-mode");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[skills.auto_propose]\nenabled = true\n\
         \n[skills.auto_propose.heuristic]\nmode = \"maybe\"\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "skills.auto_propose.heuristic.mode");
            assert!(reason.contains("any") && reason.contains("all"));
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

/// `mode` is case-insensitive (the deserialize target lowercases
/// before matching). Phase 112's runtime MatchMode also
/// deserializes lowercase; staying consistent here means the
/// operator's TOML matches the serialized JSON we'd round-trip
/// in tests.
#[test]
fn skills_auto_propose_mode_is_case_insensitive() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[skills.auto_propose]\nenabled = true\n\
         \n[skills.auto_propose.heuristic]\nmode = \"ALL\"\n",
        "sap-case",
    );
    let sap = cfg.skill_auto_propose.expect("section present");
    assert_eq!(
        sap.heuristic.mode,
        crate::SkillsAutoProposeMatchMode::All
    );
    drop(env);
}

/// Heuristic-only sub-section (no top-level
/// `[skills.auto_propose]` keys, just the nested heuristic)
/// still arms the section. The defaults fill the top-level.
#[test]
fn skills_auto_propose_heuristic_only_arms_section() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[skills.auto_propose.heuristic]\ntool_call_count_min = 7\n",
        "sap-heur-only",
    );
    let sap = cfg
        .skill_auto_propose
        .expect("nested-only still arms the section");
    assert!(sap.enabled); // default
    assert_eq!(sap.heuristic.tool_call_count_min, 7);
    drop(env);
}

// ---- Phase 114 — [persona.auto_propose] -----------------------

/// No `[persona.auto_propose]` section → `persona_auto_propose:
/// None`. The Phase 113 alias still works independently
/// through `skill_auto_propose`.
#[test]
fn persona_auto_propose_absent_section_is_none() {
    let env = EnvScope::new();
    let cfg = AivyxConfig::load_from_env_and_toml(
        &LoadOptions::test_env_only(),
    )
    .expect("load");
    assert!(cfg.persona_auto_propose.is_none());
    drop(env);
}

/// `[persona.auto_propose]` with only `enabled = true` →
/// defaults filled per the per-category policy.
#[test]
fn persona_auto_propose_minimal_section_uses_per_category_defaults() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[persona.auto_propose]\nenabled = true\n",
        "pap-minimal",
    );
    let pap = cfg.persona_auto_propose.expect("section present");
    assert!(pap.enabled);
    // Scalar defaults: enabled=false, threshold=0.99
    assert!(!pap.per_category.assistant_name.enabled);
    assert!(
        (pap.per_category.assistant_name.auto_accept_confidence_threshold
            - crate::DEFAULT_PERSONA_SCALAR_THRESHOLD)
            .abs()
            < 1e-6
    );
    assert!(!pap.per_category.operator_profile.enabled);
    assert!(!pap.per_category.communication_style.enabled);
    // List defaults: enabled=true, threshold=0.85
    assert!(pap.per_category.behavioral_preferences.enabled);
    assert!(
        (pap.per_category
            .behavioral_preferences
            .auto_accept_confidence_threshold
            - crate::DEFAULT_PERSONA_LIST_THRESHOLD)
            .abs()
            < 1e-6
    );
    assert!(pap.per_category.learned_skill.enabled);
    // Phase 118 — the two new categories default-enabled
    // (list-shaped). Threshold honored at parse but ignored
    // at routing (always-staged).
    assert!(pap.per_category.profile_hint.enabled);
    assert!(pap.per_category.role_definition_suggestion.enabled);
    assert!(
        (pap.per_category.profile_hint.auto_accept_confidence_threshold
            - crate::DEFAULT_PERSONA_LIST_THRESHOLD)
            .abs()
            < 1e-6
    );
    drop(env);
}

/// Phase 118 — operator can disable proposing the new
/// Profile/Role categories via the dedicated sub-section
/// while leaving everything else on defaults.
#[test]
fn persona_auto_propose_phase_118_per_category_disable_works() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[persona.auto_propose.profile_hint]\n\
         enabled = false\n\
         \n[persona.auto_propose.role_definition_suggestion]\n\
         enabled = false\n",
        "pap-phase-118-disable",
    );
    let pap = cfg.persona_auto_propose.expect("section present");
    assert!(!pap.per_category.profile_hint.enabled);
    assert!(!pap.per_category.role_definition_suggestion.enabled);
    // Other categories untouched at defaults.
    assert!(pap.per_category.behavioral_preferences.enabled);
    assert!(pap.per_category.learned_skill.enabled);
    assert!(!pap.per_category.assistant_name.enabled);
    drop(env);
}

/// Per-category override: operator enables `assistant_name` with
/// a custom threshold. Other categories still take defaults.
#[test]
fn persona_auto_propose_per_category_override_works() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[persona.auto_propose.assistant_name]\n\
         enabled = true\n\
         auto_accept_confidence_threshold = 0.995\n",
        "pap-override",
    );
    let pap = cfg.persona_auto_propose.expect("section present");
    assert!(pap.per_category.assistant_name.enabled);
    assert!(
        (pap.per_category.assistant_name.auto_accept_confidence_threshold
            - 0.995)
            .abs()
            < 1e-6
    );
    // Untouched categories: defaults.
    assert!(!pap.per_category.operator_profile.enabled);
    assert!(pap.per_category.behavioral_preferences.enabled);
    drop(env);
}

/// Per-category threshold out of [0.0, 1.0] → Invalid with
/// field-specific reason.
#[test]
fn persona_auto_propose_per_category_threshold_out_of_range_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("pap-bad-thresh");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[persona.auto_propose.behavioral_preferences]\n\
         enabled = true\nauto_accept_confidence_threshold = 1.7\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err =
        AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(
                field,
                "persona.auto_propose.behavioral_preferences.\
                 auto_accept_confidence_threshold"
            );
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

/// Phase 113 alias still works independently: configuring only
/// `[skills.auto_propose]` populates `skill_auto_propose` and
/// leaves `persona_auto_propose = None`.
#[test]
fn phase_113_alias_skills_auto_propose_still_works() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[skills.auto_propose]\nenabled = true\n",
        "alias-skills",
    );
    assert!(cfg.skill_auto_propose.is_some());
    assert!(cfg.persona_auto_propose.is_none());
    drop(env);
}

/// Both sections present: each populates its own field. The
/// bin-side wiring (Task 6) picks `persona_auto_propose` when
/// present; this test only confirms the loader accepts both
/// cleanly.
#[test]
fn both_sections_present_populate_their_own_fields() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[skills.auto_propose]\nenabled = true\n\
         \n[persona.auto_propose]\nenabled = true\n",
        "both-sections",
    );
    assert!(cfg.skill_auto_propose.is_some());
    assert!(cfg.persona_auto_propose.is_some());
    drop(env);
}

/// PerCategoryConfigSet's lookup matches the runtime
/// equivalent's category-label dispatch.
#[test]
fn per_category_lookup_covers_all_eleven_labels() {
    let set = crate::PerCategoryConfigSet::defaults();
    for label in [
        "AssistantName",
        "OperatorProfile",
        "CommunicationStyle",
        "PrimaryUseCases",
        "BehavioralPreferences",
        "BehavioralConstraints",
        "LearnedContext",
        "CommunicationAdaptations",
        "CharacterTraits",
        "RelationshipMilestones",
        "LearnedSkill",
    ] {
        assert!(set.lookup(label).is_some(), "missing label {label}");
    }
    assert!(set.lookup("NotARealCategory").is_none());
}

// ---- Phase 115 — failure-feedback config ----

/// Absent `from_failed_turns` field → default `false`,
/// failure_outcomes defaults (Failed=true, TimedOut=true,
/// Cancelled=false, Escalated=false).
#[test]
fn persona_auto_propose_failure_defaults_when_section_arms_via_other_field() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[persona.auto_propose]\nenabled = true\n",
        "pap-failure-defaults",
    );
    let pap = cfg.persona_auto_propose.expect("section present");
    assert!(!pap.from_failed_turns);
    assert!(pap.failure_outcomes.failed);
    assert!(pap.failure_outcomes.timed_out);
    assert!(!pap.failure_outcomes.cancelled);
    assert!(!pap.failure_outcomes.escalated);
    drop(env);
}

/// Explicit `from_failed_turns = true` flips the master
/// switch on; failure_outcomes still takes defaults.
#[test]
fn persona_auto_propose_from_failed_turns_explicit_true() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[persona.auto_propose]\nfrom_failed_turns = true\n",
        "pap-ftt-true",
    );
    let pap = cfg.persona_auto_propose.expect("section present");
    assert!(pap.from_failed_turns);
    drop(env);
}

/// Per-failure-outcome override: operator enables
/// cancelled + escalated, disables timed_out.
#[test]
fn persona_auto_propose_failure_outcomes_override_works() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[persona.auto_propose]\nfrom_failed_turns = true\n\
         \n[persona.auto_propose.failure_outcomes]\n\
         cancelled = true\nescalated = true\ntimed_out = false\n",
        "pap-failure-override",
    );
    let pap = cfg.persona_auto_propose.expect("section present");
    assert!(pap.from_failed_turns);
    assert!(pap.failure_outcomes.failed);  // default
    assert!(pap.failure_outcomes.cancelled);  // overridden
    assert!(!pap.failure_outcomes.timed_out);  // overridden
    assert!(pap.failure_outcomes.escalated);  // overridden
    drop(env);
}

/// Section armed by failure_outcomes sub-section alone
/// (no top-level fields set) still produces Some.
#[test]
fn persona_auto_propose_failure_outcomes_only_arms_section() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[persona.auto_propose.failure_outcomes]\nescalated = true\n",
        "pap-fo-only",
    );
    let pap = cfg.persona_auto_propose.expect("nested-only arms section");
    assert!(pap.failure_outcomes.escalated);
    // Section enabled defaults to true (operator-conservative
    // posture: configuring any sub-section means they want the
    // feature on).
    assert!(pap.enabled);
    drop(env);
}

// ---- Phase 116 — [tool_relevance] config ----

#[test]
fn tool_relevance_absent_section_is_none() {
    let env = EnvScope::new();
    let cfg = AivyxConfig::load_from_env_and_toml(
        &LoadOptions::test_env_only(),
    )
    .expect("load");
    assert!(cfg.tool_relevance.is_none());
    drop(env);
}

// ----- Phase 121 — [ollama] generation options -----

#[test]
fn phase_121_ollama_absent_section_yields_all_none_options() {
    // No [ollama] section → every field stays None; the binary
    // propagates None values so Ollama's per-model defaults
    // apply.
    let env = EnvScope::new();
    let cfg = AivyxConfig::load_from_env_and_toml(
        &LoadOptions::test_env_only(),
    )
    .expect("load");
    assert!(cfg.ollama_options.is_empty());
    drop(env);
}

#[test]
fn phase_121_ollama_partial_section_parses_set_fields_only() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[ollama]\nnum_ctx = 16384\nmirostat = 2\n",
        "phase121-ollama-partial",
    );
    assert_eq!(cfg.ollama_options.num_ctx, Some(16384));
    assert_eq!(cfg.ollama_options.mirostat, Some(2));
    // Unset fields stay None.
    assert!(cfg.ollama_options.num_predict.is_none());
    assert!(cfg.ollama_options.top_p.is_none());
    assert!(cfg.ollama_options.seed.is_none());
    drop(env);
}

#[test]
fn phase_121_ollama_full_section_parses_every_field() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[ollama]\n\
         num_ctx = 32768\n\
         num_predict = 2048\n\
         num_thread = 8\n\
         mirostat = 1\n\
         top_k = 40\n\
         top_p = 0.9\n\
         repeat_penalty = 1.1\n\
         repeat_last_n = 64\n\
         seed = 42\n",
        "phase121-ollama-full",
    );
    let opts = &cfg.ollama_options;
    assert_eq!(opts.num_ctx, Some(32768));
    assert_eq!(opts.num_predict, Some(2048));
    assert_eq!(opts.num_thread, Some(8));
    assert_eq!(opts.mirostat, Some(1));
    assert_eq!(opts.top_k, Some(40));
    assert!((opts.top_p.unwrap() - 0.9).abs() < 1e-6);
    assert!((opts.repeat_penalty.unwrap() - 1.1).abs() < 1e-6);
    assert_eq!(opts.repeat_last_n, Some(64));
    assert_eq!(opts.seed, Some(42));
    drop(env);
}

#[test]
fn phase_121_ollama_is_empty_helper_pins_default() {
    let opts = crate::OllamaOptions::default();
    assert!(opts.is_empty());
    let opts = crate::OllamaOptions {
        num_ctx: Some(1),
        ..crate::OllamaOptions::default()
    };
    assert!(!opts.is_empty());
}

// ----- Phase 122 Task 2 — detect_model_family + OllamaFamilyStrategy -----

#[test]
fn phase_122_detect_qwen_models() {
    // qwen3.6:27b and qwen3.5:7b → qwen3 (major version
    // only; qwen3.x minor revisions share substrate and the
    // same per-family default strategy).
    assert_eq!(
        crate::detect_model_family("qwen3.6:27b").as_deref(),
        Some("qwen3")
    );
    assert_eq!(
        crate::detect_model_family("qwen3.5:7b").as_deref(),
        Some("qwen3")
    );
    // qwen2.5:7b → qwen2 (separate major).
    assert_eq!(
        crate::detect_model_family("qwen2.5:7b").as_deref(),
        Some("qwen2")
    );
}

#[test]
fn phase_122_detect_gemma_models() {
    assert_eq!(
        crate::detect_model_family("gemma4:31b").as_deref(),
        Some("gemma4")
    );
    assert_eq!(
        crate::detect_model_family("gemma3:9b").as_deref(),
        Some("gemma3")
    );
}

#[test]
fn phase_122_detect_llama_models() {
    assert_eq!(
        crate::detect_model_family("llama3.1:latest").as_deref(),
        Some("llama3")
    );
    assert_eq!(
        crate::detect_model_family("llama3.2:1b").as_deref(),
        Some("llama3")
    );
    assert_eq!(
        crate::detect_model_family("llama2:13b").as_deref(),
        Some("llama2")
    );
}

#[test]
fn phase_122_detect_returns_none_for_non_ollama_model_names() {
    // Cloud model names don't follow Ollama's family:tag
    // convention.
    assert!(crate::detect_model_family("claude-haiku-4-5").is_none());
    assert!(crate::detect_model_family("gpt-4").is_none());
    assert!(crate::detect_model_family("gpt-4o-mini").is_none());
}

#[test]
fn phase_122_detect_returns_none_for_bare_family_without_digits() {
    // "qwen" alone — no version, no family-key.
    assert!(crate::detect_model_family("qwen").is_none());
    assert!(crate::detect_model_family("qwen:latest").is_none());
    assert!(crate::detect_model_family("gemma").is_none());
}

#[test]
fn phase_122_detect_returns_none_for_empty_input() {
    assert!(crate::detect_model_family("").is_none());
    assert!(crate::detect_model_family(":tag-only").is_none());
}

#[test]
fn phase_124_family_strategy_defaults_match_sign_off() {
    // qwen3 and gemma4 default to FewShotExamples (Phase 124
    // upgrade from Phase 122's StructuredInjection). Phase 122
    // empirically showed StructuredInjection wasn't enough;
    // Phase 124 attempts breakthrough with worked examples.
    assert_eq!(
        crate::OllamaFamilyStrategy::default_for_family("qwen3"),
        crate::OllamaFamilyStrategy::FewShotExamples
    );
    assert_eq!(
        crate::OllamaFamilyStrategy::default_for_family("gemma4"),
        crate::OllamaFamilyStrategy::FewShotExamples
    );
    // llama3's tool-use protocol is presumed more reliable;
    // None preserved across both phases.
    assert_eq!(
        crate::OllamaFamilyStrategy::default_for_family("llama3"),
        crate::OllamaFamilyStrategy::None
    );
    // Unknown families default to None — operator-conservative.
    assert_eq!(
        crate::OllamaFamilyStrategy::default_for_family("unknown"),
        crate::OllamaFamilyStrategy::None
    );
    assert_eq!(
        crate::OllamaFamilyStrategy::default_for_family(""),
        crate::OllamaFamilyStrategy::None
    );
}

#[test]
fn detect_gpt_oss_models() {
    // POLISH_WAVES.md sub-project 4, item B.1 — gpt-oss has no numbered
    // generations to date, unlike qwen/gemma/llama, so this matches the
    // literal family-part string rather than extracting a major-version
    // digit.
    assert_eq!(
        crate::detect_model_family("gpt-oss:20b").as_deref(),
        Some("gpt-oss")
    );
    assert_eq!(
        crate::detect_model_family("gpt-oss:120b").as_deref(),
        Some("gpt-oss")
    );
    // A plain "gpt-4"/"gpt-4o-mini" cloud model name must NOT match —
    // guards against a future broadening of this branch accidentally
    // catching OpenAI's own cloud model names (already asserted None by
    // `phase_122_detect_returns_none_for_non_ollama_model_names` above;
    // this test re-confirms it stays that way once the gpt-oss branch
    // exists).
    assert!(crate::detect_model_family("gpt-4").is_none());
    assert!(crate::detect_model_family("gpt-4o-mini").is_none());
}

#[test]
fn gpt_oss_defaults_to_few_shot_examples() {
    // POLISH_WAVES.md sub-project 4, item B.1 — reuses the existing
    // lever already proven for qwen3/gemma4 (worked examples), just
    // re-targeted at gpt-oss's post-tool finishing gap rather than
    // tool-availability refusal. Not a new OllamaFamilyStrategy variant.
    assert_eq!(
        crate::OllamaFamilyStrategy::default_for_family("gpt-oss"),
        crate::OllamaFamilyStrategy::FewShotExamples
    );
}

#[test]
fn resolve_gpt_oss_prompt_strategy_uses_few_shot_default() {
    let overrides = std::collections::BTreeMap::new();
    assert_eq!(
        crate::resolve_ollama_prompt_strategy("gpt-oss:20b", &overrides),
        crate::OllamaFamilyStrategy::FewShotExamples
    );
}

#[test]
fn phase_122_family_strategy_label_is_stable_lowercase() {
    // Labels match the TOML wire form so the operator's
    // aivyx-pa.toml can pass `prompt_strategy = "none"` /
    // `prompt_strategy = "structured_injection"` /
    // `prompt_strategy = "few_shot_examples"` directly.
    assert_eq!(crate::OllamaFamilyStrategy::None.label(), "none");
    assert_eq!(
        crate::OllamaFamilyStrategy::StructuredInjection.label(),
        "structured_injection"
    );
    assert_eq!(
        crate::OllamaFamilyStrategy::FewShotExamples.label(),
        "few_shot_examples"
    );
}

#[test]
fn phase_124_parse_accepts_few_shot_examples_wire_label() {
    assert_eq!(
        crate::OllamaFamilyStrategy::parse("few_shot_examples").unwrap(),
        crate::OllamaFamilyStrategy::FewShotExamples
    );
    // Case-insensitive (matches the other variants).
    assert_eq!(
        crate::OllamaFamilyStrategy::parse("FEW_SHOT_EXAMPLES").unwrap(),
        crate::OllamaFamilyStrategy::FewShotExamples
    );
}

#[test]
fn phase_124_parse_error_message_lists_few_shot_examples() {
    let err = crate::OllamaFamilyStrategy::parse("aggressive").unwrap_err();
    assert!(
        err.contains("few_shot_examples"),
        "error message should list few_shot_examples as a valid option; got: {err}"
    );
}

#[test]
fn phase_122_family_strategy_default_trait_returns_none() {
    let s = crate::OllamaFamilyStrategy::default();
    assert_eq!(s, crate::OllamaFamilyStrategy::None);
}

// ----- Phase 122 Task 5 — parse + resolve + TOML override surface -----

#[test]
fn phase_122_parse_accepts_wire_labels() {
    // Labels round-trip with `.label()` so the operator's TOML
    // can use the same spelling the helper emits.
    assert_eq!(
        crate::OllamaFamilyStrategy::parse("none").unwrap(),
        crate::OllamaFamilyStrategy::None
    );
    assert_eq!(
        crate::OllamaFamilyStrategy::parse("structured_injection").unwrap(),
        crate::OllamaFamilyStrategy::StructuredInjection
    );
}

#[test]
fn phase_122_parse_is_case_insensitive_and_trims_whitespace() {
    // Operators typing "None" or "  none  " shouldn't get a
    // confusing config error.
    assert_eq!(
        crate::OllamaFamilyStrategy::parse("None").unwrap(),
        crate::OllamaFamilyStrategy::None
    );
    assert_eq!(
        crate::OllamaFamilyStrategy::parse("STRUCTURED_INJECTION").unwrap(),
        crate::OllamaFamilyStrategy::StructuredInjection
    );
    assert_eq!(
        crate::OllamaFamilyStrategy::parse("  none  ").unwrap(),
        crate::OllamaFamilyStrategy::None
    );
}

#[test]
fn phase_122_parse_rejects_unknown_strategy() {
    let err = crate::OllamaFamilyStrategy::parse("aggressive").unwrap_err();
    assert!(
        err.contains("none") && err.contains("structured_injection"),
        "error should name valid options; got: {err}"
    );
}

#[test]
fn phase_122_resolve_uses_default_when_no_override_present() {
    let overrides = std::collections::BTreeMap::new();
    // qwen3 → FewShotExamples (Phase 124 default upgrade).
    assert_eq!(
        crate::resolve_ollama_prompt_strategy("qwen3.6:27b", &overrides),
        crate::OllamaFamilyStrategy::FewShotExamples
    );
    // llama3 → None (preserved across phases).
    assert_eq!(
        crate::resolve_ollama_prompt_strategy("llama3.1:latest", &overrides),
        crate::OllamaFamilyStrategy::None
    );
}

#[test]
fn phase_122_resolve_operator_override_beats_default() {
    let mut overrides = std::collections::BTreeMap::new();
    // Operator opts out of qwen3's structured-injection default.
    overrides.insert("qwen3".to_string(), crate::OllamaFamilyStrategy::None);
    // Operator opts llama3 INTO structured-injection.
    overrides.insert(
        "llama3".to_string(),
        crate::OllamaFamilyStrategy::StructuredInjection,
    );
    assert_eq!(
        crate::resolve_ollama_prompt_strategy("qwen3.6:27b", &overrides),
        crate::OllamaFamilyStrategy::None
    );
    assert_eq!(
        crate::resolve_ollama_prompt_strategy("llama3.1:latest", &overrides),
        crate::OllamaFamilyStrategy::StructuredInjection
    );
}

#[test]
fn phase_122_resolve_returns_none_for_undetected_model() {
    let mut overrides = std::collections::BTreeMap::new();
    // Override for a family the cloud model can't be detected as.
    overrides.insert(
        "qwen3".to_string(),
        crate::OllamaFamilyStrategy::StructuredInjection,
    );
    // Cloud model name → detect_model_family returns None →
    // resolve returns None regardless of any overrides.
    assert_eq!(
        crate::resolve_ollama_prompt_strategy("claude-haiku-4-5", &overrides),
        crate::OllamaFamilyStrategy::None
    );
}

#[test]
fn phase_122_loader_parses_prompt_strategies_section() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[ollama.prompt_strategies]\n\
         qwen3 = \"none\"\n\
         llama3 = \"structured_injection\"\n",
        "phase122-prompt-strategies",
    );
    assert_eq!(
        cfg.ollama_prompt_strategies.get("qwen3").copied(),
        Some(crate::OllamaFamilyStrategy::None)
    );
    assert_eq!(
        cfg.ollama_prompt_strategies.get("llama3").copied(),
        Some(crate::OllamaFamilyStrategy::StructuredInjection)
    );
    // Unset family → not in map; resolve falls through to default.
    assert!(!cfg.ollama_prompt_strategies.contains_key("gemma4"));
    drop(env);
}

#[test]
fn phase_122_loader_rejects_unknown_strategy_string() {
    let env = EnvScope::new();
    let tmp = TempDir::new("phase122-bad-strategy");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[ollama.prompt_strategies]\n\
         qwen3 = \"aggressive\"\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("loader rejects unknown strategy string");
    let msg = err.to_string();
    assert!(
        msg.contains("ollama.prompt_strategies") && msg.contains("qwen3"),
        "error should name section + offending family; got: {msg}"
    );
    drop(env);
}

#[test]
fn chapter_k_loader_parses_pricing_overrides() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[pricing.gpt-5]\ninput = 2.0\noutput = 8.0\n\
         \n[pricing.claude-opus-4-8]\ninput = 12.0\noutput = 60.0\ncache_read = 1.2\n",
        "chapter-k-pricing",
    );
    let opus = cfg.pricing.get("claude-opus-4-8").expect("opus override");
    assert_eq!(opus.input, 12.0);
    assert_eq!(opus.output, 60.0);
    assert_eq!(opus.cache_read, 1.2);
    assert_eq!(opus.cache_write, 0.0, "unset cache class defaults to 0");
    assert!(cfg.pricing.contains_key("gpt-5"));
    assert!(!cfg.pricing.contains_key("gpt-4o"), "only declared models");
    drop(env);
}

#[test]
fn chapter_k_loader_rejects_negative_rate() {
    let env = EnvScope::new();
    let tmp = TempDir::new("chapter-k-bad-pricing");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[pricing.bad-model]\ninput = -1.0\noutput = 5.0\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("loader rejects a negative rate");
    let msg = err.to_string();
    assert!(
        msg.contains("pricing") && msg.contains("bad-model"),
        "error should name the section + offending model; got: {msg}"
    );
    drop(env);
}

#[test]
fn chapter_k_loader_defaults_budget_uncapped() {
    let env = EnvScope::new();
    let cfg = load_with_toml("\n", "chapter-k-budget-default");
    // No [budget] section ⇒ uncapped, Deny-on-exceeded, 0.8 alert default.
    assert_eq!(cfg.budget, aivyx_cost::BudgetConfig::default());
    assert!(cfg.budget.per_run_usd.is_none());
    assert!(cfg.budget.per_day_usd.is_none());
    drop(env);
}

#[test]
fn chapter_k_loader_parses_budget_section() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[budget]\nper_run_usd = 5.0\nper_day_usd = 25.0\n\
         on_exceeded = \"alert\"\nalert_at = 0.5\n",
        "chapter-k-budget",
    );
    assert_eq!(cfg.budget.per_run_usd, Some(5.0));
    assert_eq!(cfg.budget.per_day_usd, Some(25.0));
    assert_eq!(cfg.budget.on_exceeded, aivyx_cost::BudgetAction::Alert);
    assert_eq!(cfg.budget.alert_at, Some(0.5));
    drop(env);
}

#[test]
fn ballast_loader_parses_per_mission_caps() {
    // Chapter Ballast — the per-mission caps load from `[budget]` and default
    // to None (unbounded) when absent.
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[budget]\nper_mission_tokens = 200000\nper_mission_usd = 1.0\n",
        "ballast-mission-caps",
    );
    assert_eq!(cfg.budget.per_mission_tokens, Some(200_000));
    assert_eq!(cfg.budget.per_mission_usd, Some(1.0));
    // The run/day caps stay independent (unset here).
    assert!(cfg.budget.per_run_usd.is_none());

    let bare = load_with_toml("\n[budget]\nper_run_usd = 2.0\n", "ballast-default");
    assert!(bare.budget.per_mission_tokens.is_none());
    assert!(bare.budget.per_mission_usd.is_none());
    drop(env);
}

#[test]
fn chapter_k_loader_rejects_negative_budget_cap() {
    let env = EnvScope::new();
    let tmp = TempDir::new("chapter-k-bad-budget");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(&toml_path, "\n[budget]\nper_run_usd = -1.0\n").unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts)
        .expect_err("loader rejects a negative cap");
    let msg = err.to_string();
    assert!(
        msg.contains("budget") && msg.contains("per_run_usd"),
        "error should name the section + offending field; got: {msg}"
    );
    drop(env);
}

#[test]
fn phase_122_loader_absent_section_yields_empty_map() {
    let env = EnvScope::new();
    let cfg = AivyxConfig::load_from_env_and_toml(
        &LoadOptions::test_env_only(),
    )
    .expect("load");
    assert!(cfg.ollama_prompt_strategies.is_empty());
    drop(env);
}

// ----- Phase 120 — [providers] tool_name_auto_correct_threshold -----

#[test]
fn phase_120_threshold_absent_section_uses_default() {
    // No [providers] section → loader supplies
    // DEFAULT_TOOL_NAME_AUTO_CORRECT_THRESHOLD (0.80, matches Phase
    // 112's fuzzy default). FieldSource::Default tagged so the
    // startup banner can show the operator where the value came from.
    let env = EnvScope::new();
    let cfg = AivyxConfig::load_from_env_and_toml(
        &LoadOptions::test_env_only(),
    )
    .expect("load");
    assert!(
        (cfg.tool_name_auto_correct_threshold.value
            - crate::DEFAULT_TOOL_NAME_AUTO_CORRECT_THRESHOLD)
            .abs()
            < 1e-6
    );
    assert_eq!(
        cfg.tool_name_auto_correct_threshold.source,
        FieldSource::Default
    );
    drop(env);
}

#[test]
fn phase_120_threshold_explicit_value_parses() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[providers]\ntool_name_auto_correct_threshold = 0.65\n",
        "phase120-explicit",
    );
    assert!(
        (cfg.tool_name_auto_correct_threshold.value - 0.65).abs() < 1e-6
    );
    assert_eq!(
        cfg.tool_name_auto_correct_threshold.source,
        FieldSource::Toml
    );
    drop(env);
}

#[test]
fn phase_120_threshold_zero_is_valid() {
    // 0.0 → never auto-correct. Operator-conservative posture.
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[providers]\ntool_name_auto_correct_threshold = 0.0\n",
        "phase120-zero",
    );
    assert!(cfg.tool_name_auto_correct_threshold.value.abs() < 1e-6);
    drop(env);
}

#[test]
fn phase_120_threshold_one_is_valid() {
    // 1.0 → exact match only.
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[providers]\ntool_name_auto_correct_threshold = 1.0\n",
        "phase120-one",
    );
    assert!((cfg.tool_name_auto_correct_threshold.value - 1.0).abs() < 1e-6);
    drop(env);
}

#[test]
fn phase_120_threshold_above_one_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("phase120-above");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[providers]\ntool_name_auto_correct_threshold = 1.5\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).unwrap_err();
    match err {
        ConfigError::Invalid { field, reason } => {
            assert_eq!(field, "providers.tool_name_auto_correct_threshold");
            assert!(reason.contains("must be in [0.0, 1.0]"));
        }
        other => panic!("expected ConfigError::Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn phase_120_threshold_negative_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("phase120-neg");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[providers]\ntool_name_auto_correct_threshold = -0.1\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).unwrap_err();
    assert!(matches!(err, ConfigError::Invalid { .. }));
    drop(env);
}

#[test]
fn tool_relevance_minimal_section_uses_defaults() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[tool_relevance]\nenabled = true\n",
        "tr-minimal",
    );
    let tr = cfg.tool_relevance.expect("section present");
    assert!(tr.enabled);
    assert_eq!(tr.max_keywords, 5);
    assert_eq!(tr.min_outcomes_to_show, 2);
    assert_eq!(tr.top_k_per_section, 5);
    drop(env);
}

#[test]
fn tool_relevance_full_section_parses_all_fields() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[tool_relevance]\n\
         enabled = true\n\
         max_keywords = 7\n\
         min_outcomes_to_show = 3\n\
         top_k_per_section = 10\n",
        "tr-full",
    );
    let tr = cfg.tool_relevance.expect("section present");
    assert!(tr.enabled);
    assert_eq!(tr.max_keywords, 7);
    assert_eq!(tr.min_outcomes_to_show, 3);
    assert_eq!(tr.top_k_per_section, 10);
    drop(env);
}

#[test]
fn tool_relevance_zero_max_keywords_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("tr-bad-mk");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[tool_relevance]\nenabled = true\nmax_keywords = 0\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err =
        AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(field, "tool_relevance.max_keywords");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn tool_relevance_zero_min_outcomes_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("tr-bad-mo");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[tool_relevance]\nenabled = true\nmin_outcomes_to_show = 0\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err =
        AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(field, "tool_relevance.min_outcomes_to_show");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

#[test]
fn tool_relevance_zero_top_k_is_invalid() {
    let env = EnvScope::new();
    let tmp = TempDir::new("tr-bad-tk");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(
        &toml_path,
        "\n[tool_relevance]\nenabled = true\ntop_k_per_section = 0\n",
    )
    .unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err =
        AivyxConfig::load_from_env_and_toml(&opts).expect_err("must error");
    match err {
        ConfigError::Invalid { field, .. } => {
            assert_eq!(field, "tool_relevance.top_k_per_section");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    drop(env);
}

/// Disabled section is still loaded (so the operator can stage
/// config without enabling). `enabled = false` is the explicit
/// off state; absent section is the implicit off state.
#[test]
fn tool_relevance_disabled_section_loads_with_enabled_false() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[tool_relevance]\nenabled = false\nmax_keywords = 7\n",
        "tr-disabled",
    );
    let tr = cfg.tool_relevance.expect("section present");
    assert!(!tr.enabled);
    assert_eq!(tr.max_keywords, 7);
    drop(env);
}

// ------------------------------------------------------------------
// Chapter N — access levels ([access] section)
// ------------------------------------------------------------------

/// No `[access]` section ⇒ `sandbox` ⇒ today's behavior: fs_root defaults
/// to `$HOME/aivyx-pa-sandbox`, confirm_destructive off. The byte-for-byte
/// backwards-compat guarantee.
#[test]
fn access_absent_section_defaults_to_sandbox() {
    let env = EnvScope::new();
    let cfg = AivyxConfig::load_from_env_and_toml(&LoadOptions::test_env_only())
        .expect("load");
    assert_eq!(cfg.access_level.value, AccessLevel::Sandbox);
    assert_eq!(cfg.access_level.source, FieldSource::Default);
    assert!(cfg.fs_root.value.ends_with("aivyx-pa-sandbox"));
    assert_eq!(cfg.fs_root.source, FieldSource::Default);
    assert!(!cfg.confirm_destructive.value, "sandbox ⇒ no confirm gate");
    assert_eq!(cfg.confirm_destructive.source, FieldSource::Default);
    drop(env);
}

/// `level = "home"` ⇒ fs_root = `$HOME`, confirm_destructive defaults on.
#[test]
fn access_home_roots_at_home_and_confirms() {
    let env = EnvScope::new();
    let home = std::env::var("HOME").expect("EnvScope sets HOME");
    let cfg = load_with_toml("\n[access]\nlevel = \"home\"\n", "access-home");
    assert_eq!(cfg.access_level.value, AccessLevel::Home);
    assert_eq!(cfg.access_level.source, FieldSource::Toml);
    assert_eq!(cfg.fs_root.value, PathBuf::from(home));
    assert_eq!(cfg.fs_root.source, FieldSource::Default);
    assert!(cfg.confirm_destructive.value, "home defaults confirm on");
    drop(env);
}

/// `level = "full"` ⇒ fs_root = `/`.
#[test]
fn access_full_roots_at_filesystem_root() {
    let env = EnvScope::new();
    let cfg = load_with_toml("\n[access]\nlevel = \"full\"\n", "access-full");
    assert_eq!(cfg.access_level.value, AccessLevel::Full);
    assert_eq!(cfg.fs_root.value, PathBuf::from("/"));
    assert!(cfg.confirm_destructive.value);
    drop(env);
}

/// `level = "workspace"` with an explicit `root` ⇒ fs_root = that root.
#[test]
fn access_workspace_uses_explicit_root() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[access]\nlevel = \"workspace\"\nroot = \"/tmp/proj\"\n",
        "access-ws",
    );
    assert_eq!(cfg.access_level.value, AccessLevel::Workspace);
    assert_eq!(cfg.fs_root.value, PathBuf::from("/tmp/proj"));
    assert_eq!(cfg.fs_root.source, FieldSource::Toml);
    drop(env);
}

/// `level = "workspace"` with no root anywhere ⇒ typed `Invalid` error.
#[test]
fn access_workspace_without_root_is_typed_error() {
    let env = EnvScope::new();
    let tmp = TempDir::new("access-ws-noroot");
    let toml_path = tmp.path().join("aivyx-pa.toml");
    std::fs::write(&toml_path, "\n[access]\nlevel = \"workspace\"\n").unwrap();
    let opts = LoadOptions {
        toml_path: Some(toml_path),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let err = AivyxConfig::load_from_env_and_toml(&opts).expect_err("needs a root");
    match err {
        ConfigError::Invalid { field, .. } => assert_eq!(field, "access.root"),
        other => panic!("expected Invalid(access.root), got {other:?}"),
    }
    drop(env);
}

/// An explicit `[fs] root` overrides the level-derived default.
#[test]
fn explicit_fs_root_overrides_access_level_default() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[fs]\nroot = \"/tmp/explicit\"\n[access]\nlevel = \"home\"\n",
        "access-override",
    );
    assert_eq!(cfg.access_level.value, AccessLevel::Home);
    assert_eq!(cfg.fs_root.value, PathBuf::from("/tmp/explicit"));
    assert_eq!(cfg.fs_root.source, FieldSource::Toml);
    drop(env);
}

/// `confirm_destructive = false` overrides the per-level default.
#[test]
fn access_confirm_destructive_explicit_override() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[access]\nlevel = \"home\"\nconfirm_destructive = false\n",
        "access-noconfirm",
    );
    assert_eq!(cfg.access_level.value, AccessLevel::Home);
    assert!(!cfg.confirm_destructive.value, "explicit override wins");
    assert_eq!(cfg.confirm_destructive.source, FieldSource::Toml);
    drop(env);
}

// ------------------------------------------------------------------
// `[confine]` section — OS-level process confinement enforcement
// ------------------------------------------------------------------

/// No `[confine]` section ⇒ `require_enforcement` defaults to `true`
/// (fail-closed).
#[test]
fn confine_require_enforcement_defaults_to_true_when_absent() {
    let env = EnvScope::new();
    let cfg = load_with_toml("\n", "confine-absent");
    assert!(cfg.require_enforcement.value);
    assert_eq!(cfg.require_enforcement.source, FieldSource::Default);
    drop(env);
}

/// An explicit `[confine] require_enforcement = false` overrides the
/// fail-closed default.
#[test]
fn confine_require_enforcement_reads_an_explicit_false() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[confine]\nrequire_enforcement = false\n",
        "confine-explicit-false",
    );
    assert!(!cfg.require_enforcement.value);
    assert_eq!(cfg.require_enforcement.source, FieldSource::Toml);
    drop(env);
}

/// No `[agent] injection_scan_enabled` key ⇒ defaults to `true`
/// (fail-closed), matching `require_enforcement`'s posture.
#[test]
fn agent_injection_scan_enabled_defaults_to_true_when_absent() {
    let env = EnvScope::new();
    let cfg = load_with_toml("\n", "injection-scan-absent");
    assert!(cfg.injection_scan_enabled.value);
    assert_eq!(cfg.injection_scan_enabled.source, FieldSource::Default);
    assert!(cfg.injection_scan_exempt.is_empty());
    drop(env);
}

/// An explicit `[agent] injection_scan_enabled = false` overrides the
/// fail-closed default.
#[test]
fn agent_injection_scan_enabled_reads_an_explicit_false() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[agent]\ninjection_scan_enabled = false\n",
        "injection-scan-explicit-false",
    );
    assert!(!cfg.injection_scan_enabled.value);
    assert_eq!(cfg.injection_scan_enabled.source, FieldSource::Toml);
    drop(env);
}

/// `[agent] injection_scan_exempt` reads a populated list of tool
/// names verbatim, with no validation against a known-tools registry.
#[test]
fn agent_injection_scan_exempt_reads_an_explicit_list() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[agent]\ninjection_scan_exempt = [\"gmail.read\", \"not.a.real.tool\"]\n",
        "injection-scan-exempt-list",
    );
    assert_eq!(
        cfg.injection_scan_exempt,
        vec!["gmail.read".to_string(), "not.a.real.tool".to_string()]
    );
    drop(env);
}

// --- Chapter Reins (RN.2) — the `[autonomy]` section -----------------

/// No `[autonomy]` section ⇒ `assisted` ⇒ today's behavior: the effective
/// posture is `todays_default`. The byte-for-byte backwards-compat guarantee
/// at the config layer.
#[test]
fn autonomy_absent_section_defaults_to_assisted() {
    let env = EnvScope::new();
    let cfg = AivyxConfig::load_from_env_and_toml(&LoadOptions::test_env_only())
        .expect("load");
    assert_eq!(cfg.autonomy_level.value, AutonomyLevel::Assisted);
    assert_eq!(cfg.autonomy_level.source, FieldSource::Default);
    assert!(cfg.autonomy_overrides.is_empty());
    assert!(cfg.autonomy_auto_approve.is_empty());
    assert_eq!(
        cfg.effective_autonomy(None),
        AutonomyPosture::todays_default(),
        "absent [autonomy] must resolve to today's posture",
    );
    drop(env);
}

/// `level = "autonomous"` parses and sources from TOML.
#[test]
fn autonomy_explicit_level_parses() {
    let env = EnvScope::new();
    let cfg = load_with_toml("\n[autonomy]\nlevel = \"autonomous\"\n", "auto-level");
    assert_eq!(cfg.autonomy_level.value, AutonomyLevel::Autonomous);
    assert_eq!(cfg.autonomy_level.source, FieldSource::Toml);
    assert_eq!(
        cfg.effective_autonomy(None),
        AutonomyLevel::Autonomous.expand()
    );
    drop(env);
}

/// Per-domain `[[autonomy.override]]` resolves most-specific-then-global, and
/// the `[autonomy.auto_approve]` allowlist parses.
#[test]
fn autonomy_per_domain_overrides_resolve() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[autonomy]\nlevel = \"supervised\"\n\
         \n[[autonomy.override]]\ndomain = \"email\"\nlevel = \"manual\"\n\
         \n[[autonomy.override]]\ndomain = \"shell\"\nlevel = \"autonomous\"\n\
         \n[autonomy.auto_approve]\nscopes = [\"fs.write\", \"net.fetch\"]\n",
        "auto-overrides",
    );
    assert_eq!(cfg.autonomy_level.value, AutonomyLevel::Supervised);
    // email → manual, shell → autonomous, anything else → the global supervised.
    assert_eq!(
        cfg.effective_autonomy(Some("email")),
        AutonomyLevel::Manual.expand()
    );
    assert_eq!(
        cfg.effective_autonomy(Some("shell")),
        AutonomyLevel::Autonomous.expand()
    );
    assert_eq!(
        cfg.effective_autonomy(Some("git")),
        AutonomyLevel::Supervised.expand()
    );
    assert_eq!(cfg.autonomy_auto_approve, vec!["fs.write", "net.fetch"]);
    drop(env);
}

/// An `[[autonomy.override]]` with no `domain` is a typed `Invalid` error —
/// invalid config never silently loads.
#[test]
fn autonomy_override_without_domain_is_typed_error() {
    let env = EnvScope::new();
    let err = load_with_toml_result(
        "\n[autonomy]\nlevel = \"supervised\"\n\
         \n[[autonomy.override]]\nlevel = \"manual\"\n",
        "auto-bad-override",
    )
    .expect_err("missing override domain must error");
    assert!(
        matches!(err, ConfigError::Invalid { field, .. } if field == "autonomy.override.domain"),
        "expected an Invalid error on autonomy.override.domain, got {err:?}",
    );
    drop(env);
}

/// An unknown `level` token is rejected at parse time (serde), not silently
/// defaulted.
#[test]
fn autonomy_unknown_level_is_rejected() {
    let env = EnvScope::new();
    let err = load_with_toml_result("\n[autonomy]\nlevel = \"yolo\"\n", "auto-bad-level")
        .expect_err("unknown level must error");
    assert!(
        matches!(err, ConfigError::TomlParse { .. }),
        "expected a TOML parse error for the unknown level, got {err:?}",
    );
    drop(env);
}

// ------------------------------------------------------------------
// Chapter O — agent workspace ([workspace] section)
// ------------------------------------------------------------------

/// Absent `[workspace]` ⇒ enabled, default path `$HOME/.aivyx-pa/workspace`,
/// journaling on at the default interval.
#[test]
fn workspace_absent_section_defaults_enabled() {
    let env = EnvScope::new();
    let home = std::env::var("HOME").expect("EnvScope sets HOME");
    let cfg = AivyxConfig::load_from_env_and_toml(&LoadOptions::test_env_only())
        .expect("load");
    assert!(cfg.workspace_enabled.value);
    assert_eq!(cfg.workspace_enabled.source, FieldSource::Default);
    assert_eq!(
        cfg.workspace_path.value,
        PathBuf::from(home).join(".aivyx-pa").join("workspace")
    );
    assert!(cfg.workspace_journaling_enabled.value);
    assert_eq!(
        cfg.workspace_journaling_interval_secs.value,
        crate::DEFAULT_WORKSPACE_JOURNALING_INTERVAL_SECS
    );
    drop(env);
}

/// `[workspace]` overrides: disabled, custom path, journaling off + interval.
#[test]
fn workspace_explicit_overrides() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[workspace]\nenabled = false\npath = \"/tmp/ws\"\n\
         [workspace.journaling]\nenabled = false\ninterval_secs = 300\n",
        "ws-override",
    );
    assert!(!cfg.workspace_enabled.value);
    assert_eq!(cfg.workspace_path.value, PathBuf::from("/tmp/ws"));
    assert_eq!(cfg.workspace_path.source, FieldSource::Toml);
    assert!(!cfg.workspace_journaling_enabled.value);
    assert_eq!(cfg.workspace_journaling_interval_secs.value, 300);
    assert_eq!(
        cfg.workspace_journaling_interval_secs.source,
        FieldSource::Toml
    );
    drop(env);
}

/// `AIVYX_PA_WORKSPACE` env beats the TOML path.
#[test]
fn workspace_env_beats_toml_path() {
    let env = EnvScope::new();
    env.set("AIVYX_PA_WORKSPACE", "/tmp/env-ws");
    let cfg = load_with_toml("\n[workspace]\npath = \"/tmp/toml-ws\"\n", "ws-env");
    assert_eq!(cfg.workspace_path.value, PathBuf::from("/tmp/env-ws"));
    assert_eq!(cfg.workspace_path.source, FieldSource::Env);
    drop(env);
}

// --- Chapter Roster (RO.1): `[team] config_path` -------------------------

#[test]
fn team_config_path_absent_section_is_none() {
    // No `[team]` section → no team-config pointer (the daemon falls back to
    // the conventional `team.toml` / built-in Nonagon).
    let env = EnvScope::new();
    let cfg = load_with_toml("\n[agent]\nprovider = \"ollama\"\n", "team-absent");
    assert_eq!(cfg.team_config_path, None);
    drop(env);
}

#[test]
fn team_config_path_is_parsed_from_the_team_section() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[team]\nconfig_path = \"teams/boh.toml\"\n",
        "team-present",
    );
    assert_eq!(cfg.team_config_path, Some(PathBuf::from("teams/boh.toml")));
    drop(env);
}

// ------------------------------------------------------------------
// Backlog #1 — silent dead-memory config: `profile = smart` with no
// `[embedding]` provider warns (semantic recall would be inert).
// ------------------------------------------------------------------

#[test]
fn smart_profile_without_embedding_warns() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[memory]\nprofile = \"smart\"\n",
        "smart-no-embedding",
    );
    assert!(
        cfg.warnings.iter().any(|w| w.contains("profile = smart")
            && w.contains("[embedding]")),
        "expected a dead-memory warning, got: {:?}",
        cfg.warnings
    );
    drop(env);
}

#[test]
fn smart_profile_with_embedding_does_not_warn() {
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[memory]\nprofile = \"smart\"\n\
         [embedding]\nmodel = \"nomic-embed-text\"\ndimensions = 768\n",
        "smart-with-embedding",
    );
    assert!(
        !cfg.warnings.iter().any(|w| w.contains("semantic recall is inert")),
        "no dead-memory warning when embedding is configured: {:?}",
        cfg.warnings
    );
    drop(env);
}

#[test]
fn lite_profile_without_embedding_does_not_warn() {
    // `lite` is embedding-free by design (lexical + co-occurrence over
    // existing data), so it must NOT trip the dead-memory warning.
    let env = EnvScope::new();
    let cfg = load_with_toml(
        "\n[memory]\nprofile = \"lite\"\n",
        "lite-no-embedding",
    );
    assert!(
        !cfg.warnings.iter().any(|w| w.contains("semantic recall is inert")),
        "lite profile is intentionally embedding-free: {:?}",
        cfg.warnings
    );
    drop(env);
}
