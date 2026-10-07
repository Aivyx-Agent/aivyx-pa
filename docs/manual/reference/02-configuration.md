# Configuration reference

Every section and key `aivyx-pa.toml` accepts. This page is generated from
the config structs in `crates/aivyx-config` by
`scripts/gen-config-reference.py` — edit the code or the script, not this
page. For a ready-to-copy, commented example, see
[`examples/aivyx-pa.toml`](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/examples/aivyx-pa.toml);
the **Example** column below comes from it.

## How the config is found

The first that exists wins:

1. `$AIVYX_PA_CONFIG_PATH`
2. `./aivyx-pa.toml` in the current directory
3. your instance's config file — `~/.config/aivyx-pa/aivyx-pa.toml` for the
   default instance, `~/.config/aivyx-pa/instances/<name>/aivyx-pa.toml`
   for a named one (see [Files and paths](04-files-and-paths.md))

Every section is optional; a missing section means its defaults. Where a
setting also has an environment variable, the variable wins — see
[Environment variables](05-environment-variables.md).
`aivyx-pa doctor` checks the config and tells you what to fix.

## Reading the tables

- **Type**: `string`, `integer`, `number`, `bool`, `path`, a list, or the
  allowed words (`` `a` | `b` ``).
- `[[name]]` is an array of tables: repeat the block once per entry.
- `<name>` in a heading is a name you choose (e.g. `[pricing.<name>]` →
  `[pricing."claude-sonnet-5"]`).


## Models and providers

### `[agent]`

