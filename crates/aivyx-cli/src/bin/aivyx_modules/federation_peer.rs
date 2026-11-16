//! A19 (FED.3, preview) — `aivyx-pa federation card | peer add/list/remove |
//! ping | send`, and the daemon's start-up loading of the federation
//! identity, transport key and peers.
//!
//! None of these commands talk to the daemon. `card` and `ping` need only
//! the master key (the sealed key files under `federation_dir()`), so they
//! work while a daemon runs; the first `card`, which creates the keys, checks
//! the passphrase against the store first. `peer add|list|remove` touch only
//! the config file. `send` writes its outbound crossing to the audit chain,
//! so it opens the store and refuses while a daemon holds it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use aivyx_audit::PersistentAuditLog;
use aivyx_channel::federation_server::{FederationPeer, load_peers};
use aivyx_config::config_write::{
    FederationPeerWrite, read_federation_peers, remove_federation_peer, set_federation_id,
    write_federation_peer,
};
use aivyx_config::{AivyxConfig, FederationConfig, FederationVerb, LoadOptions};
use aivyx_crypto::{Argon2Params, MasterKey};
use aivyx_federation::identity::{Identity, validate_instance_id};
use aivyx_federation_net::{Link, NetError, PeerCard, TransportKey, fingerprint};
use aivyx_storage::{RedbStorage, Storage, StorageConfig};

/// The sealed Ed25519 identity key, under `federation_dir()`.
pub const IDENTITY_FILE: &str = "identity.key";
/// The sealed X25519 transport key, under `federation_dir()`.
pub const TRANSPORT_FILE: &str = "transport.key";
/// `peer add` defaults (match the config loader's).
const DEFAULT_VERBS: &[&str] = &["chat"];
const DEFAULT_CEILING: &str = "manual";
const DEFAULT_TASK_TIMEOUT_SECS: u64 = 900;
/// How long `ping` waits for the empty reply.
const PING_REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// One `aivyx-pa federation …` command (other than `yubikey-init`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Card,
    PeerAdd(PeerAddArgs),
    PeerList,
    PeerRemove { id: String },
    Ping { peer: String },
    Send { peer: String, verb: FederationVerb, text: String },
}

/// `peer add`'s arguments, as typed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PeerAddArgs {
    pub id: String,
    pub card: String,
    pub address: String,
    pub verbs: Option<Vec<String>>,
    pub scopes: Vec<String>,
    pub ceiling: Option<String>,
    pub task_timeout_secs: Option<u64>,
}

const PEER_ADD_USAGE: &str = "Usage: `aivyx-pa federation peer add <id> <card> --address <host:port> \
     [--verbs chat,task,search] [--scopes fs.read,shell.exec] [--ceiling manual|assisted] \
     [--task-timeout-secs N]`";

fn list(v: &str) -> Vec<String> {
    v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

/// Parse `args` (everything after `federation`) for the commands this
/// module owns. `args[0]` is `card`, `peer`, `ping` or `send`.
pub fn parse(args: &[String]) -> Result<Command, String> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["card"] => Ok(Command::Card),
        ["card", extra, ..] => Err(format!("`aivyx-pa federation card` takes no arguments. Got: `{extra}`")),
        ["peer", "list"] => Ok(Command::PeerList),
        ["peer", "remove", id] => Ok(Command::PeerRemove { id: id.to_string() }),
        ["peer", "remove", ..] => Err("Usage: `aivyx-pa federation peer remove <id>`".to_string()),
        ["peer", "add", rest @ ..] => parse_peer_add(rest).map(Command::PeerAdd),
        ["peer", other, ..] => Err(format!(
            "unrecognized federation peer subcommand: `{other}` (add, list or remove)"
        )),
        ["peer"] => Err("`aivyx-pa federation peer` needs add, list or remove".to_string()),
        ["ping", peer] => Ok(Command::Ping { peer: peer.to_string() }),
        ["ping", ..] => Err("Usage: `aivyx-pa federation ping <peer>`".to_string()),
        ["send", peer, verb, text] => {
            let verb = FederationVerb::from_wire(verb)
                .ok_or_else(|| format!("unknown verb `{verb}` (chat, task or search)"))?;
            Ok(Command::Send { peer: peer.to_string(), verb, text: text.to_string() })
        }
        ["send", ..] => Err(
            "Usage: `aivyx-pa federation send <peer> <chat|task|search> \"<text>\"`".to_string(),
        ),
        _ => Err("unrecognized federation subcommand".to_string()),
    }
}

