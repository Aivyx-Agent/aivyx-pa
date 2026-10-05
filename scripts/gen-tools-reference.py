#!/usr/bin/env python3
"""Generate the tool list in docs/manual/reference/03-tools.md from the code.

Every `impl Tool for X` outside test code contributes its `name()` and the
first sentence or two of its `description()` — the same text the model sees.
Re-run after adding or renaming a tool:

    python3 scripts/gen-tools-reference.py > docs/manual/reference/03-tools.md

A crate with tools but no entry in `GROUPS` fails the run, so a new
integration can't silently go missing from the reference.
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
GUIDE = "../../guide"

# Crate → (section title, intro). Order is the page order.
GROUPS = {
    "aivyx-core": ("Built in", "Always available, subject to your access level and the role's capabilities."),
    "aivyx-memory": ("Memory", f"Long-term memory. See [Memory]({GUIDE}/05-memory.md)."),
    "aivyx-dataread": ("Documents and spreadsheets", "Read and write CSV, Excel and PDF files, within the same file access as `fs.read` / `fs.write`."),
    "aivyx-channel": ("Agent machinery", "Missions, routines, reminders, skills, reflection and the like — the agent managing its own work."),
    "aivyx-toolkit": ("Toolkit", "Everyday utilities — tasks, budget, dates, units, health checks. A tool process: install `aivyx-toolkit` and add it as a `[[tool_process]]` (see [INSTALL.md](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/INSTALL.md))."),
    "aivyx-gmail": ("Gmail", "Needs `aivyx-pa connect gmail`."),
    "aivyx-calendar": ("Google Calendar", "Needs `aivyx-pa connect calendar`."),
    "aivyx-drive": ("Google Drive", "Needs `aivyx-pa connect drive`."),
    "aivyx-contacts": ("Google Contacts", "Needs `aivyx-pa connect contacts`."),
    "aivyx-notion": ("Notion", "A tool process: add it as a `[[tool_process]]` with your Notion integration token (see [INSTALL.md](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/INSTALL.md))."),
    "aivyx-obsidian": ("Obsidian", "A tool process over your vault folder: add it as a `[[tool_process]]` (see [INSTALL.md](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/INSTALL.md))."),
    "aivyx-n8n": ("n8n", "A tool process for your n8n instance: add it as a `[[tool_process]]` (see [INSTALL.md](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/INSTALL.md))."),
    "aivyx-apps": ("Desktop apps", "Linux/X11 only; off unless `[applications] enabled = true`."),
    "aivyx-vision": ("Images", "A tool process: add `aivyx-vision` as a `[[tool_process]]`. `vision.generate_image` needs an image backend configured."),
    "aivyx-kitchen-toolkit": ("Kitchen pack (example vertical)", "Only with the kitchen vertical pack installed."),
}
# Crates whose `impl Tool` blocks are test fixtures or wrappers, not tools.
IGNORED_CRATES = {"aivyx-tool", "aivyx-mcp", "aivyx-cli", "aivyx-slack", "aivyx-discord"}

IMPL = re.compile(r"^\s*(?:#\[[^\]]*\]\s*)*impl(?:<[^>]*>)?\s+(?:[\w:]+::)?Tool\s+for\s+([\w:<>, ]+?)\s*\{", re.M)
TEST_MOD = re.compile(r"^#\[cfg\(test\)\]\s*\n(?:#\[[^\]]*\]\s*\n)*mod\s+\w+\s*\{", re.M)


def crate_name(path: pathlib.Path) -> str:
    rel = path.relative_to(ROOT / "crates").parts
    if rel[0] in ("verticals", "verticals-private"):
        return rel[1]
    return rel[0]


def block(text: str, start: int) -> str:
    """The `{ … }` block whose opening brace ends at `start`."""
    i, depth = start, 1
    while depth and i < len(text):
        c = text[i]
        if c == '"':  # skip string literals so braces inside them don't count
            i += 1
            while i < len(text) and text[i] != '"':
                i += 2 if text[i] == "\\" else 1
        depth += {"{": 1, "}": -1}.get(c, 0)
        i += 1
    return text[start: i - 1]


def rust_string(src: str, text: str):
    """The value of the expression `src` returns: a literal, a `const`, or `concat!`."""
    src = src.strip()
    m = re.fullmatch(r"(?:&\s*)?(?:self\.\w+|[A-Z][A-Z0-9_]*)", src)
    if m and not src.startswith("self."):
        c = re.search(r"const\s+" + re.escape(src.lstrip("& ")) + r"\s*:\s*&(?:'static\s+)?str\s*=\s*(.+?);\s*$",
                      text, re.S | re.M)
        if c:
            return rust_string(c.group(1), text)
        return None
    parts = re.findall(r'r#"(.*?)"#|"((?:[^"\\]|\\.)*)"', src, re.S)
    if not parts:
        return None
    out = ""
    for raw, cooked in parts:
        if raw:
            out += raw
        else:
            s = re.sub(r"\\\n\s*", "", cooked)
            out += s.replace('\\"', '"').replace("\\n", " ").replace("\\\\", "\\")
    return re.sub(r"\s+", " ", out).strip()


def method_body(impl: str, name: str):
    m = re.search(r"fn\s+" + name + r"\s*\(\s*&self\s*\)\s*->\s*&(?:'static\s+)?str\s*\{", impl)
    return block(impl, m.end()).strip() if m else None


def summary(desc: str) -> str:
    if re.search(r"not yet implemented", desc, re.I):
        first = re.split(r"(?<=[.!?])\s+", desc)[0]
        return first.replace("|", "\\|") + " **Not built yet** — every call fails for now."
    parts = re.split(r"(?<=[.!?])\s+(?=[A-Z`(])", desc)
    text = parts[0]
    if len(parts) > 1 and len(text) + len(parts[1]) < 220 and not re.match(r"(Input|Returns|Args)\b", parts[1]):
        text += " " + parts[1]
    return text.replace("|", "\\|")


def collect():
    tools = {}
    for path in sorted((ROOT / "crates").glob("**/src/**/*.rs")):
        if "tests" in path.parts or path.name in ("tests.rs", "test_support.rs"):
            continue
        crate = crate_name(path)
        if crate in IGNORED_CRATES:
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        t = TEST_MOD.search(text)
        live = text[: t.start()] if t else text
        for m in IMPL.finditer(live):
            impl = block(live, m.end())
            name_src, desc_src = method_body(impl, "name"), method_body(impl, "description")
            if name_src is None:
                continue
            name = rust_string(name_src, text)
            desc = rust_string(desc_src, text) if desc_src else None
            if not name or "." not in name:
                continue  # dynamic names (MCP proxies, test doubles)
            tools.setdefault(crate, {})[name] = summary(desc) if desc else ""
    return tools


PREAMBLE = f"""# Tools

