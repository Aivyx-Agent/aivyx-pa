# Environment variables

Environment variables Aivyx PA reads. Where a variable and the config file
both set something, **the variable wins**. An empty variable counts as
unset.

Secrets (API keys, tokens, the passphrase) are better kept in the encrypted
store — `aivyx-pa init`, `aivyx-pa connect` and the Studio put them there.
Use these variables for scripts, containers and one-off overrides.

## Choosing the instance and config

| Variable | Effect |
|---|---|
| `AIVYX_PA_INSTANCE` | Which [named instance](../../guide/16-named-instances.md) to use. Unset means `default`. The `--instance` flag wins over it. |
| `AIVYX_PA_CONFIG_PATH` | Use this config file instead of the usual lookup. |
| `AIVYX_PA_PASSPHRASE` | The store passphrase, for unattended starts. Otherwise it comes from the OS keyring or a prompt. (A saved `daemon.env` is just this variable, loaded by the background service.) |

## Model and provider

| Variable | Overrides |
|---|---|
| `AIVYX_PA_PROVIDER` | `[agent] provider` |
| `AIVYX_PA_MODEL` | `[agent] model` |
| `AIVYX_PA_SYSTEM_PROMPT` | `[agent] system_prompt` |
| `AIVYX_PA_ROLE` | the active role (the `--role` flag wins) |
| `ANTHROPIC_API_KEY` | `[anthropic] api_key` |
| `AIVYX_PA_OPENAI_API_KEY` | `[openai] api_key` |
| `AIVYX_PA_OPENAI_BASE_URL` | `[openai] base_url` |
| `AIVYX_PA_EMBEDDING_API_KEY` | `[embedding] api_key` |
| `AIVYX_PA_KVCACHE_STORE_PATH` | `[kvcache] store_path` |
| `AIVYX_PA_ANTHROPIC_PDF_PAGE_CAP` | Most PDF pages sent to Anthropic in one request. Default 100. |

## Files and memory

| Variable | Overrides |
|---|---|
| `AIVYX_PA_STORAGE_PATH` | `[storage] path` |
| `AIVYX_PA_FS_ROOT` | `[fs] root` |
| `AIVYX_PA_WORKSPACE` | `[workspace] path` |
| `AIVYX_PA_MEMORY_MAX_PER_TOPIC` | `[memory] max_per_topic` |
| `AIVYX_PA_MEMORY_TTL_SECS` | `[memory] ttl_secs` |

## Chat apps

| Variable | Overrides |
|---|---|
| `AIVYX_PA_TELEGRAM_TOKEN` | `[telegram] token` |
| `AIVYX_PA_TELEGRAM_CHAT_ID` | `[telegram] chat_id` |
| `AIVYX_PA_DISCORD_TOKEN` | `[discord] token` |
| `AIVYX_PA_DISCORD_APPLICATION_ID` | `[discord] application_id` |
| `AIVYX_PA_DISCORD_CHANNEL_ID` | `[discord] channel_filter` |
| `AIVYX_PA_SLACK_BOT_TOKEN` | `[slack] bot_token` |
| `AIVYX_PA_SLACK_APP_TOKEN` | `[slack] app_token` |
| `AIVYX_PA_SLACK_TEAM_ID` | `[slack] team_id` |
| `AIVYX_PA_SLACK_CHANNEL_ID` | `[slack] channel_filter` |

## Integrations and tools

| Variable | Read by | Effect |
|---|---|---|
| `AIVYX_PA_CALENDAR_CACHE_TTL_SECS` | the Calendar integration | How long its list of writable calendars is cached, in seconds. Default 300. |
| `AIVYX_PA_KITCHEN_API_KEY`, `AIVYX_PA_KITCHEN_BASE_URL`, `AIVYX_PA_KITCHEN_ORGANIZATION_ID` | `aivyx-pa connect kitchen` | KitchenDB credentials for the kitchen vertical pack, read instead of prompting. |
| `BRAVE_SEARCH_API_KEY`, `SERPAPI_KEY` | `aivyx-pa mcp-server web-search` | The search backend: Brave first, then SerpAPI, else DuckDuckGo (no key needed). |
| `EDITOR` | `aivyx-pa profile edit` | The editor it opens. |

MCP server recipes (`aivyx-pa mcp recipes`) mention variables such as
`BRAVE_API_KEY` or `SLACK_BOT_TOKEN`. Those are read by the third-party
server, not by Aivyx PA — pass them through the server's `env` table.

## The desktop app

| Variable | Effect |
|---|---|
| `AIVYX_PA_BIN` | The `aivyx-pa` binary the app drives. Default: the one installed beside the app, else `aivyx-pa` on `PATH`. |
| `AIVYX_PA_STUDIO_URL` | Open a Studio somewhere else (e.g. a home server) instead of the local one. |
| `AIVYX_PA_STUDIO_TOKEN` | The sign-in token for that Studio. Default: the local automatic token. |
| `XDG_STATE_HOME` | Where the app writes the output of a daemon it starts. |

## Standard variables

| Variable | Used for |
|---|---|
| `HOME` | The base of every default path. |
| `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_RUNTIME_DIR` | Config, data and socket locations — see [Files and paths](04-files-and-paths.md). |
| `TMPDIR` | Temporary files. |
| `WAYLAND_DISPLAY` | The voice channel's clipboard: `wl-paste` under Wayland, else `xclip`. |
