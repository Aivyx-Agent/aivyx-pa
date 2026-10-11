# Matrix channel — design

Status: approved in brainstorming, 2026-10-11. Sub-project 1 of 3 of the
private-channels work (below).

## The private-channels work (context)

Three new ways to reach Aivyx PA, chosen for privacy and security, each its
own spec → plan → implementation, in this order:

1. **Matrix** — this spec.
2. **Proton Mail** — both a channel (email your assistant; it replies by
   email) and tools (read, search, draft, send on your Proton mailbox), all
   through Proton Mail Bridge's local IMAP/SMTP. Separate spec.
3. **Signal** — through a separately installed `signal-cli` run as its own
   process (the Rust Signal libraries are AGPL and can't be linked into
   BUSL code). The assistant has **its own number**, not a linked device on
   the operator's account. Separate spec.

## Goal

The operator talks to their assistant from any Matrix client (Element and
others), in an end-to-end-encrypted room, with the assistant trusting only
the operator's own verified devices — so neither a stolen password nor the
homeserver can impersonate the operator.

## Architecture

A new crate, `crates/aivyx-matrix`, shaped like `aivyx-discord`:

- `transport.rs` — `MatrixTransport` trait: receive messages (with room,
  sender, sender device, verification state and encryption state), send a
  reply, join or decline an invite, typing notice. Two implementations: the
  real one over `matrix-sdk` 0.19 (Apache-2.0; features `e2e-encryption`,
  `sqlite`, `rustls-aws-lc-rs`; no default features), and a scripted
  transport for tests, as the other chat crates have.
- `matrix_channel.rs` — `ChannelContext`: the trust tier (below), and the
  reply sent once complete (like Telegram), not edited in place.
- `session.rs` — the receive loop: one conversation per room, `/cancel`,
  `/team …` gated like the other channels, reconnection with backoff.
- Daemon mode — `FrontendType::Matrix` in `aivyx-ipc` and
  `aivyx-channel/src/matrix_daemon_frontend.rs`, matching Discord's.
- Config — `[matrix]`: `homeserver`, `user_id` (the assistant's Matrix ID),
  `room_filter` (the allowlisted room id), `allowed_senders` (the
  operator's Matrix IDs), and the same `team_run_channel`,
  `team_trigger_rate_limit`, `team_command_allowed_senders` as Discord and
  Slack. The access token and device id are **not** config: they live in
  pa's encrypted store, written by `aivyx-pa connect matrix`.
- Paths — the `matrix-sdk` SQLite store lives under the instance's data
  directory, built only through `aivyx-instance` (`InstancePaths`), so each
  named instance is its own Matrix device. The no-stray-paths guard test
  covers it.

## Trust

A message gets `SemiTrusted` — the tier an allowlisted chat gets on
Telegram, Discord and Slack — only when **all** of these hold:

1. it is in the `room_filter` room;
2. its sender is in `allowed_senders`;
3. it came from a device the assistant has verified.

Anything else is `Untrusted` (public memory only): another room, another
sender, or the operator's own account from an unverified device.

## Encryption

- The assistant answers only in encrypted rooms; it refuses to start when
  `room_filter` names an unencrypted room, saying so.
- The `matrix-sdk` store's passphrase is a subkey of pa's master key
  (`MasterKey::derive_subkey(b"aivyx-pa matrix store")`), so the store is
  encrypted at rest and useless without the operator's pa passphrase.
  Nothing new is written in the clear.
- A message that can't be decrypted gets one reply per room ("I couldn't
  read that — verify my device with `aivyx-pa matrix verify`"), not one per
  message, and never a silent drop.

## Setup

1. `aivyx-pa connect matrix` asks for the homeserver, the assistant's
   Matrix ID and its password; logs in once; stores the access token and
   device id in the encrypted store; discards the password.
2. `aivyx-pa matrix verify` runs SAS (emoji) verification in the terminal:
   the operator starts verification from their Matrix client, compares the
   emoji, and confirms on both sides. The assistant then trusts that device.
3. Room invites are accepted only from `allowed_senders`; others are
   declined.

## Errors

- Connection lost: the receive loop reconnects with backoff (capped at
  60 s) and logs it; a turn in flight finishes and its reply is sent once
  connected.
- Token rejected (device logged out from the operator's client): the
  channel stops with "run `aivyx-pa connect matrix` again" — no retry loop.
- Undecryptable message: the once-per-room reply above.
- Send failure: logged and audited; the turn's outcome stands, as for
  Discord.
- `[budget]` and `[rate_limit]` apply exactly as for the other channels.

## Testing

- Unit tests against the scripted transport, mirroring `aivyx-discord`'s
  suite: the trust tier for every combination of room, sender and device
  verification; unencrypted-room refusal; the once-per-room undecryptable
  reply; `/cancel`; `/team` gating; invites from unknown senders declined;
  reconnect after a drop.
- Config and CLI: `[matrix]` parsing, the docs drift tests, and
  `connect matrix` storing the token without the password.
- A live end-to-end test, `#[ignore]`d: a local Conduit homeserver in
  Docker, two registered users, the bot device verified, one encrypted
  exchange. Run by hand, like the other live tests.

## Docs

The chat-apps guide (Matrix section), the generated config reference, the
CLI reference (`connect matrix`, `matrix verify`), and THREAT_MODEL.md
(Matrix's tiers and the verified-device rule).

## Out of scope

Key backup and recovery, cross-signing the operator's whole identity,
voice/video, file attachments, and editing replies in place.