Every tool Aivyx PA can give the agent, with the description the model
itself reads. This page is generated from the code by
`scripts/gen-tools-reference.py` — edit the tool, not this page.

## How the agent gets tools

- **Built in** — file, shell, web, memory, workspace and the agent's own
  machinery. Always there; what each may touch is set by your
  [access level]({GUIDE}/08-access-and-settings.md).
- **Integrations** — separate, sandboxed tool processes. Gmail, Calendar,
  Drive and Contacts are set up with `aivyx-pa connect <name>`
  ([Chat apps and accounts]({GUIDE}/13-chat-apps-and-accounts.md)); the
  others (Notion, Obsidian, n8n, the toolkit, images) are added as a
  `[[tool_process]]` in your config.
- **MCP servers** — any `[[mcp_server]]` in your config. Their tools keep
  the server's own names and need the `mcp.call:<server>:<tool>` capability.
- **Your own tool processes** — any `[[tool_process]]`, in any language
  ([Tool processes](../developer/04-tool-processes.md)).

## Who decides what runs

Each tool call needs a **capability** (its scope). A role grants
capabilities; the channel's **trust tier** caps them, so a chat app can
never get more than its tier allows. Before anything irreversible —
deleting, overwriting, sending, a destructive shell command — the agent
asks you, according to your [autonomy level]({GUIDE}/15-autonomy-and-routines.md)
and `[access] confirm_destructive`. Every call is recorded in the audit log.

For each tool's capability scope and lowest trust tier, see
[`docs/TOOLS.md`](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/TOOLS.md).
`aivyx-pa --print-role` shows what the active role can use, and the agent
can list its own tools with `tools.list`.
"""


def main():
    tools = collect()
    unknown = sorted(set(tools) - set(GROUPS))
    if unknown:
        sys.exit(f"add these crates to GROUPS (or IGNORED_CRATES): {unknown}")
    out = [PREAMBLE]
    for crate, (title, intro) in GROUPS.items():
        if crate not in tools:
            continue
        out.append(f"\n## {title}\n\n{intro}\n")
        out.append("| Tool | What it does |\n|---|---|")
        for name in sorted(tools[crate]):
            out.append(f"| `{name}` | {tools[crate][name]} |")
    missing = [g for g in GROUPS if g not in tools]
    if missing:
        sys.exit(f"no tools found for {missing} — remove them from GROUPS or fix the parser")
    sys.stdout.write("\n".join(out) + "\n")


if __name__ == "__main__":
    main()
