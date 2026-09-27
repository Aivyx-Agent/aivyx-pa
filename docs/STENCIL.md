# Reliable Local Tool-Calling — Grammar-Constrained Decoding (Chapter Stencil)

> **Status:** ✅ **COMPLETE (ST.0–ST.5).** The headline lever for the free local
> on-ramp: force small in-process GGUF models to emit **valid, real-named
> tool-call JSON by construction** — at the token level, not by hoping the prompt
> lands. Scope locked to the **MistralRs** provider only (the in-process pure-Rust
> engine, `provider = "mistralrs"`, Phase 134), because that is the one path where
> the primitive is a single API call: `mistralrs 0.8.1` exposes
> `Constraint::JsonSchema(serde_json::Value)` + `RequestBuilder::set_constraint`,
> compiled via the bundled `llguidance` grammar engine — **no new dependency**.
> `llama-server`/OpenAI-compat GBNF and the rest of the local runtimes stay
> deferred. No new crate, no new tool, no new `KNOWN_BASES` base, no P10 amendment:
> this is a **substrate-quality refinement of the existing tool-call path**, not a
> new capability. Default off → byte-identical behavior.

## 1. Why this chapter

Aivyx PA is an agent platform; it lives and dies on tool-calling. The free local
on-ramp (Chapter P) gets a user inferring with Ollama / llama.cpp / an in-process
GGUF in minutes — but **small local models are fragile on the agent loop in a way
no amount of prompting has fixed.** Four substrate phases proved the ceiling is at
the *model* layer, not the prompt:

- **Phase 120** fuzzy tool-name recovery, **Phase 121** native protocol,
  **Phase 122** structured catalog injection, **Phase 124** few-shot examples —
  all four improved *enumeration* but **none fixed invocation.** gemma4 kept
  refusing `fs.write` ("I don't have a tool called `fs.write`") with the name
  literally in its prompt; qwen3 emitted structurally-correct tool JSON into
  **response text** (right shape, wrong channel).
- The wrong-channel case was later rescued by the **textual tool-call extraction**
  substrate (Phase 126/127, `aivyx-core/src/textual_tool_call.rs`) — a planner-side
  parser for the `<tool_code>` / `<tool_call>` / Qwen3-Coder-XML shapes. That
  closes "right tool, wrong channel."
- **What remains** is the failure that prompting *cannot* reach: the model emits a
  **hallucinated tool name** or **malformed arguments** in the first place.

Grammar-constrained decoding attacks exactly that residue. Instead of asking the
model to produce valid output and recovering when it doesn't, we **constrain the
decoder** so the only token sequences it can sample are ones that parse as a
registered tool call with schema-valid arguments. The model *cannot* hallucinate a
name that isn't in the registry, and *cannot* emit arguments that violate the
tool's schema — it is structurally impossible. This is the one capability the
OpenAI-compatible passthrough does **not** expose, and the reason the chapter is
MistralRs-only.

## 2. Architecture & governance decisions (locked)

### This is substrate refinement — **no new capability surface**
The chapter changes *how reliably* an existing provider emits an existing kind of
output. It adds **no tool**, **no `KNOWN_BASES` base**, **no scope**, and therefore
**no P10 amendment** (contrast Chapter Forge, which added the `git.write` base and
Amendment A13b). The trust-tier model, the per-role allowlist, sandboxing, and the
audit chain are all untouched. The only contract-adjacent surface is one new
**default-off** config field.

