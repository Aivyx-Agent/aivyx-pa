//! `aivyx-pa connect signal` and `aivyx-pa signal verify`.

use std::io::{BufRead, Write};

use aivyx_config::{AivyxConfig, SignalConfig};
use aivyx_signal::toolconf::{self, ToolConfig};
use aivyx_signal::transport::{CliTransport, SignalTransport, TrustLevel};

fn signal_config(config: &AivyxConfig) -> Result<&SignalConfig, String> {
    config.signal.as_ref().ok_or_else(|| {
        "no [signal] section in your config. Add account and allowed_senders first — \
         see `docs/guide/13-chat-apps-and-accounts.md`."
            .to_string()
    })
}

fn transport() -> Result<(CliTransport, std::path::PathBuf), String> {
    let instance = aivyx_instance::InstancePaths::current().map_err(|e| e.to_string())?;
    let socket = toolconf::socket_path(&instance)
        .ok_or("can't resolve the instance's folders (is HOME set?)")?;
    Ok((CliTransport::new(socket.clone()), socket))
}

/// The signal-cli command line `connect signal` tells the operator to run.
fn daemon_command(account: &str, socket: &std::path::Path) -> String {
    format!(
        "signal-cli -a {account} --trust-new-identities never daemon \\\n  --socket {} \\\n  \
         --receive-mode manual --ignore-attachments",
        socket.display()
    )
}

/// Checks signal-cli answers on the instance's socket and writes the
/// tool's `send_to` (`signal-tool.toml`).
pub async fn run_connect(config: &AivyxConfig) -> Result<(), String> {
    let s = signal_config(config)?;
    let (t, socket) = transport()?;
    let version = t.version().await.map_err(|e| {
        format!(
            "{e}\n\nStart it (the guide has a systemd user unit) with:\n\n{}",
            daemon_command(&s.account, &socket)
        )
    })?;
    let instance = aivyx_instance::InstancePaths::current().map_err(|e| e.to_string())?;
    let conf = toolconf::tool_config_path(&instance)
        .ok_or("can't resolve the instance's folders (is HOME set?)")?;
    toolconf::save(&conf, &ToolConfig { send_to: s.send_to.clone() })?;
    eprintln!("Connected to signal-cli {version} at {}.", socket.display());
    eprintln!(
        "Next: `aivyx-pa signal verify`, then `aivyx-pa --channel signal`. For \
         signal.send, add to aivyx-pa.toml and restart the daemon:\n\n[[tool_process]]\n\
         name = \"signal\"\ncommand = \"aivyx-signal\"\n"
    );
    Ok(())
}

/// For each allowed sender and `send_to` contact: show the safety number,
/// and mark it verified if the operator says it matches their phone.
pub async fn run_verify(config: &AivyxConfig) -> Result<(), String> {
    let s = signal_config(config)?;
    let (t, _) = transport()?;
    let mut people: Vec<String> = s.allowed_senders.clone();
    for n in &s.send_to {
        if !people.contains(n) {
            people.push(n.clone());
        }
    }
    let mut verified = 0;
    for who in &people {
        match t.identity(who).await.map_err(|e| e.to_string())? {
            None => eprintln!(
                "{who}: no key yet. Send the assistant a message from that number (or try \
                 a signal.send to it), then run this again."
            ),
            Some(id) if id.trust == TrustLevel::TrustedVerified => {
                eprintln!("{who}: already verified.");
                verified += 1;
            }
            Some(id) => {
                eprintln!(
                    "\n{who}: safety number\n\n  {}\n\nOn that phone, open the chat with the \
                     assistant → View safety number.",
                    id.safety_number
                );
                eprint!("Is it the same? Type yes if so: ");
                let _ = std::io::stderr().flush();
                let mut line = String::new();
                let _ = std::io::stdin().lock().read_line(&mut line);
                if line.trim().eq_ignore_ascii_case("yes") {
                    t.trust_verified(who, &id.safety_number)
                        .await
                        .map_err(|e| e.to_string())?;
                    eprintln!("{who}: verified.");
                    verified += 1;
                } else {
                    eprintln!("{who}: not verified.");
                }
            }
        }
    }
    eprintln!("\n{verified} of {} verified.", people.len());
    Ok(())
}
