#!/usr/bin/env python3
"""Generate docs/manual/reference/02-configuration.md from the config structs.

The reference is built from the code itself — `RawToml` in
crates/aivyx-config/src/lib.rs (the TOML file's shape) and the structs it
points to — so it lists exactly the sections and keys the loader accepts.
Each key's meaning comes from, in order: its doc comment, the doc comment on
the resolved struct it loads into, `MEANINGS` below, and the trailing comment
in examples/aivyx-pa.toml (which also supplies the Example column).

Re-run after changing the config schema:

    python3 scripts/gen-config-reference.py > docs/manual/reference/02-configuration.md

A new top-level section, or a key with no meaning from any source, fails the
run until it is placed in `GROUPS`/`INTROS` or given a doc comment, so the
reference can't silently fall behind.
"""
import json
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
EXAMPLE = ROOT / "examples" / "aivyx-pa.toml"
GUIDE = "../../guide"


def dependency_sources(names):
    """`src/**/*.rs` of the named Cargo dependencies (e.g. aivyx-route)."""
    try:
        meta = subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--offline"],
            cwd=ROOT, capture_output=True, text=True, check=True,
        ).stdout
    except (OSError, subprocess.CalledProcessError):
        return []
    out = []
    for pkg in json.loads(meta)["packages"]:
        if pkg["name"] in names:
            out += sorted(pathlib.Path(pkg["manifest_path"]).parent.glob("src/**/*.rs"))
    return out


SOURCES = sorted((ROOT / "crates").glob("*/src/**/*.rs")) + dependency_sources({"aivyx-route"})
TEXTS = {p: p.read_text(encoding="utf-8", errors="replace") for p in SOURCES}
PRIMITIVES = {
    "String", "bool", "u8", "u16", "u32", "u64", "usize", "i32", "i64",
    "f32", "f64", "PathBuf", "SecretString", "Value",
}

# ---------------------------------------------------------------------------
# Hand-written parts: section grouping, intros, and meanings for keys whose
# code has no doc comment. Plain words, for operators.
# ---------------------------------------------------------------------------

GROUPS = [
    ("Models and providers", [
        "agent", "anthropic", "openai", "ollama", "mistralrs", "broker", "kvcache",
        "providers", "routing", "embedding",
    ]),
    ("Reach and safety", ["fs", "access", "autonomy", "confine", "sandbox", "git", "aivyx_pa", "storage"]),
    ("Identity and roles", ["profile", "persona_seed", "role", "team", "pack"]),
    ("Memory and learning", [
        "memory", "workspace", "recall_cluster", "wiki", "graph", "recall_judgment",
        "recall_feedback", "correction_judgment", "correction_signal", "persona_lifecycle",
        "persona_consolidation", "correction_consolidation", "skills", "persona",
        "skill_refinement", "skill_authoring", "skill_defaults", "tool_relevance",
        "reflection_schedule",
    ]),
    ("Chat apps and voice", ["telegram", "discord", "slack", "voice", "email"]),
    ("Tools and integrations", ["mcp_server", "tool_process", "applications"]),
    ("Routines and the autonomous loop", [
        "schedule", "webhook", "file_watch", "notify_target", "reminders", "proactive", "loop",
    ]),
    ("Cost and limits", ["pricing", "budget", "rate_limit"]),
    ("The daemon and the Studio", ["daemon"]),
]

