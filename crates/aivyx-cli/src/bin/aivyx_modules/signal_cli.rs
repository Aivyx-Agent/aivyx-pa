//! `aivyx-pa connect signal` and `aivyx-pa signal verify`.

use std::io::{BufRead, Write};

use aivyx_config::{AivyxConfig, SignalConfig};
use aivyx_signal::toolconf::{self, ToolConfig};
use aivyx_signal::transport::{CliTransport, SignalTransport, TransportError, TrustLevel};

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
    let stdin = std::io::stdin();
    let verified = verify_each(&t, &people, &mut stdin.lock(), &mut std::io::stderr()).await?;
    eprintln!("\n{verified} of {} verified.", people.len());
    Ok(())
}

/// Shows each of `people`'s safety number and marks it verified when the
/// operator types `yes` on `input`; returns how many are verified. A
/// lookup or trust error for one contact is reported and skipped; only
/// signal-cli being unreachable stops it.
async fn verify_each<T: SignalTransport + ?Sized>(
    t: &T,
    people: &[String],
    input: &mut impl BufRead,
    out: &mut impl Write,
) -> Result<usize, String> {
    let mut verified = 0;
    for who in people {
        let id = match t.identity(who).await {
            Ok(id) => id,
            Err(e @ TransportError::Unreachable(_)) => return Err(e.to_string()),
            Err(e) => {
                let _ = writeln!(out, "{who}: couldn't look up the key ({e})");
                continue;
            }
        };
        match id {
            None => {
                let _ = writeln!(
                    out,
                    "{who}: no key yet. Send the assistant a message from that number, \
                     then run this again."
                );
            }
            Some(id) if id.trust == TrustLevel::TrustedVerified => {
                let _ = writeln!(out, "{who}: already verified.");
                verified += 1;
            }
            Some(id) => {
                let _ = writeln!(
                    out,
                    "\n{who}: safety number\n\n  {}\n\nOn that phone, open the chat with the \
                     assistant → View safety number.",
                    id.safety_number
                );
                let _ = write!(out, "Is it the same? Type yes if so: ");
                let _ = out.flush();
                let mut line = String::new();
                let _ = input.read_line(&mut line);
                if !line.trim().eq_ignore_ascii_case("yes") {
                    let _ = writeln!(out, "{who}: not verified.");
                    continue;
                }
                match t.trust_verified(who, &id.safety_number).await {
                    Ok(()) => {
                        let _ = writeln!(out, "{who}: verified.");
                        verified += 1;
                    }
                    Err(e @ TransportError::Unreachable(_)) => return Err(e.to_string()),
                    Err(e) => {
                        let _ = writeln!(out, "{who}: couldn't mark it verified ({e})");
                    }
                }
            }
        }
    }
    Ok(verified)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_signal::transport::ScriptedTransport;

    const ME: &str = "+15550001111";
    const SAM: &str = "+15550002222";
    const ALEX: &str = "+15550003333";

    async fn verify(t: &ScriptedTransport, people: &[&str], typed: &str) -> (Result<usize, String>, String) {
        let people: Vec<String> = people.iter().map(|p| p.to_string()).collect();
        let mut out = Vec::new();
        let r = verify_each(t, &people, &mut typed.as_bytes(), &mut out).await;
        (r, String::from_utf8(out).unwrap())
    }

    #[tokio::test]
    async fn one_contacts_lookup_error_does_not_stop_the_rest() {
        let t = ScriptedTransport::new(vec![])
            .failing_identity(SAM, TransportError::Refused("Unregistered user".into()))
            .with_identity(ME, TrustLevel::TrustedUnverified, "11111")
            .with_identity(ALEX, TrustLevel::TrustedVerified, "33333");
        let (r, out) = verify(&t, &[SAM, ME, ALEX], "yes\n").await;
        assert_eq!(r.unwrap(), 2, "{out}");
        assert!(out.contains(&format!("{SAM}: couldn't look up the key")), "{out}");
        assert!(out.contains("Unregistered user"), "{out}");
        assert_eq!(t.trusted().await, vec![(ME.to_string(), "11111".to_string())]);
    }

    #[tokio::test]
    async fn signal_cli_unreachable_stops_it() {
        let t = ScriptedTransport::new(vec![])
            .failing_identity(ME, TransportError::Unreachable("no socket".into()));
        let (r, _) = verify(&t, &[ME, SAM], "").await;
        assert!(r.unwrap_err().contains("isn't reachable"));
    }

    #[tokio::test]
    async fn only_yes_verifies() {
        let t = ScriptedTransport::new(vec![])
            .with_identity(ME, TrustLevel::Untrusted, "99999");
        let (r, out) = verify(&t, &[ME, SAM], "no\n").await;
        assert_eq!(r.unwrap(), 0);
        assert!(out.contains(&format!("{ME}: not verified.")), "{out}");
        assert!(out.contains(&format!("{SAM}: no key yet")), "{out}");
        assert!(t.trusted().await.is_empty());
    }
}
