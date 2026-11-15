# Matrix activation check (Launch Task 14)

Matrix has been a preview since v0.19.0, tested only against a local Conduit
homeserver. This session runs it with a real homeserver and real Element. If
every step passes, Matrix leaves preview in 1.0. If a step fails and can't be
fixed now, Matrix stays a preview and the reason goes under **Outcome**.

- **Instance:** `--instance matrix-check` (a scratch instance; delete it after).
- **Build:** pa `main`, `cargo build --release -p aivyx-cli`.
- **Accounts:** the assistant's account and the operator's own account, given
  in the session. Account names and room IDs are not written into this file.
- **Who does what:** the operator acts in Element, the controller runs pa,
  and the Result column is filled in during the session.

Date: ____  pa commit: ____  Homeserver: ____  Element version: ____

| Step | Action | Expected | Result | Notes |
|---|---|---|---|---|
| 1. Log in | Set `[matrix]` in the instance config (homeserver, user_id, room_id of an **encrypted** room, allowed_senders = the operator), then `aivyx-pa --instance matrix-check connect matrix` | Logs in once; the password isn't kept | | |
| 2. Bootstrap cross-signing | Part of `connect matrix` | The assistant gets its own cross-signing identity; no error | | |
| 3. Verify SAS from Element | `aivyx-pa --instance matrix-check matrix verify`; in Element open the assistant → **Verify**, compare the emoji, confirm on both sides | Both sides show verified; a DM opened by Element is joined only for the emoji exchange | | |
| 4. Encrypted turn | `aivyx-pa --instance matrix-check --channel matrix`; send a message in the room | It joins the room, and replies there, encrypted (Element shows no "unencrypted" mark) | | |
| 5. Non-allowed sender | A second account, not in `allowed_senders`, invited to the room, sends a message | Ignored: no turn, no reply | | |
| 6. Homeserver-added device | Log the operator's account in on a new, **not** cross-signed device (e.g. a second Element session) and send from it | No turn runs; one "please verify" reply, which that device can't decrypt; replies never get keys for that device | | |
| 7. Reply after restart | Ctrl-C the channel, start it again, send a message | Replies; no new device, no re-verify | | |
| 8. `matrix verify` DM rule | During `matrix verify`, invite the assistant to a DM from the allowed sender and from the non-allowed account | Joins only the allowed sender's DM | | |

## Outcome

Pass / stays a preview: ____

Fixes made in this session (commit, test): ____