INTROS = {
    "agent": "The model the agent uses and how a turn behaves. Pairs with one provider section below. "
             f"See [Models and routing]({GUIDE}/12-models-and-routing.md).",
    "anthropic": "Anthropic as the provider (`[agent] provider = \"anthropic\"`).",
    "openai": "OpenAI, or any OpenAI-compatible server (llama.cpp, LM Studio, vLLM, Jan, Lemonade…) via `base_url`.",
    "ollama": "Generation options passed to Ollama when it is the provider. Unset keys keep Ollama's own defaults.",
    "mistralrs": "The embedded, in-process mistral.rs engine (`provider = \"mistralrs\"`) — runs a local GGUF model with no separate server.",
    "broker": "Share one local GPU server fairly with other processes (such as aivyx-coder) through `aivyx-broker`.",
    "kvcache": "Where saved KV-cache slots live when a local llama.cpp server is the backend.",
    "providers": "Provider-independent model settings.",
    "routing": "Per-call model routing: send each task to the best-suited model you have, escalate when needed, "
               f"never send sensitive conversations to the cloud. Off by default. See [Models and routing]({GUIDE}/12-models-and-routing.md).",
    "embedding": "Semantic memory search: the embeddings server, and how recall picks memories for each turn.",
    "fs": f"The directory the file tools work in. Usually set through `[access]` instead — see [Access and settings]({GUIDE}/08-access-and-settings.md).",
    "access": "How far the agent can reach on your machine and network: the access level, plus the guards for secrets "
              f"and private network addresses. See [Access and settings]({GUIDE}/08-access-and-settings.md).",
    "autonomy": "How much the agent may do without asking you, with per-domain exceptions. "
                f"See [Autonomy and routines]({GUIDE}/15-autonomy-and-routines.md).",
    "confine": "OS-level process confinement (Landlock + seccomp) for the commands the agent runs.",
    "sandbox": "The default sandbox wrapped around tool processes that don't declare their own.",
    "git": "The repositories the `git.*` tools may read. Empty means the git tools are not offered.",
    "aivyx_pa": "The store passphrase. Prefer the OS keyring or `AIVYX_PA_PASSPHRASE` over writing it here.",
    "storage": "Where the encrypted store lives.",
    "profile": f"Who you are and how you like to work — the agent reads this every turn. See [Create your agent]({GUIDE}/03-create-your-agent.md).",
    "persona_seed": "Facets the agent's Persona starts with on first launch.",
    "role": "Roles: named bundles of model, prompt and capabilities you can switch between (`--role`). "
            "With no `[[role]]` blocks, a single `default` role is made from `[agent]`.",
    "team": f"A custom team for multi-agent missions. See [Teams]({GUIDE}/07-teams.md).",
    "pack": "Publisher keys you trust for `aivyx-pa pack install`, on top of the built-in Aivyx key.",
    "memory": f"Long-term memory: the profile, size caps and retention. See [Memory]({GUIDE}/05-memory.md).",
    "workspace": "The agent's own workspace — a directory for its notes, plans and journal, separate from your files.",
    "recall_cluster": "Recall memories that usually come up together. Off by default.",
    "wiki": "Background synthesis of a knowledge wiki from memory. Off by default.",
    "graph": "Background extraction of a typed knowledge graph from memory. Off by default.",
    "recall_judgment": "Let a model judge, during reflection, whether each recalled memory actually helped. Off by default; costs model calls.",
    "recall_feedback": "Use the model's judgments (above) instead of the built-in heuristic when scoring recall.",
    "correction_judgment": "Let a model classify your corrections during reflection. Off by default; costs model calls.",
    "correction_signal": "Also attribute your corrections to the tools the corrected turn used.",
    "persona_lifecycle": "Merge near-duplicate Persona facets and retire unhelpful ones over time.",
    "persona_consolidation": "Propose Persona updates from patterns in what you recall together.",
    "correction_consolidation": "Propose Persona updates from topics you keep correcting.",
    "skills": f"Have the agent propose new skills from turns that went well. Proposals wait for your approval. See [Skills and persona]({GUIDE}/06-skills-and-persona.md).",
    "persona": "Have the agent propose Persona and Profile updates, per category. Proposals wait for your approval.",
    "skill_refinement": "Propose improvements to skills that keep underperforming.",
    "skill_authoring": "Propose new skills from well-developed wiki topics.",
    "skill_defaults": "Extra directories of `SKILL.md` capability packages, alongside the bundled defaults.",
    "tool_relevance": "Track which tools and skills help for which kinds of request, and tell the model each turn.",
    "reflection_schedule": "When the agent reflects on recent turns — the pass that feeds the learning features above.",
    "telegram": f"The Telegram bot channel. See [Chat apps and accounts]({GUIDE}/13-chat-apps-and-accounts.md).",
    "discord": f"The Discord bot channel. See [Chat apps and accounts]({GUIDE}/13-chat-apps-and-accounts.md).",
    "slack": f"The Slack bot channel (Socket Mode). See [Chat apps and accounts]({GUIDE}/13-chat-apps-and-accounts.md).",
    "voice": "The voice channel (`--channel voice`): speech recognition and text-to-speech engines and devices.",
    "email": "Outgoing mail (SMTP) for the email tools and notifications.",
    "mcp_server": "MCP servers whose tools the agent can use. One block per server.",
    "tool_process": "Tool processes: out-of-process tools (any language) that speak the tool protocol. One block per tool.",
    "applications": "Let the agent see and drive the desktop apps you have open (Linux/X11). Off by default.",
    "schedule": f"Routines: run a prompt on a cron schedule. See [Autonomy and routines]({GUIDE}/15-autonomy-and-routines.md).",
    "webhook": "Run a prompt when a webhook is called.",
    "file_watch": "Run a prompt when a file or directory changes.",
    "notify_target": "Where routines and the agent can send notifications (`notify.send`).",
    "reminders": "How often due reminders are checked.",
    "proactive": "Let the agent reach out first — rarely, and only for a concrete reason. Off by default.",
    "loop": f"The autonomous loop: works through a queue of stories until done or a limit is hit. Off by default. See [Autonomy and routines]({GUIDE}/15-autonomy-and-routines.md).",
    "pricing": "Per-model prices, in USD per million tokens, overriding the built-in table. Used for cost reports and budgets.",
    "budget": "Spending caps in USD (and tokens), and what happens when one is reached.",
    "rate_limit": "Caps on tool calls per turn, overall and per tool.",
    "daemon": f"The daemon's web listeners: the Studio and webhooks. See [Desktop app]({GUIDE}/11-desktop-app.md) and [Named instances]({GUIDE}/16-named-instances.md).",
}

