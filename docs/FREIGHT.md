# Sealed Crates — signed binary pack bundles (Chapter Freight)

> **Status: COMPLETE (FR.0–FR.4, 2026-07-04).** v1.0-runway decision
> 1, locked 2026-07-04: paid vertical packs ship as **signed binary
> bundles over the tool-process boundary** — the customer is an
> *operator*, not a Rust developer. A pack is compiled tool-process
> binaries + config TOMLs + a manifest in one Ed25519-signed archive;
> `aivyx-pa pack install` verifies and wires it the Mise way. Development
> stays in the in-tree private workspace glob; customers only ever see
> binaries. Kitchen is the free worked example proving the format.
>
> **2026-10-07:** the format now lives in its own shared crate,
> [`aivyx-pack`](https://github.com/Aivyx-Agent/aivyx-pack), used by
> aivyx-pa and aivyx-coder alike, with a publisher command (`aivyx-pack
> keygen/build/inspect`). It adds **format 2, config packs** —
> configuration only, for aivyx-pa and/or aivyx-coder, one file for every
> platform. `aivyx-pa pack install` installs one as a new named instance
> (see `docs/manual/developer/05-vertical-packs.md`). Everything below
> describes format 1, which is unchanged.

## 1. The bundle format (locked)

`<name>-<version>-<target>.aivyxpack` — an **outer plain tar** with
exactly three entries:

| entry | what |
|---|---|
| `payload.tar.gz` | the signed unit: `manifest.toml` + `bin/*` (tool-process executables) + `config/*` (team pack TOMLs etc.) |
| `signature.bin` | 64-byte Ed25519 signature **over the raw `payload.tar.gz` bytes** |
| `publisher.txt` | base64 of the publisher's verifying key — a *selector* only; it must match a trusted key, it is never trusted itself |

Why two layers: the signature cannot live inside what it signs, and
signing the compressed payload bytes makes verification a single pass
over one file — no canonicalization questions.

`manifest.toml` (inside the payload — the *signed* copy is
authoritative): `name`, `version`, `target` (Rust triple — install
refuses a foreign host), `min_daemon_version`, `publisher` (display
label), `[[tool_process]]` entries (name + `bin`-relative command),
optional `team_config` (a `config/`-relative path wired
`[team] config_path` no-clobber).

## 2. Trust model (locked for v0.9; the v1.0 key ceremony is separate)

- Verification requires the publisher key to appear in
  `[pack] trusted_publishers` (base64 Ed25519 keys, operator config) —
  **unioned with** the compiled-in `AIVYX_PUBLISHER_KEYS` (empty until
  the v1.0 web presence establishes the real publisher key; documented
  TODO, not a placeholder key).
- The signature proves **authenticity and integrity** (protects the
  customer from tampered packs). It is deliberately not DRM.
- Archive extraction is path-sanitized: absolute paths, `..`
  components, and symlink entries are refused outright.

## 3. CLI surface

- `aivyx-pa pack keygen <keyfile>` — publisher-side: new Ed25519 keypair
  (secret 0600; prints the base64 verifying key).
- `aivyx-pa pack build <staging-dir> --key <keyfile> --out <file>` —
  stage dir must hold `manifest.toml` + `bin/` + `config/`; builds the
  payload, signs, writes the bundle.
- `aivyx-pa pack inspect <file>` — verify + print the manifest (trust
  check included; `--allow-untrusted` prints anyway, loudly).
- `aivyx-pa pack install <file>` — verify → target/version checks →
  unpack to `~/.aivyx-pa/packs/<name>/<version>/` → Mise-pattern wiring
  (reuses `connect`'s `append_tool_process` / no-clobber
  `[team] config_path`).
- `aivyx-pa pack update` — **deferred to the v1.0 web presence** (there is
  no distribution endpoint to update from yet).

## 4. Phase plan

| Phase | What | Proof |
|---|---|---|
| **FR.0** ✅ | This doc. | Reviewed. |
| **FR.1** | The format core: manifest types, build/sign, verify/inspect, path sanitization; `tar` + `flate2` workspace deps; `[pack] trusted_publishers` config. Round-trip + tamper + untrusted-key + sanitize tests with generated keys. | `cargo test`. |
| **FR.2** | `aivyx-pa pack` CLI (keygen/build/inspect/install) + the Mise-pattern install wiring. Temp-HOME install test. | `cargo test`. |
| **FR.3** ✅ | **DONE (live on the rig).** `just pack-kitchen` stages the release kitchen-toolkit + `kitchen-boh.toml` + a generated manifest into a signed bundle (dev key in git-ignored `.pack-dev/`). Live proof: `pack inspect` → `signature: VERIFIED`; `pack install` unpacked to `~/.aivyx-pa/packs/kitchen/0.8.0/` and wired both the tool process and `[team] config_path`; on restart the daemon **loaded the pack's team config** and launched the tool process sandboxed. The toolkit then exited at handshake wanting its KitchenDB credentials file — exactly the pre-`aivyx-pa connect kitchen` state (the pack delivers capability; `connect` provides the operator's DB credentials; the two compose). Rig config restored to soak posture afterward. | Journal: `loaded team config from …/packs/kitchen/0.8.0/config/kitchen-boh.toml`. |
| **FR.4** ✅ | **DONE.** INSTALL.md "Vertical packs" section: operator trust/inspect/install/connect flow + publisher keygen/build guide. | Committed 53f5e3a. |
