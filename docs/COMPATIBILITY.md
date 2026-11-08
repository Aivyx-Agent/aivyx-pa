# Compatibility

From 1.0, Aivyx PA follows semantic versioning for its stable surface.

## Stable

- **Config keys and their defaults.** Every key path the config
  accepts, with its type, the values a key that takes one of a fixed set
  accepts, and the aliases it accepts (snapshot: `docs/stable/config.txt`),
  and the config an empty file resolves to (snapshot:
  `docs/stable/defaults.txt`, with the defaults `[memory] profile`
  changes listed for `lite` and `smart`). A default follows its key's
  tag in `config.txt` (an `(advanced)` key's default is not promised
  either), and the `(movable)` defaults may change in a minor release
  (see Not stable).
- **The CLI commands and flags.** Each command and its usage lines, the
  global flags (`--instance`, `--provider`, `--mcp`, ...) and the old flag
  spellings still accepted. Snapshot: `docs/stable/cli.txt`.
- **The environment variables.** The names the config loader reads
  (`AIVYX_PA_*`, plus `ANTHROPIC_API_KEY`) and what each one sets.
  Snapshot: `docs/stable/env.txt`. The variables other parts read (the
  desktop app, one integration or tool) are listed in the environment
  variables reference but are not in the snapshot.
- **The instance layout on disk.** Versioned by a `layout-version` file
  in each instance's data directory (`instance.layout_version` in
  `docs/stable/formats.txt`). An instance from before the file is
  version 1; the file is written when the instance is created and the
  first time an existing one starts. An instance whose layout is newer
  than the binary knows is refused, with a message naming the file.
- **The data stores.** Any 1.x reads data written by 1.0. The snapshot
  records the store versions: the table names, the record version of
  each table, the key-derivation salt, the audit chain's format, and the
  schema versions of the toolkit stores and the Google token file. See
  `docs/stable/formats.txt` for the list. The record versions live in
  the store's `aivyx_schema_versions` table and the audit chain's format
  in a header record beside the chain (outside the hash chain, so
  existing chains verify unchanged). A store or chain from before these
  versions is version 1 and gets them on its next open; one newer than
  the binary knows is refused.
- **The pack formats.** Formats 1 (tool packs) and 2 (config-only packs).
  Snapshot: `docs/stable/formats.txt`. A format 1 pack is stable as a
  bundle (it installs and verifies in every 1.x), but the tool protocol
  its tools speak (`tool.protocol_version`, tagged `(not stable)`) and
  the `aivyx-vertical-sdk` crate they are built with are not stable in
  1.0: a tool built against them may need a rebuild in a minor release.
  They will be stabilised in a 1.x minor release once a real tool pack
  exists.
- **Webhook payloads.** The field paths of the notify payload (which
  carries `"version": 1`) and of the trigger reply. Snapshot:
  `docs/stable/formats.txt`.
- **The `aivyx-pa audit export` lines.** Each JSONL line carries
  `"version": 1` (`audit_export.version`) and the fields listed in
  `docs/AUDIT_EXPORT.md`.

## Not stable

Some lines of the snapshots in `docs/stable/` end with a tag in
parentheses. A tagged line is listed so that a change to it is still
deliberate, but it is not part of the promise. The tags are below; the
one list of tagged entries is `NOT_STABLE` in
`crates/aivyx-config/src/not_stable.rs`.

- **Advanced learning and recall settings**, tagged `(advanced)`. The
  tuning keys of the learning system, which keep changing as it does:
  `[persona.auto_propose]`, `[skills.auto_propose]`,
  `[persona_lifecycle]`, `[persona_consolidation]`,
  `[correction_consolidation]`, `[correction_judgment]`,
  `[correction_signal]`, `[recall_judgment]`, `[recall_feedback]`,
  `[recall_cluster]`, `[skill_refinement]`, `[skill_authoring]`,
  `[tool_relevance]`, `[providers]`, `[wiki]`, `[graph]`, the
  `[embedding]` keys `recall_*`, `rag_*` and `ann_*`, and
  `[ollama] prompt_strategies`. Each section's `enabled` switches stay
  stable, and so does `[memory] profile`, the setting that turns the
  learning features on as a set.
