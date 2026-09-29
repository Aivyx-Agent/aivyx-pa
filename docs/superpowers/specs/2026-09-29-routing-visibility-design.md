# Routing Visibility — Design

**Status:** approved 2026-09-29. It is work item **B** of the 2026-09-28 UI/UX audit, which found:

- **No front end but the CLI can see model routing.** The IPC protocol has no routing query, and the chat stream never says which model answered.
- **The Studio lacks the controls.** It has no models screen and no "allow cloud" control.
- **The TUI** shows no model.
- **`aivyx-pa`'s chat** has no `/model` commands. `aivyx-coder` has them.
- **The cloud-consent request reads as an LLM error with internal names:** "completed: LLM error: model routing: this needs a cloud model: `m@e` (tier), about N tokens…".
- **Telegram, Discord and Slack users are told to send `/allow-cloud`**, which those channels can never grant: consent needs Trusted, and the bots cap at SemiTrusted.

## Decisions (operator, 2026-09-29)

1. **The Studio** gets a status-bar model, a consent card with an "Allow cloud" button, and a Models screen.
2. **Chat** gets `aivyx-coder`'s routing commands: `/models`, `/models why`, `/model <id>` and `/model auto`.
3. **The consent request** becomes a structured event that each front end renders natively, with friendlier text worded per channel.

**Unchanged:** Amendment A15's rules stay exactly as they are:
- consent is per conversation, held in memory, and granted only from a Trusted/Kernel channel or the CLI;
- a tainted conversation never escalates;
- every decision is audited.

The locked turn-outcome types (D3) are not changed. The daemon picks up a consent request through the routing guard instead.

## B1 — the daemon protocol

### New stream events (`aivyx-ipc`, `StreamEventPayload`)

Both are additive. Adapters skip unknown kinds, per the channel SDK contract.

**`ModelRouted { model: String, task: String, reason: String }`**
- The daemon sends it after each turn once the conversation has had a routed call, just before `TurnComplete`, from `routed.router().last_decision(session)`. It carries the latest decision, so a turn with no routed call of its own repeats the previous one: front ends show the model and react only when it changes. A plain `render_for_cli` prints nothing for it, so `--headless` output is unchanged (decided after the Task 2 review).
- `model` is the `id@endpoint` key and `reason` is the router's human sentence.
- It is not sent when routing is off, or when the conversation had no routed call.

**`CloudConsentRequested { model: String, endpoint: String, why: String, estimated_tokens: u32, can_allow_here: bool }`**
- It is sent when a turn stopped to ask for cloud consent.
- `model` is the model id and `endpoint` the endpoint name.
- `why` is plain words:
  - `no_local_candidate` becomes "no local model can handle this request";
  - `tiers` becomes "this kind of request is set to use the cloud";
  - `on_failure` becomes "the local model got stuck".
- `can_allow_here` is true when the channel is Trusted or Kernel.

### Consent plumbing

- The `EscalationArming` trait method `note_consent_requested(session)` gains the request: `note_consent_requested(session, &ConsentRequest { model, endpoint, trigger, estimated_tokens })`.
- `RoutingGuard` keeps the latest request per session.
- Its `take_consent_request(session)` hands it over once.
- The daemon calls it after the turn. When a request is present, the daemon:
  - **(a) Sends** `CloudConsentRequested`.
  - **(b) Replaces the outcome text** with the per-channel message.
    - **Terminal / TUI / Studio** (`can_allow_here`): "This needs a cloud model: `<model>` (your `<endpoint>` endpoint), because `<why>`. About `<N>` tokens — this conversation plus the assistant's instructions — would be sent. Send /allow-cloud to allow it for this conversation, then resend your message."
    - **Otherwise:** the same first two sentences, then "Cloud use can only be allowed by the operator — from the terminal (`/allow-cloud`) or the Studio."
    - `<N>` is formatted with thousands separators.
- **The on_failure hint path** ("The local model got stuck; send /allow-cloud and resend…") also gets per-channel wording.
- **`RoutedProvider`'s own error text** becomes the channel-neutral first two sentences. No internal trigger names and no `LLM error:` framing reach an operator. The in-process REPL shows the terminal wording.

### New queries (`QueryPayload` / `QueryResponsePayload`)

**`GetRoutingStatus { session_id: Option<String> }`**
- It returns `RoutingStatus(RoutingStatusView)`, which carries:
  - `enabled`;
  - the default model;
  - the candidates, each with key, tier, capabilities, unknown capabilities, context window, availability and residency note (`loaded` / `needs_load` / `wont_fit` / none);
  - total and available VRAM;
  - the escalation mode (off / ask / auto / never);
  - whether the classifier is on;
  - for the given session: current model, pin, last decision (model and reason), tainted, and cloud consent allowed.
- When routing is off it returns `enabled: false` with empty lists, not an error.
- It shares one builder with the `routing.status` tool, so the two can't drift.

