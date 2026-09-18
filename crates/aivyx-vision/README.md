# aivyx-vision

The `aivyx-pa` tool process exposing `vision.generate_svg` — LLM-prompted,
sanitized SVG generation. Part of Aivyx-Vision's Milestone 1; wraps the
standalone [`aivyx-vision-svg`](https://github.com/Aivyx-Agent/aivyx-vision)
crate.

This tool process runs as a separate OS process from the daemon and
cannot share the daemon's own LLM provider — it has its own,
separately-configured one via `~/.aivyx-pa/tool-processes/vision/config.toml`:

```toml
provider = "ollama"   # "ollama" | "anthropic" | "openai"
model = "qwen3:8b"
# base_url = "http://127.0.0.1:11434"   # ollama only, optional
# api_key = "sk-..."                     # required for anthropic/openai
```

Add the corresponding `[[tool_process]]` entry to `aivyx-pa.toml`:

```toml
[[tool_process]]
name = "vision"
command = "aivyx-vision"
```

`vision.generate_svg` takes `{"prompt": "a small red circle icon"}` and
returns `{"svg": "<svg ...>...</svg>"}` — the sanitized SVG markup; the
caller decides whether and where to save it. Gated by the
`vision.generate` capability (`SemiTrusted`-and-above by default).

See `aivyx-ecosystem/docs/superpowers/specs/2026-09-18-aivyx-vision-v1-design.md`
for the full design.
