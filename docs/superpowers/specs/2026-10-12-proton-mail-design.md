# Proton Mail channel and tools — design

Status: approved in brainstorming, 2026-10-12. Sub-project 2 of 3 of the
private-channels work (Matrix — done; Proton — this spec; Signal — next).

## Goal

The operator emails their assistant from Proton Mail and gets the answer
back by email, in the same thread; and the assistant can search, read,
draft and send mail in the operator's Proton mailbox. Everything goes
through Proton Mail Bridge's local IMAP/SMTP — Proton's supported route —
so mail is decrypted only on the operator's machine.

## Decisions

- **One mailbox — the operator's own.** Bridge serves the operator's
  account. The tools act on that mailbox; the channel listens only on a
  dedicated address of the operator's (e.g. `you+assistant@proton.me`).
- **Trust:** a turn only for mail from the operator inside Proton, or from
  an allowlisted external address that passes DMARC (below).
- **Architecture:** a new crate `crates/aivyx-proton` holding both the
  channel and the tools (approach A). Proton-specific header logic is
  confined to the policy module so the IMAP/SMTP code stays reusable.
- **Bridge credential:** the OS keyring, with a `0600` file fallback on a
  headless machine — the same posture as pa's master passphrase.

## Architecture

`crates/aivyx-proton`, shaped like `aivyx-gmail` and `aivyx-matrix`:

- `transport.rs` — a `MailTransport` trait (wait for new mail, fetch a
  message's headers and text, mark read, move to a folder, append a draft,
  send) with a real implementation and a scripted double for tests.
- `imap.rs` — async IMAP to Bridge (`imap_addr`, default `127.0.0.1:1143`)
  with IDLE and a 60 s poll fallback; STARTTLS pinned to Bridge's own
  certificate (captured at `connect proton`), never system-trust or
  unverified.
- `smtp.rs` — SMTP to Bridge (`smtp_addr`, default `127.0.0.1:1025`) via
  `lettre` (already in the workspace), same certificate pin.
- `policy.rs` — pure: which messages become turns (Trust), loop guards,
  body extraction rules.
- `proton_channel.rs` + `session.rs` — the `ChannelContext` (reply sent
  once complete) and the in-process receive loop; one conversation per
  email thread.
- Daemon mode — `FrontendType::Proton` in `aivyx-ipc` and
  `aivyx-channel/src/proton_daemon_frontend.rs`, shaped like Matrix's: the
  policy runs in the frontend, so only approved mail reaches the daemon.
- `tools/` + a tool-process binary — `proton.search`, `proton.read`,
  `proton.draft`, `proton.send`, wired via `[[tool_process]]` and sandboxed
  like Gmail.
- `credentials.rs` — the keyring/`0600` storage of the Bridge username,
  bridge password and certificate pin; keyring account names and file
  paths built only through `aivyx-instance` (`InstancePaths`), so each
  named instance has its own.

## Trust

A message becomes a turn — at `SemiTrusted`, like an allowlisted chat on
Telegram, Discord, Slack and Matrix — only when **all** hold:

1. it is addressed to `channel_address` (see 3);
2. either
   - **from you:** its From is one of `own_addresses` (addresses of the
     Bridge account) **and** the same Proton message is also in the
     account's **Sent** folder — i.e. this account really sent it. Found by
     `X-Pm-Internal-Id`, which Bridge always sets to Proton's own message
     ID (overwriting any sender-supplied copy). Folder membership is
     account metadata, not a header: a forged "From: you" from outside can
     never be in your Sent folder. Mail from your *other* Proton accounts
     is not "from you" here; list those addresses in
     `allowed_external_senders` instead;
   - **allowlisted:** its From is in `allowed_external_senders` **and**
     Proton's own DMARC result passes for that From's domain. Proton's
     receiving server adds one `Authentication-Results` value per check
     (`dmarc`, `spf`, `arc`, `dkim`), each stamped `mail.protonmail.ch`,
     ahead of any value the sender wrote; Bridge preserves the order of a
     header's values. So: take the `Authentication-Results` values in
     order, keep the leading run stamped `mail.protonmail.ch`, and use the
     **first** `dmarc=` result in it — it must be `dmarc=pass` with
     `header.from` equal to the From address's domain (case-insensitive).
     No Proton DMARC result in that run → not trusted. A sender-written
     `mail.protonmail.ch; dmarc=pass` line can only come after Proton's own
     result and is never the first.
3. **addressed to the channel:** `channel_address` is matched against
   `Delivered-To` / `X-Original-To`, which Proton's receiving server sets
   for the actual delivery — not `To:`, which the sender writes.

Planning research (2026-10-12, Proton Bridge source at b9c5dac): Bridge
serves Proton's stored headers (`ParsedHeaders`) in their original order
(`pkg/message/build.go` `toMessageHeader`), always sets
`X-Pm-Internal-Id` to the Proton message ID (`getMessageHeader`), and does
not expose Proton's internal/end-to-end message flags over IMAP — so
`X-Pm-Origin`-style headers are deliberately **not** relied on. Confirmed on the operator's account (2026-10-12, a received external
message's headers, recorded with the address redacted as the fixture
`docs/superpowers/specs/fixtures/proton-external-dmarc-pass.headers`, copied into the crate's test fixtures by the plan):
Proton's authserv-id is `mail.protonmail.ch`; it adds separate
`dmarc` / `spf` / `arc` / `dkim` results, with DMARC first and recording
the policy (`dmarc=pass (p=reject dis=none) header.from=<domain>`); and it
sets `Delivered-To` / `X-Original-To`. Still to confirm on the operator's
account before the channel's trust code is written: mail you send to your
own `+assistant` address appears in both INBOX and Sent. If not, the plan
stops and this section is revisited.