**`SetRoutingPin { session_id: String, model: Option<String> }`**
- It returns `RoutingPinned { model: Option<String> }` or `QueryError`.
- `model` accepts `id@endpoint`, or a bare id served by exactly one endpoint. The resolution matches `aivyx-coder`'s `resolve`.
- `None` unpins.
- Trust: the same as the other Studio settings queries.

## B2 — chat commands

- **Commands:** `/models`, `/models refresh`, `/models why`, `/model`, `/model <id>` and `/model auto`.
- **Wording:** exactly `aivyx-coder`'s (`aivyx-coder/crates/aivyx-core/src/routing_commands.rs`):
  - "Routing candidates (* = this conversation's model):";
  - the residency block;
  - "Pinned this conversation to `…`.";
  - "Pin cleared; routing chooses this conversation's model again on the next call.";
  - "No routed call yet in this conversation.";
  - and so on.
- **The one difference:** the routing-off message names `aivyx-pa`'s config, "Model routing commands are not available here — they need `[routing] enabled = true`."
- **Where they're handled:** the daemon intercepts them like `/allow-cloud`: a whole message is a command, never reaching the model or the history. Pins are not audited: no audit tag fits a manual pin, and A15/A16 scope audit to escalation (decided in Task 3). They're available from any channel, since reading and pinning aren't escalation. In-process chat is never routed (its planner attaches no route hint), so there the commands reply that routing applies to daemon conversations and how to start the daemon (decided after the Task 4 review).
- **Shared code:** one module in `aivyx-channel` produces the replies for both paths.

## B3 — the terminal front ends

- **TUI:**
  - The status bar shows `model <id@endpoint>` from the latest `ModelRouted`, cleared on a new conversation.
  - A `CloudConsentRequested` shows as a highlighted notice line with the per-channel text.
- **REPL (daemon and in-process):** it prints `routing → <model> (<reason>)` when the conversation's routed model changes, as `aivyx-coder` does. The consent text is shown as the outcome.
- **Telegram, Discord and Slack:** they ignore `ModelRouted` and show the consent outcome text, which the daemon already words for them.

## B4 — the Studio (`aivyx-web`)

- **Status bar:** shows the model from the latest `ModelRouted` for the Studio's session, with the reason in the tooltip. It says "routing off" when there are none.
- **Consent card:** in Chat, a `CloudConsentRequested` renders as a card:
  - it states the model, why, and the token estimate;
  - it has an **"Allow cloud for this conversation"** button, which sends `AllowCloudEscalation { session_id }`;
  - on success it shows "Allowed for this conversation until the page reloads or the daemon restarts." (matching the Models screen, since a reload starts a new conversation) and a **"Resend"** button that resubmits the last message.
  - When a card is shown, the duplicate outcome text line is suppressed.
- **Models screen:** a new `View::Models` (slug `models`), in the sidebar's System group with its own icon. It shows:
  - whether routing is enabled, and if not, how to enable it;
  - a candidates table (model, tier, plain-word capabilities such as "tools · vision · thinking: unknown", context, availability, loaded / needs load / may not fit);
  - VRAM;
  - the escalation mode and the classifier;
  - "This conversation": current model, why, and a pin selector (Auto plus each candidate) using `SetRoutingPin`.
  - It polls `GetRoutingStatus` like the other screens do.
- **Bundle:** rebuilt with `dx` (the `just build-web` recipe; `.br` sidecars stripped) and committed. This also fixes the stale "v0.9.4" status-bar version.

## B5 — docs

- **A new guide page,** `docs/guide/12-models-and-routing.md`, added to the Studio's Guide. It covers what routing does, the Models screen, the chat commands, and cloud consent.
- **The screens reference** gains a Models row.
- **`docs/DAEMON_IPC.md`** gets the new events and queries.
- **`CHANGELOG.md`.**

## Testing

- **B1:**
  - serde round-trips for the new events and queries;
  - the guard stores and takes a consent request;
  - the daemon sends `ModelRouted`, and sends `CloudConsentRequested` with per-channel text for a Trusted versus a SemiTrusted channel. Use the existing daemon test harnesses with a scripted routed provider;
  - `GetRoutingStatus` with routing on and off; `SetRoutingPin` resolving and failing;
  - the Python SDK conformance suites still pass.
- **B2:** each command's reply matches `aivyx-coder`'s wording; the model is never called; pin and auto change `Router::pinned`.
- **B3:** TUI model and reducer tests for the status bar and the consent line; the REPL line only on a change.
- **B4:**
  - `aivyx-web` clippy on native and a check on wasm32;
  - reducer and pure-function tests where the crate has them;
  - a real run with headless Chrome: routing on against Lemonade, a turn that routes, then the status bar and Models screen show it;
  - a `tiers = ["chat"]` escalation config with a dummy key shows the consent card. Don't click Allow: that would call the cloud.

## Out of scope

- Item C's quick fixes. The Studio bundle rebuild here incidentally fixes its stale version.
- Changing escalation semantics.
- A Studio control for the escalation mode.
