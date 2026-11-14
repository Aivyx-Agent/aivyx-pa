# Card unlock: real-card checklist (A18)

Run on a machine with a real YubiKey, `pcscd` running, the helper installed
(`aivyx-pa tool-programs install`) and a test instance (`--instance cardtest`).
Record the date, card model and PA version at the top when run.

Date: ____  Card: ____  PA version: ____

- [ ] 1. Enroll a fresh card (let `card add` generate the key), and a card that
      already has a decryption key. `card list` shows both.
- [ ] 2. Make sure no passphrase is saved (`card add` removed it, and
      `AIVYX_PA_PASSPHRASE` / `[storage] passphrase` are unset). Restart the
      service. `aivyx-pa daemon status` shows `locked: yes`.
      `aivyx-pa unlock` (PIN and touch) then shows `locked: no`.
- [ ] 3. `aivyx-pa lock`, then unlock again. After the `lock`, `ps` shows the
      same daemon pid and no tool or MCP child process from before the lock
      survives (compare `ps --ppid <pid>` before and after).
- [ ] 4. Unplug the card and run `aivyx-pa unlock --passphrase`.
- [ ] 5. Enter the wrong PIN twice (the tries left are shown), then the right
      one.
- [ ] 6. Don't touch the card for 15 seconds: "No touch — still locked."
- [ ] 7. `aivyx-pa card remove <serial>`, including the last-card phrase
      (`I know my passphrase`).
- [ ] 8. Ask PA's `fs.read` tool to read `~/.config/aivyx-pa/card-unlock.toml`:
      it is refused (Ward).

Failures go back to the task that owns them before release.