# Plain-words meanings. An exact "<section path>.<key>" entry wins over the
# key's doc comment (use it where the comment is written for developers); a
# "*.<tail>" entry is only a fallback for keys with no doc comment anywhere.
MEANINGS = {
    "*.enabled": "Turn this on or off.",
    "*.name": "A unique name for this entry.",
    "*.role": "The role the prompt runs as.",
    "*.prompt": "The prompt sent to the agent when this fires.",
    "*.wrap_mission": "Record each run as a mission (visible in missions and the Studio).",
    "*.auto_accept_confidence_threshold": "Apply a proposal without asking when the judge's confidence is at least this (0–1).",
    "anthropic.api_key": "Your Anthropic API key. Prefer the encrypted store (`aivyx-pa init` or the Studio) or `ANTHROPIC_API_KEY`.",
    "openai.api_key": "API key for the server. Not needed for a local server. Prefer the encrypted store or `OPENAI_API_KEY`.",
    "agent.model": "The model id, as the provider names it (e.g. `qwen3:14b`, `claude-sonnet-5`).",
    "agent.system_prompt": "Extra instructions added to the agent's system prompt.",
    "agent.provider": "Which backend serves the model.",
    "fs.root": "The directory the file tools may work in.",
    "access.level": "How far the agent reaches: `sandbox` (its own directory), `workspace` (a directory you choose), `home`, `full`, or `custom`.",
    "access.root": "The directory for the `workspace` and `custom` levels (required for those).",
    "access.confirm_destructive": "Ask before irreversible actions (delete, overwrite, destructive shell, outbound messages). On by default above `sandbox`.",
    "autonomy.level": "The global autonomy level.",
    "autonomy.override.domain": "A capability domain, such as `shell` or `email`.",
    "autonomy.override.level": "The autonomy level for calls in that domain. The most specific match wins.",
    "autonomy.auto_approve.scopes": "Reversible capability scopes that may run without asking. Never covers irreversible actions.",
    "workspace.enabled": "Turn the agent's workspace on or off. On by default.",
    "kvcache.store_path": "The KV-cache store directory. When aivyx-coder shares the same llama.cpp server, point both at the same directory.",
    "memory.max_per_topic": "Most entries kept per topic; past it, the least recently read entry is evicted.",
    "memory.retention.topic_glob": "Topics this retention rule applies to (glob, e.g. `project/**`).",
    "git.repos": "Paths to the repositories the git tools may read. Each must exist and contain `.git`.",
    "team.config_path": "Path to a team-config TOML file. Unset means the built-in team.",
    "pack.trusted_publishers": "Base64 Ed25519 public keys of pack publishers you trust.",
    "proactive.signal_recall_cluster": "Surface a topic whose memories have proved consistently helpful.",
    "proactive.signal_due_reminder": "Surface a reminder whose time has arrived.",
    "persona_lifecycle.signal_decay": "Retire Persona facets that keep proving unhelpful.",
    "routing.sensitive.tool_prefixes": "Tool-name prefixes that mark a conversation sensitive (it then never escalates to a cloud model). The default list covers tools that return your own data.",
    "reminders.check_interval_secs": "Seconds between checks for due reminders. Default 30.",
    "skills.auto_propose.judge_model": "Model that judges candidate proposals. Unset means the agent's own model.",
    "skills.auto_propose.judge_max_tokens": "Most tokens the judge may write. Default 800.",
    "skills.auto_propose.fuzzy_match_threshold": "Similarity (0–1) above which a candidate skill counts as a duplicate of an existing one.",
    "*.heuristic.tool_call_count_min": "A turn qualifies only with at least this many tool calls…",
    "*.heuristic.distinct_tool_id_min": "…and at least this many different tools…",
    "*.heuristic.duration_ms_min": "…and lasting at least this long, in milliseconds.",
    "*.heuristic.require_gate_resolve": "Also require that you approved a gated action during the turn.",
    "persona.auto_propose.failure_outcomes.failed": "Learn from turns that failed.",
    "persona.auto_propose.failure_outcomes.cancelled": "Learn from turns you cancelled.",
    "persona.auto_propose.failure_outcomes.timed_out": "Learn from turns that timed out.",
    "persona.auto_propose.failure_outcomes.escalated": "Learn from turns that escalated to you.",
    "providers.tool_name_auto_correct_threshold": "When the model calls a tool name that doesn't exist, use the closest real tool if the similarity is at least this (0–1).",
    "ollama.num_ctx": "Context window, in tokens.",
    "ollama.num_predict": "Most tokens to generate per reply.",
    "ollama.num_thread": "CPU threads to use.",
    "ollama.mirostat": "Mirostat sampling: 0 off, 1 or 2 on.",
    "ollama.top_k": "Top-k sampling.",
    "ollama.top_p": "Top-p (nucleus) sampling.",
    "ollama.repeat_penalty": "Penalty for repeating tokens.",
    "ollama.repeat_last_n": "How many recent tokens the repeat penalty looks at.",
    "ollama.seed": "Random seed, for repeatable output.",
    "pricing.input": "USD per million input tokens.",
    "pricing.output": "USD per million output tokens.",
    "pricing.cache_read": "USD per million cached input tokens read.",
    "pricing.cache_write": "USD per million input tokens written to the cache.",
    "broker.base_url": "The broker's address.",
    "aivyx_pa.passphrase": "The store passphrase in plain text. Avoid this outside throwaway setups.",
    "mcp_server.sandbox.wrapper": "The sandbox program to run the server under (e.g. `bwrap`).",
    "mcp_server.sandbox.args": "Arguments for the sandbox program.",
    "tool_process.command": "The program to run.",
    "tool_process.args": "Arguments for the program.",
    "tool_process.env": "Extra environment variables for the program.",
    "tool_process.sandbox.wrapper": "The sandbox program to run the tool under (e.g. `bwrap`).",
    "tool_process.sandbox.args": "Arguments for the sandbox program.",
    "sandbox.default_backend": "`none` (no default sandbox), `auto` (bubblewrap, else firejail), `bubblewrap` or `firejail`. `aivyx-pa init` writes `auto`.",
    "schedule.team_mission.goal": "Run a team mission with this goal instead of a single prompt.",
    "file_watch.path": "The file or directory to watch.",
    "file_watch.debounce_ms": "Wait this long, in milliseconds, for changes to settle before firing.",
    "*.notify_targets": "Names of `[[notify_target]]` entries to send the result to.",
    "*.notify_when": "When to send it: `always` (default), `on_failed`, or `on_completed_non_empty`.",
    "routing.endpoints.kind": "What kind of server this is.",
    "routing.endpoints.base_url": "The server's URL. Unset uses the kind's usual local address.",
    "routing.tasks.tier": "The tier this task prefers. Keyed by task name (`chat`, `code_edit`, `plan`, `judge`, `summarize`, `compact`, `classify`, `embed`).",
    "routing.tasks.strengths": "Strengths this task prefers in a model.",
    "confine.allow_unix_sockets": "Let confined commands reach local daemons (Unix sockets). Any daemon that runs commands (D-Bus, docker) is then a way out of the sandbox.",
    "confine.allow_leaving_process_group": "Allow `setsid`/`setpgid` (job control, some test runners). A process that leaves survives the call.",
    "confine.share_system_tmp": "Give confined commands read and write on the shared `/tmp`, including other programs' temp files.",
    "persona_lifecycle.signal_consolidate": "Merge near-duplicate Persona facets.",
    "proactive.signal_ttl_expiry": "Surface a memory about to expire.",
    "skill_defaults.project_dir": "A directory holding one `<skill-name>/SKILL.md` per skill. A wrong layout gives zero skills, not an error.",
    "skill_defaults.user_dir": "A second such directory, e.g. under `~/.config/aivyx-pa/skills`.",
    # Overrides for doc comments written for developers.
    "*.cron": "When it runs, as a cron pattern in your local time: 6 or 7 fields, seconds first (`0 0 9 * * *` = 09:00 daily). A 5-field pattern is rejected.",
    "schedule.cron": "When it runs, as a cron pattern in your local time: 6 or 7 fields, seconds first (`0 0 9 * * *` = 09:00 daily). A 5-field pattern is rejected.",
    "reflection_schedule.cron": "When it runs, as a cron pattern in your local time: 6 or 7 fields, seconds first. A 5-field pattern is rejected.",
    "reflection_schedule.role_override": "Run the reflection as this role instead of the built-in reflection setup.",
    "reflection_schedule.skip_when_idle": "Skip a run when little has happened since the last one. Off by default.",
    "reflection_schedule.min_audit_entries_to_fire": "With `skip_when_idle`, the fewest new audit entries that still count as activity. Default 1.",
    "telegram.chat_id": "Your chat with the bot. Only messages from this chat are handled.",
    "notify_target.retry_count": "Retries after a transient failure (network error, timeout, or a 5xx reply). Default 0.",
    "proactive.max_per_window": "Most proactive messages per `window_secs`, whatever the signals say. Default 3.",
    "budget.per_mission_usd": "Most USD one team mission started by the loop may spend across all its specialists. Unset means no cap.",
    "daemon.web_ui_auth_token": "Your own Studio sign-in token. Unset means the automatic one, kept in a `studio-token` file next to the store.",
    "daemon.web_ui": "Serve the Studio. On by default; `false` wins over `web_ui_port`.",
    "daemon.web_ui_port": "The Studio's port. Default 7843; each named instance needs its own (`instances create` picks one from 7844).",
    "daemon.web_ui_insecure_no_auth": "Allow the Studio with no token at all — only for binding beyond loopback behind your own authenticating proxy.",
    "*.notify_target": "A single notify target (older spelling of `notify_targets`; don't set both).",
    "embedding.recall_lexical_weight": "Weight of keyword matching in hybrid recall. Default 1.0.",
    "telegram.token": "The bot token from @BotFather. Prefer the encrypted store (`aivyx-pa connect`) over writing it here.",
    "tool_process.expected_scopes": "Per tool name, the widest scope that tool may declare for itself. Once set, it also acts as an allowlist of tool names for this process.",
    "openai.base_url": "The server's URL. Default `https://api.openai.com`; point it at a local server for llama.cpp, Jan, LM Studio and the like.",
    "embedding.api_key": "API key for the embeddings server. Not needed for a local one.",
    "routing.enabled": "Turn routing on. Off (the default) means every call uses `[agent] model`.",
    "routing.discover": "Ask each endpoint what models it serves at startup. Off means use only `[[routing.models]]`.",
    "routing.models.id": "The model id, as its endpoint names it.",
    "routing.models.endpoint": "Which `[routing.endpoints.<name>]` serves it. Unset means the main provider.",
    "routing.models.tier": "Its size class, for matching tasks to models.",
    "routing.models.strengths": "What it is good at.",
    "routing.models.context_window": "The context window you actually serve it with, in tokens.",
    "routing.escalation.mode": "Whether a call may escalate to a cloud model: `never`, `ask` (default) or `auto`.",
    "routing.escalation.on_failure": "After a local failure, escalate the next turn (through the same `mode` gate; sensitive conversations never escalate).",
    "routing.sensitive.channels": "Channels whose conversations count as sensitive, e.g. `[\"telegram\"]`.",
    "confine.require_enforcement": "Refuse to run a command when confinement can't be set up. On by default; off fails open.",
    "role.name": "The role's unique name.",
    "role.system_prompt": "The role's system prompt. Unset uses the built-in default prompt.",
    "role.tool_allowlist": "Tools this role may call. Leave it out to allow every tool; an empty list allows none.",
    "role.memory_topic_prefix": "Prefix added to every memory topic while this role is active (e.g. `coder/`), keeping roles' memories apart.",
    "role.capability_scopes": "Capability scopes this role adds to what it inherits from its parent.",
    "role.trust_ceiling": "The highest trust tier this role may run at.",
    "role.parent_role": "The role this one inherits from. Cycles and missing parents are rejected at load time.",
    "memory.profile": "`lite` (smarter recall over existing memory, no model calls), `smart` (adds the wiki and knowledge graph, which cost model calls), or unset for neither.",
    "memory.ttl_secs": "Forget memory entries older than this many seconds. Unset keeps them.",
    "memory.canonicalize_topics": "Normalise topic names so near-identical topics merge. Off by default.",
    "workspace.path": "Where the workspace lives. Default `~/.aivyx-pa/workspace`.",
    "workspace.journaling.enabled": "Let the agent write in its journal on its own after recent activity. On by default.",
    "workspace.journaling.interval_secs": "Seconds between journaling passes. Default 21600 (6 hours).",
    "skills.starter": "Give a fresh agent the built-in starter skills. On by default.",
    "skills.trigger_injection": "Each turn, add the procedure of the best-matching approved skill to the agent's context. On by default; off means the agent must look skills up itself.",
    "skills.auto_propose.heuristic.mode": "`any` (default) — one condition above is enough; `all` — every one must hold.",
    "persona.auto_propose.heuristic.mode": "`any` (default) — one condition above is enough; `all` — every one must hold.",
    "persona.auto_propose.enabled": "Turn proposals on. Defaults on once the section exists.",
    "persona.auto_propose.judge_model": "Model that judges candidate proposals. Unset means the agent's own model.",
    "persona.auto_propose.fuzzy_match_threshold": "Similarity (0–1) above which a candidate skill counts as a duplicate of an existing one.",
    "persona.auto_propose.from_failed_turns": "Also learn from turns that went wrong (see `failure_outcomes`). Off by default.",
}

