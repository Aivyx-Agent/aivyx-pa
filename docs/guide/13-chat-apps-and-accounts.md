# Chat apps and accounts

Your assistant can live in more places than this Studio: you can message it
from **Telegram, Discord or Slack**, and give it your **Gmail, Google Calendar,
Drive or Contacts**. Each is opt-in, set up once, and uses credentials you
create yourself — nothing goes through an Aivyx PA server.

## Chat apps: Telegram, Discord, Slack

A chat app is one more window onto the same assistant: same memory, same
personality, same audit log. Risky actions still stop for your approval: the
assistant tells you what it wants to do, and replying "yes" in your next
message lets it go ahead once. (Team missions have their own commands —
`/team approve`, `/team reject` and so on — which only senders you list in
`team_command_allowed_senders` may use.)

Setting one up has three parts:

1. **Make a bot** in the chat app and copy its token.
2. **Give aivyx-pa the token and your chat's id.** The id is the important
   part: messages from an allowlisted chat get your normal trust level, while
   messages from anywhere else are treated as strangers and can do almost
   nothing. Leave it unset only if the bot is truly private.
3. **Start the bot:** `aivyx-pa --channel telegram` (or `discord`, `slack`).
   The bot runs for as long as that command does — it connects to your
   running daemon, so keep it open (or run it as its own service).

Saving a token in the Studio (**Notifications → Channel adapters**) stores it,
but doesn't start the bot by itself — step 3 does.

### Telegram

1. In Telegram, message **@BotFather**, send `/newbot`, and follow the prompts.
   It gives you a token like `123456:ABC…`.
2. Send your new bot any message, then open
   `https://api.telegram.org/bot<your token>/getUpdates` in a browser. Your
   chat id is the number under `"chat": {"id": …}`.
3. Add both to your config (`~/.config/aivyx-pa/aivyx-pa.toml`):

   ```toml
   [telegram]
   token = "123456:ABC…"
   chat_id = 123456789
   ```

4. Run `aivyx-pa --channel telegram` and message your bot.

Keep `chat_id` set: it's the allowlist. Without it, anyone who finds your bot
can talk to it — only at the `Untrusted` tier, which can read nothing but
public memory, but on your model and your budget.

### Matrix

Matrix gives you an end-to-end-encrypted room with your assistant, on a
homeserver you choose (or run yourself).

1. Create a Matrix account for the assistant (e.g. `@my-assistant:matrix.org`),
   and from your own account create a room, **turn on encryption**, and invite
   it. Copy the room's ID (`!…:server`, in the room's advanced settings). Use
   the ID, not an alias like `#room:server`.
2. Add to your config:

   ```toml
   [matrix]
   homeserver = "https://matrix.org"
   user_id = "@my-assistant:matrix.org"
   room_filter = "!abcdef:matrix.org"
   allowed_senders = ["@you:matrix.org"]
   ```

3. `aivyx-pa connect matrix` — logs the assistant in once (the password isn't
   kept).
4. `aivyx-pa matrix verify` (stop `aivyx-pa --channel matrix` first if it's
   running) — then,
   in your Matrix app, open the assistant and choose **Verify**; compare the
   emoji and confirm on both sides. Once is enough: your other cross-signed
   devices are trusted too.
5. `aivyx-pa --channel matrix`. The assistant accepts your invite and joins
   the room once the channel is running, not before. It declines invites to
   any other room.

It answers only you (`allowed_senders`), only in that room, and only from
devices you've verified — a stolen password or the homeserver can't
impersonate you. From an unverified device it tells you once to verify, and
a message it can't decrypt gets one "I couldn't read that" reply; everyone
else is ignored.

Good to know:

- **Your account needs cross-signing.** Element and other modern clients set
  it up by default. `connect matrix` gives the assistant its own
  cross-signing identity, and verifying signs *your* identity with it — so
  you verify once, and every device you've cross-signed (including ones you
  add and cross-sign later) is trusted. A device you haven't cross-signed is
  refused: no turn runs, and the assistant replies once asking you to
  verify. If your account has no cross-signing, `matrix verify` says so and
  fails.
- **Verifying from Element may open a DM** with the assistant (Element
  sends the request there). While `matrix verify` runs, the assistant joins
  a DM you invite it to, just to exchange the emoji; it only ever answers in
  the home room.
- **If you reset your Matrix cross-signing identity, run `aivyx-pa matrix
  verify` again.**
- **Replies go only to your cross-signed devices.** Someone who adds a
  device to your account (with your password, or a hostile homeserver)
  can't read them. It also means a device you haven't cross-signed sees
  the assistant's messages — including the "please verify" one — as
  "unable to decrypt"; cross-sign it from one of your other devices.
