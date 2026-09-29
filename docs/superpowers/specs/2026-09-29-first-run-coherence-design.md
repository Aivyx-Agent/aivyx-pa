# First-Run Coherence — Design

**Status:** approved 2026-09-29. It is work item **A** of the 2026-09-28 UI/UX audit, whose findings are recorded in the session's audit notes and in operator memory.

## The problem

The audit ran the 0.10.1 binary and found that a new user who follows the guide ends up somewhere other than where the guide says:

- `aivyx-pa` with no daemon running chats **in-process**. It never starts a daemon.
- That means there is no Studio, which is also off unless `[daemon] web_ui = true` is set.
- Cloud escalation and `/allow-cloud` silently do nothing. `/allow-cloud` is even sent to the model as an ordinary prompt.
- `--help`, `-h` and `help` are all rejected as unrecognised, and the error's list of supported options is out of date.
- `aivyx-pa init` offers only Ollama, Anthropic and OpenAI, and defaults to Anthropic when it finds no Ollama.
- Settings › Model tells users to change provider with `init`, which can't express most providers.

## Decisions (operator, 2026-09-29)

1. **The REPL starts the daemon**, as the TUI already does, whenever the passphrase can be found without prompting. Otherwise it runs in-process with a clear notice.
2. **The Studio is on by default on loopback, protected by an auto-generated token and a one-time sign-in link.** Loopback is reachable by every local user account, which would bypass the daemon's 0600-socket trust boundary, so default-on must not mean unauthenticated.
3. **`init` detects every local runtime and never defaults to a cloud provider.**

## A1 — the REPL starts the daemon

- **When it applies:**
  - an interactive session: stdin is a terminal and `--no-daemon` is not given;
  - no daemon is listening;
  - `select_passphrase_source` would return a non-interactive source (the environment, a TOML passphrase or the OS keyring), because a started daemon inherits all three.
- **What happens:**
  - it starts the daemon with `aivyx_channel::daemon_client::spawn_daemon_and_wait`, which logs to `daemon.log` next to the socket and uses the same timeout the TUI uses;
  - it connects over the existing daemon-session path;
  - it prints one line: `Started the aivyx-pa daemon in the background (log: <path>; stop it with \`aivyx-pa daemon stop\`).`
- **When it can't** (the passphrase needs a prompt, or the start or connect fails), the REPL runs in-process and prints one notice naming the reason and what is unavailable:
  - `Running without the daemon — the Studio, scheduled routines and cloud escalation need it. Store your passphrase (\`aivyx-pa keyring set\`) and aivyx-pa will start it for you.`
  - When a start failed, the notice leads with the failure instead.
- **Piped (non-TTY) input is unchanged.** It still runs in-process with no daemon, and still refuses in favour of `--headless` when a daemon holds the store. Scripts and `scripts/dev-verify.sh` keep working.
- **Ordering constraint.** The daemon must be started before the REPL opens or locks the store in-process, or the new daemon can't take the store lock. The implementer checks the current order first.
- **In-process fixes (F14):**
  - A whole-message `/allow-cloud` in in-process mode gets a local reply and is never sent to the model: `Cloud escalation needs the daemon — start it with \`aivyx-pa daemon run\` (or store your passphrase so aivyx-pa starts it), then allow it there.`
  - In-process startup prints a warning when `[routing.endpoints]` contains a cloud endpoint, because escalation cannot happen in this mode.

## A2 — the Studio is on by default and token-protected

- **Default.** `[daemon] web_ui` defaults to `true`, so the daemon serves `127.0.0.1:7843`. `web_ui = false` turns it off — and wins over an explicit `web_ui_port` (operator decision 2026-09-29; previously an explicit port turned the Studio on regardless). `web_ui_port = N` alone enables it on that port, as before. The config resolution changes accordingly (`aivyx-config`), with tests.
- **Token.** When the Studio is enabled, `web_ui_auth_token` is unset and `web_ui_insecure_no_auth` is not set, the daemon uses an automatic token:
  - **Location:** `studio-token`, next to the store: the parent directory of the effective `storage_path`.
  - **Format:** 256 bits, alphanumeric, 43 characters, the same shape the Harbor appliance entrypoint uses.
  - **Writing:** atomically, mode 0600, reused on later starts. A present but unreadable or corrupt file is an error with a message, never silently replaced.
  - **Ward:** this path joins Ward's extra-deny list, so the agent can't read it.
- **Configured token.** An operator-set `web_ui_auth_token` always wins, and the file is not created.
- **The off-host interlock (Gatehouse)** is unchanged. It still applies to a configured host beyond loopback.
- **Sign-in link.** `GET /?token=<t>`:
  - compares the token in constant time, the same way `request_carries_token` does;
  - when it matches, answers `302` to `/`, setting the existing `aivyx_web_token` cookie (`HttpOnly; SameSite=Strict; Path=/`), so the token leaves the address bar (browser history still records the pre-redirect URL);
  - when it doesn't match, gets the existing `401`.
  - Basic auth and Bearer keep working unchanged.
- **Where the link is shown.** Only once the Studio has actually bound its port:
  - the `aivyx-pa daemon run` banner: `Studio: http://127.0.0.1:7843/?token=…` — **only when stderr is a terminal** (operator decision 2026-09-29, after review); otherwise `Studio: http://127.0.0.1:7843/ (sign-in link: run \`aivyx-pa doctor\`)`, so the token never lands in the journal or `docker logs`;
  - the REPL banner when connected to a daemon, fetched from the daemon over IPC, or from the token file when running as the same user;
  - `aivyx-pa doctor`'s Web UI section, which already reveals configured tokens;
  - `init`'s closing message.
