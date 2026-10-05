# Contributing

Aivyx PA is a single-operator personal agent by design. Well-shaped
contributions — new channels, tools and provider adapters that fit the
existing SDKs — are welcome. The full guidance is in
[`CONTRIBUTING.md`](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/CONTRIBUTING.md).

## Licence and sign-off

Aivyx PA is **source-available** under the Business Source License 1.1:
free for personal, educational, research and other non-commercial use, with
commercial use under a separate licence
([`COMMERCIAL.md`](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/COMMERCIAL.md)).
Each version becomes MIT-licensed four years after its release.

So that contributions can be offered under both, every commit needs a
sign-off:

```sh
git commit -s
```

The `Signed-off-by` line certifies the Developer Certificate of Origin and
accepts the Contributor License Agreement (see
[`CLA.md`](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/CLA.md)). You
keep your copyright. A pull request with unsigned commits can't be merged.

## Before you build

1. **Open an issue first** for anything beyond a small fix, and agree the
   approach.
2. **Architectural changes** that touch `DESIGN.md` or `PRODUCT.md` need a
   formal amendment under `docs/amendments/`; raise it in the issue.
3. New channels, tools and adapters should fit the
   [channel](03-channel-adapters.md) and [tool](04-tool-processes.md) SDKs.

## The bar for a pull request

- [ ] Every commit signed off (`git commit -s`).
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean — the
      workspace holds at zero warnings.
- [ ] `cargo test --workspace` passes, and `just check-web` if you touched
      the Studio.
- [ ] New behaviour has tests.
- [ ] Docs updated: the guide or reference page for anything a user sees;
      regenerate the [config and tools references](01-building-from-source.md#generated-documentation)
      if you changed the config schema or a tool.
- [ ] Any `DESIGN.md` / `PRODUCT.md` change has its amendment.

See [Building from source](01-building-from-source.md) for the commands and
the pre-commit hook.

## Security issues

Please don't open a public issue for a vulnerability. See
[`SECURITY.md`](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/SECURITY.md)
for how to report one, and
[`docs/THREAT_MODEL.md`](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/THREAT_MODEL.md)
for what's already known and accepted.