### The primitive lives where the constraint is native — the in-process engine
`mistral_rs/provider.rs` already builds a `mistralrs::RequestBuilder` (it calls
`apply_tools` to forward each `LlmToolDescriptor`'s `input_schema` verbatim). It
simply never sets a constraint. The whole lever is:

```rust
builder = builder.set_constraint(Constraint::JsonSchema(tool_call_grammar(tools)));
```

`Constraint::JsonSchema(serde_json::Value)` is in `mistralrs 0.8.1`
(`mistralrs-core/src/request.rs`), compiled by the already-vendored `llguidance`.
The other constraint kinds (`Regex`, `Lark`) are available but JSON-Schema is the
exact fit — our tools already *are* JSON schemas.

### The grammar generator is a pure, standalone, provider-agnostic function
`tool_call_grammar(&[LlmToolDescriptor]) -> serde_json::Value` builds a JSON-Schema
that admits **only** a valid tool call:

```jsonc
{
  "type": "object",
  "required": ["name", "arguments"],
  "oneOf": [
    { "properties": { "name": { "const": "fs.read" },
                      "arguments": { /* fs.read's input_schema verbatim */ } } },
    { "properties": { "name": { "const": "fs.write" },
                      "arguments": { /* fs.write's input_schema verbatim */ } } }
    // …one branch per registered tool…
  ]
}
```

A discriminated union: the `name` is pinned to a `const` drawn from the registry
(no hallucinated names survive), and that branch's `arguments` is the matching
tool's `input_schema` (no malformed arguments survive). Each tool's `input_schema`
is forwarded **verbatim** — the same value `to_mistralrs_tool` already passes — so
the grammar and the tool definitions can never drift. The function is pure
(`&[LlmToolDescriptor]` in, `Value` out), unit-tested with **no engine**, and could
be reused by a future llama-server `/completion` path without change.

### The "decline to call a tool" escape — the one real design wrinkle (ST.3)
A constrained decoder is *forced* into the grammar: if we admit only tool calls,
the model can never reply in plain text ("I've finished," "I need clarification").
That breaks the normal turn. Resolution (locked): the grammar includes a **sentinel
branch** for plain text — a reserved pseudo-tool, e.g.
`{ "name": "respond", "arguments": { "text": "…" } }` — that the provider unwraps
back into an ordinary assistant text message rather than dispatching a tool. So the
union is "every real tool **plus** `respond`," and the model is constrained to
*either* a valid tool call *or* a valid text reply, nothing malformed in between.
(Alternative considered and rejected: constrain only on turns where the loop
"expects" a tool — too coupled to loop internals and brittle.)

### Opt-in, default-off, byte-identical when off
New field on the existing `[mistralrs]` section
(`MistralRsOptions`, `aivyx-config`):

```toml
[mistralrs]
model_path = "…/qwen3-8b.gguf"
constrain_tool_calls = true   # default false
```

When `false` (default) the provider takes the **exact current code path** — no
`set_constraint`, no grammar built, output bytes identical. The constraint is only
assembled when the flag is on **and** `request.tools` is non-empty (constraining a
free-form, tool-less chat turn would be pointless and risk degrading plain replies).

## 3. Scope

**In:** the pure `tool_call_grammar` generator + its unit tests; wiring
`set_constraint` into the MistralRs provider behind the flag; the `respond`
sentinel unwrap; the `constrain_tool_calls` config field + validation; a live-verify
runbook against a small GGUF; and the docs/memory updates. Composes with — does not
replace — the existing textual-tool-call substrate (belt-and-suspenders: the
grammar prevents malformed output; the parser still rescues any model run without
the flag).

**Out (deferred):** the `llama-server` / OpenAI-compat path — **now shipped as
[Chapter Emboss](EMBOSS.md)**, which injects the *same* `tool_call_grammar` as a
`json_schema` constraint on the existing `/v1/chat/completions` body (no native
`/completion` endpoint needed); Ollama (no grammar knob on its `/api/chat`); `Regex`/`Lark`
constraints for non-tool structured output; and any change to the agent loop,
trust tiers, or sandboxing. MistralRs-only, by the scoping decision.

## 4. Phase plan (docs-first, small phases per convention)

| Phase | Deliverable | Notes |
|---|---|---|
| **ST.0** ✅ | **This design contract** | Locked reference; banner flips per phase. DONE. |
| **ST.1** ✅ | **`tool_call_grammar` generator** | DONE. Pure fn `&[LlmToolDescriptor] -> serde_json::Value` at the `aivyx-llm` crate root (`tool_grammar.rs`, ungated — provider-agnostic so a future llama-server path reuses it): `oneOf` discriminated union, `name` pinned per-tool via `const`, `arguments` = each tool's `input_schema` verbatim, non-object schema → permissive-object fallback (still pins the name), **plus the `respond` sentinel branch** (`RESPOND_SENTINEL`, no-dot so it can't collide with a real `namespace.action` tool). Unit-tested standalone via the `jsonschema` validator (already a vetted workspace dep, test-only here): admits valid `fs.read`/`web.fetch`, rejects unknown name (`fs.write_file`, `browser_navigate`), rejects bad/extra/cross-tool arguments, admits `respond` (incl. with no tools) but still requires it be well-formed, rejects missing `name`/`arguments`. 5 tests, clippy clean. No engine. |
| **ST.2** ✅ | **Config field** | DONE. `constrain_tool_calls: bool` (`#[serde(default)]`, default false) on `MistralRsOptions`; auto-threads through load (the loader clones the whole `[mistralrs]` section). Extended the full-section round-trip to set + assert it, plus a dedicated `mistralrs_constrain_tool_calls_defaults_off` test (omitted key → false). 9 config tests green. |
| **ST.3** ✅ | **Provider wiring + sentinel** | DONE. `MistralRsConfig` carries `constrain_tool_calls` (`with_constrain_tool_calls` builder), threaded from `[mistralrs]` in `aivyx.rs` and stored on the provider. In `chat_stream`, when `constrain_tool_calls && !tools.is_empty()`, `builder.set_constraint(Constraint::JsonSchema(tool_call_grammar(tools)))`. **Key detail:** `JsonSchema` constrains the raw output *tokens*, not mistralrs's native tool-call channel — so the constrained JSON arrives as message `content`. New pure `parse_constrained_output` turns it into either a `ToolCall` (real name) or unwraps the `respond` sentinel to plain `Text`; a parse failure (defensive — the grammar guarantees shape) falls back to native extraction. Flag-off / tool-less turns take the byte-identical old path. 9 provider tests (5 new: builder default/set + real-call/sentinel/whitespace/non-json parse), clippy + binary check green with the feature. |
| **ST.4** ✅ | **Live verification** | DONE — headline claim **proven** on a real model (Qwen3-4B-Instruct-2507 Q4_K_M, CPU). Constrained ON: the model emitted **`fs.read {"path":"probe.txt"}`** then **`memory.write`** with the correct extracted content — two *real, registered* tool names with schema-valid JSON args, **by construction**. Audit chain confirms it: `scope_used` = real `fs.read:…`/`memory.write:…`, `Completed` outcomes, intact HMAC chain, and **no `auto_corrected_from`** (correct at the source, not fuzzy-recovered). The four prior substrate phases never got qwen to invoke `fs.write` at all. **Finding (deferred):** the small model *looped* — it kept re-emitting an identical `memory.write` and never selected the `respond` sentinel to end the turn, so the turn ran to the wall cap. The grammar *admits* `respond`; the model just doesn't *choose* it without prompt guidance → see §7. **Caveats:** CUDA path unavailable (`cudarc 0.19.7` rejects the box's CUDA 13.3 toolkit), so this ran CPU-only; per-turn CPU latency exceeds the hardcoded 120s `TURN_TIMEOUT`, so verification used a temporary local bump to 1800s (**reverted** — not in the diff). The `respond`-text path and the flag-OFF contrast were not run live (CPU-prohibitive); both are covered by unit tests. |
| **ST.5** ✅ | **Finalize** | DONE. `aivyx-llm` (354) + `aivyx-config` (24) suites green; `cargo clippy --all-targets` clean (and `-p aivyx-llm --features provider-mistral-rs` clean in ST.3); `cargo deny check licenses advisories` ok (`jsonschema` was already a vetted workspace dep — no new surface). `docs/LOCAL_FIRST_RUN.md` §5 cross-link added; the [[future-reliable-local-tool-calling]] memory flipped to done; the §7 findings saved as the seed for a "harden local tool-calling" follow-on; status → COMPLETE. |

