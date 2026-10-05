# Building from source

## Toolchain

- **Rust stable**, via [rustup](https://rustup.rs). The repository pins the
  `stable` channel with `rustfmt` and `clippy` in `rust-toolchain.toml`;
  the minimum supported version is 1.85 (edition 2024).
- **Linux desktop app only:** a webview and tray libraries —
  Debian/Ubuntu `libwebkit2gtk-4.1-dev libayatana-appindicator3-dev libxdo-dev`,
  Arch `webkit2gtk-4.1 libayatana-appindicator xdotool`.
- **The Studio bundle only:** the `wasm32-unknown-unknown` target and the
  Dioxus CLI (`rustup target add wasm32-unknown-unknown`,
  `cargo install dioxus-cli`), plus [`just`](https://github.com/casey/just).
- **Voice only:** a C++ compiler (for `whisper-rs`).

## Build and run

```sh
git clone https://github.com/Aivyx-Agent/aivyx-pa
cd aivyx-pa
cargo build --release          # the aivyx-pa binary → target/release/aivyx-pa
./target/release/aivyx-pa init
```

A plain `cargo build` or `cargo test` (no `-p`) covers the workspace's
`default-members`. Two crates are deliberately left out and built
explicitly:

| Crate | Why it's separate | Build it |
|---|---|---|
| `aivyx-web` | The Studio — Dioxus, compiled to WebAssembly | `just check-web` (compile check), `just build-web` (the bundle) |
| `aivyx-desktop` | The desktop app — links a system webview | `cargo build -p aivyx-desktop --release` |

`just build-web` writes the bundle to `crates/aivyx-web/dist/`, which the
daemon's build script embeds. Without it the daemon serves a small fallback
page, so a plain `cargo build` never needs the wasm toolchain.

## Test and lint

```sh
cargo clippy --workspace --all-targets -- -D warnings   # zero warnings, always
cargo test --workspace
cargo test -p aivyx-core some_test_name                 # one crate / one test

# Conformance suites for the Python SDK examples (no daemon needed)
python3 -m unittest discover examples/python-channel/tests
python3 -m unittest discover examples/python-tool/tests
```

Install the pre-commit hook once per clone; it runs the clippy sweep before
every commit:

```sh
./scripts/install-hooks.sh
```

## A disposable local run

`scripts/dev-run.sh` builds `aivyx-pa` and runs it against a local Ollama
with all state under a git-ignored `.dev-run/` directory, so experiments
never touch your real assistant:

```sh
./scripts/dev-run.sh                    # debug build, interactive session
./scripts/dev-run.sh --release --reset  # release build, fresh state
./scripts/dev-run.sh -- --verify-only   # scripted verification pass
```

Another way to keep a test agent apart is a
[named instance](../../guide/16-named-instances.md):
`aivyx-pa instances create scratch`.

## Generated documentation

Two reference pages are generated from the code; re-run the scripts after
changing the config schema or adding a tool, and commit the result:

```sh
python3 scripts/gen-config-reference.py > docs/manual/reference/02-configuration.md
python3 scripts/gen-tools-reference.py  > docs/manual/reference/03-tools.md
```

The CLI reference is hand-written, but a test fails if a command in
`help.rs` has no section in it, and another fails if a guide page in
`docs/guide/` isn't registered in the Studio.
