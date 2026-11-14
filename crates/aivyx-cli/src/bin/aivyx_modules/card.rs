//! A18 — `aivyx-pa card add|list|remove`, `aivyx-pa unlock`, `aivyx-pa lock`.
//!
//! The PIN, admin PIN and passphrase are read with no echo, go to the daemon
//! (or the card helper) over a socket / pipe, and never reach argv, the
//! environment or a log.

use std::io::{BufRead, Write};
use std::path::Path;

use aivyx_card_helper::{CardHelper, ErrorKind, HelperError, ProcessHelper};
use aivyx_channel::card_unlock::{CardUnlockFile, enroll, remove};
use aivyx_channel::daemon_client;
use secrecy::SecretString;

use crate::CardSubcommand;

/// Removing the last card with no saved passphrase source needs this exact phrase.
pub const LAST_CARD_PHRASE: &str = "I know my passphrase";

pub fn removal_needs_confirmation(cards_left_after: usize, passphrase_saved: bool) -> bool {
    cards_left_after == 0 && !passphrase_saved
}

/// Does a raw typed line equal the last-card phrase? Only the trailing line
/// ending is stripped; surrounding spaces make the answer a mismatch.
pub fn is_last_card_phrase(raw_line: &str) -> bool {
    let line = raw_line
        .strip_suffix("\r\n")
        .or_else(|| raw_line.strip_suffix('\n'))
        .unwrap_or(raw_line);
    line == LAST_CARD_PHRASE
}

/// The plain-words line for a refused unlock.
pub fn unlock_failure_text(code: &str, message: &str, tries_left: Option<u8>) -> String {
    match code {
        "wrong_pin" => match tries_left {
            Some(n) => format!(
                "Wrong PIN — {n} {} left.",
                if n == 1 { "try" } else { "tries" }
            ),
            None => "Wrong PIN.".to_string(),
        },
        "pin_blocked" => "The card's PIN is blocked. Unlock with `aivyx-pa unlock --passphrase`, \
            and reset the PIN with your card's admin PIN."
            .to_string(),
        "no_card" if !message.is_empty() => message.to_string(),
        "no_card" => "No enrolled card is plugged in.".to_string(),
        "not_enrolled" if !message.is_empty() => message.to_string(),
        "not_enrolled" => "That card isn't enrolled. See `aivyx-pa card list`.".to_string(),
        "touch_timeout" => "No touch — still locked.".to_string(),
        "helper_missing" => format!(
            "{message}\nInstall it with `aivyx-pa tool-programs install`, or unlock with \
             `aivyx-pa unlock --passphrase`."
        ),
        "wrong_passphrase" => "Wrong passphrase — still locked.".to_string(),
        "busy" => "Another unlock is in progress — try again in a moment.".to_string(),
        "backoff" if !message.is_empty() => message.to_string(),
        "backoff" => "Too many failed unlocks — try again later.".to_string(),
        "not_locked" => "PA is already unlocked.".to_string(),
        _ => format!("Couldn't unlock: {message}"),
    }
}

/// Does `key` really open the store at `store_path`? The locked daemon's check
/// that a typed passphrase (or an unwrapped card key) is the right one, and
/// `card add`'s check when no daemon is running. Opens the store, reads one
/// sealed audit row (see `PersistentAuditLog::key_opens_store`) and closes it
/// again before returning, so the store lock is free for the real open that
/// follows. True when there is no store yet: there is nothing to check. Drives
/// its own small runtime, so call it from a blocking thread, not from inside
/// another runtime's task.
pub fn store_opens_with(store_path: &Path, key: &aivyx_crypto::MasterKey) -> bool {
    use aivyx_audit::PersistentAuditLog;
    use aivyx_storage::{RedbStorage, StorageConfig};
    if !store_path.exists() {
        return true;
    }
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("aivyx-pa: can't check the key: {e}");
            return false;
        }
    };
    let path = store_path.to_path_buf();
    let key = key.clone();
    rt.block_on(async move {
        let storage = match RedbStorage::open(StorageConfig::new(path), key).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("aivyx-pa: can't open the store to check the key: {e}");
                return false;
            }
        };
        let opens = PersistentAuditLog::key_opens_store(&storage).await;
        drop(storage);
        opens.unwrap_or_else(|e| {
            eprintln!("aivyx-pa: can't check the key: {e}");
            false
        })
    })
}

