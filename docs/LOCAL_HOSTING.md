# Local Hosting on Capable Hardware

> **Who this is for.** [`LOCAL_FIRST_RUN.md`](LOCAL_FIRST_RUN.md) makes the *free
> on-ramp* "just work" on modest hardware — a small tool-capable model
> (`qwen3:8b`), a conservative auto `num_ctx` (≤16 384), zero config. **This
> guide is the opposite end:** running Aivyx PA on a **dedicated or capable GPU
> box** — e.g. a 24 GB **RTX 3090** — where the goal is to use the hardware
> *well*: a bigger, more capable model, a larger context window, and (optionally)
> embedded in-process inference.
>
> The defaults are deliberately tuned for the lowest common denominator, so on a
> capable card they leave most of the GPU idle. Nothing here is a code change —
> it's **how to configure** the local path Aivyx PA already supports. Treat every
> number below as a **starting point to verify on your own card**, not gospel:
> model footprints, quantization quality, and token throughput vary, and the
> whole point of a dedicated box is that you can *measure* and tune.

---

## 1. Two ways to run a local model

| Path | What it is | When to choose it |
|---|---|---|
| **Ollama** *(recommended start)* | A separate local server (`ollama serve`) Aivyx PA talks to over HTTP. Manages models, GPU layers, and quantization for you. | Easiest. Great for a dedicated box — run it as a service, point Aivyx PA at it. |
| **llama.cpp / Jan** | Any OpenAI-compatible local server. | You already run one, or want fine control over the server. |
| **Embedded `mistralrs` (CUDA)** | Inference compiled *into* the Aivyx PA binary — no separate server. Built with `--features provider-mistral-rs-cuda`. | A single self-contained process; no server to manage. Heavier build; validate on your card. |

For a first dedicated-box setup, **Ollama is the path of least resistance.** The
embedded CUDA route is the "one process, no server" option — worth it once the
Ollama path is proven and you want to collapse the stack.

## 2. Pick a model for your VRAM

A rough map. Footprints assume ~4-bit quantization (`q4_K_M`-class, ~4.5 bits/
weight); a model's weights must fit **with room left for the KV cache**, which
grows with `num_ctx`. Bigger context = less room for weights, and vice-versa.

| VRAM | Comfortable model class | `num_ctx` to try | Notes |
|---|---|---|---|
| ~8 GB | 8B (`qwen3:8b`) | 16 384 (the auto default) | The on-ramp tier. |
| ~12 GB | 14B-class | 24–32k | A real step up in capability. |
| ~16 GB | 14B at big context, **or** 32B at modest context | 32k / 16k | First tier where 32B-class fits (tightly). |
| **~24 GB (RTX 3090 / 4090)** | **32B-class at ~16–24k, *or* 14B-class at 48–64k+** | **24k / 64k** | The sweet-spot tradeoff: *smarter* (32B) vs *more context* (14B). Test both for your workload. |
| 48 GB+ / multi-GPU | 32B at large context, or 70B-class | 32k+ | Beyond a single 3090. |

**The 24 GB tradeoff is the real decision**, and it's workload-dependent:

- **Reach for the bigger model (32B-class)** if your tasks need stronger
  reasoning / tool-use judgement and your contexts are short-to-moderate.
- **Reach for more context (14B-class at 48–64k+)** if your agent does
  long-running missions, big documents, or deep memory recall, and 14B's quality
  is enough.

A dedicated box lets you keep *both* pulled and switch (`aivyx-pa autonomy`/config +
restart) to compare on your actual tasks — which is exactly the capability
testing you're planning. **Measure tokens/sec and answer quality; don't guess.**

## 3. Raise the context window (the one knob that matters most)

Aivyx PA auto-sets `num_ctx = min(native, 16384)` **only when you haven't set it** —
a VRAM-safe default for unknown hardware. On a 24 GB card that's leaving capacity
on the table. Set it explicitly in `aivyx-pa.toml`:

```toml
[ollama]
# Use the context your card can afford. 32B-class at 24k, or 14B-class at 64k.
num_ctx = 24576
# Optional: cap generation length, threads, etc. — see OllamaOptions.
```

