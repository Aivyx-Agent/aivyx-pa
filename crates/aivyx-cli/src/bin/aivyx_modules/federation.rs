//! `aivyx-pa federation` CLI surface — hardware-backed federation identity
//! provisioning (Chapter Passport / `docs/FEDERATION.md`, Task 8).
//!
//! This whole module is compiled only under the `yubikey` Cargo feature
//! (see `aivyx.rs`'s `mod federation;` declaration and its dispatch arm's
//! `#[cfg(not(feature = "yubikey"))]` fallback): both dependencies this
//! module names directly (`aivyx-yubi`, and `aivyx-federation` built with
//! its own `yubikey` feature) transitively need `libpcsclite` (via
//! `pcsc-sys`) at build time, which isn't installed in the default CI job
//! or on every contributor's machine — a plain `cargo build -p aivyx-cli`
//! must never require it. See `aivyx-cli`'s own `Cargo.toml` for the
//! feature wiring.
//!
//! # Provisioning flow (`run_yubikey_init`)
//!
//! 1. List every attached, OpenPGP-capable card
//!    (`aivyx_yubi::discovery::list_cards`) and print each one's serial
//!    and whether its Signature slot already holds a key. Requires
//!    `pcscd` running and a card inserted; zero attached cards fails with
//!    that same guidance. Exactly one attached card is used automatically;
//!    with several, the operator must pass `--card <serial>` to pick one
//!    (`choose_card`, a pure helper — see its own doc comment). The chosen
//!    card is then opened by serial
//!    (`aivyx_yubi::discovery::discover_real_card_by_serial`), not just
//!    "the first one `card_backends()` happens to enumerate" — the same
//!    serial just shown to the operator in the listing above.
//! 2. If the chosen card's Signature slot already holds a key, stop
//!    before asking for any PIN and explain, unless the operator passed
//!    `--overwrite-existing-key` — many YubiKey owners keep a real GPG
//!    signing key in that slot, and `aivyx_yubi::provision::
//!    generate_signature_key` now refuses an occupied slot
//!    (`YubiError::SignatureSlotOccupied`) for exactly that reason. With
//!    `--overwrite-existing-key`, the card's serial is shown again and the
//!    operator must type it back exactly to confirm
//!    (`typed_serial_confirms_overwrite`, a pure helper) before this
//!    command calls `provision::generate_signature_key_overwriting`
//!    instead. Without a real terminal on stdin, this refuses rather than
//!    prompting — there's no safe way to "confirm" non-interactively.
//! 3. Refuse if the card's User and/or Admin PIN is still the OpenPGP-card
//!    factory default (`aivyx_yubi::pin::require_pin_changed`). This
//!    command **never** changes a PIN on the operator's behalf — Chapter
//!    Passport's design spec's Global Constraints require refusing to
//!    proceed instead, and `pin::require_pin_changed` must only be called
//!    once per attempt (see its own doc comment on why: repeated calls
//!    burn real PIN retry attempts, and the Admin PIN has no self-recovery
//!    once blocked). `require_pin_changed` can also now fail with
//!    `YubiError::PinRetriesReduced` (some PIN's retry counter was already
//!    below maximum before this command touched the card at all) — that
//!    case gets its own guidance pointing at `gpg --card-status` instead
//!    of the factory-default-PIN-change instructions, which would be the
//!    wrong advice for it.
//! 4. Verify the (already-changed) Admin PIN, prompted interactively via
//!    `rpassword` (input hidden, never passed as a CLI argument or
//!    logged).
//! 5. Generate a fresh Ed25519 keypair in the Signature slot
//!    (`aivyx_yubi::provision::generate_signature_key`, or
//!    `generate_signature_key_overwriting` after step 2's confirmation) —
//!    **destructive** if the slot already holds a key; see those
//!    functions' own doc comments. Step 2 above is what makes that safe to
//!    call unconditionally here: by this point either the slot was empty,
//!    or the operator explicitly confirmed overwriting it.
//! 6. Set the Signature slot's touch-policy to `Fixed`
//!    (`aivyx_yubi::provision::set_signature_touch_policy_fixed`) — every
//!    future signature requires a physical touch, with no PIN-only
//!    fast-path.
//! 7. Build the binding record (`{instance_id, card_serial,
//!    public_key_base64}`) from data already in hand and write it to
//!    `key_binding_path` as plain (non-secret) JSON.
//! 8. As a closing sanity check, round-trip through
//!    `aivyx_federation::Identity::load_hardware` — the exact production
//!    load path a daemon will use later — confirming the freshly
//!    provisioned card's public key and serial are accepted by
//!    `aivyx-federation`'s own validation. This re-discovers the card via
//!    a fresh `aivyx_yubi::YubiKeySigner::new`, which now refuses an empty
//!    User PIN at construction (`YubiError::EmptyPin`) rather than
//!    deferring that to the first `sign()` call — so this step prompts for
//!    the real User PIN (hidden, like the Admin PIN prompt in step 4) and
//!    passes it, even though this verification pass never itself calls
//!    `sign()`. The binding record file from step 7 is already written by
//!    this point — a failure here is reported as an error, but does not
//!    un-write it (provisioning is already real and irreversible on the
//!    card by this point).
//!
//!    **This step opens a second, independent PC/SC connection to the same
//!    physical reader.** The provisioning transaction and card handle from
//!    steps 3-7 are explicitly `drop`ped before this step runs (Finding
//!    C-1) — `SCardBeginTransaction` blocks indefinitely (it does not fail
//!    fast) if another exclusive transaction is still held on the same
//!    reader, so failing to release the first transaction first would hang
//!    this command forever right after the card has already been
//!    irreversibly re-keyed.
//!
//!    **What this step does NOT verify**: whether the touch-policy setting
//!    from step 6 actually took effect live on the card. `YubiKeySigner::
//!    sign` hard-refuses if the live touch policy isn't `Fixed`, but doing
//!    that check here would require a real physical touch, which isn't
//!    this provisioning command's job (Finding I-1) — see this step's own
//!    user-facing message for the accurate, non-overclaiming description
//!    of what was and wasn't confirmed.