// ---------------------------------------------------------------------------
// Prompts
// ---------------------------------------------------------------------------

fn secret(prompt: &str) -> Result<String, String> {
    rpassword::prompt_password(prompt).map_err(|e| format!("could not read input: {e}"))
}

fn ask_line(prompt: &str) -> Result<String, String> {
    Ok(ask_raw_line(prompt)?.trim().to_string())
}

/// Like `ask_line`, but keeps the line exactly as typed (line ending included).
fn ask_raw_line(prompt: &str) -> Result<String, String> {
    print!("{prompt}");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| format!("could not read input: {e}"))?;
    Ok(line)
}

fn ask_yes(prompt: &str) -> Result<bool, String> {
    Ok(matches!(
        ask_line(prompt)?.to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

// ---------------------------------------------------------------------------
// unlock / lock
// ---------------------------------------------------------------------------

const NOT_RUNNING: &str = "aivyx-pa isn't running — start it with `aivyx-pa daemon start`";

pub async fn run_unlock(
    card: Option<String>,
    passphrase: bool,
    socket: &Path,
) -> Result<(), String> {
    if !daemon_client::daemon_is_running(socket).await {
        return Err(NOT_RUNNING.to_string());
    }
    let (locked, _) = daemon_client::lock_state(socket)
        .await
        .map_err(|e| e.to_string())?;
    if !locked {
        println!("PA is already unlocked.");
        return Ok(());
    }
    if passphrase {
        let p = secret("Passphrase: ")?;
        return finish_unlock(daemon_client::unlock_with_passphrase(socket, p).await);
    }
    loop {
        let pin = secret("YubiKey PIN: ")?;
        println!("Touch your YubiKey…");
        match daemon_client::unlock(socket, pin, card.clone()).await {
            Err(r) if r.code == "wrong_pin" && r.tries_left == Some(1) => {
                println!("{}", unlock_failure_text(&r.code, &r.message, r.tries_left));
                if ask_yes("One try left before the PIN blocks. Try again? [y/N] ")? {
                    continue;
                }
                return Err("still locked".to_string());
            }
            other => return finish_unlock(other),
        }
    }
}

fn finish_unlock(result: Result<String, daemon_client::UnlockRefused>) -> Result<(), String> {
    match result {
        Ok(_) => {
            println!("PA is unlocked.");
            Ok(())
        }
        Err(r) => Err(unlock_failure_text(&r.code, &r.message, r.tries_left)),
    }
}

pub async fn run_lock(socket: &Path) -> Result<(), String> {
    if !daemon_client::daemon_is_running(socket).await {
        return Err(NOT_RUNNING.to_string());
    }
    daemon_client::lock(socket).await?;
    println!("PA is locked. Run `aivyx-pa unlock` to open it again.");
    Ok(())
}

// ---------------------------------------------------------------------------
// card list / add / remove
// ---------------------------------------------------------------------------

fn card_file_path() -> Result<std::path::PathBuf, String> {
    aivyx_instance::InstancePaths::current()
        .map_err(|e| e.to_string())?
        .card_unlock_file()
        .ok_or_else(|| "can't resolve the instance's config folder (is HOME set?)".to_string())
}

fn load_file(path: &Path) -> Result<CardUnlockFile, String> {
    Ok(CardUnlockFile::load(path)
        .map_err(|e| e.to_string())?
        .unwrap_or_default())
}

pub async fn run_card(sub: CardSubcommand, socket: &Path) -> Result<(), String> {
    match sub {
        CardSubcommand::List => run_list(),
        CardSubcommand::Remove { serial } => run_remove(&serial),
        CardSubcommand::Add { card, label } => run_add(card, label, socket).await,
    }
}

fn run_list() -> Result<(), String> {
    let file = load_file(&card_file_path()?)?;
    if file.cards.is_empty() {
        println!("No cards enrolled. Add one with `aivyx-pa card add`.");
        return Ok(());
    }
    let plugged: Result<Vec<String>, String> = ProcessHelper::find()
        .and_then(|h| h.list())
        .map(|cards| cards.into_iter().map(|c| c.serial).collect())
        .map_err(|e| match e {
            HelperError::NotInstalled => "unknown (helper not installed)".to_string(),
            other => format!("unknown ({other})"),
        });
    for c in &file.cards {
        let added = c.added.split('T').next().unwrap_or(&c.added);
        let here = match &plugged {
            Ok(list) if list.contains(&c.serial) => "yes".to_string(),
            Ok(_) => "no".to_string(),
            Err(why) => why.clone(),
        };
        println!(
            "{}  {}  added {}  plugged in: {}",
            c.label, c.serial, added, here
        );
    }
    Ok(())
}

/// Where a saved passphrase lives, for the last-card guard, the
/// "remove it?" offer after `card add`, and `lock`'s re-check (A18). The
/// order is start-up's (`select_passphrase_source`): env, TOML, keyring,
/// daemon.env.
pub enum SavedPassphrase {
    /// `AIVYX_PA_PASSPHRASE` in this process's environment.
    Env,
    /// `[storage] passphrase` in this TOML file.
    Toml(std::path::PathBuf),
    Keyring,
    EnvFile(std::path::PathBuf),
}

impl SavedPassphrase {
    /// Where it is, in words.
    pub fn place(&self) -> String {
        match self {
            SavedPassphrase::Env => {
                format!("the {} environment variable", crate::DEFAULT_ENV_VAR)
            }
            SavedPassphrase::Toml(p) => format!("`[storage] passphrase` in {}", p.display()),
            SavedPassphrase::Keyring => "the OS keyring".to_string(),
            SavedPassphrase::EnvFile(p) => p.display().to_string(),
        }
    }

    /// How the operator removes it.
    pub fn how_to_remove(&self) -> String {
        match self {
            SavedPassphrase::Env => format!(
                "remove {} from the service's environment",
                crate::DEFAULT_ENV_VAR
            ),
            SavedPassphrase::Toml(p) => {
                format!("delete the `passphrase` line under `[storage]` in {}", p.display())
            }
            SavedPassphrase::Keyring => "run `aivyx-pa keyring clear`".to_string(),
            SavedPassphrase::EnvFile(p) => format!("delete {}", p.display()),
        }
    }
}

/// The saved passphrase source a start-up would use now, read afresh (the
/// config is reloaded). `Err` when the config can't be read.
pub fn saved_passphrase_checked() -> Result<Option<SavedPassphrase>, String> {
    let toml_path = aivyx_config::resolve_config_path();
    let cfg = aivyx_config::AivyxConfig::load_from_env_and_toml(&aivyx_config::LoadOptions {
        toml_path: Some(toml_path.clone()),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    })
    .map_err(|e| format!("failed to load the config: {e}"))?;
    if let Some(secret) = &cfg.passphrase {
        return Ok(Some(match secret.source {
            aivyx_config::FieldSource::Env => SavedPassphrase::Env,
            _ => SavedPassphrase::Toml(toml_path),
        }));
    }
    if matches!(aivyx_channel::keyring_store::retrieve(), Ok(Some(_))) {
        return Ok(Some(SavedPassphrase::Keyring));
    }
    let Ok(path) = crate::daemon_service::passphrase_file_path() else {
        return Ok(None);
    };
    Ok(matches!(
        crate::daemon_service::read_passphrase_file(&path),
        Ok(Some(_))
    )
    .then_some(SavedPassphrase::EnvFile(path)))
}

/// Like [`saved_passphrase_checked`]; a config that can't be read counts as
/// no saved passphrase (so the last-card guard still asks).
fn saved_passphrase() -> Option<SavedPassphrase> {
    saved_passphrase_checked().ok().flatten()
}

fn run_remove(serial: &str) -> Result<(), String> {
    let path = card_file_path()?;
    let mut file = load_file(&path)?;
    if !file.cards.iter().any(|c| c.serial == serial) {
        return Err(format!("card {serial} isn't enrolled"));
    }
    let left = file.cards.len() - 1;
    if removal_needs_confirmation(left, saved_passphrase().is_some()) {
        println!(
            "This is your last card and no passphrase is saved on this machine: \
             without the card you'll need your passphrase to unlock PA."
        );
        let typed = ask_raw_line(&format!("Type \"{LAST_CARD_PHRASE}\" to remove it: "))?;
        if !is_last_card_phrase(&typed) {
            return Err("not removed".to_string());
        }
    }
    // No unlock needed: removing an entry can't open anything, and the file
    // is the operator's own.
    let entry = remove(&mut file, serial).map_err(|e| e.to_string())?;
    file.save(&path).map_err(|e| e.to_string())?;
    println!("Removed {} ({}).", entry.label, entry.serial);
    Ok(())
}

/// The card to enroll: `--card`, the only card plugged in, or the operator's pick.
fn pick_card(helper: &dyn CardHelper, wanted: Option<String>) -> Result<(String, bool), String> {
    let cards = helper.list().map_err(|e| e.to_string())?;
    if let Some(w) = wanted {
        return cards
            .into_iter()
            .find(|c| c.serial == w)
            .map(|c| (c.serial, c.has_decryption_key))
            .ok_or_else(|| format!("card {w} isn't plugged in"));
    }
    match cards.len() {
        0 => Err("no YubiKey found — plug one in".to_string()),
        1 => Ok((cards[0].serial.clone(), cards[0].has_decryption_key)),
        _ => {
            for (i, c) in cards.iter().enumerate() {
                println!("  {}) {}", i + 1, c.serial);
            }
            let n: usize = ask_line("Which card? ")?
                .parse()
                .map_err(|_| "enter the number of a card".to_string())?;
            cards
                .get(n.wrapping_sub(1))
                .map(|c| (c.serial.clone(), c.has_decryption_key))
                .ok_or_else(|| "enter the number of a card".to_string())
        }
    }
}

async fn run_add(card: Option<String>, label: Option<String>, socket: &Path) -> Result<(), String> {
    let helper = ProcessHelper::find().map_err(|e| e.to_string())?;
    let (serial, has_key) = pick_card(&helper, card)?;

    let running = daemon_client::daemon_is_running(socket).await;
    if running {
        let (locked, _) = daemon_client::lock_state(socket)
            .await
            .map_err(|e| e.to_string())?;
        if locked {
            return Err(
                "Unlock PA first (`aivyx-pa unlock --passphrase`), then add the card.".to_string(),
            );
        }
    }

    // `has_decryption_key` is also false for a key PA can't use (not
    // X25519): ask the card before offering to make one, so the admin PIN
    // is never asked for a slot the helper would refuse.
    let has_key = has_key || slot_has_usable_key(helper.public_key(&serial))?;

    let mut generate = false;
    let mut admin_pin: Option<String> = None;
    if !has_key {
        if !ask_yes("This card has no decryption key. Make one on the card? [y/N] ")? {
            return Err("this card has no decryption key to use".to_string());
        }
        generate = true;
        admin_pin = Some(secret("Admin PIN: ")?);
    }
    let label = label
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| serial.clone());
    let pin = secret("YubiKey PIN: ")?;

    if running {
        println!("Touch your YubiKey…");
        daemon_client::enroll_card(socket, serial.clone(), label, pin, generate, admin_pin).await?;
    } else {
        // `store_opens_with` drives its own runtime, so run off this one's thread.
        let serial = serial.clone();
        tokio::task::spawn_blocking(move || {
            enroll_offline(&helper, &serial, &label, &pin, generate, admin_pin)
        })
        .await
        .map_err(|e| format!("enroll task failed: {e}"))??;
    }
    println!("PA can now be unlocked with this card.");
    offer_to_remove_saved_passphrase()
}

/// No daemon: derive the master key from the typed passphrase, check it opens
/// the store, then wrap it for the card.
fn enroll_offline(
    helper: &ProcessHelper,
    serial: &str,
    label: &str,
    pin: &str,
    generate: bool,
    admin_pin: Option<String>,
) -> Result<(), String> {
    use aivyx_channel::passphrase::{PassphraseSource, derive_master_key};
    use aivyx_crypto::Argon2Params;

    let cfg = aivyx_config::AivyxConfig::load_from_env_and_toml(&aivyx_config::LoadOptions {
        toml_path: Some(aivyx_config::resolve_config_path()),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    })
    .map_err(|e| format!("failed to load the config: {e}"))?;
    let store = cfg.storage_path.value.clone();
    let salt = crate::salt_path_for(&store);

    let typed = secret("Passphrase: ")?;
    let master = derive_master_key(
        PassphraseSource::FromConfig(SecretString::from(typed)),
        &salt,
        Argon2Params::d7_default(),
    )
    .map_err(|e| format!("failed to derive the key: {e}"))?;
    if !store_opens_with(&store, &master) {
        return Err("that isn't the passphrase for this PA's store".to_string());
    }

    println!("Touch your YubiKey…");
    let card_public = if generate {
        let admin = SecretString::from(admin_pin.ok_or("an admin PIN is needed to make a key")?);
        helper
            .generate(serial, &admin)
            .map_err(describe_helper_error)?
    } else {
        helper
            .public_key(serial)
            .map_err(describe_helper_error)?
            .ok_or("this card has no decryption key")?
    };
    let path = card_file_path()?;
    let mut file = load_file(&path)?;
    let instance = crate::current_instance_name();
    enroll(
        &mut file,
        &master,
        instance.as_str(),
        serial,
        label,
        card_public,
        helper,
        &SecretString::from(pin.to_string()),
    )
    .map_err(|e| e.to_string())?;
    file.save(&path).map_err(|e| e.to_string())
}

/// From the card's `public_key` answer: does its decryption slot hold a
/// usable key (`true`), or is it empty (`false`)? A key of another type
/// stops `card add` here.
fn slot_has_usable_key(answer: Result<Option<[u8; 32]>, HelperError>) -> Result<bool, String> {
    match answer {
        Ok(key) => Ok(key.is_some()),
        Err(HelperError::Card {
            kind: ErrorKind::UnsupportedKey,
            ..
        }) => Err("This card's decryption key isn't X25519, so PA can't use it. \
             Nothing was changed."
            .to_string()),
        Err(e) => Err(describe_helper_error(e)),
    }
}

fn describe_helper_error(e: HelperError) -> String {
    match e {
        HelperError::Card {
            kind: ErrorKind::UnsupportedKey,
            message,
            ..
        } => {
            format!("{message}\nThis card's key type isn't supported; nothing was changed.")
        }
        other => other.to_string(),
    }
}

fn offer_to_remove_saved_passphrase() -> Result<(), String> {
    let Some(saved) = saved_passphrase() else {
        return Ok(());
    };
    // The env var and the TOML line are the operator's own settings: say
    // what they do and how to remove them, but don't edit them.
    if let Some(note) = kept_passphrase_note(&saved) {
        println!("{note}");
        return Ok(());
    }
    if !ask_yes(&format!(
        "Remove the saved passphrase from {}? [y/N] ",
        saved.place()
    ))? {
        return Ok(());
    }
    match saved {
        SavedPassphrase::Keyring => aivyx_channel::keyring_store::clear()
            .map_err(|e| format!("could not clear the OS keyring: {e}"))?,
        SavedPassphrase::EnvFile(p) => std::fs::remove_file(&p)
            .map_err(|e| format!("could not remove {}: {e}", p.display()))?,
        SavedPassphrase::Env | SavedPassphrase::Toml(_) => {}
    }
    println!("Removed the saved passphrase.");
    Ok(())
}

/// For a passphrase PA doesn't remove itself (env var, TOML): what it does
/// and how to remove it. `None` for the keyring and daemon.env.
fn kept_passphrase_note(saved: &SavedPassphrase) -> Option<String> {
    match saved {
        SavedPassphrase::Env | SavedPassphrase::Toml(_) => Some(format!(
            "A passphrase is still set in {}: it keeps the service starting unlocked. \
             To start locked and use the card, {}.",
            saved.place(),
            saved.how_to_remove()
        )),
        SavedPassphrase::Keyring | SavedPassphrase::EnvFile(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_non_x25519_key_stops_card_add_before_the_admin_pin() {
        assert_eq!(slot_has_usable_key(Ok(None)), Ok(false));
        assert_eq!(slot_has_usable_key(Ok(Some([1u8; 32]))), Ok(true));
        let unsupported = HelperError::Card {
            kind: ErrorKind::UnsupportedKey,
            message: "RSA key".into(),
            tries_left: None,
        };
        let err = slot_has_usable_key(Err(unsupported)).unwrap_err();
        assert!(err.contains("isn't X25519"), "{err}");
    }

    #[test]
    fn an_env_or_toml_passphrase_is_explained_not_removed() {
        let env = kept_passphrase_note(&SavedPassphrase::Env).unwrap();
        assert!(env.contains("AIVYX_PA_PASSPHRASE"), "{env}");
        assert!(env.contains("starting unlocked"), "{env}");
        let toml = kept_passphrase_note(&SavedPassphrase::Toml("/c/aivyx-pa.toml".into())).unwrap();
        assert!(toml.contains("[storage]") && toml.contains("/c/aivyx-pa.toml"), "{toml}");
        assert!(kept_passphrase_note(&SavedPassphrase::Keyring).is_none());
        assert!(kept_passphrase_note(&SavedPassphrase::EnvFile("/d.env".into())).is_none());
    }

    #[test]
    fn the_last_card_needs_the_phrase_only_without_a_saved_passphrase() {
        assert!(removal_needs_confirmation(0, false));
        assert!(!removal_needs_confirmation(0, true));
        assert!(!removal_needs_confirmation(1, false));
    }

    #[test]
    fn unlock_failures_read_plainly() {
        assert_eq!(
            unlock_failure_text("wrong_pin", "PIN incorrect", Some(2)),
            "Wrong PIN — 2 tries left."
        );
        assert_eq!(
            unlock_failure_text("wrong_pin", "", Some(1)),
            "Wrong PIN — 1 try left."
        );
        assert_eq!(
            unlock_failure_text("touch_timeout", "", None),
            "No touch — still locked."
        );
        assert!(
            unlock_failure_text("helper_missing", "card unlock needs …", None)
                .contains("tool-programs install")
        );
    }

    #[test]
    fn no_card_and_not_enrolled_keep_the_daemon_message() {
        let msg = "None of your enrolled cards is plugged in (enrolled: laptop (S1))";
        assert!(unlock_failure_text("no_card", msg, None).contains("laptop (S1)"));
        let msg = "no card reader found — is pcscd running?";
        assert!(unlock_failure_text("no_card", msg, None).contains("pcscd"));
        assert_eq!(
            unlock_failure_text("no_card", "", None),
            "No enrolled card is plugged in."
        );
        assert_eq!(
            unlock_failure_text("not_enrolled", "card X is not enrolled", None),
            "card X is not enrolled"
        );
        assert!(unlock_failure_text("not_enrolled", "", None).contains("card list"));
    }

    #[test]
    fn backoff_returns_the_daemon_message_alone() {
        let msg = "Too many failed unlocks — try again in 30 s";
        assert_eq!(unlock_failure_text("backoff", msg, None), msg);
        assert_eq!(
            unlock_failure_text("backoff", "", None),
            "Too many failed unlocks — try again later."
        );
    }

    #[test]
    fn last_card_phrase_strips_only_the_line_ending() {
        assert!(is_last_card_phrase("I know my passphrase\n"));
        assert!(is_last_card_phrase("I know my passphrase\r\n"));
        assert!(is_last_card_phrase("I know my passphrase"));
        assert!(!is_last_card_phrase(" I know my passphrase\n"));
        assert!(!is_last_card_phrase("I know my passphrase \n"));
        assert!(!is_last_card_phrase("i know my passphrase\n"));
    }
}
