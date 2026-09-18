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

## Image and 3D generation (Milestone 2 Pass A)

Add a `[mold]` section to the same `config.toml` to enable
`vision.generate_image`/`vision.generate_3d`:

```toml
[mold]
broker_url = "http://127.0.0.1:8899"    # aivyx-broker
mold_url = "http://127.0.0.1:7680"      # mold serve
# api_key = "..."                        # optional, only if mold serve sets MOLD_API_KEY
# output_dir = "..."                     # optional, defaults to ~/.local/share/aivyx-pa/vision/
```

Without `[mold]`, the process still starts with just `vision.generate_svg`
registered — this is fully backward compatible with an existing install.

`vision.generate_image` takes
`{"prompt": "...", "width"?: int, "height"?: int, "seed"?: int,
"style_hint"?: string, "reference_image"?: string}` and returns
`{"path": "...", "backend": "mold", "seed_used": int|null}`.
`reference_image`, if given, must be a **bare filename** (no path
separators) previously returned by this same tool — arbitrary filesystem
paths are rejected, since this tool process has no access to the daemon's
own fs sandbox.

`vision.generate_3d` takes `{"prompt": "..."}` and returns the same shape
on success — but every call fails today with a clear error message; mold's
3D generation ("Pass B") isn't built yet.

Both gated by the same `vision.generate` capability (`SemiTrusted`-and-above)
as `vision.generate_svg`.

See `aivyx-ecosystem/docs/superpowers/specs/2026-09-18-aivyx-vision-v1-design.md`
for the full design.
