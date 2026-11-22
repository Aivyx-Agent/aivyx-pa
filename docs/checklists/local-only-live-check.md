# Local-only live check (A23, 2.0)

- **Date:** 2026-11-22.
- **Build:** debug `aivyx-pa` from branch `local-only` (pins aivyx-route
  0.2.0 `fd56957`, aivyx-vision `5525de6`, aivyx-kvcache `b1f0dc7`), built
  with `cargo build -p aivyx-cli --bin aivyx-pa`. Release `aivyx-coder`
  2.0.0 from `aivyx-coder` branch `local-only` at `2128afb`.
- **Model:** llama-server at `http://127.0.0.1:8090`, Qwen3.5-9B Q4_K_M,
  `provider = "llamacpp"`. Nothing used Lemonade or any non-local server.
- **Isolation:** as in `board-coder-live-check.md`: a throwaway HOME under
  `$SCRATCH`, every command through a wrapper that runs
  `env -i HOME=<tmp> … XDG_RUNTIME_DIR=<short runtime dir>
  AIVYX_PA_PASSPHRASE=<throwaway> AIVYX_CODER=<coder 2.0> target/debug/aivyx-pa "$@"`.
  Own webhook port 17872 and Studio port 17873. The operator's daemon and
  `~/.config/aivyx-pa` were never touched.

## pa

| # | Step | Result |
|---|---|---|
| 1 | `doctor` with the 9B config | ✅ `✓ local-only: every model URL is on this machine or your network`; `✓ the model answers: "OK"` |
| 2 | A chat turn (`--headless` over the throwaway daemon) | ✅ `completed: PONG` |
| 3 | `[openai] base_url = "https://api.openai.com"` | ✅ refused by `daemon run` and by `doctor`'s local-only row (below) |
| 4 | `provider = "anthropic"` | ✅ refused at load with the named fix (below) |
| 5 | The machine's LAN IP (`http://10.80.80.73:8090`) | ✅ accepted: `doctor` shows `✓ local-only`, `daemon run` starts |
| 6 | A board task owned by `coder:proj`, run by aivyx-coder 2.0 | ✅ "Add cube": Review, `verification: test passed`, 1 changed file, 4 checkpoints |

### 3. A cloud URL

```
$ aivyx-pa daemon run
aivyx-pa: [openai] base_url: https://api.openai.com/ is not local (cloud host) — aivyx is local-only; point it at a model server on this machine or your own network
$ aivyx-pa doctor
Local-only:
  ✗ local-only: [openai] base_url: https://api.openai.com/ is not local (cloud host) — aivyx is local-only; point it at a model server on this machine or your own network
     → edit <tmp>/.config/aivyx-pa/aivyx-pa.toml so it points at a local server
```

### 4. The removed provider

```
$ aivyx-pa doctor
aivyx-pa: failed to load <tmp>/.config/aivyx-pa/aivyx-pa.toml: TOML parse failed at …: TOML parse error at line 2, column 12
  |
2 | provider = "anthropic"
  |            ^^^^^^^^^^^
provider "anthropic" was removed in 2.0 — aivyx is local-only; use llamacpp, ollama, jan, lemonade, broker or openai (an OpenAI-compatible local server) with a local base_url
```

### 5. The LAN IP

The 9B's llama-server listens on loopback only, so this checks the guard
(the address is accepted as local) rather than a reply from that address.

### 6. Board coder run

The first try ("Add square") correctly changed nothing: the scratch
project from the 1.5.0 check already had `square`. "Add cube" then added
`cube` and a `cube_cubes_value` test.

## aivyx-coder 2.0 (release build, throwaway HOME)

| # | Step | Result |
|---|---|---|
| 1 | `--base-url https://api.openai.com/v1` | ✅ `[backend] base_url: https://api.openai.com/v1 is not local (cloud host) — …` |
| 2 | `--base-url http://8.8.8.8:8090/v1` | ✅ `… is not local (resolves to a public address 8.8.8.8) — …` |
| 3 | `--base-url http://10.80.80.73:8090/v1` (LAN IP) | ✅ accepted; the run reached the connect step (nothing listens there) |
| 4 | `--headless --auto` on a scratch crate (`add` was `todo!()`) | ✅ goal achieved, `detected-tests` passed, `src/lib.rs` +1 −1 |

coder needs `[sandbox] extra_read_paths` for the real `~/.rustup` and
`~/.cargo` when HOME is a throwaway; without it the sandboxed `cargo test`
can't reach the toolchain and coder discards the edit (1.x behaves the same).

## Found and fixed during the check

- The daemon's startup config banner still printed an
  `anthropic_api_key = <unset>` line. Removed; `doctor` still notes a
  stored, unused key.
