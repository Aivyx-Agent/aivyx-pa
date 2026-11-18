# Aivyx PA Docs

This directory holds the **living reference docs**, the **roadmaps**,
and **amendments**. The frozen **phase journals** and point-in-time
reviews now live under [`archive/`](archive/) — see [Archive](#archive)
below. The docs here are deliberately separate from the contract
documents in the repo root:

- **`VISION.md`** is the **north star** — the mission (*Build It Right
  First*), what Aivyx PA is, the architecture's destination (local Nonagon
  teams generalizing into a network of agents), the order of growth, and
  the guiding test every chapter is held against. It sits *above* the two
  contracts: they say *how* the assistant works, it says *what* the
  ecosystem is and *in what order* it grows. The commercial strategy that
  rides on it is kept privately, outside this public repo.
- **`DESIGN.md`** is the **technical contract** — the locked
  architectural decisions ("how the agent works") that every phase
  must respect. It is edited rarely, and only through the amendment
  process described below.
- **`PRODUCT.md`** is the **product contract** — the locked
  product-shape decisions ("who the agent is for, what it commits
  to do, where the line is") that every phase must respect. It is
  the sibling of `DESIGN.md`, edited under the same amendment
  process. Drafted at the Phase 12.5 product-shape review.
- **`docs/archive/phases/PHASE_N.md`** are **phase journals** — working
  documents for one phase at a time. They churn freely during the phase,
  are frozen at phase exit, and are kept under `archive/` as historical
  artifacts.
- **`docs/ROADMAP.md`** is the **technical roadmap** — one-paragraph
  intents for upcoming numbered phases.
- **`docs/PRODUCT_ROADMAP.md`** is the **product roadmap** — the
  sibling of `ROADMAP.md`, listing forward product-shape milestones
  derived from `PRODUCT.md`'s commitments. Milestones are named, not
  numbered, and may span one or more technical phases.
- **`docs/POST_V1_ROADMAP.md`** is the **post-v1.0.0 idea parking
  lot** — things explicitly *not* being built now, each with a reason
  and a trigger condition for revisiting. Not a schedule; an idea
  graduates into `ROADMAP.md`/`PRODUCT_ROADMAP.md` once its trigger
  fires.
- **`docs/amendments/`** (created when first needed) holds one file per
  contract change. An amendment is the *only* legal way to modify a
  locked `DESIGN.md` *or* `PRODUCT.md` decision from inside a later
  phase.

## Why the split?

The failure mode this layout defends against is **silent contract
drift** — a later phase quietly editing a Phase 0 decision because it
became inconvenient. With the contract in one file and the phase work
in another, any such edit shows up as a `DESIGN.md` diff in a phase
commit, which is an obvious review flag.

Rule of thumb: if you're writing something that future-you in a fresh
session needs to rely on, it goes in `DESIGN.md` (technical) or
`PRODUCT.md` (product). If it's a decision log, task list, or "why we
chose A over B today," it goes in the phase doc. If it's a one-paragraph
intent for a future phase or milestone, it goes in `ROADMAP.md` or
`PRODUCT_ROADMAP.md`.

## Reviews

Point-in-time audits of the framework's state — current at top level,
historical under [`archive/`](archive/):

- [`BACKEND_AUDIT_2026-06-16.md`](BACKEND_AUDIT_2026-06-16.md) — the
  **current** backend audit (v0.2.0, post-Studio): health snapshot,
  prior-gap status, and the ranked findings (F1 CSWSH **resolved**;
  F2–F4 roadmap-shaped) that seed the next roadmap.
- [`archive/BACKEND_REVIEW_2026-06-06.md`](archive/BACKEND_REVIEW_2026-06-06.md)
  and [`archive/AGENT_REVIEW_2026-06-05.md`](archive/AGENT_REVIEW_2026-06-05.md)
  — the prior Phase-179/184 reviews (superseded).

## Cross-phase reference docs

Living documents that span multiple phases and capture patterns
learned across the phase sequence. Unlike phase journals, these are
edited freely when a new adapter or subsystem teaches us something new.

- [`ADAPTER_PATTERN.md`](ADAPTER_PATTERN.md) — future-proof checklist
  for adding a new `ChannelContext` adapter, grounded in the two
  adapters in tree (`LocalChannel`, `TelegramChannel`). Read this
  first if you're about to add a third.
- [`NONAGON.md`](NONAGON.md) — the **Nonagon** (Chapter J): the
  multi-agent team capability — a lead convening ≤9 attenuated
  specialists, the mission DAG, the safety invariant (NT-02), and the
  `aivyx-pa team` CLI. ✅ complete.
- [`VERTICAL_PACKS.md`](VERTICAL_PACKS.md) — the **pack** model: how to
  specialize the one agent to a domain (template + toolkit + scopes +
  **team** + skills + integrations) without forking the substrate.
  Worked example: the Kitchen / Back-of-House pack.
- [`FRONTEND.md`](FRONTEND.md) — the **Studio** (Chapters R–Z): the Stitch
  design system + every web screen (Command, Missions, Chat, Memory, Settings,
  Agents, Teams, Documents), each contract + phase plan + live-verify record.
  ✅ complete.
- [`PERSONA_SEED.md`](PERSONA_SEED.md) — the **onboarding seed** (Chapters W–X):
  the end user gives the agent a starting Persona + Skills (config-driven boot
  seed + live `SeedPersona` IPC + LLM "describe it" drafting), planted on the
  signed chain. ✅ complete.
- [`COST_GOVERNANCE.md`](COST_GOVERNANCE.md) — **token accounting + budgets**
  (Chapter K): per-turn dollar pricing, the `LlmCost` audit event, `aivyx-pa cost`,
  and `[budget]` caps that alert/deny. ✅ shipped.
- [`RATE_LIMITS.md`](RATE_LIMITS.md) — **tool-call rate limits & quotas**
  (Chapter Throttle): the per-tool / per-turn / sliding-window gate that bounds
  *how often* tools run — the budget gate's sibling for call counts; closes
  audit F2. ✅ shipped.
- [`TOOLS.md`](TOOLS.md) — the **tool catalog** (Chapter Atlas): every tool, its
  capability scope base, minimum trust tier, and delivery tier
  (substrate / infrastructure / tool-process / MCP) + the name→scope mapping.
  Drift-guarded against `KNOWN_BASES`; the agent enumerates its own tools at
  runtime via `tools.list`. ✅ shipped.
- [`ALMANAC.md`](ALMANAC.md) — the **Studio Tools screen** (Chapter Almanac): a
  read-only, searchable browse of the daemon's registered tool catalog — name,
  description, capability base, minimum trust tier — grouped by domain. The
  Studio-side companion to `TOOLS.md`'s static catalog. 🟡 in progress.
- [`SECURITY_POSTURE.md`](SECURITY_POSTURE.md) — **what an autonomous agent can
  and cannot do**: the four-layer containment model (access level →
  `confirm_destructive` → loop caps → Kernel-tier no-self-escalation) over the
  HMAC audit chain, the attended/unattended split, the threat model (secret
  read at `full`, the `confirm_destructive` linchpin), and recommended
  per-profile configurations. The page to read before granting real reach.
- [`AUTONOMY.md`](AUTONOMY.md) — **one dial the end user controls** (Chapter
  Reins, ✅ shipped — see the doc's closeout): a single `[autonomy] level`
  (`manual`→`unleashed`) that composes the scattered autonomy knobs into named
  tiers + per-domain overrides, operable from `aivyx-pa autonomy` and the Studio,
  arming the autonomous loop. The two "loosening" levers (bounded AutoApprove,
  skill auto-adoption) were deliberately **not** built — one approves nothing
  today, the other violates PRODUCT.md P8 — which is the `SECURITY_POSTURE.md`
  containment model working as designed.
- [`FEDERATION.md`](FEDERATION.md) — **agent identity & cross-boundary trust**
  (the keystone, `VISION.md` §2): the one primitive to get right early —
  Ed25519 operator-owned identity, per-peer `TrustPolicy` with NT-02 attenuation
  generalized across operators, the procedures-travel-data-never privacy line,
  one delegation protocol for local (Nonagon) and remote (Nexus) — designed now,
  built last. Unlocks both multi-node (Factory) and the agent network (Nexus).
- [`BOARD.md`](BOARD.md) — **the work board** (A20, preview, Factory's first
  piece): goals, projects and tasks you plan and assign to a role or the team;
  role runs and team runs, review, send back, the CLI and the Studio view.
- [`LOCAL_HOSTING.md`](LOCAL_HOSTING.md) — **running Aivyx PA on a capable GPU box**
  (e.g. a 24GB RTX 3090): VRAM-tiered model + `num_ctx` choices, the
  Ollama-vs-embedded-CUDA tradeoff, tool-calling reliability on local models, and
  a dedicated-host setup recipe. The capable-hardware counterpart to
  `LOCAL_FIRST_RUN.md`'s modest on-ramp.

## Archive

Frozen, point-in-time artifacts live under [`archive/`](archive/) so the
top of `docs/` stays focused on living reference + roadmap material:

- [`archive/phases/`](archive/phases/) — the 188 **phase journals**
  (`PHASE_0.md`–`PHASE_188.md`), indexed by the Phase status table below.
- [`archive/walkthrough.md`](archive/walkthrough.md) — the frozen Phase 9
  codebase audit.
- [`archive/AGENT_REVIEW_2026-06-05.md`](archive/AGENT_REVIEW_2026-06-05.md)
  and [`archive/BACKEND_REVIEW_2026-06-06.md`](archive/BACKEND_REVIEW_2026-06-06.md)
  — dated review snapshots.

See [`archive/README.md`](archive/README.md) for the archive's own index.

## Phase status

| Phase    | Status  | Doc                        | Commit    |
|----------|---------|----------------------------|-----------|
| Phase 0  | Frozen  | [PHASE_0.md](archive/phases/PHASE_0.md)   | `1b4f271` |
| Phase 1  | Frozen  | [PHASE_1.md](archive/phases/PHASE_1.md)   | `33012be` |
| Phase 2  | Frozen  | [PHASE_2.md](archive/phases/PHASE_2.md)   | `2b6f876` |
| Phase 3  | Frozen  | [PHASE_3.md](archive/phases/PHASE_3.md)   | `fa0f4ea` |
| Phase 4  | Frozen  | [PHASE_4.md](archive/phases/PHASE_4.md)   | `999ce87` |
| Phase 5  | Frozen  | [PHASE_5.md](archive/phases/PHASE_5.md)   | `6dab2a7` |
| Phase 6  | Frozen  | [PHASE_6.md](archive/phases/PHASE_6.md)   | `912f022` |
| Phase 7  | Frozen  | [PHASE_7.md](archive/phases/PHASE_7.md)   | `8164317` |
| Phase 8  | Frozen  | [PHASE_8.md](archive/phases/PHASE_8.md)   | `0484606` |
| Phase 9  | Frozen  | [PHASE_9.md](archive/phases/PHASE_9.md)   | `7052ecc` |
| Phase 10 | Frozen  | [PHASE_10.md](archive/phases/PHASE_10.md) | `f8f4d28` |
| Phase 11 | Frozen  | [PHASE_11.md](archive/phases/PHASE_11.md) | `16422e2` |
| Phase 12 | Frozen  | [PHASE_12.md](archive/phases/PHASE_12.md) | `16e618c` |
| Phase 13 | Frozen  | [PHASE_13.md](archive/phases/PHASE_13.md) | `25a09de` |
| Phase 14 | Frozen  | [PHASE_14.md](archive/phases/PHASE_14.md) | `0d94d32` |
| Phase 15 | Frozen  | [PHASE_15.md](archive/phases/PHASE_15.md) | `06dfdfd` |
| Phase 16 | Frozen  | [PHASE_16.md](archive/phases/PHASE_16.md) | `1ed3f90` |
| Phase 17 | Frozen  | [PHASE_17.md](archive/phases/PHASE_17.md) | `277d910` |
| Phase 18 | Frozen  | [PHASE_18.md](archive/phases/PHASE_18.md) | `6dd4f23` |
| Phase 19 | Frozen  | [PHASE_19.md](archive/phases/PHASE_19.md) | `986c519` |
| Phase 20 | Frozen  | [PHASE_20.md](archive/phases/PHASE_20.md) | `8e77075` |
| Phase 21 | Frozen  | [PHASE_21.md](archive/phases/PHASE_21.md) | `05cc349` |
| Phase 22 | Frozen  | [PHASE_22.md](archive/phases/PHASE_22.md) | `549bc6e` |
| Phase 23 | Frozen  | [PHASE_23.md](archive/phases/PHASE_23.md) | `8f104e8` |
| Phase 24 | Frozen  | [PHASE_24.md](archive/phases/PHASE_24.md) | `84962d1` |
| Phase 25 | Frozen  | [PHASE_25.md](archive/phases/PHASE_25.md) | `5e4144f` |
| Phase 26 | Frozen  | [PHASE_26.md](archive/phases/PHASE_26.md) | `e550229` |
| Phase 27 | Frozen  | [PHASE_27.md](archive/phases/PHASE_27.md) | `426bcc2` |
| Phase 28 | Frozen  | [PHASE_28.md](archive/phases/PHASE_28.md) | `2cd41d9`  |
| Phase 29 | Frozen  | [PHASE_29.md](archive/phases/PHASE_29.md) | `03a804b`  |
| Phase 30 | Frozen  | [PHASE_30.md](archive/phases/PHASE_30.md) | `8b93180` |
| Phase 31 | Frozen  | [PHASE_31.md](archive/phases/PHASE_31.md) | `7f16f7a` |
| Phase 32 | Frozen  | [PHASE_32.md](archive/phases/PHASE_32.md) | `4aabea8` |
| Phase 33 | Frozen  | [PHASE_33.md](archive/phases/PHASE_33.md) | `d02dda6` |
| Phase 34 | Frozen  | [PHASE_34.md](archive/phases/PHASE_34.md) | `0004cf2` |
| Phase 35 | Frozen  | [PHASE_35.md](archive/phases/PHASE_35.md) | `5786449` |
| Phase 36 | Frozen  | [PHASE_36.md](archive/phases/PHASE_36.md) | `1f726f9`  |
| Phase 37 | Frozen  | [PHASE_37.md](archive/phases/PHASE_37.md) | `b2f3bda`  |
| Phase 38 | Frozen  | [PHASE_38.md](archive/phases/PHASE_38.md) | `b596207`  |
| Phase 39 | Frozen  | [PHASE_39.md](archive/phases/PHASE_39.md) | `6f7eaf1`  |
| Phase 40 | Frozen  | [PHASE_40.md](archive/phases/PHASE_40.md) | `d995afd`  |
| Phase 41 | Frozen  | [PHASE_41.md](archive/phases/PHASE_41.md) | `87d87c2`  |
| Phase 42 | Frozen  | [PHASE_42.md](archive/phases/PHASE_42.md) | `0478461`  |
| Phase 43 | Frozen  | [PHASE_43.md](archive/phases/PHASE_43.md) | `066e633`  |
| Phase 44 | Frozen  | [PHASE_44.md](archive/phases/PHASE_44.md) | `104c101`  |
| Phase 45 | Frozen  | [PHASE_45.md](archive/phases/PHASE_45.md) | `1ca3dc4`  |
| Phase 46 | Frozen  | [PHASE_46.md](archive/phases/PHASE_46.md) | `074d167`  |
| Phase 47 | Frozen  | [PHASE_47.md](archive/phases/PHASE_47.md) | `cc0a64f`  |
| Phase 48 | Frozen  | [PHASE_48.md](archive/phases/PHASE_48.md) | `7fbb534`  |
| Phase 49 | Frozen  | [PHASE_49.md](archive/phases/PHASE_49.md) | `8cadab8`  |
| Phase 50 | Frozen  | [PHASE_50.md](archive/phases/PHASE_50.md) | `0104c17`  |
| Phase 51 | Frozen  | [PHASE_51.md](archive/phases/PHASE_51.md) | `2503d90`  |
| Phase 52 | Frozen  | [PHASE_52.md](archive/phases/PHASE_52.md) | `57f6c14`  |
| Phase 53 | Skipped | (folded into Phase 54)     |            |
| Phase 54 | Frozen  | [PHASE_54.md](archive/phases/PHASE_54.md) | `f4e47be`  |
| Phase 55 | Frozen  | [PHASE_55.md](archive/phases/PHASE_55.md) | `8a750a1`  |
| Phase 56 | Frozen  | [PHASE_56.md](archive/phases/PHASE_56.md) | `2128e6a`  |
| Phase 57 | Frozen  | [PHASE_57.md](archive/phases/PHASE_57.md) | `81a6885`  |
| Phase 58 | Frozen  | [PHASE_58.md](archive/phases/PHASE_58.md) | `e512d3b`  |
| Phase 59 | Frozen  | [PHASE_59.md](archive/phases/PHASE_59.md) | `20516f2`  |
| Phase 60 | Frozen  | [PHASE_60.md](archive/phases/PHASE_60.md) | `cff2c4c`  |
| Phase 61 | Frozen  | [PHASE_61.md](archive/phases/PHASE_61.md) | `4e68a06`  |
| Phase 62 | Frozen  | [PHASE_62.md](archive/phases/PHASE_62.md) | `f3ec668`  |
| Phase 63 | Frozen  | [PHASE_63.md](archive/phases/PHASE_63.md) | `055207e`  |
| Phase 64 | Frozen  | [PHASE_64.md](archive/phases/PHASE_64.md) | `5fba6c8`  |
| Phase 65 | Frozen  | [PHASE_65.md](archive/phases/PHASE_65.md) | `661d4bc`  |
| Phase 66 | Frozen  | [PHASE_66.md](archive/phases/PHASE_66.md) | `372f78e`  |
| Phase 67 | Frozen  | [PHASE_67.md](archive/phases/PHASE_67.md) | `fe8cdb1`  |
| Phase 68 | Frozen  | [PHASE_68.md](archive/phases/PHASE_68.md) | `6683f21`  |
| Phase 69 | Frozen  | [PHASE_69.md](archive/phases/PHASE_69.md) | `97b6eff`  |
| Phase 70 | Frozen  | [PHASE_70.md](archive/phases/PHASE_70.md) | `7728a45`  |
| Phase 71 | Frozen  | [PHASE_71.md](archive/phases/PHASE_71.md) | `7d67b06`  |
| Phase 72 | Frozen  | [PHASE_72.md](archive/phases/PHASE_72.md) | `645c901`  |
| Phase 73 | Frozen  | [PHASE_73.md](archive/phases/PHASE_73.md) | `99928ef`  |
| Phase 74 | Frozen  | [PHASE_74.md](archive/phases/PHASE_74.md) | `a2ede10`  |
| Phase 75 | Frozen  | [PHASE_75.md](archive/phases/PHASE_75.md) | `b4aea53`  |
| Phase 76 | Frozen  | [PHASE_76.md](archive/phases/PHASE_76.md) | `aace61a`  |
| Phase 77 | Frozen  | [PHASE_77.md](archive/phases/PHASE_77.md) | `0317cbb`  |
| Phase 78 | Frozen  | [PHASE_78.md](archive/phases/PHASE_78.md) | `02b78f6`  |
| Phase 79 | Frozen  | [PHASE_79.md](archive/phases/PHASE_79.md) | `e763a5c`  |
| Phase 80 | Frozen  | [PHASE_80.md](archive/phases/PHASE_80.md) | `5afbaf5`  |
| Phase 81 | Frozen  | [PHASE_81.md](archive/phases/PHASE_81.md) | `f7a44d0`  |
| Phase 82 | Frozen  | [PHASE_82.md](archive/phases/PHASE_82.md) | `33239ee`  |
| Phase 83 | Frozen  | [PHASE_83.md](archive/phases/PHASE_83.md) | `d915e62`  |
| Phase 84 | Frozen  | [PHASE_84.md](archive/phases/PHASE_84.md) | `ae8b222`  |
| Phase 85 | Frozen  | [PHASE_85.md](archive/phases/PHASE_85.md) | `061b521`  |
| Phase 86 | Frozen  | [PHASE_86.md](archive/phases/PHASE_86.md) | `a331bcb`  |
| Phase 87 | Frozen  | [PHASE_87.md](archive/phases/PHASE_87.md) | `aa51577`  |
| Phase 88 | Frozen  | [PHASE_88.md](archive/phases/PHASE_88.md) | `7a69ac5`  |
| Phase 89 | Frozen  | [PHASE_89.md](archive/phases/PHASE_89.md) | `6e070d5`  |
| Phase 90 | Frozen  | [PHASE_90.md](archive/phases/PHASE_90.md) | `6457f7d`  |
| Phase 91 | Frozen  | [PHASE_91.md](archive/phases/PHASE_91.md) | `cf6a9fd`  |
| Phase 92 | Frozen  | [PHASE_92.md](archive/phases/PHASE_92.md) | `576d9c1`  |
| Phase 93 | Frozen  | [PHASE_93.md](archive/phases/PHASE_93.md) | `f6d5d07`  |
| Phase 94 | Frozen  | [PHASE_94.md](archive/phases/PHASE_94.md) | `d440dea`  |
| Phase 95 | Frozen  | [PHASE_95.md](archive/phases/PHASE_95.md) | `5250efd`  |
| Phase 96 | Frozen  | [PHASE_96.md](archive/phases/PHASE_96.md) | `c56cf30`  |
| Phase 97 | Frozen  | [PHASE_97.md](archive/phases/PHASE_97.md) | `cafbf13`  |
| Phase 98 | Frozen  | [PHASE_98.md](archive/phases/PHASE_98.md) | `59936fc`  |
| Phase 99 | Frozen  | [PHASE_99.md](archive/phases/PHASE_99.md) | `a648bbb`  |
| Phase 100 | Frozen | [PHASE_100.md](archive/phases/PHASE_100.md) | `08e2f28`  |
| Phase 101 | Frozen | [PHASE_101.md](archive/phases/PHASE_101.md) | `4967b11`  |
| Phase 102 | Frozen | [PHASE_102.md](archive/phases/PHASE_102.md) | `13dec8f`  |
| Phase 103 | Frozen | [PHASE_103.md](archive/phases/PHASE_103.md) | `38ce9f4`  |
| Phase 104 | Frozen | [PHASE_104.md](archive/phases/PHASE_104.md) | `701c33a`  |
| Phase 105 | Frozen | [PHASE_105.md](archive/phases/PHASE_105.md) | `a4d719e`  |
| Phase 106 | Frozen | [PHASE_106.md](archive/phases/PHASE_106.md) | `67c879d`  |
| Phase 107 | Frozen | [PHASE_107.md](archive/phases/PHASE_107.md) | `c157a28`  |
| Phase 108 | Frozen | [PHASE_108.md](archive/phases/PHASE_108.md) | `19b029d`  |
| Phase 109 | Frozen | [PHASE_109.md](archive/phases/PHASE_109.md) | `2e8eff3`  |
| Phase 110 | Frozen | [PHASE_110.md](archive/phases/PHASE_110.md) | `d1f19f1`  |
| Phase 111 | Frozen | [PHASE_111.md](archive/phases/PHASE_111.md) | `1bbe8fa`  |
| Phase 112 | Frozen | [PHASE_112.md](archive/phases/PHASE_112.md) | `cb1cb89`  |
| Phase 113 | Frozen | [PHASE_113.md](archive/phases/PHASE_113.md) | `149e0c4`  |
| Phase 114 | Frozen | [PHASE_114.md](archive/phases/PHASE_114.md) | `acef4be`  |
| Phase 115 | Frozen | [PHASE_115.md](archive/phases/PHASE_115.md) | `888d6bb`  |
| Phase 116 | Frozen | [PHASE_116.md](archive/phases/PHASE_116.md) | `4ed30d6`  |
| Phase 117 | Frozen | [PHASE_117.md](archive/phases/PHASE_117.md) | `1a0a4db`  |
| Phase 118 | Frozen | [PHASE_118.md](archive/phases/PHASE_118.md) | `bf259ad`  |
| Phase 119 | Frozen | [PHASE_119.md](archive/phases/PHASE_119.md) | `f6993d3`  |
| Phase 120 | Frozen | [PHASE_120.md](archive/phases/PHASE_120.md) | `25cbc56`  |
| Phase 121 | Frozen | [PHASE_121.md](archive/phases/PHASE_121.md) | `2c558dd`  |
| Phase 122 | Frozen | [PHASE_122.md](archive/phases/PHASE_122.md) | `1897d08`  |
| Phase 123 | Frozen | [PHASE_123.md](archive/phases/PHASE_123.md) | `c8f5cfd`  |
| Phase 124 | Frozen | [PHASE_124.md](archive/phases/PHASE_124.md) | `870efbc`  |
| Phase 125 | Frozen | [PHASE_125.md](archive/phases/PHASE_125.md) | `1f649b1`  |
| Phase 126 | Frozen | [PHASE_126.md](archive/phases/PHASE_126.md) | `59e34bb`  |
| Phase 127 | Frozen | [PHASE_127.md](archive/phases/PHASE_127.md) | `3dd222e`  |
| Phase 128 | Frozen | [PHASE_128.md](archive/phases/PHASE_128.md) | `c6f54b0`  |
| Phase 129 | Frozen | [PHASE_129.md](archive/phases/PHASE_129.md) | `95cdc26`  |
| Phase 130 | Frozen | [PHASE_130.md](archive/phases/PHASE_130.md) | `215734d`  |
| Phase 131 | Frozen | [PHASE_131.md](archive/phases/PHASE_131.md) | `6fb0f38`  |
| Phase 132 | Frozen | [PHASE_132.md](archive/phases/PHASE_132.md) | `904b67c`  |
| Phase 133 | Frozen | [PHASE_133.md](archive/phases/PHASE_133.md) | `0b8811c`  |
| Phase 134 | Frozen | [PHASE_134.md](archive/phases/PHASE_134.md) | `f290caa`  |
| Phase 135 | Frozen | [PHASE_135.md](archive/phases/PHASE_135.md) | `7fdadbd`  |
| Phase 136 | Frozen | [PHASE_136.md](archive/phases/PHASE_136.md) | `0157ce0`  |
| Phase 137 | Frozen | [PHASE_137.md](archive/phases/PHASE_137.md) | `9b6b8a1`  |
| Phase 138 | Frozen | [PHASE_138.md](archive/phases/PHASE_138.md) | `6bf36d6`  |
| Phase 139 | Frozen | [PHASE_139.md](archive/phases/PHASE_139.md) | `38734ec`  |
| Phase 140 | Frozen | [PHASE_140.md](archive/phases/PHASE_140.md) | `45ff66e`  |
| Phase 141 | Frozen | [PHASE_141.md](archive/phases/PHASE_141.md) | `a7e0440`  |
| Phase 142 | Frozen | [PHASE_142.md](archive/phases/PHASE_142.md) | `d46b43c`  |
| Phase 143 | Frozen | [PHASE_143.md](archive/phases/PHASE_143.md) | `182021d`  |
| Phase 144 | Frozen | [PHASE_144.md](archive/phases/PHASE_144.md) | `bb28671`  |
| Phase 145 | Frozen | [PHASE_145.md](archive/phases/PHASE_145.md) | `e0a0b81`  |
| Phase 146 | Frozen | [PHASE_146.md](archive/phases/PHASE_146.md) | `034ff01`  |
| Phase 147 | Frozen | [PHASE_147.md](archive/phases/PHASE_147.md) | `24ced96`  |
| Phase 148 | Frozen | [PHASE_148.md](archive/phases/PHASE_148.md) | `36b6158`  |
| Phase 149 | Frozen | [PHASE_149.md](archive/phases/PHASE_149.md) | `62b7a0c`  |
| Phase 150 | Frozen | [PHASE_150.md](archive/phases/PHASE_150.md) | `5945702`  |
| Phase 151 | Frozen | [PHASE_151.md](archive/phases/PHASE_151.md) | `10a711f`  |
| Phase 152 | Frozen | [PHASE_152.md](archive/phases/PHASE_152.md) | `1002330`  |
| Phase 153 | Frozen | [PHASE_153.md](archive/phases/PHASE_153.md) | `4410b17`  |
| Phase 154 | Frozen | [PHASE_154.md](archive/phases/PHASE_154.md) | `ae518a4`  |
| Phase 155 | Frozen | [PHASE_155.md](archive/phases/PHASE_155.md) | `ad84b49`  |
| Phase 156 | Frozen | [PHASE_156.md](archive/phases/PHASE_156.md) | `302c142`  |
| Phase 157 | Frozen | [PHASE_157.md](archive/phases/PHASE_157.md) | `16c666a`  |
| Phase 158 | Frozen | [PHASE_158.md](archive/phases/PHASE_158.md) | `dda59c2`  |
| Phase 159 | Frozen | [PHASE_159.md](archive/phases/PHASE_159.md) | `2fc8bca`  |
| Phase 160 | Frozen | [PHASE_160.md](archive/phases/PHASE_160.md) | `9f091ff`  |
| Phase 161 | Frozen | [PHASE_161.md](archive/phases/PHASE_161.md) | `7d8c293`  |
| Phase 162 | Frozen | [PHASE_162.md](archive/phases/PHASE_162.md) | `82bc141`  |
| Phase 163 | Frozen | [PHASE_163.md](archive/phases/PHASE_163.md) | `d7b7c24`  |
| Phase 164 | Frozen | [PHASE_164.md](archive/phases/PHASE_164.md) | `7a0de2d`  |
| Phase 165 | Frozen | [PHASE_165.md](archive/phases/PHASE_165.md) | `da63ace`  |
| Phase 166 | Frozen | [PHASE_166.md](archive/phases/PHASE_166.md) | `108bc1c`  |
| Phase 167 | Frozen | [PHASE_167.md](archive/phases/PHASE_167.md) | `75799bd`  |
| Phase 168 | Frozen | [PHASE_168.md](archive/phases/PHASE_168.md) | `92fd777`  |
| Phase 169 | Frozen | [PHASE_169.md](archive/phases/PHASE_169.md) | `e1c5bf4`  |
| Phase 170 | Frozen | [PHASE_170.md](archive/phases/PHASE_170.md) | `7bc29db`  |
| Phase 171 | Frozen | [PHASE_171.md](archive/phases/PHASE_171.md) | `03fad6f`  |
| Phase 172 | Frozen | [PHASE_172.md](archive/phases/PHASE_172.md) | `b0ca1f0`  |
| Phase 173 | Frozen | [PHASE_173.md](archive/phases/PHASE_173.md) | `36610d9`  |
| Phase 174 | Frozen | [PHASE_174.md](archive/phases/PHASE_174.md) | `01d5777`  |
| Phase 175 | Frozen | [PHASE_175.md](archive/phases/PHASE_175.md) | `5cc78eb`  |
| Phase 176 | Frozen | [PHASE_176.md](archive/phases/PHASE_176.md) | `2857c06`  |
| Phase 177 | Frozen | [PHASE_177.md](archive/phases/PHASE_177.md) | `3996fc1`  |
| Phase 178 | Frozen | [PHASE_178.md](archive/phases/PHASE_178.md) | `b3577e7`  |
| Phase 179 | Frozen | [PHASE_179.md](archive/phases/PHASE_179.md) | `0d369d1`  |
| Phase 180 | Frozen | [PHASE_180.md](archive/phases/PHASE_180.md) | `eec1893`  |
| Phase 181 | Frozen | [PHASE_181.md](archive/phases/PHASE_181.md) | `fc8eafe`  |
| Phase 182 | Frozen | [PHASE_182.md](archive/phases/PHASE_182.md) | `c600ab0`  |
| Phase 183 | Frozen | [PHASE_183.md](archive/phases/PHASE_183.md) | `b9e59d0`  |
| Phase 184 | Frozen | [PHASE_184.md](archive/phases/PHASE_184.md) | `ff0463d`  |
| Phase 185 | Frozen | [PHASE_185.md](archive/phases/PHASE_185.md) | _pending_  |
| Phase 186 | Frozen | [PHASE_186.md](archive/phases/PHASE_186.md) | `08d14e43`  |
| Phase 187 | Frozen | [PHASE_187.md](archive/phases/PHASE_187.md) | `5933a491`  |
| Phase 188 | Frozen | [PHASE_188.md](archive/phases/PHASE_188.md) | `7c05a9bc`  |
| Phase 189 | Frozen | [PHASE_189.md](archive/phases/PHASE_189.md) | `9c7e2c6f`  |
| Phase 190 | Frozen | [PHASE_190.md](archive/phases/PHASE_190.md) | `59834774`  |
| Phase 191 | Frozen | [PHASE_191.md](archive/phases/PHASE_191.md) | `68b15766`  |

Frozen means the phase doc is no longer edited except through commits
with a message starting `docs(phase-N):` — a convention, not an
enforcement, but it makes drift visible in `git log`.

**Planned phases** (6 and beyond) live in [`ROADMAP.md`](ROADMAP.md)
as one-paragraph intents rather than as separate PHASE_N.md files.
A phase gets its own doc only when it opens — that way there are no
stale task lists sitting in files for phases we haven't started yet.

## Amendment process

When a Phase N decision needs to override a `DESIGN.md` or
`PRODUCT.md` contract:

1. Create `docs/amendments/<date>-<short-slug>.md` — one file per
   amendment, describing what changed, why, and which `DESIGN.md`
   or `PRODUCT.md` section it supersedes (or adds, if it introduces
   a new commitment).
2. Edit the relevant section of `DESIGN.md` or `PRODUCT.md` to
   reference the amendment inline (blockquote-style pointer to the
   amendment file). For added sections (e.g. a new Product
   Commitment), the pointer appears at the head of the new section.
3. Commit the amendment and the contract edit together with message
   `docs(amendment): <slug>` for standalone amendment commits, or as
   a task inside a phase using `docs(phase-N): task M — Ax <slug>`.

The directory holds **ten amendments** as of Phase 56 (2026-05-12):
A1–A4 (Phase 22 — daemon IPC, mission state machine, capability
taxonomy, workspace layout), A5 (Phase 38 — substrate tool count
7→8), A6 (Phase 40 — parallel tool execution), A7 (Phase 41 —
protocol negotiation), A8 (Phase 56 — PRODUCT.md pitch reframe),
A9 (Phase 56 — P13 Assistant Profile), A10 (Phase 56 — P14
Persona).