- **Encrypted rooms only.** The assistant refuses to start if the
  `room_filter` room isn't encrypted.
- **Homeserver address.** It must be `https://`. Plain `http://` is accepted
  only on this machine (`localhost`, `127.0.0.1`, `[::1]`).
- **Running `connect matrix` again creates a new device.** The old device's
  encrypted store is moved aside to `store.old-<timestamp>` in the instance's
  Matrix data directory, not deleted, and you must verify the new device with
  `aivyx-pa matrix verify`. A failed login (wrong password, unreachable
  server) leaves your existing setup untouched.
- **One process at a time.** Only the `aivyx-pa --channel matrix` process
  opens the assistant's Matrix store (the daemon never does, in either
  mode). While it runs, `connect matrix` and `matrix verify` refuse with
  "the Matrix channel is running in another process": stop it, run them,
  then start `aivyx-pa --channel matrix` again so it uses the new device.
  The daemon can stay up.
- **`/cancel` with the daemon.** When `--channel matrix` runs through the
  daemon, `/cancel` is read between turns, so it can't stop a turn already
  running (same as Discord). Run without the daemon, it cancels the
  current turn. `/team` and gate commands work only through the daemon.
- **If the device is logged out** (say, from your Matrix app), the channel
  stops and tells you to run `aivyx-pa connect matrix` again. It doesn't
  retry.

### Proton Mail