# ---------------------------------------------------------------------------


def crate_of(path):
    """The `src` directory a source file belongs to."""
    parts = path.parts
    return pathlib.Path(*parts[: len(parts) - 1 - parts[::-1].index("src") + 1]) if "src" in parts else path


DEF = re.compile(
    r"^[ \t]*(?:pub(?:\([a-z]+\))?\s+)?struct\s+(?P<struct>\w+)\s*\{"
    r"|^[ \t]*pub\s+type\s+(?P<alias>\w+)\s*=\s*(?P<target>[^;]+);"
    r"|^[ \t]*pub\s+struct\s+(?P<newtype>\w+)\s*\(\s*pub\s+(?P<inner>[^;]+)\)\s*;"
    r"|^[ \t]*pub\s+enum\s+(?P<enum>\w+)\s*\{",
    re.M,
)


def index_definitions():
    """{name: [(file, kind, payload)]} for every struct, alias, newtype and enum."""
    defs = {}
    for path, text in TEXTS.items():
        for m in DEF.finditer(text):
            if m["struct"]:
                i, depth = m.end(), 1
                while depth and i < len(text):
                    depth += {"{": 1, "}": -1}.get(text[i], 0)
                    i += 1
                entry = (m["struct"], "struct", text[m.end(): i - 1])
            elif m["alias"] or m["newtype"]:
                target = (m["target"] or m["inner"]).strip()
                entry = (m["alias"] or m["newtype"], "alias", re.sub(r"\bBTreeSet<", "Vec<", target))
            else:
                attrs = []
                for line in reversed(text[: m.start()].splitlines()):
                    if not line.strip().startswith(("#[", "///")):
                        break
                    attrs.append(line)
                body = text[m.end(): text.index("\n}", m.end())]
                entry = (m["enum"], "enum", (" ".join(attrs), body))
            defs.setdefault(entry[0], []).append((path, entry[1], entry[2]))
    return defs


