//! `aivyx-pa connect proton`: pin Proton Mail Bridge's certificate, test
//! its login, and store both in the instance's
//! `tool-processes/proton/credentials.toml` (`0600`).

use std::io::{BufRead, Write};

use aivyx_config::AivyxConfig;
use aivyx_proton::credentials::{self, BridgeCredentials};
use aivyx_proton::tls;

pub async fn run_connect(config: &AivyxConfig) -> Result<(), String> {
    let p = config.proton.as_ref().ok_or(
        "no [proton] section in your config. Add own_addresses (and channel_address for \
         the email channel) first — see `docs/guide/13-chat-apps-and-accounts.md`.",
    )?;
    let instance = aivyx_instance::InstancePaths::current().map_err(|e| e.to_string())?;
    let path = credentials::path_for(&instance)
        .ok_or("can't resolve the instance's folders (is HOME set?)")?;

    eprintln!(
        "Connecting to Proton Mail Bridge at {} (IMAP) and {} (SMTP).",
        p.imap_addr, p.smtp_addr
    );
    let (imap_pin, smtp_pin) = aivyx_proton::bridge::capture_pins(&p.imap_addr, &p.smtp_addr)
        .await
        .map_err(|e| format!("{e} — is Proton Mail Bridge running and signed in?"))?;
    eprintln!(
        "Bridge's certificates (SHA-256):\n  IMAP {}\n  SMTP {}\nThese are pinned: from now \
         on only this Bridge is trusted. If you reinstall Bridge, run this again.",
        tls::pin_to_hex(&imap_pin),
        tls::pin_to_hex(&smtp_pin)
    );

    eprint!("Bridge username (Bridge → your account → mailbox details): ");
    let _ = std::io::stderr().flush();
    let mut username = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut username)
        .map_err(|e| format!("read username: {e}"))?;
    let username = username.trim().to_string();
    if username.is_empty() {
        return Err("no username given".into());
    }
    let password = rpassword::prompt_password(
        "Bridge password (the one Bridge shows you, not your Proton password): ",
    )
    .map_err(|e| format!("read password: {e}"))?;

    let creds = BridgeCredentials {
        username,
        password,
        imap_addr: p.imap_addr.clone(),
        smtp_addr: p.smtp_addr.clone(),
        imap_pin: tls::pin_to_hex(&imap_pin),
        smtp_pin: tls::pin_to_hex(&smtp_pin),
        from_address: p.own_addresses[0].clone(),
    };
    creds
        .transport(&p.folder)?
        .check_login()
        .await
        .map_err(|e| format!("Bridge didn't accept that login: {e}"))?;
    credentials::save(&path, &creds)?;
    eprintln!("Connected. Saved to {} (readable only by you).", path.display());
    eprintln!(
        "Next: `aivyx-pa --channel proton` for the email channel. For the proton.* \
         tools, add to aivyx-pa.toml and restart the daemon:\n\n[[tool_process]]\n\
         name = \"proton\"\ncommand = \"aivyx-proton\"\n"
    );
    Ok(())
}