- **Desktop app.** It reads the token file for the same user and opens the sign-in link. Its approval watcher sends the token as Bearer. `AIVYX_PA_STUDIO_TOKEN` still overrides.
- **Service install.** `init`'s question "Also serve the Studio web UI?" is removed.
- **Docs.** `docs/THREAT_MODEL.md` §4.11 gains the local-other-user threat and this mitigation. The "No token generation in `aivyx-pa daemon install` for native installs" note in `docs/GATEHOUSE.md` is reversed, with the reason.

## A3 — real help

- **Top-level help.** `aivyx-pa --help`, `-h` and `help` print grouped usage covering:
  - chat and front ends (REPL, `tui`, `--headless`, `--channel`);
  - the daemon;
  - setup (`init`, `doctor`, `keyring`, `access`, `autonomy`);
  - the agent (`role`, `persona`, `profile`, `skills`, `team`, `loop`, `memory`, `learning`);
  - tools and integrations (`tools`, `tool`, `mcp`, `mcp-server`, `connect`, `pack`, `notify`);
  - inspection (`audit`, `cost`, `routing`, `identity`, `federation`, `tool-relevance`, `workspace`);
  - the global flags;
  - a closing line: `Run \`aivyx-pa <command> --help\` for details.`
- **Per-command help.** `aivyx-pa <command> --help` or `-h` prints that command's summary and usage lines from **one central table** (name, summary, usage). It is intercepted before dispatch, so no subcommand module changes.
- **Errors.** Every "unrecognized argument" error that prints a `Supported:` list instead ends with `Run \`aivyx-pa --help\` to see every command.`, and the stale lists go.
- **Drift guard.** A test asserts that every top-level subcommand the dispatcher recognises has a help-table entry.

## A4 — `init` covers every provider, local first

- **Detection.** `init` probes:

  | Runtime | Port | Endpoint |
  |---|---|---|
  | Ollama | 11434 | existing |
  | Lemonade | 13305 | `/api/v1/health` |
  | llama.cpp | 8080 | `/health` |
  | Jan | 1337 | `/v1/models` |
  | aivyx-broker | 8899 | `/status` |

  Each probe has a short timeout, and the probes run concurrently.
- **Menu.**
  - It lists every provider: Ollama, Lemonade Server, llama.cpp server, Jan, aivyx-broker, Anthropic, OpenAI, and mistral.rs when built with `provider-mistral-rs`.
  - Detected runtimes come first, marked "(detected)", and the first detected one is pre-selected.
  - **If nothing is detected, nothing is pre-selected.** The operator must pick, and the menu is preceded by: `No local model server found. Install Ollama (https://ollama.com) or Lemonade Server for a free, private local model — or choose a cloud provider.`
  - `prompt_choice` gains a "no default" mode for this.
- **Model selection.**
  - For Lemonade, llama.cpp, Jan and the broker, `init` lists the server's models using `aivyx_route::discovery::discover` with the matching `EndpointKind`: Lemonade `lemonade`, llama.cpp `llama_router`/`openai_compat`, Jan `openai_compat`, broker via `openai_compat` against `{base}/v1`. The operator picks one; if there are none, they type an id.
  - `init` writes `[agent] provider`, `model` and the provider's base-URL key (`[openai] base_url` or `[broker] base_url`) only when it differs from the default.
  - mistral.rs asks for a GGUF path and validates it with the doctor's model check.
- **Existing flows** (Ollama pull, cloud key verification) are unchanged.

## A5 — docs

- **`docs/guide/02-getting-started.md`:**
  - every provider, local first;
  - the daemon starting automatically, and how to store the passphrase;
  - the Studio sign-in link.
- **`docs/guide/10-troubleshooting.md`:** "the Studio asks for a token", "aivyx-pa said it's running without the daemon".
- **Stale links:** all four `github.com/Aivyx-Agent/aivyx/...` links go to `aivyx-pa`.
- **`examples/aivyx-pa.toml`:** the `web_ui` default and the automatic token.
- **`CHANGELOG.md`:** entries under Unreleased, including the Studio-on-by-default behaviour change.

## Testing

- **A1:**
  - unit tests for the decision "spawn / in-process with reason" over the passphrase sources, TTY and `--no-daemon`;
  - the in-process `/allow-cloud` reply;
  - a real run: a TTY REPL starts the daemon and connects; piped input is unchanged.
- **A2:**
  - config default and opt-out tests;
  - token file: create, reuse, 0600, corrupt → error, configured token wins;
  - `/?token=` gives 302 plus the cookie, and a bad token gives 401;
  - Ward denies the path;
  - a real run: headless-Chrome sign-in through the link, then the Studio loads.
- **A3:**
  - `--help`, `-h`, `help` and `<cmd> --help` for a sample of commands;
  - the drift-guard test;
  - the error text.
- **A4:**
  - detection and ordering with fake servers;
  - no pre-selected default when nothing is detected;
  - the written config for each provider kind;
  - a real run against Lemonade.
- **Everywhere:** the full workspace tests and clippy, plus the web and desktop checks where touched.

## Out of scope

Routing visibility (item B) and the quick fixes (item C): the config-error panic, the stale Studio bundle version, the red neutral states, sidebar overflow, Chat vs Terminal naming, and `aivyx-coder`'s items.
