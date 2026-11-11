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
# model = "flux2-klein:q8"               # optional, defaults to the image model downloaded on mold serve
```

Tested against mold 0.37. A mold in Docker or on another machine must be
started with `--allow-direct-api` (`MOLD_ALLOW_DIRECT_API=1`); otherwise it
refuses plain-HTTP API calls with a 426, which the tool explains.

Without `[mold]`, the process still starts with just `vision.generate_svg`
registered — this is fully backward compatible with an existing install.

`vision.generate_image` takes
`{"prompt": "...", "width"?: int, "height"?: int, "seed"?: int,
"style_hint"?: string, "reference_image"?: string, "strength"?: number}`
and returns
`{"path": "...", "backend": "mold", "seed_used": int|null, "warnings": [string]}`.
`reference_image`, if given, must be a **bare filename** (no path
separators) previously returned by this same tool — arbitrary filesystem
paths are rejected, since this tool process has no access to the daemon's
own fs sandbox. Without `width`/`height`, an image-to-image result keeps
the reference's size. `strength` (0.0–1.0, image-to-image only; mold's
default 0.75) is how far the result may move from the reference.
`count` (1–4) makes that many variations with consecutive seeds and
returns `{"images": [...], "failed": [string]}` instead.

`vision.upscale_image` takes `{"image": "..."}`, a bare filename this
process generated (up to 1 megapixel), enlarges it 4x with mold's
`real-esrgan-x4plus` upscaler (`mold pull real-esrgan-x4plus:fp16`, 31 MB)
and returns the same shape as `vision.generate_image`.

Image results also carry a small JPEG preview of each image (at most
768 px) under the reserved `_images` key, so a model that can see images
is shown what it made and can check it; the daemon removes the preview
from the text. Set `show_model_previews = false` under `[mold]` to leave
them out (each image shown costs roughly 600 tokens on a local model).

Generated files carry no prompt: mold is asked not to embed it, and any
embedded text is stripped before the file is written. mold itself still
keeps image-to-image results and every prompt on its own machine; see
`aivyx-vision-mold`'s README.

`vision.generate_3d` takes `{"prompt": "...", "reference_image"?: string,
"style_hint"?: string, "seed"?: int}` and returns the same shape, with a
`.glb` mesh (about 40,000 faces, untextured; about half a minute). Without
`reference_image`, an image of the object is generated first and kept.
A reference should show one whole object on a plain background. It needs
a Hunyuan3D model on mold (`mold pull hunyuan3d-mini-turbo:fp16`, 3.8 GB).
**Licence:** Hunyuan3D's weights are Tencent's, under the Tencent Hunyuan
3D 2.0 Community License, which does not apply in the EU, the UK or South
Korea and needs a separate Tencent licence above 1 million monthly active
users. mold asks you to accept it before downloading.

All gated by the same `vision.generate` capability (`SemiTrusted`-and-above)
as `vision.generate_svg`.

See `aivyx-ecosystem/docs/superpowers/specs/2026-09-18-aivyx-vision-v1-design.md`
for the full design.