fn parse_peer_add(rest: &[&str]) -> Result<PeerAddArgs, String> {
    let (Some(id), Some(card)) = (rest.first(), rest.get(1)) else {
        return Err(PEER_ADD_USAGE.to_string());
    };
    let mut out = PeerAddArgs { id: id.to_string(), card: card.to_string(), ..PeerAddArgs::default() };
    let mut address = None;
    let mut i = 2;
    while i < rest.len() {
        let flag = rest[i];
        let value = rest.get(i + 1).ok_or_else(|| format!("`{flag}` needs a value. {PEER_ADD_USAGE}"))?;
        let once = |set: bool| if set { Err(format!("`{flag}` given twice")) } else { Ok(()) };
        match flag {
            "--address" => {
                once(address.is_some())?;
                address = Some(value.to_string());
            }
            "--verbs" => {
                once(out.verbs.is_some())?;
                out.verbs = Some(list(value));
            }
            "--scopes" => {
                once(!out.scopes.is_empty())?;
                out.scopes = list(value);
            }
            "--ceiling" => {
                once(out.ceiling.is_some())?;
                out.ceiling = Some(value.to_string());
            }
            "--task-timeout-secs" => {
                once(out.task_timeout_secs.is_some())?;
                out.task_timeout_secs =
                    Some(value.parse().map_err(|_| format!("`--task-timeout-secs` needs a number, got `{value}`"))?);
            }
            other => return Err(format!("unexpected argument `{other}`. {PEER_ADD_USAGE}")),
        }
        i += 2;
    }
    out.address = address.ok_or_else(|| format!("`--address <host:port>` is required. {PEER_ADD_USAGE}"))?;
    Ok(out)
}