Both kinds of sender run at the same tier: daemon mode fixes the tier per
connection. Everything else is **ignored silently** — no auto-reply, which
would confirm the address and create backscatter to forged senders.

DMARC proves the external sender's domain authorised the message; it does
not prove the person, and the reply to an external address leaves Proton
as ordinary email. THREAT_MODEL.md says so.

## Mail flow

- **Start:** the first run begins at "now" (the inbox's next UID), so
  existing mail is never processed. The last handled UID is remembered
  (instance data dir), so a restart neither repeats nor misses mail.
- **Input:** the `text/plain` part, else HTML converted to text; quoted
  history stripped; subject included; capped at 32 KiB (the reply says if
  it was cut). Attachments are not read; the reply says they were ignored.
- **Conversations:** one per thread (`References` / `In-Reply-To` root).
- **Reply:** sent once complete through Bridge's SMTP, from
  `channel_address` to the sender, as `Re: <subject>` with `In-Reply-To` /
  `References`, so it threads in Proton Mail. To the operator's own Proton
  address it stays end-to-end encrypted inside Proton.
- **Handled mail:** marked read and moved, with the assistant's reply, to
  `folder` (default `Aivyx`, created if missing).
- **Commands:** `/team …` on the email's first line, gated exactly as on
  the other channels. No `/cancel` (an email arrives after the turn is
  usually over).
- **Loop guards:** never a turn for mail from `channel_address`, or with
  `Auto-Submitted` (other than `no`) or `Precedence: bulk/list/junk`.

## Tools

| Tool | Capability | What it does |
|---|---|---|
| `proton.search` | `email.read` | Search All Mail by from / to / subject / date range / text; returns a capped list (id, from, subject, date, snippet). |
| `proton.read` | `email.read` | One message: headers and text body, capped; attachments listed by name only. |
| `proton.draft` | `email.write` | Save a draft to Drafts — new, or a reply to a message id. |
| `proton.send` | `email.send` | Send via Bridge SMTP — new or a threaded reply; pauses for approval every call, like `gmail.send`. |

Minimum tier matches Gmail's (`Trusted`) — so, as with `gmail.*` on
Telegram, the mail tools are not available inside an email turn
(`SemiTrusted`); the channel and the tools are separate reaches. `search` and `read` mark their
output untrusted, so email content passes the injection scan (Picket) and
fencing (Bulwark) before the model sees it.

## Config — `[proton]`

`imap_addr`, `smtp_addr` (loopback only: `127.0.0.1`, `localhost`, `[::1]`),
`channel_address`, `own_addresses`, `allowed_external_senders`, `folder`,
and the shared `team_run_channel`, `team_trigger_rate_limit`,
`team_command_allowed_senders`. Validation fails loudly on a non-loopback
address, a malformed address, or an empty `own_addresses`. The Bridge
credential and certificate pin are never config.

## Setup

1. Install Proton Mail Bridge, sign in, and copy its IMAP/SMTP username and
   bridge password (Bridge → account → mailbox details).
2. Add `[proton]` (and the `[[tool_process]]` entry for the tools).
3. `aivyx-pa connect proton` — asks for the Bridge username and password,
   captures and pins Bridge's certificate, tests IMAP and SMTP, and stores
   the credential in the keyring (or the `0600` fallback).
4. `aivyx-pa --channel proton`.

## Errors

- Bridge unreachable: reconnect with backoff capped at 60 s, logged.
- Bridge rejects the credential, or its certificate changed: the channel
  stops with "run `aivyx-pa connect proton` again" — never a retry with an
  unpinned certificate.
- Reply send failure: logged and audited; the original stays unread in the
  inbox (not moved), so the operator sees it went unanswered.
- `[budget]` and `[rate_limit]` apply as on every channel.

## Testing

- Unit tests against the scripted transport: every Trust combination
  (including a forged "From: you" not in Sent, forged lower
  `Authentication-Results`, and a missing Proton result), loop
  guards, threading, body extraction and caps, the folder move, a failed
  send leaving mail in place, `/team` gating, restart resume.
- Recorded real-Bridge header samples as fixtures for the policy parser.
- An `#[ignore]`d live test against GreenMail (IMAP/SMTP) in Docker: IDLE,
  fetch, send, threading, folder move end to end.
- A short manual checklist against the operator's real Bridge (Bridge
  needs a paid account and can't run in CI): one email to yourself answered (and found in
  Sent), one external allowlisted email answered, one forged external
  email ignored, Proton's `Authentication-Results` sample recorded, one `proton.search`, one
  `proton.send` with approval.

## Docs

The chat-apps guide (Proton section: Bridge, the `+assistant` address),
the generated config reference, the CLI reference (`connect proton`,
`--channel proton`), TOOLS.md (`proton.*`), and THREAT_MODEL.md (the
email trust rule and what DMARC does and doesn't prove).

## Out of scope

Reading or sending attachment content, HTML replies, Proton Calendar /
Drive / Pass, PGP to external recipients.
