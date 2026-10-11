//! `aivyx-pa connect matrix` and `aivyx-pa matrix verify`.

use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::Arc;

use aivyx_config::AivyxConfig;
use aivyx_matrix::credentials::{self, MatrixKeys};

fn matrix_config(config: &AivyxConfig) -> Result<&aivyx_config::MatrixConfig, String> {
    config.matrix.as_ref().ok_or_else(|| {
        "no [matrix] section in your config. Add homeserver, user_id, room_filter \
         and allowed_senders first — see `docs/guide/13-chat-apps-and-accounts.md`."
            .to_string()
    })
}

/// Logs the assistant in once and seals the session. The password is read
/// without echo and never written anywhere.
pub async fn run_connect(config: &AivyxConfig, keys: &MatrixKeys, dir: &Path) -> Result<(), String> {
    let m = matrix_config(config)?;
    eprintln!("Logging in to {} as {}.", m.homeserver, m.user_id);
    let password = rpassword::prompt_password("The assistant's Matrix password (not stored): ")
        .map_err(|e| format!("read password: {e}"))?;
    let (session, archived) =
        aivyx_matrix::sdk::login(&m.homeserver, dir, &keys.store_passphrase(), &m.user_id, &password).await?;
    credentials::save(keys, dir, &session)?;
    if let Some(old) = archived {
        eprintln!(
            "Your previous Matrix device's store was moved to {} (not deleted). \
             The new device must be verified again: run `aivyx-pa matrix verify`.",
            old.display()
        );
    }
    eprintln!(
        "Connected: device {} for {}. Next: `aivyx-pa matrix verify`, then \
         `aivyx-pa --channel matrix`.",
        session.device_id, session.user_id
    );
    eprintln!(
        "If the daemon is already running the Matrix channel, restart it \
         (`aivyx-pa daemon stop`, then start it again with `aivyx-pa daemon run`) \
         so it uses the new device."
    );
    Ok(())
}

/// Waits for the operator to start verification from their Matrix app,
/// shows the emoji, and confirms only if they say they match.
pub async fn run_verify(config: &AivyxConfig, keys: &MatrixKeys, dir: &Path) -> Result<(), String> {
    let m = matrix_config(config)?;
    let session = credentials::load(keys, dir)?
        .ok_or("not connected yet — run `aivyx-pa connect matrix` first")?;
    eprintln!(
        "Waiting for a verification request. In your Matrix app, open the \
         assistant ({}) and choose \"Verify\". Ctrl-C to stop.",
        m.user_id
    );
    let confirm: aivyx_matrix::verify::Confirm = Arc::new(|emoji: String| {
        eprintln!("\nDo these match what your Matrix app shows?\n\n  {emoji}\n");
        eprint!("Type yes if they match: ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        let _ = std::io::stdin().lock().read_line(&mut line);
        line.trim().eq_ignore_ascii_case("yes")
    });
    let ok = aivyx_matrix::verify::run(
        &session,
        dir,
        &keys.store_passphrase(),
        m.allowed_senders.clone(),
        confirm,
    )
    .await?;
    if ok {
        eprintln!("Verified. The assistant now answers that device.");
        Ok(())
    } else {
        Err("verification didn't complete (cancelled or the emoji didn't match)".into())
    }
}