DEFS = index_definitions()


def lookup(name: str, kinds, near):
    """The best definition of `name` among `kinds`: `near`'s file, then its crate, then any."""
    found = [d for d in DEFS.get(name.split("::")[-1], []) if d[1] in kinds]
    if not found:
        return None
    if near is not None:
        for test in (lambda p: p == near, lambda p: crate_of(p) == crate_of(near)):
            for d in found:
                if test(d[0]):
                    return d
    return found[0]


def find_struct(name: str, near=None):
    """(fields, file) of `struct <name> { … }`, preferring `near`'s file and crate.

    A `type` alias or a newtype (`struct X(pub BTreeMap<String, T>)`) comes back
    as `({"alias": "<type>"}, file)`."""
    d = lookup(name, ("struct", "alias"), near)
    if d is None:
        return None, None
    path, kind, payload = d
    return (parse_fields(payload) if kind == "struct" else {"alias": payload}), path


def resolve(type_name: str, near):
    """Follow aliases: (final type, file it was found in)."""
    for _ in range(5):
        found, path = find_struct(unwrap(type_name)[1], near)
        if not isinstance(found, dict):
            return type_name, near
        kind, _ = unwrap(type_name)
        inner = found["alias"]
        type_name = {"array": f"Vec<{inner}>", "map": f"BTreeMap<String, {inner}>"}.get(kind, inner)
        near = path
    return type_name, near


