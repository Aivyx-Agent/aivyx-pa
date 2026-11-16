# Federation: two-machine checklist (A19)

Run on two real machines on one tailnet (or LAN), each with its own PA
install and its own store: here `desktop` and `laptop`. Follow
[`FEDERATION_SETUP.md`](../FEDERATION_SETUP.md) for the setup. Use test
instances (`--instance fedtest`) if the machines run your everyday PA.
Record the date, both PA versions and the network at the top when run.

Passing every step removes federation's `(preview)` entries
(`crates/aivyx-config/src/not_stable.rs`, `docs/COMPATIBILITY.md`, the
manual's reference pages and `docs/TOOLS.md`).

Date: ____  PA versions: ____ / ____  Network: ____

- [ ] 1. With the daemon stopped, `aivyx-pa federation card` on each
      machine asks for the id the first time and prints
      `aivyx-peer-v1:…`. With the daemon running, a first `card` on a
      third, fresh instance says "stop the daemon first".
- [ ] 2. `peer add` both ways: on the desktop
      `peer add laptop "<laptop card>" --address <laptop>:7650 --verbs chat,task --scopes fs.read,fs.metadata`;
      on the laptop `peer add desktop "<desktop card>" --address <desktop>:7650`.
      Each prints `Added peer … (key <fingerprint>)`, and the fingerprint
      matches the one `card` printed on the other machine. Pasting the
      laptop's card as `peer add desktop …` is refused ("it is for the
      instance `laptop`").
- [ ] 3. Set `[federation] listen` to each machine's tailnet address and
      restart both daemons. Each log shows
      `aivyx-pa federation: listening on <address>`. `listen = "0.0.0.0:7650"`
      without `listen_any = true` is refused at start.
- [ ] 4. `aivyx-pa federation ping` both ways prints `ok` for every step.
      With the other daemon stopped, ping fails at `connect` with the
      hint about `[federation] listen`.
- [ ] 5. From a Studio or terminal chat session on the laptop, ask the
      assistant to chat with the desktop. The laptop asks before it sends
      (`federation.chat`, `assisted`), then shows the desktop's reply.
- [ ] 6. From the laptop, ask the desktop for a task that lists a folder
      in the desktop's sandbox. On the desktop, `aivyx-pa review` shows
      `Run a task from peer laptop: …` as waiting; approve it. The laptop
      gets the file list.
- [ ] 7. Ask again and deny the step on the desktop: the laptop gets
      `refused by the operator`. Ask the laptop for a task from the
      desktop: `verb not allowed for this peer`.
- [ ] 8. With the laptop's daemon stopped, `aivyx-pa federation send
      desktop chat "…"` prints a reply. With it running, `send` says
      "stop the daemon or use the federation tools from a session".
- [ ] 9. `aivyx-pa audit export --event-type federation_crossing` on each
      machine shows every request from steps 5 to 8 (`outbound` on the
      sender, `inbound` on the receiver, `relayed` or `denied` with the
      reason, no text). `aivyx-pa --verify-only` passes on both.
- [ ] 10. Restart the desktop's daemon while a task waits for approval:
      the laptop gets an error, the step no longer waits after the
      restart, and nothing runs on the desktop.
- [ ] 11. Ask the laptop's assistant to `federation.search` the desktop
      after allowing `search`: only skill names, triggers and steps come
      back.

Failures go back to the task that owns them before the preview tag comes
off.
