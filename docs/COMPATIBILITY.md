# Compatibility

From 1.0, Aivyx PA follows semantic versioning for its stable surface.

## Stable

- **Config keys and their defaults.** Every key path the config accepts,
  with its type, and the aliases it accepts. Snapshot: `docs/stable/config.txt`.
- **The CLI commands and flags.** Each command and its usage lines.
  Snapshot: `docs/stable/cli.txt`.
- **The data stores.** Any 1.x reads data written by 1.0. The snapshot
  records the store versions that exist today: the redb table versions
  (memory, secrets, sessions, audit, missions, schedules and the rest),
  the key-derivation salt, and the schema versions of the toolkit stores
  and the Google token file. Snapshot: `docs/stable/formats.txt`.
- **The pack formats.** Formats 1 (tool packs) and 2 (config-only packs).
  Snapshot: `docs/stable/formats.txt`.
- **Webhook payloads.** The field paths of the notify payload and of the
  trigger reply. Snapshot: `docs/stable/formats.txt`.
- **The protocols.** The daemon IPC and tool protocol versions recorded in
  `docs/stable/formats.txt`.

Three stores have no version yet: the instance layout on disk, the audit
chain format and the redb record shapes. They get versions before 1.0, and
the readiness audit decides how. Until then they are not promised.

## Not stable

- **Previews.** Today these are the Matrix, Proton Mail and Signal channels
  (updated by the Channel Activation Milestone). A preview can change or
  be removed in any release.
- **The terminal chat's look**, and keys beyond the documented ones.
- **Internal crates.** The crates in this repository are not a public API.
- **The Studio's HTTP API.** It belongs to the bundled Studio and changes
  with it.
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
The tests are `stable_config_keys` (`cargo test -p aivyx-config`) and the
`stable_` tests in `aivyx-cli`. A change to the snapshot has to be made on
purpose.

The check has limits:

- It freezes config key paths and their types. It does not freeze default
  values, or the allowed values of keys that take one of a fixed set
  (for example, the access levels). The rules above and review cover
  those.
- The CLI snapshot freezes each command and its usage lines as written in
  the help table. It does not test behaviour.
- The format snapshot lists the versions and field paths. It does not test
  that old data still reads. That rests on the migration tests and on
  review.
- The stores without a version are not in the snapshot at all.
