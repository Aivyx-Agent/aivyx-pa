# Unlock Aivyx PA with a YubiKey — design

**Date:** 2026-11-14
**Status:** approved in conversation; awaiting review of this written spec.
**Amendment:** A18 (`docs/amendments/2026-11-14-card-unlock.md`).
**Repos:** `aivyx-yubi` (new card calls), `aivyx-pa` (everything else).

This is the first sub-project of the end-user YubiKey feature: a YubiKey
that secures a user's own local Aivyx PA. Two later sub-projects get their
own specs: a touch to approve risky actions (pa and aivyx-coder), and
signed logs.

## Goal

A user can enroll one or more OpenPGP cards (a YubiKey, or any card with
an X25519 decryption key) so that PA's encrypted store opens with the
card, its PIN and a touch. No passphrase then needs to be saved in the OS
keyring or in `daemon.env`. The passphrase keeps working and is the
recovery path.

## Decisions (made with the user)

1. **Unlocking PA comes first.** Approving actions and signing logs come later.
2. **The card or the passphrase.** Either one unlocks the store, and the
   passphrase is the way back in if a card is lost. The store format does
   not change.
3. **The daemon starts locked and is unlocked later.** A background service
   with no passphrase source and an enrolled card starts locked. The user
   then runs `aivyx-pa unlock`.
4. **The decryption slot (X25519 ECDH).** The master key is encrypted to
   the public key in the card's decryption slot, and only the card can
   recover it. The signature slot is never touched, so the federation key
   or a GPG signing key kept there stays as it is.
5. **A helper program for the card.** The card code runs in a small helper,
   `aivyx-pa-card`, linked to the system PC/SC library. The static musl
   daemon runs it as a child process.

## How it works

PA's master key is `Argon2id(passphrase, salt)` (DESIGN.md D7), and every
storage subkey comes from it through HKDF. Card unlock adds a second way
to get **the same master key**:

```
enroll:  card_pub   = card's decryption-slot X25519 public key
         eph        = fresh X25519 key pair (one per entry; secret dropped after use)
         shared     = X25519(eph.secret, card_pub)      // computed on the card
         wrap_key   = HKDF-SHA256(ikm = shared,
                                  salt = "aivyx-pa-card-unlock-v1",
                                  info = instance || serial || card_pub || eph.public)
         entry      = { serial, label, card_pub, eph.public, nonce,
                        ChaCha20-Poly1305(wrap_key, nonce, master_key,
                                          aad = the same info bytes) }

unlock:  shared     = card.ECDH(eph.public)   // needs the PIN and a touch
         wrap_key   = HKDF(... as above ...)
         master_key = open(entry)              // a changed byte, or the wrong card or instance, fails here
```

The secret half of the one-off key `eph` is never written down, so the
card's ECDH is the only way to rebuild `shared`. The `info` and `aad`
bytes bind each entry to its instance, card serial and keys. An entry
copied to another instance, or edited to name another card, therefore
fails to open.

Enrolling computes `shared` on the card (with the PIN and a touch) instead
of locally from `card_pub`. That proves the card can do the ECDH before
the entry is saved.

## Components

### aivyx-yubi (new public calls, all tested against `FakeCard`)

- `decryption_public_key(tx) -> Result<Option<PublicKeyBytes>, YubiError>`:
  the X25519 key in the decryption slot. It returns `None` when the slot
  is empty, and `UnsupportedKey` when the slot holds a key of another type.
- `generate_decryption_key(admin) -> Result<PublicKeyBytes, YubiError>`:
  makes a Curve25519 ECDH key on the card and sets the decryption slot's
  touch policy to fixed. It refuses when the slot already holds a key.
  `generate_decryption_key_overwriting` exists for the caller that
  explicitly asks to replace one, the same pattern as the signature slot.
- `ecdh(tx, pin, their_public: &[u8; 32]) -> Result<SharedSecret, YubiError>`:
  `decipher(Cryptogram::ECDH)` after a user-PIN check (PW1, mode 82).
  `SharedSecret` is 32 bytes and zeroed on drop. A wrong PIN reports the
  tries left. It refuses to present the PIN again on the same card once
  one try has failed, as `sign` already does.
- `FakeCard` gains a decryption slot with real X25519 math (x25519-dalek,
  under the `test-util` feature only), so the round trip is tested.

### aivyx-crypto (pa)