def find_enum(name: str, near=None):
    """Serde spellings of `enum <name>`'s variants, or None."""
    d = lookup(name, ("enum",), near)
    if d is None:
        return None
    attrs, body = d[2]
    rule = re.search(r'rename_all\s*=\s*"([^"]+)"', attrs)
    out, rename = [], None
    for line in body.splitlines():
        s = line.strip()
        r = re.search(r'(?<![_a-z])rename\s*=\s*"([^"]+)"', s)
        if s.startswith("#[") and r:
            rename = r.group(1)
        v = re.match(r"^([A-Z][A-Za-z0-9]*)\s*(,|\{|\(|$)", s)
        if v:
            out.append(rename or spell(v.group(1), rule.group(1) if rule else None))
            rename = None
    return out


def spell(variant: str, rule):
    words = re.findall(r"[A-Z][a-z0-9]*", variant)
    if rule == "lowercase":
        return variant.lower()
    if rule == "snake_case":
        return "_".join(w.lower() for w in words)
    if rule == "kebab-case":
        return "-".join(w.lower() for w in words)
    return variant


FIELD = re.compile(r"^    (pub(\([a-z]+\))?\s+)?([a-z_][a-z0-9_]*)\s*:\s*(.+?),?\s*$")


def parse_fields(body: str):
    """Top-level fields of a struct body (4-space indent), with docs and attributes."""
    fields, docs, attrs = [], [], []
    for line in body.splitlines():
        s = line.strip()
        if s.startswith("///"):
            docs.append(s[3:].strip())
            continue
        if s.startswith("#["):
            attrs.append(s)
            continue
        if s.startswith("//") or not s:
            continue
        m = FIELD.match(line)
        if m:
            fields.append({"name": m.group(3), "type": m.group(4).rstrip(","),
                           "docs": docs, "attrs": " ".join(attrs)})
        docs, attrs = [], []
    return fields


def serde_name(field):
    m = re.search(r'(?<![_a-z])rename\s*=\s*"([^"]+)"', field["attrs"])
    return m.group(1) if m else field["name"]


def skipped(field):
    return bool(re.search(r"\bskip\b(?!_)", field["attrs"]))


def flattened(field):
    return "flatten" in field["attrs"]


def unwrap(t: str):
    """('array'|'map'|'single', inner type) for a field type."""
    t = t.strip()
    m = re.fullmatch(r"Option<(.+)>", t)
    if m:
        t = m.group(1).strip()
    m = re.fullmatch(r"(?:Vec|BTreeSet|HashSet)<(.+)>", t)
    if m:
        return "array", m.group(1).strip()
    m = re.fullmatch(r"(?:[\w:]+::)?(?:BTreeMap|HashMap)<\s*String\s*,\s*(.+)>", t)
    if m:
        return "map", m.group(1).strip()
    return "single", t


def short_type(t: str, near=None) -> str:
    kind, inner = unwrap(t)
    base = inner.split("::")[-1]
    names = {"String": "string", "SecretString": "string", "bool": "bool", "PathBuf": "path",
             "f32": "number", "f64": "number", "Value": "any"}
    for n in ("u8", "u16", "u32", "u64", "usize", "i32", "i64"):
        names[n] = "integer"
    if base in names:
        shown = names[base]
    elif unwrap(inner) != ("single", inner):
        shown = short_type(inner, near)
    elif "<" in inner:
        shown = f"`{inner}`"
    else:
        variants = find_enum(base, near)
        shown = " \\| ".join(f"`{v}`" for v in variants) if variants else base
    if kind == "array":
        return f"list of {shown}"
    if kind == "map":
        return f"table of {shown}"
    return shown


