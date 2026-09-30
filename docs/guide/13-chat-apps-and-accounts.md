# Chat apps and accounts

Your assistant can live in more places than this Studio: you can message it
from **Telegram, Discord or Slack**, and give it your **Gmail, Google Calendar,
Drive or Contacts**. Each is opt-in, set up once, and uses credentials you
create yourself — nothing goes through an Aivyx PA server.

## Chat apps: Telegram, Discord, Slack

A chat app is one more window onto the same assistant: same memory, same
personality, same audit log. Risky actions still stop for your approval — you
answer with `/approve` or `/reject` right in the chat.

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

### Discord and Slack

Both follow the same pattern with a few more steps on the chat app's side
(Discord needs the *Message Content* intent turned on; Slack needs Socket Mode
and two tokens). The
[install guide](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/INSTALL.md#running-aivyx-pa-on-discord-phase-107)
walks through each click, and the allowlist setting is `channel_filter` instead
of `chat_id`.

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
