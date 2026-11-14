# Amendment A18 — Card Unlock and a Locked Daemon State Under D7

**Date:** 2026-11-14
**Phase:** End-user YubiKey, sub-project 1 (unlock PA with a card).
**Extends:** **D7** (Storage Decision): a second way to obtain the master
key, and a daemon that can run with the store closed.
**Leaves unchanged:**
- the D7 stack (redb, ChaCha20-Poly1305, Argon2id, HKDF-SHA256);
- the HKDF domain list and its versioned salt;
- the store format;
- **P6** (OS-level operator identity) and **D1** (the turn loop).

No PRODUCT.md or DESIGN.md text is superseded.
**Reference design:** `docs/superpowers/specs/2026-11-14-yubikey-unlock-design.md`.

---

## What changed

D7 derives the master key from the operator's passphrase with Argon2id,
and the daemon needs that passphrase to start. A18 authorizes two
additions.

### 1. A wrapped copy of the master key per enrolled card

An operator may enroll OpenPGP cards (a YubiKey, or any card with an
X25519 decryption key). For each card, the master key is encrypted to the
card's decryption-slot public key: an ephemeral-static X25519 exchange,
HKDF-SHA256 with the salt `"aivyx-pa-card-unlock-v1"`, then
ChaCha20-Poly1305, with the instance, card serial and both public keys
bound into the key derivation and the AAD. The result is kept in the
per-instance file `card-unlock.toml` (`0600`).

The card, its PIN and a touch recover **the same master key** the
passphrase derives. The passphrase keeps working unchanged and is the
recovery path. Subkeys, the store and the audit chain cannot tell which
way the master key was obtained.

### 2. A locked daemon state

When no passphrase source exists and at least one card is enrolled, the
daemon starts **locked**:
- the master key, the store, the audit log, memory, channels, the
  scheduler and the autonomous loop do not exist yet;
- the IPC socket (`0600`, the operator boundary) answers only lock state,
  unlock (card PIN, or passphrase), and stop.

Unlocking continues the normal start-up path. `lock` returns a running
daemon to that state: services stop as on shutdown, and every key is
dropped and zeroed. It does this by re-executing itself in place (the same
pid, a new process image) once its services have stopped, so no key or
state from the unlocked process survives in memory.

## Why this is consistent with D7 and the threat model

- **No weaker key.** The wrapped copy opens only with an ECDH on the card,
  because the ephemeral secret is never stored. Holding
  `card-unlock.toml` without the card gives nothing.
- **Fewer saved secrets.** The reason for the change is that an operator
  with a card no longer needs the passphrase saved in the keyring or
  `daemon.env`. Those were the weakest point of a headless install.
- **No turn without a store.** No tool, channel or agent runs while the
  daemon is locked, so D1 (capability check, audit, execute, audit) has
  nothing to apply to. The first audit entry after an unlock records how
  the store was unlocked.
- **The PIN crosses the same boundary the passphrase already does.** It
  goes over the `0600` socket, as `UnlockWithPassphrase` and the existing
  passphrase prompt paths do, and to the card helper over a pipe, never
  in argv or the environment.

## The card helper

Card access needs the system PC/SC library, and PA's Linux releases are
static musl binaries, which cannot link it. Card operations therefore run
in a separate binary, `aivyx-pa-card`. The daemon and the CLI run it as a
one-shot child: one JSON request on stdin, one reply on stdout, then it
exits. It keeps no state, makes no network calls, and outputs only a
32-byte shared secret, to its parent. It is not a tool process: no agent
tool can reach it.

## Out of scope

- Requiring both the card and the passphrase, or the card alone.
- Unlocking from the TUI, Studio, the desktop app or a channel.
- Locking automatically when the card is unplugged or after idle time.
- Approving actions with a touch, and signing logs: each needs its own
  amendment.
- Changing the passphrase. PA has none today; adding one must rewrap or
  remove every card entry.