/// Run one command. Blocking; builds its own runtime where it needs one.
pub fn run(cmd: Command) -> Result<(), String> {
    let toml = aivyx_config::resolve_config_path();
    match cmd {
        Command::Card => run_card(&toml),
        Command::PeerAdd(args) => run_peer_add(&toml, &args),
        Command::PeerList => run_peer_list(&toml),
        Command::PeerRemove { id } => run_peer_remove(&toml, &id),
        Command::Ping { peer } => run_ping(&peer),
        Command::Send { peer, verb, text } => run_send(&peer, verb, &text),
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn load_config() -> Result<AivyxConfig, String> {
    AivyxConfig::load_from_env_and_toml(&LoadOptions {
        toml_path: Some(aivyx_config::resolve_config_path()),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    })
    .map_err(|e| format!("failed to load the config: {e}"))
}

fn federation_dir() -> Result<PathBuf, String> {
    aivyx_instance::InstancePaths::current()
        .map_err(|e| e.to_string())?
        .federation_dir()
        .ok_or_else(|| "can't resolve the instance's data folder (is HOME set?)".to_string())
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("failed to build tokio runtime: {e}"))
}

/// The configured `[federation]` section, or how to make one.
fn federation(cfg: &AivyxConfig) -> Result<&FederationConfig, String> {
    cfg.federation
        .as_ref()
        .ok_or_else(|| "no `[federation]` section yet: run `aivyx-pa federation card` first".to_string())
}

fn find_peer(cfg: &FederationConfig, id: &str) -> Result<FederationPeer, String> {
    let peers = load_peers(cfg)?;
    let ids: Vec<String> = peers.iter().map(|p| p.id.clone()).collect();
    peers.into_iter().find(|p| p.id == id).ok_or_else(|| {
        if ids.is_empty() {
            format!("no peer `{id}`: no peers are configured (`aivyx-pa federation peer add`)")
        } else {
            format!("no peer `{id}` (configured: {})", ids.join(", "))
        }
    })
}

/// The master key from the same sources a start-up uses: the environment,
/// `[storage] passphrase`, the OS keyring, `daemon.env`, then a prompt.
/// Needs an existing store (its salt). Not checked here: unsealing an
/// existing key file is the check, and [`check_store_key`] is the check
/// before any new key file is sealed.
fn master_key(cfg: &AivyxConfig) -> Result<(MasterKey, PathBuf), String> {
    let store = cfg.storage_path.value.clone();
    if !store.exists() {
        return Err(format!("no PA store at {}: set PA up first with `aivyx-pa init`", store.display()));
    }
    let source = crate::select_passphrase_source(cfg.passphrase.as_ref(), false)?;
    let key = aivyx_channel::passphrase::derive_master_key(
        source,
        &crate::salt_path_for(&store),
        Argon2Params::d7_default(),
    )
    .map_err(|e| format!("failed to derive the key: {e}"))?;
    Ok((key, store))
}

/// What a store-open error means for these commands.
fn store_open_error(store: &Path, e: &str) -> String {
    if e.contains("acquire lock") || e.contains("already open") {
        "the store is in use: stop the daemon first (`aivyx-pa daemon stop`), or close the \
         running PA session, then run this again"
            .to_string()
    } else {
        format!("failed to open the store at {}: {e}", store.display())
    }
}

/// Does `key` open the store? Opens and closes it (refuses, with a clear
/// message, while a daemon holds it). Blocking: call outside any runtime.
fn check_store_key(store: &Path, key: &MasterKey) -> Result<(), String> {
    let rt = runtime()?;
    rt.block_on(async {
        let storage = RedbStorage::open(StorageConfig::new(store.to_path_buf()), key.clone())
            .await
            .map_err(|e| store_open_error(store, &e.to_string()))?;
        let opens = PersistentAuditLog::key_opens_store(&storage)
            .await
            .map_err(|e| format!("can't check the key: {e}"))?;
        drop(storage);
        if opens { Ok(()) } else { Err("that isn't the passphrase for this PA's store".to_string()) }
    })
}

fn wrong_key(e: impl std::fmt::Display) -> String {
    format!("couldn't open this instance's federation keys ({e}); is it the right passphrase?")
}

/// Load the existing identity and transport key. `None` when either file
/// is missing (run `card` first).
fn existing_keys(id: &str, dir: &Path, master: &MasterKey) -> Result<Option<(Identity, TransportKey)>, String> {
    let (id_path, key_path) = (dir.join(IDENTITY_FILE), dir.join(TRANSPORT_FILE));
    if !id_path.exists() || !key_path.exists() {
        return Ok(None);
    }
    let identity = Identity::load_or_generate(id.to_string(), &id_path, master).map_err(wrong_key)?;
    let key = TransportKey::load_or_generate(&key_path, master).map_err(wrong_key)?;
    Ok(Some((identity, key)))
}

const NO_KEYS: &str = "this instance has no federation keys yet: run `aivyx-pa federation card` first";

// ---------------------------------------------------------------------------
// Daemon start-up
// ---------------------------------------------------------------------------

/// What the daemon needs from `[federation]`, loaded after the unlock.
pub struct FederationStart {
    pub identity: Arc<Identity>,
    pub key: Arc<TransportKey>,
    pub peers: Arc<Vec<aivyx_channel::federation_server::FederationPeer>>,
    pub listen: Option<std::net::SocketAddr>,
}

/// Verify every peer's card (a bad one is a start-up error naming it), then
/// load or create the sealed identity and transport key. `master` has
/// already opened the store, so a new key file is sealed under the right key.
pub fn load_for_start(cfg: &FederationConfig, master: &MasterKey) -> Result<FederationStart, String> {
    let peers = load_peers(cfg)?;
    let dir = federation_dir()?;
    let identity = Identity::load_or_generate(cfg.id.clone(), &dir.join(IDENTITY_FILE), master)
        .map_err(|e| format!("federation identity: {e}"))?;
    let key = TransportKey::load_or_generate(&dir.join(TRANSPORT_FILE), master)
        .map_err(|e| format!("federation transport key: {e}"))?;
    Ok(FederationStart {
        identity: Arc::new(identity),
        key: Arc::new(key),
        peers: Arc::new(peers),
        listen: cfg.listen,
    })
}

/// The default agent's grants for the federation tools: one
/// `federation.relay:<id>` per configured peer (`peer add` is the grant).
pub fn relay_grants(peers: &[FederationPeer]) -> Vec<aivyx_capability::Scope> {
    peers
        .iter()
        .filter_map(|p| aivyx_capability::Scope::parse(&format!("federation.relay:{}", p.id)))
        .collect()
}

// ---------------------------------------------------------------------------
// card
// ---------------------------------------------------------------------------

fn ask_id(toml: &Path) -> Result<String, String> {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        return Err(format!(
            "this instance has no federation id yet: set `[federation] id` in {} (for example \
             `id = \"laptop\"`), then run this again",
            toml.display()
        ));
    }
    println!("This instance needs a federation id: the name its peers know it by.");
    print!("Federation id (letters, digits, `-` and `_`; e.g. laptop): ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| format!("could not read input: {e}"))?;
    let id = line.trim().to_string();
    validate_instance_id(&id).map_err(|e| e.to_string())?;
    Ok(id)
}

fn run_card(toml: &Path) -> Result<(), String> {
    let cfg = load_config()?;
    let new_id = match &cfg.federation {
        Some(_) => None,
        None => Some(ask_id(toml)?),
    };
    let id = new_id
        .clone()
        .or_else(|| cfg.federation.as_ref().map(|f| f.id.clone()))
        .expect("set above");
    let (master, store) = master_key(&cfg)?;
    let dir = federation_dir()?;
    let (identity, key) = match existing_keys(&id, &dir, &master)? {
        Some(keys) => keys,
        None => {
            // New key files are sealed under the master key: prove it first.
            check_store_key(&store, &master)?;
            let identity = Identity::load_or_generate(id.clone(), &dir.join(IDENTITY_FILE), &master)
                .map_err(|e| format!("federation identity: {e}"))?;
            let key = TransportKey::load_or_generate(&dir.join(TRANSPORT_FILE), &master)
                .map_err(|e| format!("federation transport key: {e}"))?;
            (identity, key)
        }
    };
    if let Some(id) = &new_id {
        set_federation_id(toml, id).map_err(|e| e.to_string())?;
        println!("Set `[federation] id = \"{id}\"` in {}.", toml.display());
    }
    let card = runtime()?
        .block_on(PeerCard::issue(&identity, &key))
        .map_err(|e| e.to_string())?;
    println!("This instance's peer card ({id}, key {}):", fingerprint(&key.public()));
    println!();
    println!("{}", card.encode());
    println!();
    println!("On the other machine, run:");
    println!("  aivyx-pa federation peer add {id} <this card> --address <this machine's host:port>");
    Ok(())
}

// ---------------------------------------------------------------------------
// peer add / list / remove
// ---------------------------------------------------------------------------

/// Check `args` (the card's binding, the card's id, the address) and fill
/// in the defaults. Pure: no config, no key.
pub fn peer_write(args: &PeerAddArgs) -> Result<(FederationPeerWrite, String), String> {
    let card = PeerCard::decode(&args.card).map_err(|e| format!("refusing the card: {e}"))?;
    let x25519 = card.verify().map_err(|e| format!("refusing the card: {e}"))?;
    if card.instance_id != args.id {
        return Err(format!(
            "refusing the card: it is for the instance `{}`, not `{}`",
            card.instance_id, args.id
        ));
    }
    let port_ok = args
        .address
        .rsplit_once(':')
        .is_some_and(|(host, port)| !host.is_empty() && port.parse::<u16>().is_ok_and(|p| p > 0));
    if !port_ok {
        return Err(format!("`--address` must be host:port, got `{}`", args.address));
    }
    let write = FederationPeerWrite {
        id: args.id.clone(),
        card: args.card.trim().to_string(),
        address: args.address.clone(),
        verbs: args
            .verbs
            .clone()
            .unwrap_or_else(|| DEFAULT_VERBS.iter().map(|s| s.to_string()).collect()),
        allowed_scopes: args.scopes.clone(),
        ceiling: args.ceiling.clone().unwrap_or_else(|| DEFAULT_CEILING.to_string()),
        task_timeout_secs: args.task_timeout_secs.unwrap_or(DEFAULT_TASK_TIMEOUT_SECS),
    };
    Ok((write, fingerprint(&x25519)))
}

fn run_peer_add(toml: &Path, args: &PeerAddArgs) -> Result<(), String> {
    let (write, fp) = peer_write(args)?;
    let replacing = read_federation_peers(toml)
        .map_err(|e| e.to_string())?
        .iter()
        .any(|p| p.id == write.id);
    write_federation_peer(toml, &write).map_err(|e| e.to_string())?;
    let verb = if replacing { "Replaced" } else { "Added" };
    println!("{verb} peer `{}` (key {fp}) at {}.", write.id, write.address);
    println!("  It may ask this instance for: {}", write.verbs.join(", "));
    if !write.allowed_scopes.is_empty() {
        println!("  Its tasks may use at most: {}", write.allowed_scopes.join(", "));
    }
    println!("Restart the daemon for the change to take effect.");
    Ok(())
}

fn run_peer_list(toml: &Path) -> Result<(), String> {
    let peers = read_federation_peers(toml).map_err(|e| e.to_string())?;
    if peers.is_empty() {
        println!("No federation peers. Add one with `aivyx-pa federation peer add`.");
        return Ok(());
    }
    for p in peers {
        let key = PeerCard::decode(&p.card)
            .and_then(|c| c.verify())
            .map(|x| format!("key {}", fingerprint(&x)))
            .unwrap_or_else(|_| "card does not verify".to_string());
        let scopes = if p.allowed_scopes.is_empty() { "-".to_string() } else { p.allowed_scopes.join(",") };
        println!(
            "{}  {}  verbs {}  scopes {}  ceiling {}  task timeout {}s  {}",
            p.id,
            p.address,
            p.verbs.join(","),
            scopes,
            p.ceiling,
            p.task_timeout_secs,
            key
        );
    }
    Ok(())
}

fn run_peer_remove(toml: &Path, id: &str) -> Result<(), String> {
    let peers = read_federation_peers(toml).map_err(|e| e.to_string())?;
    if !peers.iter().any(|p| p.id == id) {
        return Err(format!("no peer `{id}`"));
    }
    remove_federation_peer(toml, id).map_err(|e| e.to_string())?;
    println!("Removed peer `{id}`. Restart the daemon for the change to take effect.");
    Ok(())
}

// ---------------------------------------------------------------------------
// ping
// ---------------------------------------------------------------------------

/// The steps a ping checks, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PingStep {
    Connect,
    Handshake,
    KeyMatch,
    Accepted,
}

