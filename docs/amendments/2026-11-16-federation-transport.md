# Amendment A19 — Federation Transport Between One Operator's Instances

**Date:** 2026-11-16
**Phase:** Chapter 4 (Product), FED.3.
**Extends:** **D4** (one new capability base), **D2** (two default-off
`ChannelContext` hooks and a `Peer` message origin), the audit event set,
and `docs/FEDERATION.md` §9 FED.3.
**Leaves unchanged:** D1 (the turn loop contract), D3, D5 (tiers), D6, D7
(storage), the locked relay wire shapes (`RelayRequest`, `RelayResponse`,
`Crossing`), and P6.
**Reference design:** `docs/superpowers/specs/2026-11-16-product-chapter-design.md`
(in `aivyx-ecosystem`).

---

## Why

FED.1 gave each instance an Ed25519 identity and a signed, replay-guarded
request header. FED.2 added deny-by-default trust and attenuation. The
relay wire shapes are locked as part of FED.3. Nothing yet moves those
messages between machines. FED.3
adds the transport, for one operator's own machines only: a laptop and a
desktop that both belong to the same person. This amendment records the
decisions that need an architecture change. Most of the code lands in
later FED.3 tasks.

## Transport

Peers talk over TCP, wrapped in Noise `XX`
(`Noise_XX_25519_ChaChaPoly_SHA256`, the `snow` crate). Each instance has
an X25519 transport key, sealed under the master key like the other
secrets in D7.

The transport key is tied to the instance identity by a binding
signature. The federation identity key signs the message context
`aivyx-federation-transport-v1`, the instance id and the transport public
key.

Operators exchange a peer card, a text string with the prefix
`aivyx-peer-v1:`. It holds the instance id, the Ed25519 public key, the
X25519 public key and the binding. `peer add` checks the binding and
refuses a card whose binding does not verify. Pasting a valid card on the
other machine is the trust decision. During the handshake, each side
checks that the remote static key equals the bound transport key of a
configured peer.

Limits: one application frame is at most 1 MiB. The handshake must
finish within 10 seconds.

This answers `FEDERATION.md` §10 *Transport* for one operator's machines.
A relay for other operators' instances stays open.

## D4: `federation.relay`

A19 adds one capability base, `federation.relay`. The qualifier is the
peer id, so `federation.relay:desktop` allows sending to the peer named
`desktop`.

- It is Trusted and Kernel only. SemiTrusted and below never hold it.
- It is irreversible. It sends a request to another instance, so
  at the default `assisted` level the operator confirms each outbound
  request.

`KNOWN_BASES` grows from 101 to 102 and `docs/TOOLS.md` lists the base.

## D2 hooks

A19 adds a `Peer` value to `MessageOrigin`, and two methods on
`ChannelContext`. Both have defaults that change nothing:

- `capability_ceiling() -> Option<CapabilitySet>`. The default is
  `None`. When it returns a set, the set is intersected into the turn's
  effective capabilities.
- `confirm_every_action() -> bool`. The default is `false`. When `true`,
  the turn runs in the `manual` posture: every action is confirmed.

Every existing channel keeps its current behavior.

A third default-off hook sits on the `Tool` trait:
`pauses_turn_deadline() -> bool` (default `false`). When it returns `true`,
the turn's wall-clock deadline pauses while that tool runs, as it does
while an approval waits. The three outbound federation tools return
`true`, because a task's reply is bounded by the peer's `task_timeout`
and not by the turn's 120 seconds. They still stop when the turn is
cancelled. A pausing tool in a parallel batch lifts the deadline for that
whole batch.

## Privacy line

A turn started by a peer gets no automatic memory recall, no persona or
profile context and no skill-trigger injection. It runs with a fixed peer
system prompt. What a peer can learn about the operator is what the tools
it is allowed to call return, plus the names and descriptions of the tools
its model is offered (the `federation.*` descriptions name the configured
peer ids).

## Consent

Each inbound task needs an up-front approval from the operator, such as
"Run a task from peer laptop: ...". After that, the turn's own gates
apply. Approvals are parked as "waiting" steps in the existing queue. The
peer's turn waits for the decision. Resolving a waiting step records the
decision and does not run the call; the waiting turn runs it. A waiting
step's audit entry carries a fixed summary (`<tool> from peer <id>`); the
peer's text stays in the encrypted step.

A peer's `assisted` ceiling defers to the receiving instance's own gates.
When those skip the delete and overwrite question in any area (an
`unleashed` level or override, or `[access] confirm_destructive = false`),
every `assisted` peer runs as `manual`. A peer's request never starts a
run for later (`schedule.*`, `webhook.create`, `file_watch.create`,
`team.run`), and those scopes can't be granted to a peer, because such a
run would execute outside the peer's ceiling.

## Audit

A new event, `FederationCrossing { peer_id, direction, verb, outcome, reason }`,
is written on both sides for every request, reply and refusal. `reason`
is empty for a relayed request and one of a fixed set of strings for a
refusal. The event never holds payload text or key material.

## Replay

`ReplayGuard` stays process-local. Inside a Noise session, a captured
frame cannot be replayed into a new session, because each session uses
fresh ephemeral keys. The window after a restart noted in `identity.rs`
is accepted.

## Not in A19

- A relay server for other operators.
- NAT traversal.
- Peer discovery.
- Key rotation.
- A hardware-held identity for the daemon. It would need a PIN at every
  daemon start. `federation yubikey-init` is unchanged.