- `MasterKey::wrap(&self, key: &WrapKey, aad: &[u8]) -> WrappedMasterKey`
  and `MasterKey::unwrap(wrapped, key, aad) -> Result<MasterKey, CryptoError>`
  (ChaCha20-Poly1305, random 96-bit nonce). `WrapKey` is zeroed on drop
  and built only by HKDF from a shared secret. The raw master key still
  never leaves the crate.

### `aivyx-pa-card` (new helper binary, new crate `crates/aivyx-card-helper`)

- A one-shot process that the daemon or the CLI runs. It reads a single
  JSON request on stdin and writes a single JSON reply on stdout, then
  exits. The requests:
  - `list`: the plugged-in cards (serials) and whether each decryption
    slot holds a key;
  - `public_key {serial}`;
  - `generate {serial, admin_pin}`;
  - `ecdh {serial, pin, their_public}`.
- It links aivyx-yubi and the system PC/SC library. It is built for
  `x86_64`/`aarch64-unknown-linux-gnu` and both macOS targets, and
  installed beside `aivyx-pa` by the installer.
- It keeps no state and makes no network calls. Its only secret output is
  the 32-byte shared secret, written once to the pipe its parent owns.
  PINs reach it only through stdin, never in argv or the environment.
- PA finds the helper next to its own binary first, then on `PATH`. When
  it is missing, every `card` command and an unlock attempt say how to
  install it.
- PA's existing `yubikey` feature (federation `yubikey-init`) is
  unchanged by this work. It could move to the helper later.

### `card_unlock` (new module in aivyx-channel)

- The file `card-unlock.toml` is per instance, and its path comes from
  `InstancePaths`, as A17 requires. It is written `0600` through a temp
  file and a rename. Its contents:
  - `version = 1`;
  - one `[[cards]]` table per enrolled card, with `serial`, `label`,
    `added` (RFC 3339), `card_public`, `ephemeral_public`, `nonce` and
    `wrapped` (byte fields in base64).
- The module covers four operations:
  - `enroll`: needs the master key in hand; adds or replaces the entry for
    that serial.
  - `unlock_with`: takes a helper client and a PIN, picks the first
    enrolled card that is plugged in (or the named serial), and returns
    the master key.
  - `remove`.
  - `list`.
- Ward already denies tools any read of `daemon.env` and `*.redb`. It also
  gets `card-unlock.toml`: the entry is useless without the card, but no
  agent tool should read it either way.

### Daemon: the locked state

- **Choosing the start-up mode.** If a passphrase source exists (the
  environment, the config, the keyring, `daemon.env` or a terminal
  prompt), the daemon starts exactly as today. If there is none and
  `card-unlock.toml` has at least one entry, the daemon starts **locked**.
  With neither, start-up fails with today's "no passphrase" error.
- **What runs while locked.** The store, audit log, memory, channels,
  scheduler and the autonomous loop are not started; nothing that needs
  the master key exists yet. The IPC socket (still `0600`) answers only
  these requests:
  - `LockState`: returns locked or unlocked, plus the enrolled cards'
    labels and serials;
  - `Unlock { pin, serial? }`;
  - `UnlockWithPassphrase { passphrase }`: recovery from a terminal;
  - `Stop`.

  Every other request gets `Locked`, with the message "PA is locked — run
  `aivyx-pa unlock`".
- **Unlocking.** The daemon runs the helper's `ecdh`, unwraps the master
  key, and then follows the normal start-up path from where a derived
  master key would be handed on. A successful unlock is the first audit
  entry of the session (`unlock`, method `card` with its serial, or
  `passphrase`).
- **`Lock`** (only when unlocked) stops the same services in the same
  order as shutdown, drops the master key and every subkey (zeroed), and
  returns to the locked state without the process exiting. (As built, it
  re-executes itself in place: the same pid, a new process image, which
  starts locked. Services and their child processes are stopped before
  the exec; the real-card checklist checks that none survives.)
- **Failed attempts.** One unlock at a time is allowed; a second one while
  the first runs is told to wait. After 5 failed unlocks in a row, the
  daemon waits 30 s before accepting the next. This is on top of the
  card's own PIN retry counter.

### CLI

- `aivyx-pa card add [--card <serial>] [--label <name>]`. The master key
  never leaves the process that holds it, so enrollment happens where the
  key is:
  - **Daemon running and unlocked:** the CLI collects the answers and
    sends an `EnrollCard { serial, label, pin, generate, admin_pin? }`
    request. The daemon runs the helper and writes the entry. It refuses
    this request while locked.
  - **No daemon running:** the CLI asks for the passphrase, derives the
    master key itself, enrolls, and drops the key.

  Either way, the steps are:
  1. If the decryption slot is empty, it asks: "This card has no
     decryption key. Make one on the card? [y/N]". Making one needs the
     admin PIN.
  2. It asks for the PIN and says "Touch your YubiKey…", then writes the
     entry.
  3. It ends with: "PA can now be unlocked with this card. Remove the
     saved passphrase from <keyring | daemon.env>? [y/N]".