const PING_STEPS: [PingStep; 4] = [PingStep::Connect, PingStep::Handshake, PingStep::KeyMatch, PingStep::Accepted];

impl PingStep {
    fn label(self) -> &'static str {
        match self {
            PingStep::Connect => "connect",
            PingStep::Handshake => "handshake",
            PingStep::KeyMatch => "key matches the card",
            PingStep::Accepted => "the peer knows this instance",
        }
    }
}

/// Which step a failed `Link::connect` stopped at.
pub fn connect_failure_step(e: &NetError) -> PingStep {
    match e {
        NetError::Io(_) => PingStep::Connect,
        NetError::UnknownPeer { .. } => PingStep::KeyMatch,
        _ => PingStep::Handshake,
    }
}

/// One line per step: `ok` up to the failure, the failure, then `skipped`.
pub fn ping_report(failed: Option<(PingStep, &str)>) -> Vec<String> {
    let mut past = false;
    PING_STEPS
        .iter()
        .map(|step| {
            let status = match failed {
                _ if past => "skipped".to_string(),
                Some((at, why)) if at == *step => {
                    past = true;
                    format!("failed: {why}")
                }
                _ => "ok".to_string(),
            };
            format!("{} … {status}", step.label())
        })
        .collect()
}

/// Hints for the step that failed.
fn ping_hint(step: PingStep) -> &'static str {
    match step {
        PingStep::Connect => "is the peer's daemon running with `[federation] listen` set, and is the address right?",
        PingStep::Handshake => "something answered, but not a PA federation listener (or it timed out)",
        PingStep::KeyMatch => "the peer's key is not the one on its card: re-add the peer with its current card",
        PingStep::Accepted => "the peer closed the link: has it added this instance's card (`federation peer add`)?",
    }
}

