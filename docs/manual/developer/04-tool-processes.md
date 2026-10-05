# Tool processes

A tool process gives the agent new tools from a separate program, in any
language. The daemon starts it, talks to it over its standard input and
output, and keeps every call inside the same capability checks and audit
log as the built-in tools. All the integrations — Gmail, Calendar, Notion,
the toolkit and the rest — are tool processes.

The full contract is
[`docs/TOOL_SDK.md`](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/TOOL_SDK.md);
this page is the overview.

## How it works

- The daemon starts one process per `[[tool_process]]` in the config, and
  stops it on shutdown.
- Frames are the same as the [IPC protocol](06-ipc-protocol.md): a 4-byte
  big-endian length, then JSON. Daemon → tool on **stdin**, tool → daemon
  on **stdout**; **stderr** is yours for logging.

1. The daemon sends `ToolHello { protocol_version }`.
2. The tool answers `ToolRegister` with one `ToolDescriptor` per tool:
   `name`, `description` (what the model reads), `input_schema` and
   `required_scope`.
3. The daemon checks each declared scope can be granted by some active
   role; tools that can't are logged and left out.
4. For each call: `InvokeTool { call_id, tool_name, input, turn_id }` →
   any number of `ToolEvent` progress frames → `ToolResult` or `ToolError`.
   `CancelInvocation { call_id }` asks the tool to stop; reply
   `ToolError { code: "cancelled" }` promptly.

## What you get, and what you can't skip

- **Scopes are bound at registration.** A tool can only be called with the
  authority it declared, and the operator can narrow it further
  (`scope_overrides`) — never widen it. `expected_scopes` lets the operator
  pin what a tool may declare at all.
- **Every call is audited** before your process sees it. Tools don't write
  audit entries and can't avoid them.
- **Output is treated as untrusted.** Whatever a tool process returns is
  fenced as data before the model sees it and scanned for prompt-injection
  phrasing, which makes a tool that relays web pages or mail much harder to
  use to steer the agent.

## Sandboxing

A tool process runs as the operator's user, so sandbox anything you didn't
write. A `[tool_process.sandbox]` block wraps it in bubblewrap, firejail or
any wrapper you choose; `[sandbox] default_backend = "auto"` (what
`aivyx-pa init` writes) applies a default sandbox to tool processes that
don't declare their own. TOOL_SDK.md §9 has worked examples.

## Start a new tool

```sh
aivyx-pa tool init my-tool
```

writes a buildable Rust project — handshake, the call loop and a
conformance test — using the `aivyx-tool` crate for the wire types; you
fill in `handle_invocation`. The crate isn't on crates.io yet, so the
generated `Cargo.toml` has a `path` dependency to point at your checkout.

In another language, start from
[`examples/python-tool/`](https://github.com/Aivyx-Agent/aivyx-pa/tree/main/examples/python-tool)
and its conformance suite
(`python3 -m unittest discover examples/python-tool/tests`).

Then add it to the config:

```toml
[[tool_process]]
name = "my-tool"
command = "/path/to/my-tool"
```

and restart the daemon. Its tools appear in the Studio's **Tools** screen
and in `tools.list`.