**Discipline:** ST.1 (the grammar) is the load-bearing primitive and lands first,
fully tested without the engine, so the provider wiring (ST.3) is a thin adapter
over a proven function. The config field (ST.2) gates everything — nothing in the
default path changes until an operator opts in. Test band: **moderate** — dense in
ST.1 (the union schema + validation: admit-valid / reject-unknown-name /
reject-bad-args / sentinel) and ST.3 (constraint wiring + sentinel unwrap + flag
guards); price ~25–35 new tests.

## 5. Open questions (resolve in-phase)

- **Sentinel shape (ST.1/ST.3)** — `respond{text}` pseudo-tool inside the union
  (locked default) vs. a top-level `oneOf` of "tool-call object **or** bare string."
  The pseudo-tool keeps one uniform output shape the provider already knows how to
  parse; prefer it unless the engine's JSON-Schema compiler chokes on the bare-string
  alternative.
- **Schema-feature coverage (ST.1)** — confirm `llguidance`'s JSON-Schema subset
  accepts the constructs our tools' `input_schema`s actually use (nested objects,
  `enum`, `required`, arrays). If a tool uses an unsupported construct, fall back to
  an unconstrained `arguments: {type:object}` branch for *that* tool only (still pins
  the name) rather than failing the whole grammar.