async fn ping_steps(addr: &str, key: &TransportKey, peer_x25519: [u8; 32]) -> Option<(PingStep, String)> {
    let mut link = match Link::connect(addr, key, peer_x25519).await {
        Ok(link) => link,
        Err(e) => return Some((connect_failure_step(&e), e.to_string())),
    };
    if let Err(e) = link.send(&[]).await {
        return Some((PingStep::Accepted, e.to_string()));
    }
    match tokio::time::timeout(PING_REPLY_TIMEOUT, link.recv()).await {
        Ok(Ok(reply)) if reply.is_empty() => None,
        Ok(Ok(_)) => Some((PingStep::Accepted, "the reply was not an empty frame".into())),
        Ok(Err(e)) => Some((PingStep::Accepted, e.to_string())),
        Err(_) => Some((PingStep::Accepted, "no reply in time".into())),
    }
}

fn run_ping(peer_id: &str) -> Result<(), String> {
    let cfg = load_config()?;
    let fed = federation(&cfg)?;
    let peer = find_peer(fed, peer_id)?;
    let (master, _) = master_key(&cfg)?;
    let (_, key) = existing_keys(&fed.id, &federation_dir()?, &master)?.ok_or(NO_KEYS)?;
    println!("Pinging `{}` at {}", peer.id, peer.address);
    let failed = runtime()?.block_on(ping_steps(&peer.address, &key, peer.x25519));
    for line in ping_report(failed.as_ref().map(|(s, why)| (*s, why.as_str()))) {
        println!("  {line}");
    }
    match failed {
        None => Ok(()),
        Some((step, _)) => Err(format!("ping failed at `{}`: {}", step.label(), ping_hint(step))),
    }
}

