# Security and privacy

An assistant that can read your files, run commands and send messages has
to be trustworthy by construction, not by promise. This page explains, in
plain words, what protects you — and, just as important, what doesn't.

## Where your data goes

- **Nowhere you didn't choose.** There is no Aivyx PA server. Your memory,
  settings and history stay on your machine.
- **To your model provider, if it's in the cloud.** Whatever goes into a
  conversation is sent to the model that answers it. With a local model
  (Ollama, llama.cpp, Lemonade, Jan) nothing leaves your machine. With
  Anthropic or OpenAI, your messages and the tool results the assistant
  reads go to them, under your account and their terms.
- **With routing on**, a conversation that has touched your own data —
  mail, files, memory and so on — stays on local models; going to a cloud
  model needs your say-so per conversation. See
  [Models and routing](12-models-and-routing.md).

## Encrypted at rest

Memory, persona, skills, missions, the audit log and saved API keys live in
one encrypted file (the *store*), locked with your **passphrase** using
modern, well-reviewed cryptography (Argon2id, HKDF, ChaCha20-Poly1305).
Without the passphrase the file is unreadable — including to you, so keep
it safe. Aivyx PA can't recover it.

The passphrase itself is kept in your operating system's keyring, or, where
there isn't one, in a file only your user account can read.

## Permission for every action

Every tool call is checked **before** it runs:

- **Capabilities.** Each tool needs a specific permission — reading files,
  sending email, running commands. Your assistant's role grants a set; a
  call outside it is refused.
- **Trust tiers.** Where a message comes from caps what it can ever do.
  Your own terminal and Studio are trusted; a chat app gets less; a chat
  that isn't on your allowlist gets almost nothing.
- **Access level.** How far file and shell tools reach — from one sandbox
  folder up to the whole machine. See [Access and settings](08-access-and-settings.md).
- **Approval.** Irreversible actions stop and ask you. Unattended runs
  refuse them. The assistant can never approve for you, and never raise its
  own access or autonomy.

## A record you can check

Everything the assistant does — and everything it was refused — is written
to an **audit log** in which each entry is cryptographically chained to the
one before. Change or remove an entry and the chain breaks visibly.
`aivyx-pa --verify-only` checks the whole chain; the **Audit** screen
shows it.

## Guards against specific dangers

Some protections have names you'll see in the docs and in messages from the
assistant:

| Guard | What it stops |
|---|---|
| **Ward** | Reading your secrets — SSH keys, cloud credentials, `.env` files, password files — and Aivyx PA's own store and tokens, at any access level. You can allow specific files (`allow_sensitive_paths` under `[access]`). |
| **Portcullis** | Writing to places that would let something run later without you: `authorized_keys`, shell start-up files, cron and systemd jobs, git hooks. |
| **Rampart** | Network tools reaching your local network, your router, or cloud metadata services — the classic way a web page tricks a server into attacking its own network. |
| **Bulwark** | Web pages, emails and documents being taken as instructions. Their content is handed to the model clearly marked as *data*. |
| **Picket** | Known prompt-injection phrasing in that content: a match stops the turn for your review. |
| **Confinement** | On Linux, commands the assistant runs are confined by the kernel (Landlock and seccomp) to the folders they're allowed. Integrations run as separate processes, sandboxed with bubblewrap or firejail when one is installed (setup turns this on). |
| **Gatehouse** | Exposing the Studio beyond your own machine without a sign-in token. |

The Studio itself only listens on your own machine by default and needs a
sign-in token, so other user accounts on the same computer can't use it.

## What isn't protected

No security design covers everything. Aivyx PA deliberately does **not**
defend against:

- **A compromised computer.** If someone controls your user account or the
  operating system, they can read what the assistant can.
- **Your model provider.** A cloud provider sees what you send it. Use a
  local model for anything you wouldn't send.
- **Every prompt injection.** Fencing and scanning make it much harder for
  a web page or email to steer the assistant, but can't make it impossible.
  That's why consequential actions need your approval, whatever the model
  was told.
- **MCP servers you add.** They're programs you chose to run; sandbox them
  (`[mcp_server.sandbox]`) if you don't fully trust them.
- **The chat platforms.** If your Telegram, Discord or Slack account is
  compromised, so is that channel — keep the allowlist tight.
- **Other assistants on the same account.** [Named instances](16-named-instances.md)
  can't read each other's stores, but they aren't separate operating-system
  users.

The full, technical account is the
[threat model](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/THREAT_MODEL.md).
To report a vulnerability, see
[SECURITY.md](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/SECURITY.md).
