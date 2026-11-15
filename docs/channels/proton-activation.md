# Proton Mail activation check (Launch Task 15)

Proton Mail has been a preview since v0.19.0, tested only against a local test
mail server (GreenMail). This session runs it with a real Proton Mail Bridge.
If every step passes, Proton leaves preview in 1.0. If a step fails and can't be
fixed now, Proton stays a preview and the reason goes under **Outcome**.

- **Instance:** `--instance proton-check` (a scratch instance; delete it after).
- **Build:** pa `main`, `cargo build --release -p aivyx-cli`.
- **Channel address:** the operator's separate Proton address, given in the
  session. It is never written into this file or the repo.
- **Who does what:** the operator works Bridge and their mail client, the
  controller runs pa, and the Result column is filled in during the session.

Date: ____  pa commit: ____  Bridge version: ____

| Step | Action | Expected | Result | Notes |
|---|---|---|---|---|
| 1. Split address mode | Bridge in split mode; set `[proton]` (channel_address, own_addresses); `connect proton` (check the certificate fingerprints, enter the Bridge password); `--channel proton`; email the channel address from the operator's address | One reply, in the same thread | | |
| 2. Combined address mode | Switch Bridge to combined mode, `connect proton` again, repeat step 1 | Same as step 1 | | |
| 3. Bridge restart | Restart Bridge while the channel runs, then send another email | The channel reconnects (or stops and says to `connect proton` again) and doesn't crash or lose the mail | | |
| 4. Forged From | Send an email whose From is the operator's address but whose first Authentication-Results doesn't pass DMARC | Refused: no turn | | |
| 5. Autoreply loop | Send from an address with an auto-reply / out-of-office on | At most one reply; no loop | | |
| 6. Non-ASCII search | With the `proton` tool process on, ask for a search with non-ASCII text (e.g. "Café", "Zürich") | Finds the mail | | |
| 7. Operator's own address | A turn sent from the operator's address (in `own_addresses`) | Treated as the operator, and answered | | |
| 8. Stuck email | A mail that fails every time (e.g. one the turn can't process) | Skipped after 5 tries; later mail still answered | | |

## Outcome

Pass / stays a preview: ____

Fixes made in this session (commit, test): ____
