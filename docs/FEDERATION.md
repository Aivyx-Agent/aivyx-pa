# Agent Identity & Cross-Boundary Trust — the Federation Keystone

> **Status:** design contract — the keystone [`VISION.md`](../VISION.md) §2 says
> to *get right early, even though it's built last.* This is the one primitive
> that **cannot be safely retrofitted**, so its *shape* is fixed now.
>
> **Public on purpose.** This is the trust/security substrate of the open core —
> like [`SECURITY_POSTURE.md`](SECURITY_POSTURE.md), being open is what makes it
> trustworthy. The **Nexus *product*** that rides on it (peer discovery, the
> reputation/agent-hiring market, the network economics) is kept privately,
> outside this repo. This doc is the *boundary primitive*; Nexus is one thing
> built on it, the Factory (multi-node) is another.

---

## 0. Why this is the keystone

`VISION.md`'s central architectural insight: **"a network of agents is a Nonagon
team with the trust boundary moved."** Same delegation, same capability
attenuation, same gates, same audit — the *only* new thing is **identity and
trust at the boundary between operators.** This document defines that boundary.

It unlocks **both** unbuilt ceilings at once:

- **Factory** — an operator's *own* multiple nodes cooperating (high mutual trust).
- **Nexus** — *different operators'* agents discovering, sharing skills, and (with
  consent) delegating to each other (low default trust, earned).

And there is real salvage: the pre-rebuild archive's **`aivyx-federation`** crate
(Ed25519 identity, signed+replay-guarded requests, per-peer `TrustPolicy`, relay
ops for chat / task-delegation / knowledge-search) already prototyped this with
the right instincts. This doc modernizes that onto the new core's primitives
rather than inventing from scratch.

## 1. The core invariant (the line that moves)

Everything the agent does today assumes **"I trust my operator."** The moment a
second agent exists, the model becomes:

> **"I trust my operator — and I trust *no peer by default.* A peer earns
> *scoped, attenuated, revocable* trust, and any content it sends (a shared
> skill, a delegated task, a message) is *untrusted input*: sandboxed,
> provenance-tracked, run under attenuated capabilities, and gated by my
> operator when it has effect."**

Three things are non-negotiable from that sentence — identity (who is the peer),
trust (how much may they do), and the privacy line (what may cross). §§2–4.

## 2. Identity — *who is this agent?*

- **An Ed25519 keypair per instance**, owned by the operator (sovereignty: the
  identity is theirs, not Aivyx-the-company's). Salvage: `FederationAuth`.
- Identity = `instance_id` + public key. Peers know each other by public key.
- **Signed requests** (salvage: `SignedHeader`): every cross-boundary request
  carries `{ instance_id, timestamp, Ed25519 signature over
  aivyx-federation-v1:recipient_id:instance_id:timestamp:body_hash }`, verified
  against the peer's known public key; the signature names the receiving
  instance, so a request made for one peer is worthless at another. A **replay guard** (timestamp window) blocks replays.
- **Key material is never logged** (the archive's manual `Debug` on the keypair —
  keep that). Key rotation + revocation distribution is an open question (§11).

## 3. Trust — *how much may a peer do?* (NT-02, generalized)

Per-peer **`TrustPolicy`**, **deny-by-default** (no policy ⇒ denied; least
privilege — the archive's relay already enforced this). It carries:

- **`allowed_scopes`** — the capability bases this peer may ever invoke
  (`["memory.read", "skills.read", …]`), expressed in the *new core's*
  `aivyx_capability::Scope` vocabulary.
- **A peer autonomy ceiling** — the maximum [`AUTONOMY.md`](AUTONOMY.md) level a
  peer's relayed request may run at, **default = confirm-first** (the archive's
  `max_tier = Leash`). A peer can never push my agent past it.

The enforced authority of any peer-relayed action is the **intersection** — the
exact NT-02 team-attenuation rule, lifted across the operator boundary:

```
effective(peer request) =
      what the peer asked for
    ∩ my TrustPolicy.allowed_scopes for that peer
    ∩ my channel/trust-tier ceiling
    ∩ my [autonomy] cap for peers
```

This reuses the machinery that already exists — `Scope::is_granted_by`,
`CapabilitySet`, the trust tiers, and the Reins autonomy posture — so a peer is
*structurally incapable* of exceeding what **both** sides allow. Trust is
**revocable**: tighten or drop the `TrustPolicy` and the peer's reach narrows
immediately.

## 4. The privacy line — *what may cross?*

> **Procedures, capabilities, reputations, public personas cross. The operator's
> private memory and data never do.**

The agent's own model already draws this line: a `LearnedSkill` is
`{name, trigger, procedure}` — *what the agent knows how to do* is shareable; *what
it knows about its operator* is not. Federation transmits the procedure, never
the memory it was learned from. This is a structural constraint on every
shareable artifact, not a setting.

**Peer content is untrusted input.** A skill, a delegated task, or a message from
a peer is treated like any hostile input: schema-validated, **sandboxed**,
**provenance-tagged** (which peer, which key), run **only** under that peer's
attenuated caps (§3), and — for anything with effect (a "hire", a delegation that
acts) — **gated by operator consent** (the Nexus "hire each other, *with the end
user accepting*" rule, enforced by the existing confirm-first / approval-gate
machinery).

## 5. The protocol — one shape, two boundaries

Nonagon's intra-operator delegation/message bus and Nexus's cross-operator relay
are the **same protocol**; the trust boundary is the only difference. The archive
already shaped the verbs:

| Verb | Salvage | What it is |
|---|---|---|
| relay a message | `RelayChatRequest` | agent ↔ agent dialogue |
| delegate a task | `RelayTaskRequest` | "do this for me" (gated, attenuated) |
| share/search knowledge | `FederatedSearchRequest` | skills / public knowledge exchange |

**Design rule (the cheap insurance `VISION.md` buys):** build the Nonagon
delegation/message types in [`aivyx-ipc`](DAEMON_IPC.md) (the wasm-clean protocol
substrate) **as if the peer on the other side might be a stranger's agent on
another machine.** Then Nexus is an *extension* of the local protocol, not a
rewrite.

## 6. Every crossing is audited

The HMAC audit chain extends across the boundary: every relayed request,
response, grant, and refusal is an `AuditEvent` carrying the peer's identity. The
[`SECURITY_POSTURE.md`](SECURITY_POSTURE.md) invariant — *"removes the human,
never removes the audit"* — becomes *"spans operators, never un-audited."* A peer
interaction is as forensically legible as a local turn.

## 7. Invariants (the lines that do not move)

- **No peer trusted by default.** Trust is earned, scoped, and revocable;
  absent a `TrustPolicy`, a peer can do nothing.
- **Attenuation across operators.** A peer's effective authority is the
  intersection of what *both* sides allow — never more (§3). NT-02, generalized.
- **Procedures travel; data never.** The privacy line holds at the boundary.
- **Peer content is untrusted input** — sandboxed, provenance-tracked, gated.
- **Operator consent for peer-initiated effect.** A peer can request; only the
  operator's gate lets it *act*.
- **No cross-boundary self-escalation.** A peer can never grant *my* agent reach
  *my* operator didn't (the `SECURITY_POSTURE.md` Kernel-tier no-self-escalation
  rule, extended: authority comes only from my operator, never from a peer).