The model the agent uses and how a turn behaves. Pairs with one provider section below. See [Models and routing](../../guide/12-models-and-routing.md).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `model` | string |  | The model id, as the provider names it (e.g. `qwen3:14b`, `claude-sonnet-5`). |
| `system_prompt` | string |  | Extra instructions added to the agent's system prompt. |
| `provider` | `anthropic` \| `openai` \| `ollama` \| `llamacpp` \| `jan` \| `mistralrs` \| `broker` \| `lemonade` | `"openai"` | Which backend serves the model. |
| `turn_timeout_secs` | integer |  | Per-turn wall-clock deadline override (seconds). Unset → the built-in 120s default. |
| `cycle_detection` | bool |  | Small-cycle breaker switch. `true` arms the loop's repeating-cycle detector (catches `A,B,A,B,…` that the consecutive-identical breaker misses) with the built-in defaults. |
| `conversation_history_turns` | integer |  | Conversation-history replay depth in messages. Unset → `8`; `0` disables. |
| `injection_scan_enabled` | bool |  | Global on/off for the active injection scan. Unset → `true` (fail-closed, matching `[confine] require_enforcement`'s posture). |
| `injection_scan_exempt` | list of string |  | Tool names exempted from the active injection scan even when `injection_scan_enabled` is `true`. Matched exactly against `Tool::name()`. |

### `[anthropic]`

Anthropic as the provider (`[agent] provider = "anthropic"`).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `api_key` | string |  | Your Anthropic API key. Prefer the encrypted store (`aivyx-pa init` or the Studio) or `ANTHROPIC_API_KEY`. |

### `[openai]`

OpenAI, or any OpenAI-compatible server (llama.cpp, LM Studio, vLLM, Jan, Lemonade…) via `base_url`.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `api_key` | string | `"sk-..."` | API key for the server. Not needed for a local server. Prefer the encrypted store or `AIVYX_PA_OPENAI_API_KEY`. |
| `base_url` | string | `"https://api.openai.com"` | The server's URL. Default `https://api.openai.com`; point it at a local server for llama.cpp, Jan, LM Studio and the like. |
| `constrain_tool_calls` | bool |  | Grammar-constrained tool-calling for the llama.cpp-family OpenAI-compat servers (`provider = "llamacpp"` / `"jan"`). Unset → off. |

### `[ollama]`

Generation options passed to Ollama when it is the provider. Unset keys keep Ollama's own defaults.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `num_ctx` | integer |  | Context window, in tokens. |
| `num_predict` | integer |  | Most tokens to generate per reply. |
| `num_thread` | integer |  | CPU threads to use. |
| `mirostat` | integer |  | Mirostat sampling: 0 off, 1 or 2 on. |
| `top_k` | integer |  | Top-k sampling. |
| `top_p` | number |  | Top-p (nucleus) sampling. |
| `repeat_penalty` | number |  | Penalty for repeating tokens. |
| `repeat_last_n` | integer |  | How many recent tokens the repeat penalty looks at. |
| `seed` | integer |  | Random seed, for repeatable output. |
| `prompt_strategies` | table of string |  | `[ollama.prompt_strategies]` operator-facing per-family override map. Keys are family strings matching `detect_model_family`'s output (`"qwen3"`, `"gemma4"`, `"llama3"`, …); values are wire-form strategy labels parsed by `OllamaFamilyStrategy::parse`. |

### `[mistralrs]`

The embedded, in-process mistral.rs engine (`provider = "mistralrs"`) — runs a local GGUF model with no separate server.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `model_path` | path |  | Absolute path to either a directory containing GGUF file(s) or a single GGUF file. Required when `provider = "mistralrs"`. |
| `model_file` | string |  | When `model_path` is a directory, names the specific GGUF file to load. Ignored when `model_path` is a file. |
| `chat_template_path` | path |  | Optional path to a chat-template JSON file. When Unset, mistralrs uses the chat template embedded in the GGUF (which most modern quantizations ship). |
| `max_seq_len` | integer |  | Optional maximum sequence length. When Unset, defers to the model's declared `max_seq_len`. |
| `constrain_tool_calls` | bool |  | Grammar-constrained tool-calling. When `true`, the in-process engine constrains decoding to a JSON-Schema grammar (`aivyx_llm::tool_grammar`) so a small GGUF model emits a valid, real-named tool call (or the `respond` text escape) *by construction* instead of hallucinating tool names or malformed arguments. |

### `[broker]`

Share one local GPU server fairly with other processes (such as aivyx-coder) through `aivyx-broker`.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `base_url` | string |  | The broker's address. |

### `[kvcache]`

Where saved KV-cache slots live when a local llama.cpp server is the backend.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `store_path` | path |  | The KV-cache store directory. When aivyx-coder shares the same llama.cpp server, point both at the same directory. |

### `[providers]`

Provider-independent model settings.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `tool_name_auto_correct_threshold` | number |  | When the model calls a tool name that doesn't exist, use the closest real tool if the similarity is at least this (0–1). |

### `[routing]`

Per-call model routing: send each task to the best-suited model you have, escalate when needed, never send sensitive conversations to the cloud. Off by default. See [Models and routing](../../guide/12-models-and-routing.md).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool | `true` | Turn routing on. Off (the default) means every call uses `[agent] model`. |
| `discover` | bool | `true` | Ask each endpoint what models it serves at startup. Off means use only `[[routing.models]]`. |
| `vram_bytes` | integer | `25769803776` | Host GPU memory in bytes, for residency scoring when no `aivyx-broker` reports it. Unset: no won't-fit term without a broker. |

#### `[routing.endpoints.<name>]`

| Key | Type | Example | Meaning |
|---|---|---|---|
| `kind` | `ollama` \| `llama_router` \| `openai_compat` \| `lemonade` \| `anthropic` \| `openai` |  | What kind of server this is. |
| `base_url` | string |  | The server's URL. Unset uses the kind's usual local address. |
| `locality` | `local` \| `cloud` |  | Operator override of the locality the address implies: `local` marks a non-cloud kind local even when its host looks public (a LAN box with a public DNS name); `cloud` marks it cloud. Never makes a cloud kind local. |

#### `[[routing.models]]`

| Key | Type | Example | Meaning |
|---|---|---|---|
| `id` | string | `"qwen3:8b"` | The model id, as its endpoint names it. |
| `endpoint` | string | `"gpu-box"` | Which `[routing.endpoints.<name>]` serves it. Unset means the main provider. |
| `locality` | `local` \| `cloud` |  | `cloud` marks this model cloud even on a local endpoint. `local` never makes a model on a cloud endpoint local (it is ignored, and `RoutingConfig::validate` reports it). |
| `tier` | `small` \| `medium` \| `large` | `"medium"` | Its size class, for matching tasks to models. |
| `strengths` | list of `code` \| `reasoning` \| `chat` \| `summarize` | `["chat"]` | What it is good at. |
| `priority` | integer | `10` | Operator tie-break; higher wins. Unset = 0 (or an earlier entry's). |
| `capabilities` | list of `completion` \| `tools` \| `vision` \| `thinking` \| `audio` \| `embedding` | `["completion", "tools"]` | Added to what discovery found. |
| `capabilities_deny` | list of `completion` \| `tools` \| `vision` \| `thinking` \| `audio` \| `embedding` | `["vision"]` | Removed from what discovery found (e.g. unreliable tool calling). |
| `context_window` | integer | `32768` | The context window you actually serve it with, in tokens. |

#### `[routing.tasks.<name>]`

| Key | Type | Example | Meaning |
|---|---|---|---|
| `tier` | `small` \| `medium` \| `large` |  | The tier this task prefers. Keyed by task name (`chat`, `code_edit`, `plan`, `judge`, `summarize`, `compact`, `classify`, `embed`). |
| `strengths` | list of `code` \| `reasoning` \| `chat` \| `summarize` |  | Strengths this task prefers in a model. |

#### `[routing.escalation]`

| Key | Type | Example | Meaning |
|---|---|---|---|
| `mode` | `never` \| `ask` \| `auto` | `"ask"` | Whether a call may escalate to a cloud model: `never`, `ask` (default) or `auto`. |
| `no_local_candidate` | bool | `true` | Escalate when no local candidate can serve the call. |
| `tiers` | list of string | `[]` | `TaskKind` names (`[routing.tasks]` keys, e.g. `"plan"`) that always prefer a cloud endpoint. |
| `on_failure` | bool |  | After a local failure, escalate the next turn (through the same `mode` gate; sensitive conversations never escalate). |

#### `[routing.sensitive]`

| Key | Type | Example | Meaning |
|---|---|---|---|
| `tool_prefixes` | list of string |  | Tool-name prefixes that mark a conversation sensitive (it then never escalates to a cloud model). The default list covers tools that return your own data. |
| `channels` | list of string | `[]` | Channels whose conversations count as sensitive, e.g. `["telegram"]`. |

#### `[routing.classifier]`

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool | `false` | Turn this on or off. |
| `timeout_ms` | integer | `2000` | Bounds the whole classifier call, planning and stream included. |

### `[embedding]`

Semantic memory search: the embeddings server, and how recall picks memories for each turn.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `base_url` | string | `"https://api.openai.com"` | Embeddings API base URL. Default `"https://api.openai.com"`. |
| `model` | string | `"text-embedding-3-small"` | Embedding model id. Default `"text-embedding-3-small"`. |
| `api_key` | string | `"sk-..."` | API key for the embeddings server. Not needed for a local one. |
| `dimensions` | integer | `1536` | Expected vector dimensionality. Default `1536` (text-embedding-3-small). |
| `rag_top_k` | integer | `5` | Automatic-recall fan-out: how many of the top semantic hits the per-turn recall hook may inject. Default `5`. |
| `rag_min_similarity` | number | `0.20` | Automatic-recall relevance floor: a hit whose cosine similarity is below this is dropped even when `rag_top_k` is not filled. This is what stops naive RAG from injecting weak/irrelevant memories on every unrelated prompt. |
| `recall_window_turns` | integer | `1` | Conversational-window relevance: the number of recent turns (current user message included) that auto-recall and adaptive-Persona selection embed together as their relevance query. Default `1`. |
| `recall_gate_min_chars` | integer | `0` | Heuristic recall gate: when the trimmed user message is **shorter than this many Unicode characters**, both auto-recall and adaptive Persona selection short-circuit before any embed call (they return Unset, which the planner already honours as the existing best-effort fallback). |
| `ann_index` | bool | `false` | Opt-in ANN index. Unset → `false` (brute-force only). |
| `ann_rebuild_threshold` | integer | `100` | Write-count threshold before the ANN index is rebuilt. Unset → 100. |
| `recall_token_budget` | integer | `0` | Token-cost hard cap on recall + Persona injection. Unset → 0 (disabled. |
| `recall_hybrid` | bool | `false` | Hybrid keyword+semantic recall fusion opt-in. Unset → `false` (semantic-only. |
| `recall_lexical_weight` | number | `1.0` | Weight of keyword matching in hybrid recall. Default 1.0. |
| `recall_graph_hops` | integer | `0` | Number of hops for the co-occurrence **graph-walk** fusion source. `0` (default) disables the graph source — recall fuses semantic + lexical only. |
| `recall_graph_decay` | number | `0.5` | Per-hop affinity decay for the graph-walk source. `0.5` (default) halves a path's strength each hop; `1.0` disables decay. |
| `recall_graph_weight` | number | `1.0` | Weight of the graph-walk ranker in the hybrid RRF fusion. `1.0` (default) weights it equally with the semantic + lexical rankers; lower it to make associative recall a gentler nudge. |
| `recall_wiki_weight` | number | `0.0` | Weight of the knowledge-wiki **page** ranker in the hybrid RRF fusion. `0.0` (default) ⇒ off: a topic's consolidated page summary does not compete in recall. |
| `recall_graph_typed_weight` | number | `1.0` | Weight of the **typed knowledge-graph** ranker in the hybrid RRF fusion. `0.0` (default) ⇒ off. |

## Reach and safety

### `[fs]`

The directory the file tools work in. Usually set through `[access]` instead — see [Access and settings](../../guide/08-access-and-settings.md).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `root` | path |  | The directory the file tools may work in. |

### `[access]`

How far the agent can reach on your machine and network: the access level, plus the guards for secrets and private network addresses. See [Access and settings](../../guide/08-access-and-settings.md).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `level` | `sandbox` \| `workspace` \| `home` \| `full` \| `custom` |  | How far the agent reaches: `sandbox` (its own directory), `workspace` (a directory you choose), `home`, `full`, or `custom`. |
| `root` | path |  | The directory for the `workspace` and `custom` levels (required for those). |
| `confirm_destructive` | bool |  | Ask before irreversible actions (delete, overwrite, destructive shell, outbound messages). Unset — the usual case — means the autonomy level decides: on at every level except `unleashed`. Set it only to override that. |
| `guard_sensitive_paths` | bool |  | Master switch for the sensitive-path read guard. Unset ⇒ on (privacy-by-default). |
| `allow_sensitive_paths` | list of string |  | Paths the operator allows the agent to read despite the built-in secret set (e.g. a project's own `.env`). `~` is expanded. |
| `allow_private_egress` | bool |  | Allow the network tools to reach loopback / private / link-local addresses. Unset ⇒ false (SSRF guard on). |
| `allow_egress_hosts` | list of string |  | When non-empty, the network tools may ONLY reach these hosts (exact or dot-suffix subdomain). Empty ⇒ any public host. |

### `[autonomy]`

How much the agent may do without asking you, with per-domain exceptions. See [Autonomy and routines](../../guide/15-autonomy-and-routines.md).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `level` | `manual` \| `assisted` \| `supervised` \| `autonomous` \| `unleashed` |  | The global autonomy level. |
| `review_expiry_days` | integer |  | How many days a step parked for your review (areas at `supervised`) waits before it lapses unreviewed. Default 7; at least 1. |

#### `[[autonomy.override]]`

| Key | Type | Example | Meaning |
|---|---|---|---|
| `domain` | string |  | The area this applies to: the first word of a capability (`fs`, `shell`, `git`, `email`…; `aivyx-pa autonomy show` lists them). An unknown area is an error; `schedules` also works. |
| `level` | `manual` \| `assisted` \| `supervised` \| `autonomous` \| `unleashed` |  | The autonomy level for calls in that area: `manual` asks before every change there; `unleashed` stops deletes and overwrites asking for that area's tools. |

#### `[autonomy.auto_approve]`

| Key | Type | Example | Meaning |
|---|---|---|---|
| `scopes` | list of string |  | Reversible capability scopes that may run without asking. Never covers irreversible actions. |

### `[confine]`

OS-level process confinement (Landlock + seccomp) for the commands the agent runs.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `require_enforcement` | bool | `true` | Refuse to run a command when confinement can't be set up. On by default; off fails open. |
| `allow_unix_sockets` | bool | `false` | Let confined commands reach local daemons (Unix sockets). Any daemon that runs commands (D-Bus, docker) is then a way out of the sandbox. |
| `allow_leaving_process_group` | bool | `false` | Allow `setsid`/`setpgid` (job control, some test runners). A process that leaves survives the call. |
| `share_system_tmp` | bool | `false` | Give confined commands read and write on the shared `/tmp`, including other programs' temp files. |

### `[sandbox]`

The default sandbox wrapped around tool processes that don't declare their own.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `default_backend` | string |  | `none` (no default sandbox), `auto` (bubblewrap, else firejail), `bubblewrap` or `firejail`. `aivyx-pa init` writes `auto`. |

### `[git]`

The repositories the `git.*` tools may read. Empty means the git tools are not offered.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `repos` | list of string |  | Paths to the repositories the git tools may read. Each must exist and contain `.git`. |

### `[aivyx_pa]`

The store passphrase. Prefer the OS keyring or `AIVYX_PA_PASSPHRASE` over writing it here.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `passphrase` | string |  | The store passphrase in plain text. Avoid this outside throwaway setups. |

### `[storage]`

Where the encrypted store lives.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `path` | path |  | Absolute path to the redb file. The parent directory must exist; `open` will not create parents. |

## Identity and roles

### `[profile]`

Who you are and how you like to work — the agent reads this every turn. See [Create your agent](../../guide/03-create-your-agent.md).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `assistant_name` | string |  | What the operator calls this specific assistant. Distinct from the product name (*Aivyx*) and from role names. |
| `operator_profile` | string |  | Short description of who the operator is — role, expertise level, primary work context. Drives domain-specific language and assumed background knowledge in the assistant's responses. |
| `communication_style` | string |  | Operator preferences on verbosity, formality, citation frequency, source referencing, list-vs-prose, etc. Free text — the wizard offers presets but the TOML is unstructured. |
| `primary_use_cases` | list of string |  | The 1–3 use-case archetypes the assistant is being shaped around (e.g. *"Rust systems programming"*, *"personal-finance analysis"*). |
| `behavioral_preferences` | list of string |  | Non-capability defaults that flavor the agent's judgment (e.g. *"prefer integration tests over mocks"*, *"always cite sources when summarizing"*). |
| `behavioral_constraints` | list of string |  | Non-capability guardrails the agent should respect across every role (e.g. *"never autonomously commit code"*, *"always confirm destructive shell commands"*). |

### `[persona_seed]`

Facets the agent's Persona starts with on first launch.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `learned_context` | list of string |  | Seed `learned_context` facets — facts about the operator/domain the agent should start with. |
| `communication_adaptations` | list of string |  | Seed `communication_adaptations` — voice refinements beyond the Profile's declared `communication_style`. |
| `character_traits` | list of string |  | Seed `character_traits` — emergent voice properties to start with. |
| `relationship_milestones` | list of string |  | Seed `relationship_milestones` — continuity anchors ("genesis: first launch"). |

#### `[[persona_seed.skill]]`

| Key | Type | Example | Meaning |
|---|---|---|---|
| `name` | string |  | Stable kebab-case identifier (`rust-review`). |
| `trigger` | string |  | When the skill applies — the trigger the agent reads each turn. |
| `procedure` | string |  | The skill body — instructions / a tool sequence / an example. |

### `[[role]]`

Roles: named bundles of model, prompt and capabilities you can switch between (`--role`). With no `[[role]]` blocks, a single `default` role is made from `[agent]`.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `name` | string | `"default"` | The role's unique name. |
| `system_prompt` | string |  | The role's system prompt. Unset uses the built-in default prompt. |
| `tool_allowlist` | list of string |  | Tools this role may call. Leave it out to allow every tool; an empty list allows none. |
| `memory_topic_prefix` | string | `"coder/"` | Prefix added to every memory topic while this role is active (e.g. `coder/`), keeping roles' memories apart. |
| `capability_scopes` | list of string |  | Capability scopes this role adds to what it inherits from its parent. |
| `trust_ceiling` | `Untrusted` \| `SemiTrusted` \| `Trusted` \| `Kernel` | `"Trusted"` | The highest trust tier this role may run at. |
| `parent_role` | string | `"default"` | The role this one inherits from. Cycles and missing parents are rejected at load time. |

### `[team]`

A custom team for multi-agent missions. See [Teams](../../guide/07-teams.md).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `config_path` | string | `"team.toml"` | Path to a team-config TOML file. Unset means the built-in team. |

### `[pack]`

Publisher keys you trust for `aivyx-pa pack install`, on top of the built-in Aivyx key.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `trusted_publishers` | list of string |  | Base64 Ed25519 public keys of pack publishers you trust. |
| `source` | string |  | The config pack this assistant was installed from (`<name>@<version>`). Written by `aivyx-pa pack install`; shown by `aivyx-pa instances list`. |

## Memory and learning

### `[memory]`

Long-term memory: the profile, size caps and retention. See [Memory](../../guide/05-memory.md).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `max_per_topic` | integer | `200` | Most entries kept per topic; past it, the least recently read entry is evicted. |
| `ttl_secs` | integer | `7776000` | Forget memory entries older than this many seconds. Unset keeps them. |
| `canonicalize_topics` | bool | `true` | Normalise topic names so near-identical topics merge. Off by default. |
| `profile` | string | `"smart"` | `lite` (smarter recall over existing memory, no model calls), `smart` (adds the wiki and knowledge graph, which cost model calls), or unset for neither. |

#### `[[memory.retention]]`

Each entry is a per-topic-glob retention rule (forever or N days). The loader compiles + validates each pattern and builds the `memory_retention: Vec<MemoryRetentionRule>` on the public type.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `topic_glob` | string | `"project/**"` | Topics this retention rule applies to (glob, e.g. `project/**`). |
| `retention` | string | `"forever"` | String discriminator. Today only `"forever"` is recognized; future variants land here. |
| `retention_days` | integer | `30` | Numeric retention period in days. Mutually exclusive with `retention`. |

### `[workspace]`

The agent's own workspace — a directory for its notes, plans and journal, separate from your files.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool | `true` | Turn the agent's workspace on or off. On by default. |
| `path` | path | `"~/.aivyx-pa/workspace"` | Where the workspace lives. Default `~/.aivyx-pa/workspace`. |

#### `[workspace.journaling]`

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool | `true` | Let the agent write in its journal on its own after recent activity. On by default. |
| `interval_secs` | integer | `21600` | Seconds between journaling passes. Default 21600 (6 hours). |

### `[recall_cluster]`

Recall memories that usually come up together. Off by default.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool | `true` | Master switch. Default `false`; even with the section present and a populated ledger, recall is unchanged until this is `true`. |
| `max_siblings` | integer | `3` | Hard per-turn cap on injected sibling memories. They share the existing `rag_top_k` budget (displacing the weakest primary hits), so this also bounds how much of the budget cluster expansion may claim. |
| `min_affinity` | number | `1.0` | A sibling's decayed co-occurrence score must be at least this for the pair to be eligible — the bar that keeps weak/noisy affinities out of recall context. |

### `[wiki]`

Background synthesis of a knowledge wiki from memory. Off by default.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool | `true` | Master switch. Default `false` — no synthesis, no sweep. |
| `max_pages_per_sweep` | integer | `20` | Max pages (re)generated per sweep, bounding LLM calls per pass. |
| `interval_secs` | integer | `3600` | Seconds between sweeps. |

### `[graph]`

Background extraction of a typed knowledge graph from memory. Off by default.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool | `true` | Master switch. Default `false` — no extraction, no sweep. |
| `max_topics_per_sweep` | integer | `20` | Max topics (re)extracted per sweep, bounding LLM calls per pass. |
| `interval_secs` | integer | `3600` | Seconds between sweeps. |
| `vocabulary` | table of list of string |  | Canonical relation → extra synonym phrases. |

### `[recall_judgment]`

Let a model judge, during reflection, whether each recalled memory actually helped. Off by default; costs model calls.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool | `true` | Master switch. Default `false`. |
| `max_recalls_per_cycle` | integer | `30` | Hard upper bound on how many recall events the batched LLM call may judge in one cron tick. Past this cap, the oldest unjudged recalls in the window are skipped for the cycle (recorded on the stat surface but never fail the cron). |

### `[recall_feedback]`

Use the model's judgments (above) instead of the built-in heuristic when scoring recall.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `use_judgment_signal` | bool | `true` | Master switch. Default `false`. |

### `[correction_judgment]`

Let a model classify your corrections during reflection. Off by default; costs model calls.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool |  | Master switch. Default `false`. |
| `max_corrections_per_cycle` | integer |  | Hard upper bound on how many corrections the batched LLM call may judge in one cron tick. Past this cap, the remaining corrections fall back to the structural signal for the cycle (counted, never dropped). |

### `[correction_signal]`

Also attribute your corrections to the tools the corrected turn used.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `attribute_tools` | bool |  | Also attribute corrections to the corrected turn's **tools** (keyed `tool:<scope_base>`), via the outcome-driven detector. Broadens the signal to no-recall turns the recall-driven detector misses. |

### `[persona_lifecycle]`

Merge near-duplicate Persona facets and retire unhelpful ones over time.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool | `true` | Master switch. Default `false`; even with the section present the pass is a no-op until this is `true`. |
| `consolidation_similarity` | number | `0.92` | Cosine threshold above which two facets in the same soft list are treated as near-duplicates and a merge is proposed. In `(0.0, 1.0]`; high by design. |
| `decay_max_age_secs` | integer | `7776000` | A soft-list facet whose originating delta is older than this many seconds, with no later reinforcing delta in its category, is proposed for decay. |
| `min_soft_facets` | integer | `6` | Never act on a soft list with fewer than this many facets — a small Soul has nothing worth pruning. |
| `decay_unhelpful_threshold` | number | `-2.0` | A recall-feedback-derived facet whose associated topic's durable decayed helpfulness is **at or below** this (negative) value is treated as "sustained low": it may be proposed for decay before the age horizon, and a strongly-positive topic (>= the magnitude of this value) instead *protects* an age-old facet from age-decay. |
| `decay_min_samples` | integer | `3` | Confidence floor: a topic's helpfulness is only consulted once it has at least this many ledger samples. Identity is never decayed (or protected) on thin evidence. |
| `decay_pair_below_affinity` | number | `1.0` | A `consolidate-pair:` facet whose pair's decayed affinity is **below** this floor is treated as "relationship no longer durable": the facet may be proposed for decay before the age horizon, and symmetrically, a pair whose affinity is **at or above** this floor *protects* its facet from age-decay. |
| `signal_consolidate` | bool | `true` | Merge near-duplicate Persona facets. |
| `signal_decay` | bool | `true` | Retire Persona facets that keep proving unhelpful. |

### `[persona_consolidation]`

Propose Persona updates from patterns in what you recall together.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool | `true` | Master switch. Default `false`; even with the section present and ledgers populated, no consolidation proposals are filed until this is `true`. |
| `min_affinity` | number | `1.0` | The decayed pair-affinity floor a candidate must clear — the same idea (and same default) as the `recall_cluster.min_affinity`, applied to the proposal-side of the symmetric arc. |
| `min_samples` | integer | `3` | Minimum observation count on the pair before it is proposal-eligible. Mirrors `decay_min_samples`: identity is never proposed on thin evidence. |
| `min_topic_helpfulness` | number | `0.0` | Both endpoints' helpfulness-ledger scores must be at least this value. Default `0.0` enforces "non-negative" — a pattern made of topics that individually hurt is never proposed; raise it to require *positive* helpfulness on both sides. |
| `max_proposals_per_cycle` | integer | `3` | Hard cap on filings per reflection cycle. Mirrors the `max_per_cycle` precedent — actuators on the reflection cadence never flood the operator's queue. |
| `enable_supersession` | bool | `true` | When `true`, the consolidation pass also detects **supersession**: an existing applied `consolidate-pair:{A}+{B}` facet whose pair has decayed plus a new candidate pair `(A, C)` sharing one endpoint that strengthens past the construction floor → file two linked proposals (`RemoveList` for the old facet, `AppendList` for the new one) sharing a `supersedes_proposal_id` so the operator-facing surface presents them as a single supersession decision. |

### `[correction_consolidation]`

Propose Persona updates from topics you keep correcting.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool |  | Master switch. Default `false`; even with the section present and the ledger populated, no correction proposals are filed until this is `true`. |
| `min_corrections` | number |  | The decayed correction count a topic must clear before it is proposal-eligible. Default `3.0` — three reworks is the "this is a pattern, not a one-off" bar, mirroring the reflection loop's recurs-in-at-least-3-turns discipline. |
| `min_samples` | integer |  | Minimum number of reflection windows that folded into the topic before it is proposal-eligible. Identity is never proposed on a single noisy window. |
| `max_proposals_per_cycle` | integer |  | Hard cap on filings per reflection cycle. Same value and reasoning as the cap — a passive actuator on the reflection cadence never floods the review queue. |

### `[skills]`

Have the agent propose new skills from turns that went well. Proposals wait for your approval. See [Skills and persona](../../guide/06-skills-and-persona.md).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `starter` | bool |  | Give a fresh agent the built-in starter skills. On by default. |
| `trigger_injection` | bool |  | Each turn, add the procedure of the best-matching approved skill to the agent's context. On by default; off means the agent must look skills up itself. |

#### `[skills.auto_propose]`

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool |  | Turn this on or off. |
| `judge_model` | string |  | Model that judges candidate proposals. Unset means the agent's own model. |
| `judge_max_tokens` | integer |  | Most tokens the judge may write. Default 800. |
| `auto_accept_confidence_threshold` | number |  | Apply a proposal without asking when the judge's confidence is at least this (0–1). |
| `fuzzy_match_threshold` | number |  | Similarity (0–1) above which a candidate skill counts as a duplicate of an existing one. |

##### `[skills.auto_propose.heuristic]`

| Key | Type | Example | Meaning |
|---|---|---|---|
| `tool_call_count_min` | integer |  | A turn qualifies only with at least this many tool calls… |
| `distinct_tool_id_min` | integer |  | …and at least this many different tools… |
| `duration_ms_min` | integer |  | …and lasting at least this long, in milliseconds. |
| `require_gate_resolve` | bool |  | Also require that you approved a gated action during the turn. |
| `mode` | string |  | `any` (default) — one condition above is enough; `all` — every one must hold. |

### `[persona]`

Have the agent propose Persona and Profile updates, per category. Proposals wait for your approval.


#### `[persona.auto_propose]`

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool |  | Turn proposals on. Defaults on once the section exists. |
| `judge_model` | string |  | Model that judges candidate proposals. Unset means the agent's own model. |
| `judge_max_tokens` | integer |  | Max tokens the judge may emit. Default 800. |
| `fuzzy_match_threshold` | number |  | Similarity (0–1) above which a candidate skill counts as a duplicate of an existing one. |
| `from_failed_turns` | bool |  | Also learn from turns that went wrong (see `failure_outcomes`). Off by default. |

##### `[persona.auto_propose.heuristic]`

| Key | Type | Example | Meaning |
|---|---|---|---|
| `tool_call_count_min` | integer |  | A turn qualifies only with at least this many tool calls… |
| `distinct_tool_id_min` | integer |  | …and at least this many different tools… |
| `duration_ms_min` | integer |  | …and lasting at least this long, in milliseconds. |
| `require_gate_resolve` | bool |  | Also require that you approved a gated action during the turn. |
| `mode` | string |  | `any` (default) — one condition above is enough; `all` — every one must hold. |

##### `[persona.auto_propose.failure_outcomes]`

| Key | Type | Example | Meaning |
|---|---|---|---|
| `failed` | bool |  | Learn from turns that failed. |
| `cancelled` | bool |  | Learn from turns you cancelled. |
| `timed_out` | bool |  | Learn from turns that timed out. |
| `escalated` | bool |  | Learn from turns that escalated to you. |

##### `[persona.auto_propose.<category>]`

One sub-table per category: `assistant_name`, `operator_profile`, `communication_style`, `primary_use_cases`, `behavioral_preferences`, `behavioral_constraints`, `learned_context`, `communication_adaptations`, `character_traits`, `relationship_milestones`, `learned_skill`, `profile_hint`, `role_definition_suggestion`.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool |  | Turn proposals on. Defaults on once the section exists. |
| `auto_accept_confidence_threshold` | number |  | Apply a proposal without asking when the judge's confidence is at least this (0–1). |

### `[skill_refinement]`

Propose improvements to skills that keep underperforming.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool | `true` | Master switch. Default `false`. |
| `floor` | number | `0.0` | Decayed-EWMA floor below which a skill is an underperformer. |
| `min_samples` | integer | `4` | Minimum folded windows before a skill is refinement-eligible. |
| `max_per_cycle` | integer | `2` | Hard cap on refinement proposals filed per reflection cycle. |

### `[skill_authoring]`

Propose new skills from well-developed wiki topics.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool | `true` | Master switch. Default `false`. |
| `min_summary_chars` | integer | `200` | Wiki-summary length floor (chars) for a topic to be a candidate. |
| `min_edges` | integer | `2` | Minimum typed-graph edges around the topic. |
| `max_per_cycle` | integer | `1` | Hard cap on authored skills per reflection cycle. |

### `[skill_defaults]`

Extra directories of `SKILL.md` capability packages, alongside the bundled defaults.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `project_dir` | string | `"/path/to/repo/.aivyx/skills"` | A directory holding one `<skill-name>/SKILL.md` per skill. A wrong layout gives zero skills, not an error. |
| `user_dir` | string | `"/home/you/.config/aivyx-pa/skills"` | A second such directory, e.g. under `~/.config/aivyx-pa/skills`. |

### `[tool_relevance]`

Track which tools and skills help for which kinds of request, and tell the model each turn.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool |  | Master switch. Default `false`. |
| `max_keywords` | integer |  | Top-K keywords extracted from the user input for the ledger key. Default `5`. |
| `min_outcomes_to_show` | integer |  | Minimum total outcomes (`success + failure`) for a row to appear in the rendered relevance section. Default `2` — don't show a tool tried just once; one data point isn't a pattern. |
| `top_k_per_section` | integer |  | Maximum rows per subsection (Tools / Skills) in the rendered relevance section. Default `5` — keeps the prompt section bounded. |

### `[[reflection_schedule]]`

When the agent reflects on recent turns — the pass that feeds the learning features above.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `name` | string | `"nightly-reflection"` | Operator-chosen name, unique across reflection schedules and across regular `[[schedule]]` entries. |
| `cron` | string | `"0 0 23 * * *"` | When it runs, as a cron pattern in your local time: 6 or 7 fields, seconds first. A 5-field pattern is rejected. |
| `lookback_window_secs` | integer | `86400` | How far back to look when summarizing turn outcomes for the reflection prompt. Minimum 60 seconds, maximum 30 days. |
| `role_override` | string |  | Run the reflection as this role instead of the built-in reflection setup. |
| `enabled` | bool |  | `true` when the entry is active; `false` keeps the entry in the config but skips scheduler registration. |
| `skip_when_idle` | bool | `true` | Skip a run when little has happened since the last one. Off by default. |
| `min_audit_entries_to_fire` | integer | `100` | With `skip_when_idle`, the fewest new audit entries that still count as activity. Default 1. |

## Chat apps and voice

### `[telegram]`

The Telegram bot channel. See [Chat apps and accounts](../../guide/13-chat-apps-and-accounts.md).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `token` | string |  | The bot token from @BotFather. Prefer the encrypted store (`aivyx-pa connect`) over writing it here. |
| `chat_id` | integer |  | Your chat with the bot. Only messages from this chat are handled. |
| `team_run_channel` | bool |  | Operator opt-in for `/team run <goal>` from this channel. Unset/false: the command is recognized but always replies with a capability-denial message, both client-side (fail-fast UX) and server-side (the real enforcement). |
| `team_trigger_rate_limit` | integer |  | Max `/team run` confirmations accepted per rolling hour from this channel (client-side enforced). Unset = unlimited. |
| `team_command_allowed_senders` | list of integer |  | The Telegram user ids allowed to issue any `/team...` command (status/approve/ reject/pause/resume/abort/run) from this channel. Empty/unset: no sender is authorized — deny by default, closing a real gap (previously, any sender in a connected chat could act). |

### `[discord]`

The Discord bot channel. See [Chat apps and accounts](../../guide/13-chat-apps-and-accounts.md).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `token` | string | `"your_bot_token_here"` | Bot token (Bot API token from the Discord developer portal; lands in the `Authorization: Bot <token>` header for REST and in the `Identify` payload for Gateway). |
| `application_id` | integer |  | Optional application_id. Reserved for future slash-command registration. |
| `channel_filter` | integer |  | Optional channel_id filter, mirroring Telegram's `chat_filter`. Unset = no channel allowlisted; `id` = only the Discord channel with this snowflake is allowlisted. |
| `team_run_channel` | bool |  | Operator opt-in for `/team run <goal>` from this channel. Unset/false: the command is recognized but always replies with a capability-denial message, both client-side (fail-fast UX) and server-side (the real enforcement). |
| `team_trigger_rate_limit` | integer |  | Max `/team run` confirmations accepted per rolling hour from this channel (client-side enforced). Unset = unlimited. |
| `team_command_allowed_senders` | list of integer |  | The Discord user ids allowed to issue any `/team...` command (status/approve/ reject/pause/resume/abort/run) from this channel. Empty/unset: no sender is authorized — deny by default, closing a real gap (previously, any sender in a connected chat could act). |

### `[slack]`

The Slack bot channel (Socket Mode). See [Chat apps and accounts](../../guide/13-chat-apps-and-accounts.md).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `bot_token` | string | `"xoxb-..."` | Bot token (`xoxb-...`) — Slack OAuth's bot-user access token. Used for REST `chat.postMessage` and any other Web API calls. |
| `app_token` | string | `"xapp-..."` | App-level token (`xapp-...`) — the Socket Mode token that lets the bot open an outbound WebSocket to Slack instead of accepting inbound Events API webhooks. |
| `team_id` | string |  | Optional `team_id` constraint (`T0123456789`). |
| `channel_filter` | string |  | Optional channel_id filter, mirroring Telegram's `chat_filter` at Slack's own `channel_id` granularity (finer than `team_id`'s workspace-wide scope). Unset = no channel allowlisted. |
| `team_run_channel` | bool |  | Operator opt-in for `/team run <goal>` from this channel. Unset/false: the command is recognized but always replies with a capability-denial message, both client-side (fail-fast UX) and server-side (the real enforcement). |
| `team_trigger_rate_limit` | integer |  | Max `/team run` confirmations accepted per rolling hour from this channel (client-side enforced). Unset = unlimited. |
| `team_command_allowed_senders` | list of string |  | The Slack user ids allowed to issue any `/team...` command (status/approve/ reject/pause/resume/abort/run) from this channel. Empty/unset: no sender is authorized — deny by default, closing a real gap (previously, any sender in a connected chat could act). |

### `[voice]`

The voice channel (`--channel voice`): speech recognition and text-to-speech engines and devices.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `asr_engine` | string |  | `"whisper-rs"` (default) or `"whisper-cpp-plus"`. |
| `tts_engine` | string |  | Currently `"piper"`. |
| `asr_model_path` | path |  | Absolute path to the Whisper `.bin` model. Required when `--channel voice`. |
| `asr_language` | string |  | ASR language code (`"en"`, `"auto"`, etc.). |
| `asr_beam_size` | integer |  | ASR beam search width. Higher = more accurate, slower. |
| `tts_model_dir` | path |  | Kokoro model directory (holds the `.onnx` model + `voices-*.bin` + optional `config.json`). Required when `--channel voice`. |
| `tts_voice_name` | string |  | Kokoro voice name (e.g. `af_heart`). |
| `tts_speed` | number |  | Kokoro speaking-rate multiplier (1.0 = normal). Optional; defaults to 1.0. |
| `input_device` | string |  | Optional cpal input device name override. |
| `output_device` | string |  | Optional cpal output device name override. |

### `[email]`

Outgoing mail (SMTP) for the email tools and notifications.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `host` | string | `"smtp.fastmail.com"` | SMTP server hostname (e.g. `"smtp.gmail.com"`, `"smtp.fastmail.com"`). |
| `port` | integer | `587` | SMTP server port. Defaults to 587 for STARTTLS or 465 for implicit TLS; an explicit override wins. |
| `tls_mode` | string | `"starttls"` | TLS mode for the connection. |
| `username` | string | `"you@example.com"` | SMTP username. Often the same as `from` but explicit so providers using account-id-as-username (some self-hosted setups) are supported. |
| `password` | string | `"<app-password>"` | SMTP password. For Gmail and most cloud providers this is an "app password," not the operator's account password. |
| `from` | string | `"aivyx@example.com"` | Sender address. Appears in the `From:` header. |

## Tools and integrations

### `[[mcp_server]]`

MCP servers whose tools the agent can use. One block per server.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `name` | string | `"github"` | A unique name for this entry. |
| `transport` | string |  | Transport kind: `"stdio"` (default), `"sse"`, or `"http"` (Streamable HTTP; alias `"streamable-http"`). |
| `command` | string | `"npx"` | Command to spawn (stdio transport). |
| `args` | list of string |  | Command-line arguments (stdio transport only). |
| `url` | string |  | SSE endpoint URL (SSE transport). |
| `env` | table of string |  | Env vars for a stdio server's child. Values may be literals or `${VAR}` references resolved from the daemon environment at load time. |
| `headers` | table of string |  | HTTP headers for sse/http transports. |
| `enabled` | bool |  | Turn this on or off. |
| `bundled` | bool |  | When `true`, resolve `command` to the current binary path at runtime. Used for bundled MCP servers that ship inside the `aivyx-pa` binary. |

#### `[mcp_server.sandbox]`

| Key | Type | Example | Meaning |
|---|---|---|---|
| `wrapper` | string |  | The sandbox program to run the server under (e.g. `bwrap`). |
| `args` | list of string |  | Arguments for the sandbox program. |

### `[[tool_process]]`

Tool processes: out-of-process tools (any language) that speak the tool protocol. One block per tool.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `name` | string |  | A unique name for this entry. |
| `command` | string |  | The program to run. |
| `args` | list of string |  | Arguments for the program. |
| `env` | table of string |  | Extra environment variables for the program. |
| `scope_overrides` | table of string |  | Per-tool scope overrides keyed by tool name. Operator may only narrow what the tool declared; the daemon rejects widenings. |
| `expected_scopes` | table of string |  | Per tool name, the widest scope that tool may declare for itself. Once set, it also acts as an allowlist of tool names for this process. |
| `enabled` | bool |  | Turn this on or off. |
| `disable_sandbox` | bool |  | Opt this tool process out of the bundled `[sandbox].default_backend` preset. Ignored when an explicit `sandbox` block is present (that always wins). |

#### `[tool_process.sandbox]`

| Key | Type | Example | Meaning |
|---|---|---|---|
| `wrapper` | string |  | The sandbox program to run the tool under (e.g. `bwrap`). |
| `args` | list of string |  | Arguments for the sandbox program. |

### `[applications]`

Let the agent see and drive the desktop apps you have open (Linux/X11). Off by default.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool |  | Master switch. Default off. |
| `binary_path` | string |  | Override the `aivyx-apps` binary path (default: `aivyx-apps` on PATH). |

## Routines and the autonomous loop

### `[[schedule]]`

Routines: run a prompt on a cron schedule. See [Autonomy and routines](../../guide/15-autonomy-and-routines.md).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `name` | string | `"morning-summary"` | A unique name for this entry. |
| `cron` | string | `"0 0 9 * * * *"` | When it runs, as a cron pattern in your local time: 6 or 7 fields, seconds first (`0 0 9 * * *` = 09:00 daily). A 5-field pattern is rejected. |
| `role` | string |  | The role the prompt runs as. |
| `prompt` | string |  | The prompt sent to the agent when this fires. |
| `enabled` | bool |  | Turn this on or off. |
| `wrap_mission` | bool | `false` | Record each run as a mission (visible in missions and the Studio). |
| `notify_target` | string | `"phone"` | Singular alias. Kept for backwards compatibility; loader bridges into `notify_targets`. |
| `notify_targets` | list of string | `["phone", "desktop"]` | Explicit list of notify target names for multi-target fan-out. Empty + a default-marked `[[notify_target]]` exists → loader resolves the default. |
| `notify_when` | string | `"on_completed_non_empty"` | Conditional dispatch gate. Default `"always"`. |
| `report_kind` | string |  | `report_kind = "digest"` → deterministic report. |

#### `[schedule.team_mission]`

Mutually exclusive with `role`/`prompt` at the loader level.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `goal` | string |  | Run a team mission with this goal instead of a single prompt. |
| `pack_config` | string |  | A path to a vertical-pack `TeamConfig` TOML file (e.g. `crates/verticals/aivyx-kitchen/assets/kitchen-boh.toml`). |

### `[[webhook]]`

Run a prompt when a webhook is called.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `name` | string |  | A unique name for this entry. |
| `role` | string |  | The role the prompt runs as. |
| `prompt` | string |  | The prompt sent to the agent when this fires. |
| `enabled` | bool |  | Turn this on or off. |
| `wrap_mission` | bool |  | Record each run as a mission (visible in missions and the Studio). |
| `notify_target` | string |  | A single notify target (older spelling of `notify_targets`; don't set both). |
| `notify_targets` | list of string |  | Names of `[[notify_target]]` entries to send the result to. |
| `notify_when` | string |  | When to send it: `always` (default), `on_failed`, or `on_completed_non_empty`. |

### `[[file_watch]]`

Run a prompt when a file or directory changes.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `name` | string |  | A unique name for this entry. |
| `path` | string |  | The file or directory to watch. |
| `role` | string |  | The role the prompt runs as. |
| `prompt` | string |  | The prompt sent to the agent when this fires. |
| `enabled` | bool |  | Turn this on or off. |
| `debounce_ms` | integer |  | Wait this long, in milliseconds, for changes to settle before firing. |
| `wrap_mission` | bool |  | Record each run as a mission (visible in missions and the Studio). |
| `notify_target` | string |  | A single notify target (older spelling of `notify_targets`; don't set both). |
| `notify_targets` | list of string |  | Names of `[[notify_target]]` entries to send the result to. |
| `notify_when` | string |  | When to send it: `always` (default), `on_failed`, or `on_completed_non_empty`. |

### `[[notify_target]]`

Where routines and the agent can send notifications (`notify.send`).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `name` | string | `"phone"` | A unique name for this entry. |
| `kind` | string | `"telegram"` | Lowercase string discriminator. Accepted values: `"telegram"`, `"webhook"`, `"email"`, and `"web-ui"`. |
| `chat_id` | string | `"123456789"` | Required when `kind = "telegram"`. The operator-owned Telegram chat the bot is authorized to message. |
| `url` | string | `"https://ntfy.sh/aivyx-personal-2026"` | Required when `kind = "webhook"`. The endpoint to POST to. |
| `to` | string | `"you@example.com"` | Required when `kind = "email"`. The recipient address; the shared SMTP credentials live in `[email]`. |
| `enabled` | bool |  | Turn this on or off. |
| `default` | bool | `true` | When `true`, this target is the global default triggers fall through to when they omit `notify_targets`. At most one notify_target may set this; loader rejects multiple defaults. |
| `retry_count` | integer | `5` | Retries after a transient failure (network error, timeout, or a 5xx reply). Default 0. |
| `retry_backoff_ms_start` | integer | `200` | Starting backoff for the first retry, in milliseconds. Each subsequent retry waits double the previous (`backoff * 2^attempt`). |
| `rate_limit_max` | integer | `20` | When both this and `rate_limit_window_secs` are `Some`, the daemon allows at most `rate_limit_max` dispatch attempts per `rate_limit_window_secs` per target. Excess attempts record `AutoNotifyOutcomeSummary::SkippedByRateLimit` in the audit chain and skip the backend call. |
| `rate_limit_window_secs` | integer | `3600` | Sliding-window length for `rate_limit_max`. Both fields must be set together or neither — the loader rejects partial config naming the missing field. |

### `[reminders]`

How often due reminders are checked.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `check_interval_secs` | integer |  | Seconds between checks for due reminders. Default 30. |

### `[proactive]`

Let the agent reach out first — rarely, and only for a concrete reason. Off by default.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool | `true` | Master switch. Default `false`; even with the section present the pass is a no-op until this is `true`. |
| `target` | string | `"me"` | Notify-target name the surfacing is dispatched to (must match a configured `[[notify_target]]`). Required when `enabled`. |
| `max_per_window` | integer | `3` | Most proactive messages per `window_secs`, whatever the signals say. Default 3. |
| `window_secs` | integer | `86400` | The cap's window, in seconds. Default `86_400`. |
| `signal_ttl_expiry` | bool | `true` | Surface a memory about to expire. |
| `signal_recall_cluster` | bool | `true` | Surface a topic whose memories have proved consistently helpful. |
| `signal_due_reminder` | bool | `true` | Surface a reminder whose time has arrived. |

### `[loop]`

The autonomous loop: works through a queue of stories until done or a limit is hit. Off by default. See [Autonomy and routines](../../guide/15-autonomy-and-routines.md).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `enabled` | bool |  | Master switch. Default `false`; even with the section present the driver is not spawned until this is `true`. |
| `max_iterations` | integer |  | Hard cap on iterations per run — the primary guardrail on a fully-autonomous, code-committing loop. A run stops once it reaches this many fresh-context iterations regardless of remaining backlog. |
| `default_priority` | integer |  | Priority assigned to a story added via `aivyx-pa loop add` without an explicit `--priority`. Lower runs first. |
| `gate_command` | string |  | The shell command the driver runs to verify the tree is green (e.g. `"cargo test"`). |
| `gate_timeout_secs` | integer |  | Kill the gate command + treat it as red if it runs longer than this many seconds. Default `600`. |
| `working_dir` | string |  | Directory the gate command runs in. Unset → the daemon's current working directory. |
| `max_run_secs` | integer |  | Wall-clock cap (seconds). A run stops once it has been running this long (checked between iterations). |
| `progress_inject_count` | integer |  | How many recent progress-log notes the driver injects into each fresh iteration's prompt (the cross-iteration learning, the Ralph `progress.txt` analog). `0` disables injection. |
| `max_run_tokens` | integer |  | Per-run token-budget cap. A run stops once the total token usage (input + output) of every turn that completes during the run exceeds this. |
| `max_run_usd` | number |  | Per-run **dollar**-budget cap. A run stops once the priced spend (`LlmCost` events over the run window, priced by the default table) reaches this. |
| `max_idle_iterations` | integer |  | Cross-iteration stall breaker. A run stops once this many *consecutive* iterations make no progress — neither completing/delegating a story (the backlog shrinks) nor recording a fresh progress note. |
| `resume_on_boot` | bool |  | Auto-resume an interrupted run on daemon boot. When `true`, a daemon restart while a run was active (a crash or a `systemctl restart`) re-starts the run if the backlog still has pending stories — so a "runs for days" agent under `Restart=on-failure` keeps working instead of silently stopping. |
| `verify_completion` | bool |  | Verify story completion with an LLM acceptance judge. When `true`, `loop.complete` is gated: the judge checks the agent's `summary` against the story's acceptance criteria (its `body`) and a FAIL keeps the story `Pending` (the agent is told why) instead of trusting the self-report. |
| `delegate_above` | integer |  | Deterministic auto-delegation threshold. `n` ⇒ before each solo turn the loop scores the next pending story (a pure structural complexity heuristic) and, if it scores `>= n`, hands it to the agent team (headless) instead of attempting it solo — so delegation does not depend on a small local model choosing `team.run`. |

## Cost and limits

### `[pricing.<name>]`

Per-model prices, in USD per million tokens, overriding the built-in table. Used for cost reports and budgets.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `input` | number |  | USD per million input tokens. |
| `output` | number |  | USD per million output tokens. |
| `cache_read` | number |  | USD per million cached input tokens read. |
| `cache_write` | number |  | USD per million input tokens written to the cache. |

### `[budget]`

Spending caps in USD (and tokens), and what happens when one is reached.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `per_run_usd` | number |  | Max USD per **run** — one mission / loop run / session. |
| `per_day_usd` | number |  | Max USD per **rolling day**. |
| `per_mission_usd` | number |  | Most USD one team mission started by the loop may spend across all its specialists. Unset means no cap. |
| `per_mission_tokens` | integer |  | Max **tokens** a single team mission may spend. Unset ⇒ unbounded. |
| `on_exceeded` | `alert` \| `deny` |  | Whether exceeding a cap alerts or denies. |
| `alert_at` | number |  | Warn once spend crosses this fraction of a cap (e.g. `0.8` = 80%). |

### `[rate_limit]`

Caps on tool calls per turn, overall and per tool.

| Key | Type | Example | Meaning |
|---|---|---|---|
| `per_turn_total` | integer |  | Max tool calls in a single turn (across all tools). |
| `default_per_turn_per_tool` | integer |  | Per-tool, per-turn cap applied to every tool without a `[tools.*]` override. |
| `on_exceeded` | `alert` \| `deny` |  | Whether exceeding a limit alerts or denies. |

#### `[rate_limit.tools.<name>]`

Per-tool overrides, keyed by tool name (e.g. `"web.fetch"`).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `per_turn` | integer |  | Max calls to this tool per turn (overrides `default_per_turn_per_tool`). |
| `per_window` | integer |  | Max calls to this tool within `window_secs` (the sliding window). |
| `window_secs` | integer |  | Sliding-window length in seconds. Defaults to 60 when `per_window` is set. |

## The daemon and the Studio

### `[daemon]`

The daemon's web listeners: the Studio and webhooks. See [Desktop app](../../guide/11-desktop-app.md) and [Named instances](../../guide/16-named-instances.md).

| Key | Type | Example | Meaning |
|---|---|---|---|
| `webhook_port` | integer |  | Port for the localhost-only webhook HTTP listener. |
| `web_ui` | bool | `false` | Serve the Studio. On by default; `false` wins over `web_ui_port`. |
| `web_ui_port` | integer | `7843` | The Studio's port. Default 7843; each named instance needs its own (`instances create` picks one from 7844). |
| `web_ui_host` | string | `"0.0.0.0"` | Bind host for the web UI server. Unset → `127.0.0.1` (the localhost-only default). |
| `web_ui_allowed_origins` | list of string |  | Extra WS Origin allowlist entries beyond the built-in loopback origins. Empty (default) keeps the localhost-only CSWSH posture. |
| `web_ui_auth_token` | string | `"..."` | Your own Studio sign-in token. Unset means the automatic one, kept in a `studio-token` file next to the store. |
| `web_ui_insecure_no_auth` | bool |  | Allow the Studio with no token at all — only for binding beyond loopback behind your own authenticating proxy. |