// ---------------------------------------------------------------------------
// send
// ---------------------------------------------------------------------------

/// `send` while a daemon runs: the daemon holds the store, and an outbound
/// crossing must be audited.
pub const SEND_NEEDS_THE_STORE: &str = "stop the daemon or use the federation tools from a session";

fn run_send(peer_id: &str, verb: FederationVerb, text: &str) -> Result<(), String> {
    let rt = runtime()?;
    let socket = crate::default_socket_path()?;
    if rt.block_on(aivyx_channel::daemon_client::daemon_is_running(&socket)) {
        return Err(SEND_NEEDS_THE_STORE.to_string());
    }
    let cfg = load_config()?;
    let fed = federation(&cfg)?;
    let peer = find_peer(fed, peer_id)?;
    let (master, store) = master_key(&cfg)?;
    let (identity, key) = existing_keys(&fed.id, &federation_dir()?, &master)?.ok_or(NO_KEYS)?;
    let audit_key = crate::audit_chain_key_for(&master)?;
    let reply = rt.block_on(async {
        let storage: Arc<dyn Storage> = RedbStorage::open(StorageConfig::new(store.clone()), master)
            .await
            .map_err(|e| store_open_error(&store, &e.to_string()))?;
        let audit = PersistentAuditLog::open(storage, audit_key)
            .await
            .map_err(|e| format!("failed to open the audit log: {e}"))?;
        aivyx_channel::federation_tool::relay(&peer, &identity, &key, verb, text, &audit, None).await
    })?;
    println!("{reply}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    async fn card_for(id: &str) -> (PeerCard, TransportKey) {
        let identity = Identity::generate(id.into()).unwrap();
        let key = TransportKey::generate().unwrap();
        (PeerCard::issue(&identity, &key).await.unwrap(), key)
    }

    fn add(id: &str, card: &PeerCard) -> PeerAddArgs {
        PeerAddArgs {
            id: id.into(),
            card: card.encode(),
            address: "100.64.0.3:7650".into(),
            ..PeerAddArgs::default()
        }
    }

    #[tokio::test]
    async fn peer_add_refuses_a_card_whose_binding_fails() {
        let (mut card, _) = card_for("desktop").await;
        let (_, other) = card_for("desktop").await;
        // Swap in a transport key the identity never signed for.
        card.x25519_public = PeerCard::issue(&Identity::generate("desktop".into()).unwrap(), &other)
            .await
            .unwrap()
            .x25519_public;
        let err = peer_write(&add("desktop", &card)).unwrap_err();
        assert!(err.contains("refusing the card") && err.contains("binding"), "{err}");
    }

    #[tokio::test]
    async fn peer_add_refuses_a_card_for_a_different_id() {
        let (card, _) = card_for("desktop").await;
        let err = peer_write(&add("laptop", &card)).unwrap_err();
        assert!(err.contains("`desktop`") && err.contains("`laptop`"), "{err}");
    }

    #[tokio::test]
    async fn peer_add_writes_defaults() {
        let (card, key) = card_for("desktop").await;
        let (w, fp) = peer_write(&add("desktop", &card)).unwrap();
        assert_eq!(w.verbs, s(&["chat"]));
        assert_eq!(w.ceiling, "manual");
        assert_eq!(w.task_timeout_secs, 900);
        assert!(w.allowed_scopes.is_empty());
        assert_eq!(w.card, card.encode());
        assert_eq!(fp, fingerprint(&key.public()));
        // ...and writes them: the config loader reads the entry back.
        let dir = std::env::temp_dir().join(format!("aivyx-fed-cli-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let toml = dir.join("aivyx-pa.toml");
        set_federation_id(&toml, "laptop").unwrap();
        write_federation_peer(&toml, &w).unwrap();
        assert_eq!(read_federation_peers(&toml).unwrap(), vec![w]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn peer_add_refuses_an_address_without_a_port() {
        let (card, _) = card_for("desktop").await;
        let mut a = add("desktop", &card);
        a.address = "100.64.0.3".into();
        assert!(peer_write(&a).unwrap_err().contains("host:port"));
    }

    #[test]
    fn ping_report_names_the_failing_step() {
        assert_eq!(connect_failure_step(&NetError::Io("refused".into())), PingStep::Connect);
        assert_eq!(connect_failure_step(&NetError::Handshake("bad".into())), PingStep::Handshake);
        assert_eq!(connect_failure_step(&NetError::Timeout("handshake")), PingStep::Handshake);
        assert_eq!(
            connect_failure_step(&NetError::UnknownPeer { fingerprint: "ab".into() }),
            PingStep::KeyMatch
        );
        assert_eq!(
            ping_report(None),
            s(&["connect … ok", "handshake … ok", "key matches the card … ok", "the peer knows this instance … ok"])
        );
        assert_eq!(
            ping_report(Some((PingStep::Connect, "refused"))),
            s(&[
                "connect … failed: refused",
                "handshake … skipped",
                "key matches the card … skipped",
                "the peer knows this instance … skipped",
            ])
        );
        assert_eq!(
            ping_report(Some((PingStep::KeyMatch, "other key"))),
            s(&[
                "connect … ok",
                "handshake … ok",
                "key matches the card … failed: other key",
                "the peer knows this instance … skipped",
            ])
        );
    }

    #[tokio::test]
    async fn every_configured_peer_is_granted_its_relay_scope() {
        let (desk, _) = card_for("desktop").await;
        let (phone, _) = card_for("phone").await;
        let peer = |id: &str, card: &PeerCard| aivyx_config::FederationPeerConfig {
            id: id.into(),
            card: card.encode(),
            address: "h:1".into(),
            verbs: vec![FederationVerb::Chat],
            allowed_scopes: vec![],
            ceiling: aivyx_config::AutonomyLevel::Manual,
            task_timeout: Duration::from_secs(900),
        };
        let cfg = FederationConfig {
            id: "laptop".into(),
            listen: None,
            peers: vec![peer("desktop", &desk), peer("phone", &phone)],
        };
        let grants = relay_grants(&load_peers(&cfg).unwrap());
        let held = aivyx_capability::CapabilitySet::from_scopes(grants.clone());
        let need = |id: &str| aivyx_capability::Scope::parse(&format!("federation.relay:{id}")).unwrap();
        assert_eq!(grants.len(), 2);
        assert!(held.grants(&need("desktop")) && held.grants(&need("phone")));
        assert!(!held.grants(&need("kitchen")), "only configured peers");
        assert!(aivyx_capability::is_irreversible_base("federation.relay"), "assisted still confirms");
    }

    #[test]
    fn parse_reads_every_command() {
        assert_eq!(parse(&s(&["card"])).unwrap(), Command::Card);
        assert_eq!(parse(&s(&["peer", "list"])).unwrap(), Command::PeerList);
        assert_eq!(parse(&s(&["peer", "remove", "x"])).unwrap(), Command::PeerRemove { id: "x".into() });
        assert_eq!(parse(&s(&["ping", "x"])).unwrap(), Command::Ping { peer: "x".into() });
        assert_eq!(
            parse(&s(&["send", "x", "task", "do it"])).unwrap(),
            Command::Send { peer: "x".into(), verb: FederationVerb::Task, text: "do it".into() }
        );
        assert!(parse(&s(&["send", "x", "teleport", "t"])).unwrap_err().contains("unknown verb"));
        let add = parse(&s(&[
            "peer", "add", "desktop", "aivyx-peer-v1:x", "--address", "h:1", "--verbs", "chat,task",
            "--scopes", "fs.read", "--ceiling", "assisted", "--task-timeout-secs", "60",
        ]))
        .unwrap();
        assert_eq!(
            add,
            Command::PeerAdd(PeerAddArgs {
                id: "desktop".into(),
                card: "aivyx-peer-v1:x".into(),
                address: "h:1".into(),
                verbs: Some(s(&["chat", "task"])),
                scopes: s(&["fs.read"]),
                ceiling: Some("assisted".into()),
                task_timeout_secs: Some(60),
            })
        );
        assert!(parse(&s(&["peer", "add", "desktop", "c"])).unwrap_err().contains("--address"));
        assert!(parse(&s(&["peer", "add", "d", "c", "--address", "h:1", "--address", "h:2"]))
            .unwrap_err()
            .contains("twice"));
    }
}