- **Identity is operator-owned and never logged.** The keypair is the operator's;
  key material stays out of logs and audit payloads.
- **Every crossing is on the one HMAC chain.**

## 8. Salvage + placement

Lift `aivyx-federation` from the archive (`Ed25519` auth, `ReplayGuard`,
`TrustPolicy`, the relay verbs) and **modernize it onto the new core**: swap the
old string scopes for `aivyx_capability::Scope`, the old `AutonomyTier` for the
Reins `AutonomyLevel`, wire the relay through `aivyx-ipc`, and route every
crossing onto `aivyx-audit`. It lands as a **primitive in the open core** — both
the Factory and Nexus consume it (consistent with "license-boundary, not
engine-fork": the *primitive* is open; the *Nexus product* is the commercial
layer).

## 9. Phase plan (built last; designed now)

| Phase | Deliverable | Status |
|---|---|---|
| **FED.0** | This design contract. | ✅ |
| **FED.1** | Identity: operator-owned Ed25519 keypair + the signed, replay-guarded request envelope (modernized `FederationAuth`/`SignedHeader`). | ✅ `aivyx-federation::identity` |
| **FED.2** | `TrustPolicy` + the cross-operator attenuation — generalize NT-02's intersection over `Scope`/`CapabilitySet` + the Reins autonomy ceiling. | ✅ `::trust` |
| **FED.3** | The relay protocol in `aivyx-ipc` (one shape, local + remote) + auditing every crossing. | ◑ verbs + audit *shape* (`aivyx-ipc::federation`, `::relay::Crossing`); the `AuditEvent` variant + live emission land with transport |
| **FED.4** | The privacy/sandbox boundary for peer content (provenance, attenuated execution). | ✅ `::received` |
| **FED.5** | Operator-consent gating for peer-initiated effect (reuse confirm-first / approval gates). | ✅ `::consent` |

**Built as Chapter Passport (2026-06-28)** — the open-core trust **substrate**
in `crates/aivyx-federation`, proven end-to-end by a two-identity in-process
integration test (`tests/keystone_e2e.rs`). What remains, by design, is
everything **network-shaped**: a **transport / relay server**, peer
**discovery**, and the live `AuditEvent::FederationCrossing` emission — plus the
**Nexus product** on top (reputation, the agent-hiring market and its
economics), designed privately and built when there's an installed base to
network (the last link in `VISION.md`'s chain).

**Hardware-backed identity (2026-09).** FED.1's operator-owned keypair can
now be generated and held on a YubiKey's OpenPGP card applet instead of a
software key sealed at rest — `Identity::load_hardware`, behind
`aivyx-federation`'s own `yubikey` Cargo feature (off by default: the
`aivyx-yubi` crate this pulls in transitively needs `libpcsclite`/`pcscd`
at build and run time — a real, separate system dependency no other Aivyx PA
command requires). Provisioning is a one-time operator step, `aivyx-pa
federation yubikey-init` — see `docs/INSTALL.md`'s "Hardware-backed
federation identity (YubiKey)" section for setup, the `pcscd` requirement,
and usage.

## 10. Open questions (resolve in-phase, not blocking FED.0)

- **Discovery** — how do agents *find* each other? (a relay/directory; a private
  Nexus concern, but the identity/trust primitive must not assume a topology).
- **Reputation** — how earned trust is represented + shared without leaking data.
- **Transport** — the archive used an HTTP relay; reconsider (libp2p? a hub
  relay? direct?) against the privacy + NAT realities.
- **Key rotation + revocation distribution** — how a rotated/compromised key
  propagates to peers.
- **Economics** — if agents hire each other, the transaction/settlement layer is
  a private-`STRATEGY.md` concern; the trust + consent primitives here are fixed.
