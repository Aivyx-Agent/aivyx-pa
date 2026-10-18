# Local First-Run Reliability — the free on-ramp (Chapter P)

> **Status:** design contract. This is the spec Chapter P scaffolds from
> (mirrors `docs/ACCESS_LEVELS.md` / `docs/AGENT_WORKSPACE.md`).
>
> Aivyx PA's headline pitch is **"runs on your hardware, no API key."** That free
> front door is the local-LLM (Ollama) path. But the path is fragile in exactly
> the way that ruins a first impression: a new user runs `aivyx-pa init`, picks
> local, and gets **empty replies**, **dropped tool calls**, or a model that
> only generates one token — because of thinking-model quirks, a streaming
> tool-call edge, and a starved `num_ctx`.
>
> **Chapter P makes the local on-ramp just work, end to end, with zero manual
> config.** A new user should go from "I downloaded it" to "I'm having a real,
> tool-using conversation" without ever learning what `num_ctx` is or debugging
> a blank turn. (Publishing downloadable binaries is the *next* chapter — gated
> on this on-ramp being solid first.)

---

## 0. What broke (the failure modes this chapter closes)

Observed live while testing local models this build:

| Symptom | Cause | State |
|---|---|---|
| Empty / verbose assistant text | thinking models (qwen3) emit reasoning into `content` when `think` is mishandled | **fixed** — send `think: true` for thinking-capable models so reasoning is routed into the discarded `thinking` field and the answer/tool_calls land in `content` (v0.7.1 corrected this from the earlier `think:false`, which current Ollama hybrid models like `qwen3:30b-a3b` ignore) |
| Tool call silently dropped (`tools=0`, no output) | Ollama 0.30.5 delivers `tool_calls` on a `done:false` chunk; the reader only read them on `done:true` | **fixed** (capture non-terminal `tool_calls`) |
| Model emits ~1 token then stops | the agent prompt (~4–11k tokens) fills the default `num_ctx` 4096, starving generation | **needs auto-fix** (P.1) — today requires hand-set `[ollama] num_ctx` |
| No usable model present | wizard only *hints* `ollama pull …` and leaves the user to it | **needs guided pull** (P.3) |
| Silent failure with no signal | nothing tells the user *why* the first turn was empty | **needs `doctor`** (P.4) |

---

## 1. The four mechanisms

1. **Auto `num_ctx` (the headline).** When `[ollama] num_ctx` is unset, the
   provider reads the model's **native context length** from `/api/show`
   (`model_info["<arch>.context_length"]`, cached on the same call that already
   fetches `family` + `capabilities`) and sets
   `num_ctx = min(native, AUTO_NUM_CTX_CAP)` (16384). Comfortably above the agent
   prompt, VRAM-sane. An explicit `num_ctx` always wins; non-Ollama providers are
   untouched.
2. **A vetted recommended model.** The wizard's local default is a small,
   **tool-capable** (`capabilities` ∋ `"tools"`) model in the ~7–9B range,
   confirmed during this chapter to drive the agent's tool-calling with the
   thinking/tool-call fixes + auto-`num_ctx`. (Chosen empirically, not assumed.)
3. **Guided pull.** When the operator picks local and the recommended model is
   absent (or Ollama has none), the wizard *offers to download it* with streamed
   progress — not a copy-paste hint.
4. **`aivyx-pa doctor` + end-of-wizard check.** Confirms the path actually works:
   Ollama reachable → a usable model present → a real test turn returns
   **non-empty** text. Each failure prints a clear, actionable next step. Run
   automatically at the end of `init`, and on demand any time.

---

## 2. Invariants

- **Zero-config local path.** A new user never needs to know what `num_ctx` is.
  Auto-detection handles it; explicit config always overrides.
- **No silent empty replies.** Every failure mode above is either fixed in the
  provider or surfaced by `doctor` with an actionable message — never a blank
  turn with no explanation.
- **Cloud path untouched.** All changes are scoped to the Ollama provider and the
  wizard's local branch; Anthropic / OpenAI behavior is unchanged.
- **Diagnose, don't guess.** `doctor` reuses the audit chain's `LlmCost`
  out-token signal + a live test turn to distinguish "context-starved",
  "thinking-empty", and "no model" — the same signals that diagnosed these bugs.

---

## 3. Phase plan

| Phase | Deliverable |
|---|---|
| **P.0** | This design contract. |
| **P.1** | Auto `num_ctx` from `/api/show` (the headline fix). |
| **P.2** | Vetted recommended model + wizard default; verify live. |
| **P.3** | Guided model pull in the wizard. |
| **P.4** | `aivyx-pa doctor` + end-of-wizard verification. |
| **P.5** | Docs (`INSTALL.md`), README, memory; tee up the Publish chapter. |

