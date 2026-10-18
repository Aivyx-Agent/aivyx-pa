# Grammar-Constrained Tool-Calling on llama-server (Chapter Emboss)

> **Status:** ✅ **COMPLETE (EB.0–EB.5).** The companion to [Chapter Stencil](STENCIL.md):
> Stencil pressed a tool-call grammar into the **in-process mistral.rs** engine;
> Emboss presses the **same grammar** into **llama.cpp's server** (`provider =
> "llamacpp"` / Jan — the OpenAI-compatible local backends), so a small GGUF served
> by `llama-server` emits a valid, real-named tool call **by construction** too. The
> primitive already exists and was built provider-agnostic for exactly this:
> `aivyx_llm::tool_grammar::tool_call_grammar` (Stencil ST.1). The lever is
> **injecting that JSON-Schema into the existing `/v1/chat/completions` body** —
> llama.cpp converts a `json_schema` constraint into a GBNF grammar internally, so
> there is **no new endpoint and no new dependency**. Reuses Stencil's
> constrained-output parser and Bridle's `respond` preamble (lifted to shared).
> Opt-in, default off, byte-identical when off.
>
> **Not recommended.** A 2026-10-18 measurement (aivyx-toolbench, `results/2026-10-18`) found that the grammar lowered tool choice on Qwen3.5-9B and Qwen3-4B compared with llama-server's native tool calling (for example 52.7 % against 87.7 % correct choice on the 9B). It stays off by default, and native tool calling is the recommended setup. This chapter describes how the opt-in works; it does not recommend using it. See `docs/LOCAL_HOSTING.md` section 4.

## 1. Why this chapter

Stencil proved grammar-constrained decoding fixes the small-local-model
tool-calling wall — but only on the **in-process** mistral.rs provider, the one
path with a direct `Constraint::JsonSchema` API. The most *popular* way people run
a local GGUF is **`llama-server`** (llama.cpp's HTTP server) behind its
OpenAI-compatible `/v1/chat/completions` endpoint — which Aivyx PA already supports as
`provider = "llamacpp"` (Phase 133, config sugar over the shared `OpenAiProvider`).
That path gets the *unconstrained* OpenAI passthrough today, so it inherits the
exact fragility Stencil cured everywhere else: hallucinated names, malformed
arguments.

Emboss closes that gap. llama.cpp's server accepts a **`json_schema`** field (an
extension on the chat-completions body) and compiles it to a GBNF grammar that
constrains decoding — the same outcome as Stencil's `Constraint::JsonSchema`,
reached over HTTP instead of an in-process call. Feed it `tool_call_grammar(tools)`
and a `llama-server`-hosted model is constrained to emit exactly one valid tool
call (or the `respond` text escape). One primitive, now covering **both** local
engines Aivyx PA ships.

## 2. Architecture & governance decisions (locked)

### This is provider wiring + a shared-helper lift — **no new capability surface**
No new tool, `KNOWN_BASES` base, scope, P10 amendment, or dependency. The change is:
(a) lift Stencil's constrained-output helpers to a shared module, (b) one
default-off config flag, (c) inject the grammar into the OpenAI-compat request body
+ parse the constrained reply. Trust tiers, sandboxing, and the audit chain are
untouched.

### Inject `json_schema` into the chat-completions body — **not** a native `/completion` path
llama.cpp's OpenAI-compat server accepts a non-standard **`json_schema`** field
(and `grammar` for raw GBNF). Setting `json_schema = tool_call_grammar(tools)`
constrains output server-side. This is decisively better than adding a second,
native `/completion` code path:
- **Reuses the entire `OpenAiProvider`** (transport, SSE streaming, message
  mapping) — Emboss adds one field to `build_request_body`, not a new client.
- **No hand-rolled GBNF** — we already produce JSON-Schema (`tool_call_grammar`);
  llama.cpp does the schema→grammar conversion. (The README's "GBNF" framing was
  directionally right; the *mechanism* is json_schema → llama.cpp's converter.)
- The native `/completion` endpoint is rejected as redundant.

### The constrained JSON lands in `content` — parse it, suppress native `tools`
As with Stencil, a grammar-constrained reply is the *raw text* `{"name":…,
"arguments":…}` in the message `content`, **not** the OpenAI `tool_calls` array. So
when constraining, Emboss **does not send the native `tools` field** (which would
make llama.cpp build its own competing tool grammar and route to `tool_calls`);
the grammar *is* the tool definition. The streamed `content` is accumulated and
parsed at stream-end into either a real `ToolCall` or an unwrapped `respond` text —
the **one new wrinkle vs. Stencil**, which was single-shot: here the constrained
JSON arrives over **SSE deltas** and is assembled before parsing.

### Shared constrained-decoding helpers (the lift)
Stencil left `parse_constrained_output`, `ConstrainedOutput`, and Bridle's
`RESPOND_PREAMBLE` inside the **mistral.rs** provider; `RESPOND_SENTINEL` already
sits in the shared `tool_grammar.rs`. Emboss **promotes the rest to
`tool_grammar.rs`** so both engines parse the identical shape from one
implementation (no drift). Pure move + re-export; the mistral.rs provider imports
them; zero behavior change there.

### Opt-in, default-off, flag-gated to the operator's backend
A new **`constrain_tool_calls`** flag on the OpenAI-compat config (surfaced as
`[llamacpp] constrain_tool_calls` / the openai-compat section). Default `false` →
the exact current passthrough, byte-identical. The operator turns it on knowing
their server is llama.cpp-family (`llama-server` or Jan); cloud OpenAI proper does
**not** support the `json_schema` extension, so the knob is documented as
local-llama.cpp-only and only consulted on the relevant provider path. Ollama's
**native** `/api/chat` provider is out of scope (different constraint story — its
own chapter if ever).

## 3. Scope

**In:** the shared-helper lift (EB.1); the `constrain_tool_calls` config field
(EB.2); injecting `json_schema` + the `respond` preamble into the OpenAI-compat
request and parsing the constrained SSE content back into a tool call / text (EB.3);
their tests; a live-verify against a real `llama-server` (EB.4). **Out:** a native
`/completion` path; Ollama's native `/api/chat` constraint; cloud OpenAI (no
`json_schema` extension); any grammar change (Stencil owns `tool_call_grammar`);
GPU (the parked CUDA watch-item).

## 4. Phase plan (docs-first, small phases per convention)

| Phase | Deliverable | Notes |
|---|---|---|
| **EB.0** 🟡 | **This design contract** | Locked reference; banner flips per phase. |
| **EB.1** ✅ | **Lift shared helpers** | DONE. Moved `RESPOND_PREAMBLE`, `system_message_for`, `ConstrainedOutput`, and `parse_constrained_output` from `mistral_rs/provider.rs` into `tool_grammar.rs` (now all `pub`, beside `RESPOND_SENTINEL` + `tool_call_grammar`); the mistral.rs provider imports them. The 5 parse/preamble unit tests moved with the functions. Pure refactor, zero behavior change: `tool_grammar` 10 tests + mistralrs provider 5 tests green; clippy clean on default *and* `provider-mistral-rs`. |
| **EB.2** ✅ | **Config field** | DONE. `constrain_tool_calls: bool` + `with_constrain_tool_calls` builder on `OpenAiConfig` (both constructors default it off). Surfaced as **`[openai] constrain_tool_calls`** (the openai-compat section that already holds `base_url` for llamacpp/jan) → `RawOpenAi.constrain_tool_calls: Option<bool>` → resolved `AivyxConfig.openai_constrain_tool_calls: bool`. Threaded into **both** the `LlamaCpp` and `Jan` provider build sites (the llama.cpp-family paths); the plain `OpenAi` and other arms leave it off (cloud OpenAI lacks the extension). Tests: config round-trip + default-off; `OpenAiConfig` builder default-off/set. openai (19) + config (356) suites green; clippy clean. |
| **EB.3** ✅ | **Provider wiring + SSE parse** | DONE. `chat_stream` computes `constrain = config.constrain_tool_calls && !tools.is_empty()`. `build_request_body(_, _, constrain)`: when constrained → `json_schema = tool_call_grammar(tools)`, system via shared `system_message_for` (appends `RESPOND_PREAMBLE`), and the native `tools` array **omitted**. `OpenAiStream` carries `constrain`; **content deltas are buffered silently** (return `Ok(None)`, so the raw JSON never leaks as `TextChunk`); `build_terminal` runs the shared `parse_constrained_output` on the accumulated text → one real `ToolCall` (Known when advertised) or unwraps `respond` to a `FinalMessage`, with a defensive raw-text fallback on a parse miss. Flag-off / tool-less → byte-identical passthrough. 3 new tests (body shape: json_schema present + tools omitted + preamble in system; SSE-assembled tool-call across split deltas with zero leaked events; `respond`→final text). openai suite 22 green; clippy clean; binary checks. |
| **EB.4** ✅ | **Live verification** | DONE — against a **real `llama-server`** (llama.cpp 2.16.0, Qwen3-4B-Instruct-2507 Q4_K_M, GPU). **Wire-level:** a raw `/v1/chat/completions` with our `json_schema` union returned grammar-conforming JSON (the server honors the extension). **End-to-end:** `provider = "llamacpp"` + `[openai] constrain_tool_calls = true` → the agent emitted `fs.read {"path":"probe.txt"}` (real, schema-valid, by construction), dispatched + `Completed`; audit chain shows `scope_used = fs.read:…`. **Useful finding:** an earlier turn had the 4B pick a *different but real* registered tool (`skills.invoke`) → `ScopeDenied` — confirming the constraint only ever emits real registered names; tool *selection* is the model's (same model-vs-mechanism line Bridle drew). The grammar primitive now covers **both** local engines. |
| **EB.5** ✅ | **Finalize** | DONE. Full workspace suite green (exit 0); clippy clean (workspace, all-targets); `cargo deny` green. `docs/STENCIL.md` (the deferred "llama-server / OpenAI-compat" line now points to Emboss as shipped) + `docs/LOCAL_FIRST_RUN.md` (a new Emboss paragraph after the Stencil one — the grammar primitive now covers **both** local engines) cross-linked. The `future-reliable-local-tool-calling` memory updated; status → COMPLETE; chapter recorded. |

**Discipline:** EB.1 (the lift) lands first so EB.3 wires against a shared, tested
parser rather than duplicating Stencil's. EB.2 gates everything — nothing on the
passthrough changes until an operator opts in. Test band: **moderate** — dense in
EB.3 (the request-shape branch + SSE accumulation + parse); price ~20–30 new tests.

## 5. Open questions (resolve in-phase)

- **`json_schema` vs `response_format` (EB.3)** — newer llama.cpp also accepts
  `response_format: {type: "json_schema", json_schema: {schema: …}}` (the more
  OpenAI-aligned shape). Prefer whichever the target `llama-server` build honors;
  default to the bare `json_schema` extension, fall back to `response_format` if a
  version needs it. Confirm against the live server in EB.4.
- **SSE assembly edge cases (EB.3)** — a constrained reply should be one JSON
  object, but confirm the accumulator handles a `respond` payload whose `text`
  itself contains braces/quotes (it's inside JSON, so fine) and an empty/`[DONE]`
  terminal chunk cleanly.
- **Tools-omitted vs tools-present (EB.3)** — locked to *omit* native `tools` when
  constraining (the grammar is the definition). Verify llama.cpp doesn't require a
  non-empty `tools` array for some code paths; if it does, send a minimal stub and
  still parse from `content`.
- **Jan parity (EB.4, optional)** — Jan is llama.cpp-backed and openai-compat, so
  the same body *should* constrain it for free; spot-check if a Jan instance is at
  hand, but don't gate the chapter on it.

---

*Chapter Emboss finishes the thought Stencil started: reliable local tool-calling
shouldn't depend on which local engine you happen to run. The same grammar that
constrains the in-process mistral.rs engine, pressed into llama.cpp's server over
the wire it already speaks — so whether an operator runs the embedded engine or
points Aivyx PA at their own `llama-server`, a small local model emits valid,
real-named tool calls by construction. One primitive, both engines, no new
dependency.*