- **Build-time features**, tagged `(build-time feature)`. What only a
  build with a non-default Cargo feature has; no release binary does:
  the `[mistralrs]` section, `agent.provider = "mistralrs"` and
  `--provider mistralrs` (`provider-mistral-rs`), the `[voice]` section
  and `--channel voice` (`channel-voice`), and
  `aivyx-pa federation yubikey-init` (`yubikey`). The last is not in
  `cli.txt` at all, because the help shows it only in a build with the
  feature.
- **Previews**, tagged `(preview)`. Today these are the Matrix, Proton
  Mail and Signal channels: their config sections, `aivyx-pa matrix
  verify`, `aivyx-pa signal verify` and their `--channel` values. A
  preview can change or be removed in any release. A channel's tag comes
  off when it has been tested with a real client.
- **Formats not promised yet**, tagged `(not stable)` in `formats.txt`: the
  tool protocol version (`tool.protocol_version`; see The pack formats
  above).
- **Movable defaults**, tagged `(movable)` in `defaults.txt`: the default
  model (`agent.model`), the default system prompt (`agent.system_prompt`
  and the built-in `default` role's), and the starter persona seed
  (`[persona_seed]`). They may change in a minor release, as better
  models and prompts arrive. A value you set yourself always wins over
  them, and `aivyx-pa init` writes your own choice of model into the
  config, so an existing setup does not move.
- **Developer commands**, tagged `(developer)`: the help's Developer
  group (`aivyx-pa tool-relevance`) and `--print-role`. (`--verify-only`,
  the forensic check, is stable.)
- **The terminal chat's look**, and keys beyond the documented ones.
- **Internal crates.** The crates in this repository are not a public API,
  `aivyx-vertical-sdk` included.
- **The Studio's HTTP API.** It belongs to the bundled Studio and changes
  with it.
- **The daemon IPC and tool protocols.** The IPC protocol runs between
  pa's own processes; the tool protocol is the one a format 1 pack's
  tools speak (see The pack formats above). Their versions are recorded
  in `docs/stable/formats.txt`, so a change is visible.
- **The tools the model sees.** Their names, arguments and descriptions
  change as models change.

## Rules

- A breaking change to the stable surface is a new major version.
- A minor release may add to it and deprecate parts of it. A deprecated
  item keeps working for at least one minor release and warns once when
  used.
- A config key is never silently given a new meaning; a changed meaning
  gets a new key.
- Data written by 1.0 is read by every 1.x; migrations run forward on
  their own and say so.

## How this is checked

`docs/stable/` is generated from the code; a test fails when it changes.
The tests are the `stable_config_keys` tests (`cargo test -p aivyx-config`;
they write `config.txt`, `defaults.txt` and `env.txt`) and the
`stable_` tests in `aivyx-cli`. A change to the snapshot has to be made on
purpose.

The check has limits:

- It freezes config key paths, their types, and the values a key that
  takes one of a fixed set accepts (for example, the access levels),
  with their accepted alternative spellings. `defaults.txt` freezes the
  defaults as the loader resolves them, under its own field names
  (`web_ui_port`, not `studio.port`). The defaults of a section that is
  off unless set (such as `[embedding]`) are not in it; review covers
  those.
- The CLI snapshot freezes each command and its usage lines, and each
  global flag, as written in the help tables. It does not test behaviour.
- The env snapshot lists the variable names the config loader reads; a
  test fails when the loader gains an `ENV_*` constant that is not in it.
- The format snapshot lists the versions and field paths. It does not test
  that old data still reads. Each store checks its schema version when
  it opens and refuses one it does not know. Reading older data is covered
  by those checks, by tests that open data in the shape the earlier code
  wrote, and by review, not by a cross-version test.
