# Amendment A23 — Local-only Models

**Date:** 2026-11-21
**Phase:** Local-only chapter (aivyx-pa 2.0, aivyx-coder 2.0, aivyx-route 0.2.0).
**Amends:** **G6** (local execution, cloud inference, privacy non-negotiable)
and **N5** (no inference-side compromises for convenience) in `PRODUCT.md`
(the G6 paragraph, around line 212, and the "Multi-Provider LLM Support"
status entry, around line 1602), and the `LlmProvider` provider descriptions
in `DESIGN.md` (the `aivyx-llm` crate rows and the provider wording around
lines 18 and 238).
**Narrows:** **A15** (consent-gated cloud escalation) and **A16**
(on-failure escalation): both stay, but they now govern only endpoints the
operator labelled `locality = "cloud"` (local URLs treated as less trusted),
never a hosted provider.
**Leaves unchanged:** D1 (the turn loop contract), D2, D3, D4 (no new
capability), D5 (tiers), D6, D7 (no new storage domain), and the rest of the
amendments. Integrations, channels and web tools are out of scope.
**Reference design:** `docs/superpowers/specs/2026-11-21-local-only-design.md`
(in `aivyx-ecosystem`).

---

## Why

aivyx-pa and aivyx-coder are local-only products: they use local LLMs and
nothing else. G6 used to allow inference "to cloud LLM providers (Anthropic
today)". That sentence no longer describes the product, and a privacy promise
that rests on the operator not pointing a setting at the wrong place is weaker
than one the code enforces.

## G6, rewritten

G6 now reads: Aivyx PA runs on the operator's hardware, and **every model
call goes to a model server on that machine or the operator's own network**.
Storage is encrypted at rest with an operator-controlled passphrase. Network
egress is gated by capability scopes. Privacy and encryption are never
compromised in the name of a feature.

"Every model call" means chat, embeddings, vision, routing and the broker.
Voice was already local. It does not mean integrations (Gmail, Calendar,
Drive, Notion, n8n), channels (Telegram, Discord, Slack, Matrix, Signal,
Proton) or web tools: those stay as optional, operator-enabled features, and
turning one on moves the operator's own data to a service the operator chose.

## N5, narrowed

N5's "the operator's API key talks directly to the model provider" becomes
"prompts go directly to the operator's own model server". The prohibitions
(no proxying through a hosted service, no batching on Aivyx servers, no
telemetry, no cloud-side context store) are unchanged.

## What counts as local

A model URL is local if its scheme is `http` or `https` and its host is one of:

- loopback (`127.0.0.0/8`, `::1`), `localhost`;
- a private LAN address (RFC 1918), link-local, unique-local IPv6
  (`fc00::/7`), CGNAT / Tailscale (`100.64.0.0/10`) or unspecified;
- a name ending in `.local`, `.lan`, `.internal` or `.home.arpa`, or a
  single-label name (a compose service name, a LAN host name);
- a DNS name for which the system resolver (the URL's port, or the scheme's
  default) returns at least one address and **every** address is local under
  the IP rules above.

Everything else is not local: another scheme, an unparsable URL, a missing
host, a DNS failure, an empty answer, any public address (so a mixed answer
is refused). The check fails closed. The refusal reads:

> `<url> is not local (<reason>) — aivyx is local-only; point it at a model
> server on this machine or your own network`

with `<url>` stripped of any `user:pass@`, and `<reason>` one of `cloud
host`, `resolves to a public address <ip>`, `doesn't resolve` or `not an
http(s) URL`.

## Where the check runs

One shared guard in `aivyx-route` (`require_local`), called by pa:

- at **config load**, for every model URL: `[openai]`, `[broker]`,
  `[embedding]`, each `[routing.endpoints.*]` and team-member `base_url`,
  and the vision tool process's own config;
- at **build time**, when a provider is constructed;
- at **request and connect time**, before each model connection, so a DNS
  change after startup cannot move model traffic off the network.

A URL that fails is refused with the message above. `aivyx-pa doctor` still
runs when one fails (it skips the model checks).

## Providers

- **`ProviderKind::Anthropic` is removed.** `provider = "anthropic"` fails
  with `provider "anthropic" was removed in 2.0 — aivyx is local-only; use
  <the local kinds> with a local base_url`. The hosted routing endpoint kinds
  (`anthropic`, `openai`) are removed from `aivyx-route` too.
- **`openai` means an OpenAI-compatible local server.** It requires
  `[openai] base_url` (no built-in default URL); its key is optional.
- **The default provider is `llamacpp`** at `http://127.0.0.1:8080`, and
  `DEFAULT_MODEL` is `local-model`.
- **`[embedding]` needs an explicit local `base_url`.** There is no built-in
  embeddings endpoint.
- **`init` offers only local providers**, and its key verification checks a
  local server's `/v1/models`.

## The locality label

A routing endpoint's `locality` can no longer lift trust. `locality = "local"`
is honoured only through the DNS-verified path above and can't make a public
address local. `locality = "cloud"` still works, to mark a local endpoint
**less trusted** (a shared server other people use, say). Such an endpoint is
an escalation target and never an ordinary routing candidate: A15's gate, the
`/allow-cloud` consent, the taint rules, the audit ops (`CloudEscalation`,
`CloudConsentGranted`, `ConversationTainted`) and A16's `on_failure` all
apply to it unchanged. The word "cloud" survives in those commands, events and
audit names; it now means "labelled less trusted", never a hosted provider.

## Money

aivyx-cost ships no cloud price tables, so a dollar amount can't be priced.
A money cap (`[budget] per_run_usd`, `per_day_usd`, `per_mission_usd`,
`[loop] max_run_usd`) is refused at load and by the Studio and IPC, with "a
money cap needs cloud prices, which were removed in 2.0 — use a token or run
cap". Token caps (`per_mission_tokens`, `max_run_tokens`) and run caps remain.
A local run costs `$0` unless the operator sets a per-model rate with
`[pricing.<model>]`.

## Stored keys

A stored Anthropic key (`[anthropic] api_key`, `AIVYX_PA_ANTHROPIC_API_KEY`)
is never deleted. It is unused; `aivyx-pa doctor` says so in a note.

## Doctor

`aivyx-pa doctor` has a `local-only` row: ✓ when every model URL is local,
otherwise one line per failing endpoint with its reason, team members
included.

## Migration

A 1.x config that names `provider = "anthropic"`, or `provider = "openai"`
without a local `base_url`, or an `[embedding]` without one, or a money cap,
no longer loads. The fix is a local server and a local URL; the CHANGELOG
lists the steps. Nothing in the encrypted store changes.

## Not in A23

Removing or restricting integrations, channels or web tools; a network-level
block on model traffic (Landlock and seccomp can't filter by address); deleting
stored keys; renaming the `/allow-cloud` command, the `CloudEscalation` audit
op or the other "cloud" names (the wire and audit formats stay).