use std::io::{self, IsTerminal, Write as _};
use std::path::Path;

use aivyx_yubi::{SecretString, YubiError, YubiKeySigner, discovery, pin, provision};
use base64::Engine as _;
use serde::Serialize;

/// The on-disk binding record `yubikey-init` produces. Plain JSON — no
/// secret or private key material, per the design spec (contrast with
/// `aivyx_federation::Identity`'s software-backed key file, which *is*
/// sealed at rest).
#[derive(Debug, Serialize)]
struct KeyBindingRecord {
    instance_id: String,
    card_serial: String,
    public_key_base64: String,
}

/// Choose which attached card `yubikey-init` should operate on, given
/// `discovery::list_cards()`'s enumeration (already printed to the
/// operator by the caller) and an optional `--card <serial>`. Pure: takes
/// a plain slice of `discovery::CardSummary` (a hardware-free struct) and
/// returns a reference into it or a descriptive error — no card I/O, so
/// it's directly unit-testable without a YubiKey attached.
///
/// - `requested_serial = Some(s)`: the card whose serial exactly matches
///   `s`, or an error naming what *is* attached if none does.
/// - `requested_serial = None`, zero cards: refuses — nothing is attached.
/// - `requested_serial = None`, exactly one card: that card.
/// - `requested_serial = None`, several cards: refuses, asking for
///   `--card <serial>`.
fn choose_card<'a>(
    cards: &'a [discovery::CardSummary],
    requested_serial: Option<&str>,
) -> Result<&'a discovery::CardSummary, String> {
    if let Some(serial) = requested_serial {
        return cards.iter().find(|c| c.serial == serial).ok_or_else(|| {
            format!(
                "no attached YubiKey has serial `{serial}` -- attached: {}",
                describe_cards(cards)
            )
        });
    }
    match cards.len() {
        0 => Err(
            "no YubiKey found -- ensure pcscd is running and a YubiKey is inserted, then retry"
                .to_string(),
        ),
        1 => Ok(&cards[0]),
        _ => Err(format!(
            "multiple YubiKeys attached ({}) -- pass --card <serial> to choose one",
            describe_cards(cards)
        )),
    }
}