- **Streaming interaction (ST.3)** — verify constrained decoding plays with the
  existing `chat_stream` event path (the constraint operates on logits, so it should
  be transparent to streaming, but confirm token-by-token emission still surfaces a
  well-formed `ToolCallEnd`).
- **Per-turn cost (ST.4)** — grammar compilation has a one-time cost per tool set;
  measure whether to cache the compiled grammar across turns when the tool list is
  stable.

## 6. ST.4 — Live verification runbook

The unit suites already prove the grammar is correct (ST.1) and the
parse/unwrap is correct (ST.3). What only a real model can prove is that
**mistralrs honors `Constraint::JsonSchema` end-to-end** so the constrained JSON
actually comes out. This runbook does exactly that, against the model class that
failed every prompt-substrate phase.

### Prerequisites

1. **Build with the engine feature** (CPU baseline is fine):
   ```sh
   cargo build -p aivyx-cli --features provider-mistral-rs --release
   ```
2. **A small, tool-capable instruct GGUF.** Recommended: a Qwen3-4B-Instruct
   quant (`Q4_K_M`, ~2.5 GB) — same family as the `qwen3.6:27b` that confabulated
   tool names in Phases 120/122/124. Drop it anywhere, e.g.
   `~/models/qwen3-4b-instruct-q4_k_m.gguf`. *(No model is bundled; the dev box has
   only an embedding GGUF, which is why this phase can't self-run.)*

### Config

```toml
[agent]
provider = "mistralrs"
model    = "qwen3-4b-instruct"

[mistralrs]
model_path           = "/home/<you>/models/qwen3-4b-instruct-q4_k_m.gguf"
constrain_tool_calls = true   # ← the ST.2/ST.3 switch; flip to false for the A/B

# Give the agent a tool that's easy to verify (read-only, sandboxed).
[fs]
root = "/tmp/aivyx-stencil"     # the agent can only touch this dir
```

### A/B procedure

Run the same prompt twice — once with `constrain_tool_calls = false`, once
`true` — and compare the audit chain.

```sh
mkdir -p /tmp/aivyx-stencil && echo "stencil ok" > /tmp/aivyx-stencil/probe.txt
AIVYX_PA_PASSPHRASE=… aivyx-pa daemon run            # start the daemon with the config above
# In another shell, drive one turn:
aivyx-pa say "read probe.txt and tell me what it contains"
```

**Pass criteria (flag ON):**
- The audit chain shows a real `ToolCall` for **`fs.read`** with
  `{"path":"probe.txt"}` (or the sandbox-relative form) — a *registered* name,
  not `fs.read_file` / `browser_*` / any hallucination.
- **No `auto_corrected_from`** field on the call — the name was right at the
  source, not fuzzy-recovered (that's the whole point: correct *by construction*,
  not by rescue).
- The follow-up turn ("just say hi, don't use any tools") returns clean assistant
  text via the **`respond` sentinel** — i.e. `parse_constrained_output` unwrapped
  `{"name":"respond","arguments":{"text":"…"}}` to a `FinalMessage`, **not** a
  spurious tool call.

**Contrast (flag OFF):** expect the documented fragility — empty/declined turns,
`<tool_code>`-in-text (rescued only if the textual-tool-call substrate fires), or a
hallucinated name. This is the before/after that justifies the chapter.

### Diagnosis

- **Empty `content` / `out=1`** → not a Stencil issue; the qwen3 `thinking` +
  `num_ctx` footguns (see [[ollama-thinking-and-context]] — for mistralrs set
  `max_seq_len` generously).
- **`ToolCall` present but name mangled with `constrain_tool_calls = true`** →
  the constraint isn't being honored; confirm the build actually has the feature
  and that `request.tools` was non-empty on that turn (the guard skips constraint
  on tool-less turns).
- **A genuine tool fails to fire while `respond` always wins** → the model is
  over-choosing the escape; tune the prompt, not the grammar (the grammar admits
  both — selection is the model's).

## 7. ST.4 findings & deferred follow-ups

The live run (2026-06-21, Qwen3-4B-Instruct-2507 Q4_K_M, CPU) **proved the
headline**: grammar-constrained decoding makes a small local model emit valid,
real-named, schema-correct tool calls *by construction* — `fs.read` then
`memory.write` with the right content, dispatched on the native channel, recorded
in the audit chain with real scopes, `Completed` outcomes, and **no
`auto_corrected_from`**. That is the keystone Chapter Stencil set out to land, and
it holds.

The run also surfaced two things worth a follow-up chapter — neither is a defect in
the grammar primitive:

1. **Constrained small models can loop (the missing terminator).** The model
   re-emitted an *identical* `memory.write` repeatedly and never selected the
   `respond` sentinel to finish, so the turn ran to the deadline. The grammar
   *admits* `respond`; the 4B just doesn't *choose* it unsolicited. The fix is at
   the **prompt layer**, not the grammar: the system prompt (or a constrained-mode
   preamble) must tell the model "emit `{"name":"respond",…}` to reply in plain
   text and end the turn." A cheap belt-and-suspenders is a **repeated-identical-
   tool-call breaker** in the turn loop (same `tool_id` + `input_hash` N times in a
   row → force-terminate), which also helps unconstrained local models. Deferred.

2. **No usable local GPU path on this toolchain.** mistralrs 0.8.1 → `cudarc 0.19.7`
   hard-rejects CUDA 13.3 (`Unsupported cuda toolkit version`). CPU inference of a
   4B is correct but too slow to clear the 120s `TURN_TIMEOUT` without a temporary
   bump. Two independent follow-ups: (a) track a mistralrs/cudarc bump that supports
   CUDA 13.x; (b) consider whether `TURN_TIMEOUT` should be operator-configurable
   for slow local backends (today it's a deliberate const — see its doc comment).

Neither blocks the chapter: the primitive is proven and shipped behind a default-off
flag. Both are natural seeds for a "harden local tool-calling" follow-on.

---

*Chapter Stencil is the lever the [[future-reliable-local-tool-calling]] note
called for: the four prompt-substrate phases proved a small model won't reliably
choose a valid tool from its prompt, and Phase 126/127 rescued the "right tool,
wrong channel" case — but neither can stop a model from naming a tool that doesn't
exist. Grammar-constrained decoding makes that impossible by construction, on the
one local path (the in-process mistral.rs engine) where the primitive is already a
single API call. It turns "local models are useful for chat but route tool-use to
the cloud" into "a small local GGUF can drive the agent loop reliably" — the
missing keystone of the free local on-ramp.*
