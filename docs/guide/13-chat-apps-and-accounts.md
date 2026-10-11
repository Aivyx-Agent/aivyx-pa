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