Email your assistant from Proton Mail and get the answer back in the same
thread — and let it search, read, draft and send in your mailbox. It all goes
through [Proton Mail Bridge](https://proton.me/mail/bridge) on this machine
(a paid Proton plan), so your mail is decrypted only here.

1. Install Proton Mail Bridge and sign in. Add an address to your account for
   the assistant (Proton → Settings → Identity and addresses), e.g.
   `you_aivyx@proton.me`; a `+` alias (`you+assistant@proton.me`) also works.
2. Add to your config:

   ```toml
   [proton]
   channel_address = "you_aivyx@proton.me"
   own_addresses = ["you@proton.me", "you_aivyx@proton.me"]
   # allowed_external_senders = ["you@work.example"]
   ```

3. `aivyx-pa connect proton` — shows Bridge's certificate fingerprints and
   pins them, asks for the username and password **Bridge** shows you
   (Bridge → your account → mailbox details; not your Proton password),
   checks them, and saves them where only you can read them.
4. `aivyx-pa --channel proton`, then email `you_aivyx@proton.me`.
5. For the tools (`proton.search`, `proton.read`, `proton.draft`,
   `proton.send`), add this and restart the daemon:

   ```toml
   [[tool_process]]
   name = "proton"
   command = "aivyx-proton"
   ```

It answers mail sent to `channel_address` from **you** — one of
`own_addresses`, and only if the message really is in your Sent folder, so a
forged "From: you" from outside is ignored — or from an address in
`allowed_external_senders` that passes Proton's own DMARC check. Everything
else is ignored without a reply.

Good to know:

- **Handled mail is filed**: marked read and moved, with a copy of the reply,
  to the `Aivyx` folder (`folder` changes it). If a reply can't be sent, it
  isn't retried: the email stays unread in your inbox.
- **It starts from now.** Mail already in your inbox when the channel first
  runs is never answered; after a restart it carries on where it stopped. If
  you stop the channel during a turn: without the daemon, that email is handled
  again (the turn runs again) after the next start; through the daemon, the
  running turn finishes and is answered first.
- **One email, one turn.** The new part of the message is read (quoted
  history is dropped), up to 32 KiB; attachments aren't opened. The reply
  says when it cut or ignored something. An email the assistant can't read
  is skipped, and so is one that keeps failing to be checked (5 tries; the
  channel logs it).
- **`/team` and gate commands** go on the first line of the email, and work
  only through the daemon. Gate commands are honoured only from you (or an
  address in `team_command_allowed_senders`). There's no `/cancel`.
- **The mail tools aren't available inside an email turn** — like Gmail's,
  they need the trusted terminal or Studio. `proton.send` asks you to approve
  every send, unless you turned off confirmations (`[access]
  confirm_destructive = false`).
- **External senders**: DMARC proves the sender's domain sent the message,
  not who typed it, and replies to them leave Proton as ordinary email. Mail
  from another Proton account may carry no DMARC result and then isn't
  answered.
- **If you reinstall or reset Bridge**, its certificate changes and the
  channel stops: run `aivyx-pa connect proton` again. Same if you change
  `imap_addr` or `smtp_addr`.
- **One channel process at a time** per instance.

### Signal

Message your assistant on Signal and get the answer back there — and let it
send a Signal message to a contact you list, with your approval each time.
The assistant has **its own Signal number**; it talks through
[signal-cli](https://github.com/AsamK/signal-cli), which you run yourself, so
pa never holds a Signal credential.

1. Get a number for the assistant (a spare SIM, or a VoIP number that can
   receive an SMS or call) and register it with signal-cli (it needs Java 25):

   ```
   signal-cli -a +15559990000 register        # add --captcha … if asked
   signal-cli -a +15559990000 verify 123-456  # the code you received
   ```

2. Run signal-cli as a daemon. A systemd user unit,
   `~/.config/systemd/user/signal-cli.service` (adjust the number and, for a
   named instance, the socket path — `connect signal` prints the right one):

   ```ini
   [Unit]
   Description=signal-cli for Aivyx PA

   [Service]
   ExecStart=signal-cli -a +15559990000 --trust-new-identities never daemon --socket %h/.aivyx-pa/tool-processes/signal/signal-cli.sock --receive-mode manual --ignore-attachments
   Restart=on-failure

   [Install]
   WantedBy=default.target
   ```

   then `systemctl --user enable --now signal-cli`. Keep signal-cli up to date
   — Signal stops accepting releases older than about three months.

3. Add to your config:

   ```toml
   [signal]
   account = "+15559990000"          # the assistant's number
   allowed_senders = ["+15550001111"] # your number
   # send_to = ["+15550002222"]       # contacts signal.send may message
   ```

4. `aivyx-pa connect signal` — checks signal-cli answers (and tells you how
   to start it if not).
5. Send the assistant a message from your phone, then `aivyx-pa signal
   verify`: it shows the safety number; compare it with your phone (the chat
   with the assistant → View safety number) and type `yes`.
6. `aivyx-pa --channel signal`.
7. For `signal.send`, add this and restart the daemon:

   ```toml
   [[tool_process]]
   name = "signal"
   command = "aivyx-signal"
   ```

It answers only you (`allowed_senders`), and only once you've verified your
safety number — a SIM-swapped or re-registered number can't drive it. With an
unverified (or changed) number it replies once asking you to verify; everyone
else, and group messages, are ignored.

Good to know:

- **Messages aren't lost while the channel is off.** signal-cli fetches them
  only for the channel; they wait on Signal's server until it runs.
- **One conversation per sender.** A message is read up to 4 KiB;
  attachments aren't downloaded (the reply says it ignored them). Long
  replies arrive as several messages. You see "typing…" while it works.
- **Disappearing messages:** signal-cli applies the chat's timer to the
  assistant's replies.
- **`/cancel`** stops the current turn when the channel runs without the
  daemon; through the daemon it's read between turns. `/team` and gate
  commands work only through the daemon.
- **`signal.send`** only messages numbers in `send_to`, needs the trusted
  terminal or Studio (it's not available inside a Signal turn), and asks you
  to approve every send (unless you turned off confirmations with `[access]
  confirm_destructive = false`). Each contact's safety number must be
  verified too (`signal verify` covers `send_to`); signal-cli learns a
  contact's key once they've messaged the assistant or after a first send
  attempt. Run `aivyx-pa connect signal` again after changing `send_to`.
- **If the number is unregistered** (re-registered elsewhere), the channel
  stops and tells you to re-register it with signal-cli. If signal-cli
  isn't running, the channel waits for it.
- **One channel process at a time** per instance.

### Discord and Slack

Both follow the same pattern with a few more steps on the chat app's side
(Discord needs the *Message Content* intent turned on; Slack needs Socket Mode
and two tokens). The
[install guide](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/INSTALL.md#running-aivyx-pa-on-discord-phase-107)
walks through each click, and the allowlist setting is `channel_filter` instead
of `chat_id` — the same advice applies.

## Google accounts: Gmail, Calendar, Drive, Contacts

```sh
aivyx-pa connect            # what's connectable, and what's connected
aivyx-pa connect gmail      # set one up (or calendar, drive, contacts)
```

`connect` walks you through it. Google requires you to create your own
**OAuth client** once (about five minutes, free) in the Google Cloud Console:

1. Pick or create a project, and enable the API for the service (e.g. the
   *Gmail API*).
2. Set up the **OAuth consent screen** — any app name, your email, audience
   *External*.
3. Under **Audience → Test users**, add your own Google account. Skip this and
   Google refuses the sign-in.
4. Create an **OAuth client ID** of type **Desktop app**, and paste its
   Client ID and Client secret into `connect`.

`connect` then opens Google's sign-in in your browser and saves the sign-in
under `~/.aivyx-pa/tool-processes/`, readable only by your user account.
Pressing Enter on a blank Client ID cancels without changing anything. The
other Google services can reuse the same OAuth client — just enable their APIs
in the same project.

Google will warn that the app "hasn't been verified". That's expected: it's
*your* app, used only by you.
