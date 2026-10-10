# Vertical packs

A vertical pack turns the general assistant into a domain crew — a
restaurant kitchen, a clinic, a workshop — without forking it. The full
design is in
[`docs/VERTICAL_PACKS.md`](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/VERTICAL_PACKS.md)
and the distribution format in
[`docs/FREIGHT.md`](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/FREIGHT.md).

## What a pack is made of

Each part rides on something the platform already has:

| Part | Built on |
|---|---|
| A **template** that seeds the Profile and default role | `aivyx-pa init --template <name>` |
| A **toolkit**: the domain's tools, as one tool process | `aivyx-vertical-sdk` (the `Tool` trait and the multi-tool runner) |
| **Scopes** for those tools, with a trust ceiling and approval gates | new capability bases in `aivyx-capability` — the one change outside the pack's own crates |
| A **team**: a lead plus up to eight least-privileged specialists | a `TeamConfig` TOML |
| Starter **skills** and **integrations** | config |

The kitchen pack in `crates/verticals/aivyx-kitchen*` is the complete,
working example: a back-of-house crew (stocktake, inventory, purchasing,
food safety) over a KitchenDB backend. `aivyx-pa connect kitchen` sets it
up.

## Building your own

VERTICAL_PACKS.md §6 walks through it step by step: the dependency rule
(packs depend on `aivyx-vertical-sdk`, never on the engine's internals),
the team crate, the toolkit crate, registering scope bases, wiring it into
the daemon and onboarding, and testing. Commercial packs can live under
`crates/verticals-private/`, which is git-ignored but still part of the
workspace.

## Signed bundles

Packs reach users as signed binary bundles,
`<name>-<version>-<target>.aivyxpack`: compiled tool processes, config and a
manifest in one Ed25519-signed archive. Users never need a Rust toolchain.

```sh
aivyx-pa pack keygen <keyfile>                                  # once, for publishers
aivyx-pa pack build <staging-dir> --key <keyfile> --out <file>
aivyx-pa pack inspect <bundle-file>
aivyx-pa pack install <bundle-file>
```

`install` checks the signature against the built-in Aivyx key and any keys
listed in `[pack] trusted_publishers`, refuses a bundle built for a
different platform, and wires the pack in.

## Config packs

A **config pack** (manifest `format = 2`) carries configuration only — no
binaries, one file for every platform — so it can only use tools Aivyx PA
already has. Its aivyx-pa part is a starter `aivyx-pa.toml` (profile,
persona seed, roles, `[[schedule]]` routines, `[autonomy]`), an optional
team config, `SKILL.md` skill folders, and the integrations it `requires`
or can use (`optional`). The format lives in the shared
[`aivyx-pack`](https://github.com/Aivyx-Agent/aivyx-pack) crate, whose
`aivyx-pack` command builds and signs packs for aivyx-pa and aivyx-coder
alike.

`aivyx-pa pack check <dir>` checks a pack's aivyx-pa part and lists every
problem: the template must load as a real config, autonomy (global and per
area) can't go above `supervised`, the team config and skills must load,
and integrations must be ones Aivyx PA knows (`gmail`, `calendar`, `drive`,
`contacts`, `notion`, `obsidian`, `n8n`, `toolkit`, `vision`).

`aivyx-pa pack install` makes a config pack a **new named instance**: it
unpacks into the instance's `packs/<name>/<version>/`, runs the same checks,
points the template at the installed team and skills, records
`[pack] source`, and runs the setup wizard with it. The pack's routines replace the starter routines a new agent otherwise gets. `just pack-kitchen` builds the
kitchen pack with a development key, as a worked example of the format.