/// Render `cards` as a short, human-readable list (`"<serial> (<slot
/// state>), ..."`) for `choose_card`'s error messages and the startup
/// listing.
fn describe_cards(cards: &[discovery::CardSummary]) -> String {
    if cards.is_empty() {
        return "none".to_string();
    }
    cards
        .iter()
        .map(|c| {
            format!(
                "{} ({})",
                c.serial,
                if c.has_signature_key {
                    "Signature slot occupied"
                } else {
                    "Signature slot empty"
                }
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `--overwrite-existing-key`'s confirmation check: the operator must type
/// the card's serial back exactly (leading/trailing whitespace trimmed,
/// e.g. the trailing newline a terminal `read_line` leaves in) to confirm
/// they intend to destroy its existing Signature-slot key. Pure string
/// comparison -- no I/O -- so it's unit-testable without a terminal or
/// hardware; the actual stdin read lives in `run_yubikey_init` itself.
fn typed_serial_confirms_overwrite(expected_serial: &str, typed: &str) -> bool {
    typed.trim() == expected_serial
}

/// Entry point for `aivyx-pa federation yubikey-init <instance-id>
/// <key-binding-path> [--card <serial>] [--overwrite-existing-key]`. See
/// this module's doc comment for the full flow.
pub fn run_yubikey_init(
    instance_id: &str,
    key_binding_path: &Path,
    card_serial: Option<&str>,
    overwrite_existing_key: bool,
) -> Result<(), String> {
    // Fail fast on an invalid instance id before touching the card at all
    // (card discovery + the PIN-factory-default check below both cost
    // real, limited PIN retry attempts on a card whose PINs are already
    // correctly changed — see `pin::require_pin_changed`'s doc comment —
    // and key generation below is destructive and irreversible on the
    // card itself). Reuses `aivyx_federation::identity::validate_instance_id`
    // directly (Finding I-2) rather than duplicating its character-class
    // rule inline, so this early check can never drift out of sync with
    // the authoritative rule `Identity::load_hardware` re-applies at the
    // end regardless (step 8).
    aivyx_federation::identity::validate_instance_id(instance_id)
        .map_err(|e| format!("aivyx-pa federation yubikey-init: {e}"))?;

    eprintln!(
        "aivyx-pa federation yubikey-init: listing attached YubiKeys (requires pcscd running)..."
    );
    let cards = discovery::list_cards().map_err(|e| format!("aivyx-pa federation yubikey-init: {e}"))?;
    for card in &cards {
        eprintln!(
            "aivyx-pa federation yubikey-init:   found card {} ({})",
            card.serial,
            if card.has_signature_key {
                "Signature slot occupied"
            } else {
                "Signature slot empty"
            }
        );
    }
    let chosen = choose_card(&cards, card_serial)
        .map_err(|e| format!("aivyx-pa federation yubikey-init: {e}"))?;
    let chosen_serial = chosen.serial.clone();
    let chosen_has_key = chosen.has_signature_key;
    eprintln!("aivyx-pa federation yubikey-init: using card {chosen_serial}");

    // Refuse to overwrite an occupied Signature slot without explicit,
    // interactively-confirmed operator consent (step 2 in this module's
    // doc comment) -- mirrors `provision::generate_signature_key`'s own
    // refusal (`YubiError::SignatureSlotOccupied`), but checked here too
    // so the operator never gets as far as typing a PIN for a run that's
    // going to be refused anyway.
    if chosen_has_key {
        if !overwrite_existing_key {
            return Err(format!(
                "aivyx-pa federation yubikey-init: card {chosen_serial} already holds a \
                 Signature-slot key -- refusing to overwrite it (many YubiKey owners keep a \
                 real GPG signing key there). Re-run with --overwrite-existing-key once you \
                 have confirmed losing the existing key is intended."
            ));
        }
        if !io::stdin().is_terminal() {
            return Err(
                "aivyx-pa federation yubikey-init: --overwrite-existing-key requires an \
                 interactive terminal to confirm the card's serial -- refusing to proceed \
                 without one"
                    .to_string(),
            );
        }
        eprint!(
            "aivyx-pa federation yubikey-init: this will PERMANENTLY DESTROY the existing \
             Signature-slot key on card {chosen_serial}. Type the card's serial to confirm: "
        );
        io::stderr()
            .flush()
            .map_err(|e| format!("aivyx-pa federation yubikey-init: failed to flush prompt: {e}"))?;
        let mut typed = String::new();
        io::stdin().read_line(&mut typed).map_err(|e| {
            format!("aivyx-pa federation yubikey-init: failed to read confirmation: {e}")
        })?;
        if !typed_serial_confirms_overwrite(&chosen_serial, &typed) {
            return Err(
                "aivyx-pa federation yubikey-init: confirmation did not match the card's \
                 serial -- aborting without touching the card"
                    .to_string(),
            );
        }
    }

    let mut card = discovery::discover_real_card_by_serial(&chosen_serial)
        .map_err(|e| format!("aivyx-pa federation yubikey-init: {e}"))?;

    let mut tx = card.transaction().map_err(YubiError::from).map_err(|e| {
        format!("aivyx-pa federation yubikey-init: failed to open a card transaction: {e}")
    })?;

    // Refuse on a still-factory-default PIN rather than changing it
    // ourselves — see this module's doc comment (step 3) and
    // `pin::require_pin_changed`'s own doc comment for why this is called
    // exactly once here, not in a retry loop. `PinRetriesReduced` (some
    // PIN's retry counter was already below maximum before this command
    // did anything PIN-related) gets its own guidance: the factory-
    // default-PIN wording below would be actively misleading for it --
    // the fix there isn't "change your PIN", it's "stop and check the
    // card's retry counters first".
    pin::require_pin_changed(&mut tx).map_err(|e| {
        let guidance = match &e {
            YubiError::PinRetriesReduced { .. } => {
                "Check the card's current retry counters with `gpg --card-status` before doing \
                 anything else -- this command does not try to recover a reduced retry counter \
                 on your behalf."
            }
            _ => {
                "This command never changes a PIN on your behalf — change both the \
                 User and Admin PIN first via the standard OpenPGP-card PIN-change \
                 command (e.g. `gpg --card-edit`, then `admin`, then `passwd`), then \
                 retry `aivyx-pa federation yubikey-init`."
            }
        };
        format!("aivyx-pa federation yubikey-init: {e}\n\n{guidance}")
    })?;

    let admin_pin = rpassword::prompt_password("Admin PIN (input hidden): ")
        .map_err(|e| format!("aivyx-pa federation yubikey-init: failed to read Admin PIN: {e}"))?;
    let mut admin = tx
        .as_admin_card(SecretString::from(admin_pin))
        .map_err(YubiError::from)
        .map_err(|e| {
            // `as_admin_card`'s failure routes through `YubiError`'s
            // generic, context-free `From<openpgp_card::Error>` blanket
            // conversion (see that impl's own doc comment in `aivyx-yubi`)
            // -- a blocked PIN comes back labeled `pin_kind: "A"` ("A PIN
            // is blocked..."), even though this call site unambiguously
            // knows it's the ADMIN PIN that just failed. Rewrite that
            // specific case with an unambiguous, correctly-labeled message
            // and accurate (non-circular) recovery guidance -- never
            // "verify with the admin PIN", since that's exactly what's
            // blocked here (Finding I-4) -- instead of relying on the
            // generic fallback's ambiguous wording.
            let description = if matches!(&e, YubiError::PinBlocked { .. }) {
                "Admin PIN is blocked (too many failed attempts). This cannot be recovered \
                 with the Admin PIN itself -- only a pre-configured Reset Code (if one was \
                 set up) or a full card reset (TERMINATE+ACTIVATE, which erases all keys) can \
                 recover from this state."
                    .to_string()
            } else {
                format!("Admin PIN verification failed: {e}")
            };
            // Finding I-3: `pin::require_pin_changed` above and this
            // verification attempt each burn one of the Admin PIN's
            // limited real retries (`aivyx-yubi`'s own `pin.rs` documents
            // this at length) -- two mistakes, not three, can permanently
            // block it, with no self-recovery short of a full card wipe.
            // Nothing else in this command's output warns the operator of
            // that before they retry blindly.
            format!(
                "aivyx-pa federation yubikey-init: {description}\n\n\
                 Warning: this failed attempt just consumed one of the Admin PIN's limited \
                 real retry attempts, and the factory-default-PIN check that already ran \
                 earlier in this same command consumed one too. A blocked Admin PIN has NO \
                 self-recovery path short of a full card wipe (TERMINATE+ACTIVATE, which \
                 erases all existing keys) -- check your retry counter (e.g. `gpg \
                 --card-status`) before retrying blindly."
            )
        })?;

    // By this point either the slot was empty, or the operator already
    // typed the card's serial back to confirm overwriting it (above) --
    // so it's safe to call the destructive variant unconditionally here
    // rather than racing `generate_signature_key`'s own occupancy check
    // against the confirmation the operator already gave.
    eprintln!(
        "aivyx-pa federation yubikey-init: generating an Ed25519 keypair in the Signature slot{}...",
        if chosen_has_key {
            " (overwriting the existing key, as confirmed above)"
        } else {
            ""
        }
    );
    let public_key = if chosen_has_key {
        provision::generate_signature_key_overwriting(&mut admin)
    } else {
        provision::generate_signature_key(&mut admin)
    }
    .map_err(|e| {
        format!("aivyx-pa federation yubikey-init: Signature-slot key generation failed: {e}")
    })?;

    eprintln!(
        "aivyx-pa federation yubikey-init: setting the Signature slot's touch policy to fixed \
         (every future signature will require a physical touch)..."
    );
    provision::set_signature_touch_policy_fixed(&mut admin).map_err(|e| {
        format!("aivyx-pa federation yubikey-init: setting the touch policy failed: {e}")
    })?;

    // `admin`'s last use was the call directly above -- NLL ends its
    // mutable borrow of `tx` here, freeing `tx` to read the serial
    // directly (same pattern `aivyx-yubi`'s own
    // `sets_the_signature_touch_policy_to_fixed` test uses).
    let card_serial = discovery::read_serial(&mut tx).map_err(|e| {
        format!("aivyx-pa federation yubikey-init: failed to read the card's serial: {e}")
    })?;

    // Finding C-1 (CRITICAL): explicitly close the exclusive PC/SC
    // transaction (`tx`) and disconnect the underlying card handle
    // (`card`) now that all real card I/O for this provisioning attempt
    // is done -- BEFORE the verification pass below opens a *second*,
    // independent PC/SC connection to the same physical reader via
    // `YubiKeySigner::new`. `SCardBeginTransaction` (what that second
    // connection's own transaction call performs internally) blocks
    // indefinitely -- it does not fail fast -- if another exclusive
    // transaction is still held on the same reader. Without this, the
    // command would hang forever right here, after the card has already
    // been irreversibly re-keyed and the binding record already written,
    // with no way for the operator to tell whether provisioning actually
    // succeeded. `tx` borrows `card` mutably (`Card<Transaction<'_>>`), so
    // it must be dropped first.
    drop(tx);
    drop(card);

    let public_key_base64 = base64::engine::general_purpose::STANDARD.encode(public_key.as_ref());

    let record = KeyBindingRecord {
        instance_id: instance_id.to_string(),
        card_serial: card_serial.clone(),
        public_key_base64,
    };
    write_binding_record(key_binding_path, &record)?;
    eprintln!(
        "aivyx-pa federation yubikey-init: wrote binding record ({{instance_id: {}, card_serial: \
         {}}}) to {}",
        record.instance_id,
        record.card_serial,
        key_binding_path.display(),
    );

    // Closing sanity check (step 8 in this module's doc comment): confirm
    // the exact production load path (`Identity::load_hardware`) accepts
    // what we just provisioned. The binding record above is already
    // written by this point regardless of this check's outcome — the
    // card's state is already real and irreversible. Safe to open a fresh
    // PC/SC connection here: `tx`/`card` were already dropped above
    // (Finding C-1), so no exclusive transaction is still held on this
    // reader.
    //
    // `YubiKeySigner::new` now refuses an empty User PIN at construction
    // (`YubiError::EmptyPin`) rather than only on the first `sign()` call
    // -- this verification pass never calls `sign()` itself, but must
    // still supply a real, non-empty PIN to construct the signer at all.
    // Prompted hidden, same as the Admin PIN above.
    let user_pin = rpassword::prompt_password(
        "User PIN (input hidden, for post-provisioning verification): ",
    )
    .map_err(|e| format!("aivyx-pa federation yubikey-init: failed to read User PIN: {e}"))?;
    // Bound by serial to the card just provisioned, so another attached
    // OpenPGP card can never be the one verified.
    let verifying_signer =
        YubiKeySigner::new_for_serial(SecretString::from(user_pin), &card_serial).map_err(|e| {
        format!(
            "aivyx-pa federation yubikey-init: wrote {} but a fresh re-discovery for verification \
             failed: {e}",
            key_binding_path.display(),
        )
    })?;
    // NB (Finding I-1): `load_hardware`'s serial check compares the
    // freshly re-discovered card's serial against `card_serial` -- which
    // was itself just read from this exact same card, seconds earlier, in
    // this exact same run. That comparison can never meaningfully fail in
    // this flow; it isn't a "wrong card" check here (unlike in `sign()`'s
    // long-lived-signer use case where it genuinely guards against a card
    // swap). What this whole pass *does* meaningfully confirm is narrower:
    // the card is discoverable again, its serial and public key match what
    // provisioning itself just reported, and `Identity::load_hardware`
    // (the real production load path) accepts all of it end to end. It
    // does NOT confirm the touch policy set in step 6 is being enforced
    // live -- see the success message below for the honest, non-
    // overclaiming summary of what was and wasn't checked.
    let identity = aivyx_federation::identity::Identity::load_hardware(
        instance_id.to_string(),
        verifying_signer,
        &card_serial,
    )
    .map_err(|e| {
        format!(
            "aivyx-pa federation yubikey-init: wrote {} but the provisioned identity failed \
             verification against aivyx-federation's own load path: {e}",
            key_binding_path.display(),
        )
    })?;
    if identity.public_key_base64() != record.public_key_base64 {
        return Err(format!(
            "aivyx-pa federation yubikey-init: wrote {} but the verification pass read back a \
             different public key than provisioning reported -- this should not happen; please \
             report this as a bug",
            key_binding_path.display(),
        ));
    }

    eprintln!(
        "aivyx-pa federation yubikey-init: post-provisioning check passed — a fresh re-discovery \
         of card {1} confirms its serial and Signature-slot public key match what provisioning \
         just wrote for instance `{0}`, and aivyx-federation's own Identity::load_hardware \
         (the real production load path) accepts them. This does NOT confirm the touch policy \
         is being enforced live on the card -- that is confirmed the first time this identity \
         actually signs a real federation request, not by this init command.",
        identity.instance_id(),
        card_serial,
    );
    Ok(())
}

/// Write `record` as pretty-printed JSON to `path`, creating parent
/// directories if needed. Plain permissions (not `0600`) — the record is
/// deliberately non-secret (see this module's doc comment).
fn write_binding_record(path: &Path, record: &KeyBindingRecord) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create parent dir {}: {e}", parent.display()))?;
        }
    }
    let json = serde_json::to_string_pretty(record)
        .map_err(|e| format!("failed to serialize the key binding record: {e}"))?;
    std::fs::write(path, json).map_err(|e| format!("failed to write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_binding_record_creates_parent_dirs_and_pretty_json() {
        let dir = std::env::temp_dir().join(format!(
            "aivyx-federation-cli-test-{}",
            uuid::Uuid::new_v4()
        ));
        let path = dir.join("nested").join("binding.json");
        let record = KeyBindingRecord {
            instance_id: "test-instance".to_string(),
            card_serial: "0006:00112233".to_string(),
            public_key_base64: "abc123==".to_string(),
        };

        write_binding_record(&path, &record).expect("write should succeed");

        let contents = std::fs::read_to_string(&path).expect("read back");
        assert!(contents.contains("\"instance_id\": \"test-instance\""));
        assert!(contents.contains("\"card_serial\": \"0006:00112233\""));
        assert!(contents.contains("\"public_key_base64\": \"abc123==\""));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_instance_id_is_rejected_before_touching_any_card() {
        // Finding M-3: a fixed temp filename, asserted not to exist, is
        // flaky on a shared `/tmp` (a leftover from a prior interrupted
        // run or another user could make this spuriously fail) -- mirror
        // `write_binding_record_creates_parent_dirs_and_pretty_json`'s own
        // UUID-based unique filename instead.
        let path = std::env::temp_dir().join(format!(
            "aivyx-federation-cli-test-{}.json",
            uuid::Uuid::new_v4()
        ));
        let err = run_yubikey_init("", &path, None, false)
            .expect_err("an empty instance id must be rejected");
        assert!(err.contains("must not be empty"), "error: {err}");
        // Must not have written anything -- the instance-id check runs
        // before any card I/O or file write (Finding I-2).
        assert!(!path.exists());
    }

    #[test]
    fn invalid_instance_id_is_rejected_before_touching_any_card() {
        // Finding I-2: the fuller validation rule (only ASCII
        // alphanumeric, `-`, and `_`) must run up front too, not just the
        // empty-string case -- an id containing e.g. a space must never
        // reach card discovery/I/O.
        let path = std::env::temp_dir().join(format!(
            "aivyx-federation-cli-test-{}.json",
            uuid::Uuid::new_v4()
        ));
        let err = run_yubikey_init("bad id with spaces", &path, None, false)
            .expect_err("an instance id with invalid characters must be rejected");
        assert!(err.contains("invalid characters"), "error: {err}");
        // Must not have written anything -- the instance-id check runs
        // before any card I/O or file write.
        assert!(!path.exists());
    }

    // --- `choose_card`: pure card-selection logic, no hardware needed ---

    fn card(serial: &str, has_signature_key: bool) -> discovery::CardSummary {
        discovery::CardSummary {
            serial: serial.to_string(),
            has_signature_key,
        }
    }

    #[test]
    fn choose_card_refuses_when_none_are_attached() {
        let cards: Vec<discovery::CardSummary> = vec![];
        let err = choose_card(&cards, None).expect_err("no attached cards must be refused");
        assert!(err.contains("no YubiKey found"), "error: {err}");
    }

    #[test]
    fn choose_card_uses_the_only_attached_card_without_a_card_flag() {
        let cards = vec![card("0006:00112233", false)];
        let chosen = choose_card(&cards, None).expect("the single card should be chosen");
        assert_eq!(chosen.serial, "0006:00112233");
    }

    #[test]
    fn choose_card_requires_the_card_flag_when_several_are_attached() {
        let cards = vec![card("0006:00112233", false), card("0006:00112244", true)];
        let err = choose_card(&cards, None)
            .expect_err("several attached cards without --card must be refused");
        assert!(err.contains("--card <serial>"), "error: {err}");
        // Both serials should be named so the operator knows what to pass.
        assert!(err.contains("0006:00112233"), "error: {err}");
        assert!(err.contains("0006:00112244"), "error: {err}");
    }

    #[test]
    fn choose_card_picks_the_requested_serial_among_several() {
        let cards = vec![card("0006:00112233", false), card("0006:00112244", true)];
        let chosen = choose_card(&cards, Some("0006:00112244"))
            .expect("the requested serial should be found");
        assert_eq!(chosen.serial, "0006:00112244");
        assert!(chosen.has_signature_key);
    }

    #[test]
    fn choose_card_reports_an_unknown_requested_serial() {
        let cards = vec![card("0006:00112233", false)];
        let err = choose_card(&cards, Some("0006:99999999"))
            .expect_err("an unknown --card serial must be refused");
        assert!(err.contains("0006:99999999"), "error: {err}");
        assert!(err.contains("0006:00112233"), "error: {err}");
    }

    #[test]
    fn choose_card_honors_the_card_flag_even_with_only_one_attached() {
        // A --card that matches the only attached card should still work
        // (not just be tolerated as redundant).
        let cards = vec![card("0006:00112233", false)];
        let chosen = choose_card(&cards, Some("0006:00112233"))
            .expect("a matching --card should be accepted even with one card attached");
        assert_eq!(chosen.serial, "0006:00112233");
    }

    // --- `typed_serial_confirms_overwrite`: pure string comparison ---

    #[test]
    fn typed_serial_confirms_overwrite_accepts_an_exact_match() {
        assert!(typed_serial_confirms_overwrite(
            "0006:00112233",
            "0006:00112233"
        ));
    }

    #[test]
    fn typed_serial_confirms_overwrite_trims_surrounding_whitespace() {
        // A real terminal `read_line` leaves a trailing newline (and an
        // operator might add leading/trailing spaces) -- that must not
        // itself cause a correct answer to be rejected.
        assert!(typed_serial_confirms_overwrite(
            "0006:00112233",
            "  0006:00112233\n"
        ));
    }

    #[test]
    fn typed_serial_confirms_overwrite_rejects_a_mismatch() {
        assert!(!typed_serial_confirms_overwrite(
            "0006:00112233",
            "0006:00112244"
        ));
    }

    #[test]
    fn typed_serial_confirms_overwrite_rejects_an_empty_answer() {
        assert!(!typed_serial_confirms_overwrite("0006:00112233", "\n"));
    }

    #[test]
    fn typed_serial_confirms_overwrite_is_case_sensitive() {
        // Serials are rendered uppercase-hex by `discovery::read_serial`;
        // a case-insensitive match would accept a typo-prone near-miss.
        assert!(!typed_serial_confirms_overwrite(
            "0006:00112233",
            "0006:AABBCCDD"
        ));
    }
}
