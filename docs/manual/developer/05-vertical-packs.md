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
different platform, and wires the pack in. `just pack-kitchen` builds the
kitchen pack with a development key, as a worked example of the format.