**Status: P.0–P.5 complete and verified live.** A bare `provider=ollama,
model=qwen3.6:27b` config now returns full responses (was 1 token); the 9B tier
emits tool calls correctly; `aivyx-pa doctor` reports green (`test reply OK: "OK"`)
and gives an actionable pull hint on a missing model. The recommended model is
`qwen3:8b` (tool-capable qwen3, verified family). **Next chapter: Publish** —
activate the cargo-dist pipeline into downloadable binaries + a one-line
installer, now that the local on-ramp is solid.

---

## 4. Out of scope (the follow-on — now done)

Publishing downloadable binaries — activating the cargo-dist / GitHub-Actions
release pipeline (`dist-workspace.toml`) into real installable releases + a
one-line installer — was the **next chapter (Chapter Q — Publish)** and is now
prepared: the release ships only the `aivyx-pa` binary, the install docs resolve,
and `v0.1.0` (pre-release) is ready to tag. It deliberately landed *on top of* a
local on-ramp that already works; shipping a binary whose free path produces
empty replies would have defeated the point. See
[`docs/INSTALL.md`](INSTALL.md#shell-installer-recommended).

## 5. Hardening tool-calling itself (Chapter Stencil)

The mechanisms above make a local model *reply*; making a **small** local model
reliably *call a tool* is the harder, later problem — four prompt-substrate phases
proved it can't be fixed from the prompt. **Chapter Stencil** ([`docs/STENCIL.md`](STENCIL.md))
adds a lever the prompt can't reach: **grammar-constrained decoding** on the
in-process mistral.rs engine (`[mistralrs] constrain_tool_calls = true`, default
off). The decoder is constrained to a JSON-Schema grammar built from the registered
tools, so a small GGUF emits a real-named tool call (or a `respond` text
escape) in the right shape. It was live-proven on Qwen3-4B.

**Chapter Emboss** ([`docs/EMBOSS.md`](EMBOSS.md)) extends the *same* grammar to the
**`llama-server`** path (`provider = "llamacpp"` / Jan, the OpenAI-compatible local
backends): with `[openai] constrain_tool_calls = true` Aivyx PA injects
`tool_call_grammar(tools)` as a `json_schema` constraint on the chat-completions
body. This is **off by default, and we do not recommend turning it on.**
A 2026-10-18 measurement (aivyx-toolbench, `results/2026-10-18`) found that the grammar lowered tool choice on Qwen3.5-9B and Qwen3-4B compared with llama-server's native tool calling (for example 52.7 % against 87.7 % correct choice on the 9B). Native tool calling on
llama-server is the recommended setup.

## 6. Making semantic memory work out of the box (Chapter Engram)

Chapter P made a local model *reply*; **Chapter Engram** makes a fresh agent
actually *remember and recall*. The gap it closes is structural: the daemon
builds the auto-recall pipeline **only when `[embedding]` is configured**
(`aivyx.rs` — `recall_context` is gated on the embedding provider), and `aivyx-pa
init` never wrote one. So the whole memory stack (Loom/Codex/Lattice/Synapse)
was dark for any operator who didn't hand-configure embeddings — flipping the
`[memory] profile` did nothing on its own.

**What init now does:**

- **Local (Ollama)** — offers to pull `nomic-embed-text` (768-dim; served over
  the OpenAI-compatible `/v1/embeddings` endpoint the provider already speaks)
  and writes a keyless `[embedding]` pointing at the local server. A declined or
  failed pull degrades to "semantic memory off" with a note, not an error.
- **Cloud (OpenAI)** — reuses the operator's key for OpenAI embeddings
  (`text-embedding-3-small`, 1536-dim).
- **Cloud (Anthropic-only)** — skipped with a clear note: Anthropic has no
  embeddings API, so semantic memory stays off until the operator adds a
  provider. No broken config is written.
- When (and only when) an embedding provider is set, init also writes
  `[memory] profile = "smart"` — the full graph-augmented stack.

**Why `smart` is written into the config, not made the compiled default.** The
`smart` profile runs the wiki / typed-graph extraction sweeps, which spend
tokens on the cloud path. Flipping the *compiled* default would surprise-bill
existing installs on upgrade, so the compiled default stays `Off` and only new
configs opt in (the same planting model as the default cron routines). Local
sweeps are free compute (just slower on a small model).

**`aivyx-pa doctor`** gained a Memory section: it reports the embedding provider +
active profile, and on the local path verifies the embedding model is actually
pulled (the common "configured but dark" failure) with an `ollama pull` hint. A
config with no `[embedding]` is valid — the section says so and how to enable it,
and does not fail the check.