- `aivyx-pa card list`: label, serial, date added, and whether the card is
  plugged in.
- `aivyx-pa card remove <serial>`: removing the **last** card while no
  passphrase source is saved needs typing `I know my passphrase`.
- `aivyx-pa unlock [--card <serial>] [--passphrase]`: PIN prompt (no
  echo), "Touch your YubiKey…", then "PA is unlocked.".
- `aivyx-pa lock`.
- `aivyx-pa status` gains `locked: yes/no`.
- The TUI, Studio, the desktop app and the chat channels show the
  `Locked` message when they reach a locked daemon. Unlocking from them is
  out of scope.
- All of this is per instance (`--instance`, A17).

## Errors

Every failure leaves the daemon fully locked or fully unlocked, never in
between.

| Case | What the user sees |
|---|---|
| Helper not installed | "Card unlock needs `aivyx-pa-card`" and how to install it |
| `pcscd` not running, or no reader | aivyx-yubi's existing hint naming `pcscd` |
| No enrolled card plugged in | "None of your enrolled cards is plugged in" and their labels and serials |
| Wrong PIN | "Wrong PIN — N tries left"; at 1 left, it asks before trying |
| No touch within 15 s | "No touch — still locked" |
| Entry fails to open (tampered, other instance, other card) | "This card's entry doesn't open — remove and re-add it"; stays locked |
| `card-unlock.toml` unreadable or invalid | The daemon starts locked and says the file is broken; `unlock --passphrase` still works |
| Decryption slot holds a non-X25519 key | "This card's decryption key isn't X25519"; enrollment offers nothing destructive |
| 5 failed unlocks in a row | "Too many failed unlocks — try again in 30 s" |

## Out of scope

- Unlocking from the TUI, Studio, the desktop app or a channel.
- Locking automatically when the card is unplugged or after idle time.
- Requiring both the card and the passphrase (two-factor), or a card on
  its own.
- Touch to approve risky actions, and signed logs (later sub-projects).
- aivyx-coder (it has no stored secrets).
- Windows (PA has no native Windows build).
- Changing the passphrase. PA has no way to do this today. If one is ever
  added, it must rewrap or remove every card entry, because each entry
  wraps the old master key.

## Testing

- **aivyx-yubi (FakeCard):**
  - an ECDH round trip;
  - generating a key, and refusing to overwrite one;
  - a wrong PIN, and refusing to present the PIN again;
  - a non-X25519 slot reported as such;
  - a test that `cargo tree` for the default build still has no PC/SC.
- **aivyx-crypto:** wrap and unwrap round trip; a changed byte in the
  ciphertext, nonce or aad is refused; the wrong key is refused.
- **card_unlock**, with a fake helper client:
  - enroll then unlock gives the same master key, checked by opening a
    real test store;
  - refused: the wrong instance, a changed serial, entries with swapped
    public keys, a changed byte;
  - the file is `0600`; a broken file is reported.
- **Daemon:**
  - with a fake helper, the daemon starts locked, refuses every other
    request with `Locked`, unlocks, locks and unlocks again;
  - a passphrase start is unchanged;
  - the 5-failure back-off;
  - the audit's first entry is the unlock.
- **CLI:** usage errors; the last-card guard; the docs-coverage test for
  the new commands.
- **Helper:** request and reply parsing; refusing unknown requests; the
  PIN never appearing in argv.
- **Manual, real card** (a checklist in `docs/`, run once on this machine
  before release):
  1. enroll with a fresh card, and with a card that already has a
     decryption key;
  2. restart the service, then `unlock`;
  3. `lock`, then unlock again;
  4. unplug the card, then `unlock --passphrase`;
  5. remove the card.

## Documentation

- PA manual:
  - CLI reference: the `card`, `unlock` and `lock` commands, and the
    `status` line;
  - files and paths: `card-unlock.toml`;
  - a guide section, "Unlock with a YubiKey".
- `THREAT_MODEL.md`: what a card protects (a saved secret on disk) and
  what it doesn't (a running, unlocked daemon; root on the machine).
- Release notes and the installer: the `aivyx-pa-card` helper.