An explicit value **always wins** over the auto-cap. If you over-set it and the
model OOMs or spills to system RAM (slow), step it down. `aivyx-pa doctor` will tell
you the model loads and replies; throughput you measure yourself.

## 4. Reliable tool-calling on a local model

Local/quantized models are far less reliable than Claude at emitting clean tool
calls — they drift, hallucinate tool names, or wrap JSON in prose. Aivyx PA has two
defenses; **turn them on for a local host:**

- **Grammar-constrained decoding** (Chapters Stencil/Emboss) — forces the model
  to emit *valid, real-named* tool-call JSON *by construction*. On the
  llama.cpp/Jan OpenAI-compat path, set `[openai] constrain_tool_calls = true`;
  the embedded mistralrs path constrains natively. (Ollama doesn't expose a
  grammar hook, so prefer llama.cpp/mistralrs if tool-call reliability on a small
  model is the priority.)
- **The runaway breakers** (Chapters Bridle/Halter) — the consecutive-identical
  and small-cycle breakers stop a model that loops `A,B,A,B…`, and
  `[agent] cycle_detection = true` arms the small-cycle breaker for the
  interactive agent (autonomous team agents always have it). Essential for a
  local model on an autonomous loop — and the daemon now survives a runaway turn
  rather than dying on it.

`qwen3` is the family verified against Aivyx PA's thinking-field + tool-call
handling; start there and branch out as you test.

## 5. Running it as a dedicated host

A recipe for a box whose job is to *be* the agent (e.g. the 3090 machine):

1. **Install the GPU stack** — NVIDIA driver + CUDA; Ollama (it handles the GPU
   layers). Confirm the card is seen: `nvidia-smi`.
2. **Pull a model for your tier** (§2) — keep two if you want to compare.
3. **Configure** `aivyx-pa.toml`: `provider = "ollama"`, `model = "<your choice>"`,
   `[ollama] num_ctx = <your tier>`, and the tool-calling/safety knobs (§4).
4. **Choose reach + autonomy deliberately** — this is a box you trust, so
   `[access] level` and `[autonomy] level` are real decisions; read
   [`SECURITY_POSTURE.md`](SECURITY_POSTURE.md) and [`AUTONOMY.md`](AUTONOMY.md)
   first. A dedicated box running autonomous loops is exactly where the
   containment model earns its keep.
5. **Verify** with `aivyx-pa doctor` (it runs a real tool-using generation through
   the agent's path), then run the daemon (`aivyx-pa daemon run`) and reach it from
   the Studio or a channel.
6. **Run it as a service** so it survives reboots (systemd user unit around
   `aivyx-pa daemon run`); keep the audit chain and budgets on.

## 6. Verify, then tune — this *is* the capability test

The dedicated box is the truest excellence test. As you exercise it:

- **`aivyx-pa doctor`** confirms the model loads and produces real tool-using
  replies, and points back here.
- **Watch the audit chain + `aivyx-pa cost`/budgets** — even local turns are
  metered (tokens), so you can see where the agent spends effort.
- **Measure** tokens/sec and answer quality on *your* tasks, then revisit §2/§3:
  bigger model or bigger context? More `num_ctx` or less?
- **Push the autonomy dial up gradually** (`manual → supervised → autonomous`)
  as trust builds — the breakers, budgets, and the never-panic-on-a-runaway
  guarantee are what make that safe to do unattended.

The right configuration is the one your measurements pick — this guide gets you
to a strong starting point so the testing is productive from turn one.

---

## See also

- [`LOCAL_FIRST_RUN.md`](LOCAL_FIRST_RUN.md) — the modest-hardware on-ramp (auto `num_ctx`, recommended model, `doctor`).
- [`SECURITY_POSTURE.md`](SECURITY_POSTURE.md) / [`AUTONOMY.md`](AUTONOMY.md) — reach + autonomy on a box you trust.
- [`BRIDLE.md`](BRIDLE.md) — the runaway breakers that keep a local model on the rails.
- `[routing]` in [`../examples/aivyx-pa.toml`](../examples/aivyx-pa.toml) — route each call to the right local model (tools, vision, context size, task tier) across several models or servers.