PROVENANCE = re.compile(
    r"^(?:(?:Phase|Chapter|Piece|Security-audit|Team-Command|Model routing|Aivyx-Skills|Q\d|A\d+\b"
    r"|Task \d+|[A-Z][a-z]+ §\d+)[^—]{0,80}—\s*)+"
)
CONST_REF = re.compile(r"\[`([A-Z][A-Z0-9_]+)`\]")
# Developer-history asides that mean nothing to an operator.
NOISE = [
    (r"^`\[[a-z_.]+\] [a-z_]+`(?: \(default [^)]*\))?[.:]\s*", ""),
    (r"^(?:[Oo]ptional )?`\[\[?[a-z_.]+\]\]?`(?: (?:table-array|sub-table|nested block|section))?[.:]\s*", ""),
    (r"\s*[,;(—–-]*\s*(?:and is |is |are |stays |which is |keeps it )?byte-\s?identical(?: to)?[^.;)]*\)?", ""),
    (r"\s*\((?:[^()]*\bQ\d+[a-z]?\b[^()]*|[^()]*\bPhase \d+[^()]*|pre-[A-Z][a-z]+[^()]*)\)", ""),
    (r"\s*,?\s*pre-Phase-\d+ behaviou?r", ""),
    (r"\bper Q\d+[a-z]?\b\s*", ""),
    (r"\bQ\d+[a-z]?(?:'s)?\s+", ""),
    (r"(?:^|(?<=[.!?]) )(?:Q-block|Phase \d+(?: Task \d+)?)[^.]*\.", ""),
    (r"\bPhase \d+(?:'s)? ", ""),
    (r"\s*\((?:today's|unchanged|Step \d)[^)]*\)", ""),
    (r"Reuses `Raw\w+`[^.]*\.", ""),
    (r"`None`/absent|`None`|\bAbsent\b", "Unset"),
    (r"\babsent\b", "unset"),
    (r"`Some\(([^)]*)\)`", r"`\1`"),
    (r"Empty `Vec`", "An empty list"),
    (r"`(\S+)` \(`\1`\)", r"`\1`"),
    (r"`Self::([a-z_]+)`", r"`\1`"),
    (r"`Option` because ([^.]*)\.", ""),
    (r"\s{2,}", " "),
]


def const_value(name: str):
    pat = re.compile(r"const\s+" + re.escape(name) + r"\s*:\s*[^=]+=\s*([^;]+);")
    for text in TEXTS.values():
        m = pat.search(text)
        if m and len(m.group(1).strip()) <= 40:
            return m.group(1).strip()
    return None


def clean(text: str) -> str:
    text = PROVENANCE.sub("", text).strip()
    text = CONST_REF.sub(lambda m: f"`{const_value(m.group(1)) or m.group(1)}`", text)
    text = re.sub(r"\[`([^`]+)`\]", r"`\1`", text)
    for pat, rep in NOISE:
        text = re.sub(pat, rep, text)
    text = re.sub(r"\s+([.,;])", r"\1", text).strip(" ,;—")
    if re.match(r"^see `", text, re.I) or text.lower().strip(".") in {"default", "optional", ""}:
        return ""
    # Two sentences are plenty for a table cell; one if they're long.
    parts = [p for p in re.split(r"(?<=[.!?])\s+(?=[A-Z`(*])", text) if p]
    text = " ".join(parts[:2])
    if len(text) > 320:
        text = parts[0]
    text = text[:1].upper() + text[1:]
    return text.replace("|", "\\|")


def first_paragraph(docs):
    out = []
    for d in docs:
        if not d:
            if out:
                break
            continue
        if d.startswith("```"):
            break
        if out and out[-1].endswith("-") and not out[-1].endswith(" -") and d[:1].islower():
            out[-1] += d
        else:
            out.append(d)
    return clean(" ".join(out))


# --- examples/aivyx-pa.toml ------------------------------------------------

HEADER = re.compile(r"^#?\s*\[\[?([a-z0-9_.]+)\]\]?\s*(#.*)?$")
KEYLINE = re.compile(r"^#?\s*([a-z][a-z0-9_]*)\s*=\s*(.*)$")
CONT = re.compile(r"^#\s{2,}#\s?(.*)$")


def split_value(rest: str):
    """('value', 'trailing comment') — a `#` outside quotes starts the comment."""
    in_str, quote = False, ""
    for i, ch in enumerate(rest):
        if ch in "\"'" and (not in_str or ch == quote):
            in_str, quote = (not in_str), ch
        elif ch == "#" and not in_str:
            return rest[:i].strip(), rest[i + 1:].strip()
    return rest.strip(), ""


def example_keys():
    """{(section, key): (example value, comment)} from examples/aivyx-pa.toml."""
    out, section, last = {}, "", None
    for line in EXAMPLE.read_text(encoding="utf-8").splitlines():
        s = line.strip()
        m = HEADER.match(s)
        if m and not KEYLINE.match(s):
            section, last = m.group(1), None
            continue
        c = CONT.match(s)
        if c and last:
            v, com = out[last]
            out[last] = (v, (com + " " + c.group(1).strip()).strip())
            continue
        last = None
        m = KEYLINE.match(s)
        if m and section:
            value, comment = split_value(m.group(2))
            if len(value) > 40 or value.count("[") != value.count("]") or value.count("{") != value.count("}"):
                value = ""
            key = (section, m.group(1))
            if key not in out:
                out[key] = (value, comment)
                last = key
    return out


EXAMPLES = example_keys()

# --- rendering --------------------------------------------------------------

UNDOCUMENTED = []


def twin_docs(struct_name: str, near):
    """Field docs from the resolved struct a `RawX` loads into (X, XConfig, XSettings)."""
    base = struct_name.split("::")[-1]
    if not base.startswith("Raw"):
        return {}
    stem = base[3:]
    for cand in (stem, stem + "Config", stem + "Settings"):
        found, _ = find_struct(cand, near)
        if isinstance(found, list) and found:
            return {f["name"]: f["docs"] for f in found}
    return {}


def plain(header: str) -> str:
    """`[[a.b]]` / `[a.<name>.c]` → `a.b` / `a.c` (the lookup path)."""
    return re.sub(r"\.<[a-z]+>", "", header.strip("[]"))


