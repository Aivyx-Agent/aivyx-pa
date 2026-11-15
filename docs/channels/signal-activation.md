# Signal activation check (Launch Task 16)

Signal has been a preview since v0.19.0, tested only against a simulated
signal-cli. This session runs it with real signal-cli and a real phone. If every
step passes, Signal leaves preview in 1.0. If a step fails and can't be fixed
now, Signal stays a preview and the reason goes under **Outcome**.

- **Instance:** `--instance signal-check` (a scratch instance; delete it after).
- **Build:** pa `main`, `cargo build --release -p aivyx-cli`.
- **Numbers:** the assistant's number and the operator's, given in the
  session. They are never written into this file or the repo.
- **Who does what:** the operator uses their phone, the controller runs pa and
  signal-cli, and the Result column is filled in during the session.

Date: ____  pa commit: ____  signal-cli version: ____

| Step | Action | Expected | Result | Notes |
|---|---|---|---|---|
| 1. Daemon flags | Run `signal-cli -a <number> daemon --socket <path from connect signal> --receive-mode manual --trust-new-identities never --ignore-attachments` | Runs; `aivyx-pa --instance signal-check connect signal` says signal-cli answers | | |
| 2. Register | `signal-cli -a <number> register`, then `verify <code>` | The assistant's number is registered | | |
| 3. Safety number | Message the assistant from the phone (one "please verify" reply), then `aivyx-pa --instance signal-check signal verify`; compare with the phone's safety number, type `yes` | The numbers match; verified | | |
| 4. Verified sender turn | `--channel signal`; message from the operator's (now TRUSTED_VERIFIED) number | Reply arrives; "typing…" shown while it works | | |
| 5. Unverified sender | Message from another number, not verified (or not in `allowed_senders`) | Ignored, or one "please verify" reply for an allowed but unverified number; no turn | | |
| 6. `signal.send` | Turn on the `signal` tool process; ask PA to message a `send_to` contact, then one not in `send_to` | Asks for approval each time; sends only to `send_to`; the other is refused | | |

## Outcome

Pass / stays a preview: ____

Fixes made in this session (commit, test): ____