def meaning_for(path: str, key: str):
    full = f"{path}.{key}"
    if full in MEANINGS:
        return MEANINGS[full]
    parts = full.split(".")
    for i in range(1, len(parts)):
        wild = "*." + ".".join(parts[i:])
        if wild in MEANINGS:
            return MEANINGS[wild]
    return ""


def row(header, key, f, twins, type_name, near):
    path = plain(header)
    value, comment = EXAMPLES.get((path, key), ("", ""))
    meaning = (
        MEANINGS.get(f"{path}.{key}")
        or first_paragraph(f["docs"])
        or first_paragraph(twins.get(f["name"], []))
        or meaning_for(path, key)
        or clean(comment)
    )
    if not meaning:
        UNDOCUMENTED.append(f"{path}.{key}")
    shown = f"`{value}`".replace("|", "\\|") if value else ""
    return f"| `{key}` | {short_type(type_name, near)} | {shown} | {meaning} |"


def is_struct(type_name: str, near) -> bool:
    base = type_name.split("::")[-1]
    if base in PRIMITIVES or not base[:1].isupper() or "<" in base:
        return False
    found, _ = find_struct(base, near)
    return isinstance(found, list) and bool(found)


def collect(struct_name, near):
    """(flat fields, nested tables) for a struct, expanding `#[serde(flatten)]`."""
    fields, here = find_struct(struct_name, near)
    if not isinstance(fields, list):
        return [], []
    twins = twin_docs(struct_name, here)
    flat, nested = [], []
    for f in fields:
        if skipped(f) or f["name"].startswith("legacy"):
            continue
        type_name, where = resolve(f["type"], here)
        kind, inner = unwrap(type_name)
        if flattened(f):
            sub_flat, sub_nested = collect(inner, where)
            flat += sub_flat
            nested += sub_nested
        elif is_struct(inner, where):
            nested.append((kind, serde_name(f), inner, f["docs"], where))
        else:
            flat.append((f, twins, type_name, where))
    return flat, nested


def emit_section(out, header, struct_name, docs, near=None, depth=0, seen=frozenset(), intro=None):
    if struct_name in seen:
        return
    seen = seen | {struct_name}
    flat, nested = collect(struct_name, near)
    out.append(f"\n{'#' * min(3 + depth, 5)} `{header}`\n")
    text = intro or first_paragraph(docs)
    if text:
        out.append(text + "\n")
    if flat:
        out.append("| Key | Type | Example | Meaning |\n|---|---|---|---|")
        out.extend(row(header, serde_name(f), f, twins, t, w) for f, twins, t, w in flat)
    prefix = header.strip("[]")
    # Several sibling sub-tables of one type (one per Persona category, say)
    # are documented once.
    by_type = {}
    for kind, key, inner, d, where in nested:
        by_type.setdefault((kind, inner, where), []).append((key, d))
    for (kind, inner, where), entries in by_type.items():
        if len(entries) > 2 and kind == "single":
            keys = ", ".join(f"`{k}`" for k, _ in entries)
            emit_section(out, f"[{prefix}.<category>]", inner, [], where, depth + 1, seen,
                         intro=f"One sub-table per category: {keys}.")
            continue
        for key, d in entries:
            if kind == "array":
                h = f"[[{prefix}.{key}]]"
            elif kind == "map":
                h = f"[{prefix}.{key}.<name>]"
            else:
                h = f"[{prefix}.{key}]"
            emit_section(out, h, inner, d, where, depth + 1, seen)


PREAMBLE = """# Configuration reference

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
`aivyx-pa --verify-only` loads the config, reports problems and exits.

## Reading the tables

- **Type**: `string`, `integer`, `number`, `bool`, `path`, a list, or the
  allowed words (`` `a` | `b` ``).
- `[[name]]` is an array of tables: repeat the block once per entry.
- `<name>` in a heading is a name you choose (e.g. `[pricing.<name>]` →
  `[pricing."claude-sonnet-5"]`).
"""


def main():
    sections = {}
    fields, root_file = find_struct("RawToml")
    for f in fields:
        if not skipped(f) and not f["name"].startswith("legacy"):
            sections[serde_name(f)] = f
    placed = [s for _, names in GROUPS for s in names]
    missing = sorted(set(sections) - set(placed))
    unknown = sorted(set(placed) - set(sections))
    no_intro = sorted(set(sections) - set(INTROS))
    if missing or unknown or no_intro:
        sys.exit(f"update GROUPS/INTROS: unplaced {missing}, unknown {unknown}, no intro {no_intro}")

    out = [PREAMBLE]
    for title, names in GROUPS:
        out.append(f"\n## {title}")
        for key in names:
            type_name, where = resolve(sections[key]["type"], root_file)
            kind, inner = unwrap(type_name)
            h = {"array": f"[[{key}]]", "map": f"[{key}.<name>]"}.get(kind, f"[{key}]")
            emit_section(out, h, inner, sections[key]["docs"], where, intro=INTROS[key])
    if UNDOCUMENTED:
        sys.exit("no meaning for: " + ", ".join(UNDOCUMENTED)
                 + " — add a doc comment or a MEANINGS entry in this script")
    sys.stdout.write("\n".join(out) + "\n")


if __name__ == "__main__":
    main()
