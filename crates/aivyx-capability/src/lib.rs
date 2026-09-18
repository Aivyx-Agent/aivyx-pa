//! # aivyx-capability
//!
//! Capability-based security for Aivyx. Defines the `Scope` type, the
//! `CapabilitySet`, the `TrustTier` enum, and the attenuation rules
//! that let trust tiers cap what an agent can do per turn.
//!
//! See DESIGN.md Deliverable 4 (capability taxonomy) and Deliverable 5
//! (trust tier model) for the locked design this crate implements.
//!
//! ## Key rule
//!
//! Effective capabilities for a turn are computed as:
//! `effective = agent.capabilities().intersect(tier.default_ceiling())`.
//! This intersection happens **once per turn**, before any LLM call.

use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Scope registry — the v1 active namespace (35 scopes).
// ---------------------------------------------------------------------------

/// The v1 active scope bases. `Scope::parse` rejects anything not in this
/// set, so unknown scopes fail at parse time, not check time.
///
/// Phase 11 Task 4 adds `tool.allowlist` as a **synthetic dispatch-layer
/// base**: the turn loop synthesizes `tool.allowlist:<tool_name>` scopes
/// to represent role-allowlist rejections and routes them through
/// `ToolOutcome::Denied { scope, held }` unchanged. Auditors distinguish
/// "capability denial" from "role-allowlist denial" by reading
/// `scope_requested.base()`. No `TrustTier` ceiling includes
/// `tool.allowlist` — it exists as a parseable label only; the scope
/// gate never holds it.
const KNOWN_BASES: &[&str] = &[
    // fs
    "fs.read",
    "fs.write",
    "fs.delete",
    "fs.metadata",
    // workspace — Chapter O. The agent's own private workspace dir
    // (separate from `fs.*` / `fs_root`). One base shared by all the
    // `workspace.*` tools (read/write/list/delete/note): it is the
    // agent's own contained notebook, so per-op granularity isn't
    // needed — you grant the workspace or you don't. Qualifier is the
    // canonical path under the workspace root.
    "workspace",
    // net
    "net.fetch",
    "net.post",
    "net.dns",
    // shell
    "shell.exec",
    "shell.spawn",
    // git — Amendment A12 (Phase 109). Read-only git repo
    // inspection. Qualifier is the canonical repo path; the
    // tools (`git.status` + `git.diff`) check it against
    // the operator's configured `[git] repos` list. One base
    // shared by both read tools by design (read-invariant
    // grouping; see A12 amendment doc for the rationale).
    "git.read",
    // git write — Chapter Forge (FG.2). The destructive sibling
    // A12 anticipated ("a future destructive git tool would
    // warrant a separate `git.write` base"). Gates the
    // `git.commit` tool (stage + commit inside an allowed repo).
    // Qualifier is the canonical repo path, checked against the
    // same operator `[git] repos` allow-set as `git.read`.
    // Trusted-tier only — present in `CEILING_TRUSTED` but not
    // `CEILING_SEMITRUSTED`: writing history is at least as
    // sensitive as `shell.exec` / `fs.delete`, so a remote
    // adapter must not hold it by default. Confirm-first at the
    // tool level when `[access] confirm_destructive` is on.
    "git.write",
    // llm
    "llm.call",
    "llm.embed",
    // memory
    "memory.read",
    "memory.write",
    "memory.forget",
    "memory.gc",
    // channel
    "channel.send",
    "channel.receive",
    // audit
    "audit.read",
    // config
    "config.read",
    "config.write",
    // role allowlist (synthetic — Phase 11 Task 4)
    "tool.allowlist",
    // email (Phase 123 — Chapter F #1, Gmail third-party
    // tool process per P10/P11/P12). Three bases mirroring the
    // Gmail OAuth scope hierarchy:
    //   email.read   — gmail.search, gmail.read
    //   email.write  — gmail.draft (creates a draft only)
    //   email.send   — gmail.send (Trusted-tier only by default,
    //                  matching shell.exec / notify.send gating).
    // Only the Trusted ceiling carries any of these; SemiTrusted
    // and Untrusted operators reading personal inboxes through
    // a remote adapter would cross the same trust boundary
    // notify.send guards against (Phase 62 Q2(a)).
    "email.read",
    "email.write",
    "email.send",
    // Personal assistant tool bundle (Phase 125 — Chapter G #1,
    // aivyx-toolkit third-party tool process). Five bases for
    // the bundled tool surface:
    //   web.search   — web.search tool (Brave Search API).
    //   task.read    — task.list.
    //   task.write   — task.create, task.complete, task.delete.
    //   health.read  — health.check.list,
    //                  health.check.recent_changes.
    //   health.write — health.check.add.
    // All five Trusted-tier only by default (same gating pattern
    // as email.*: personal-assistant tools shouldn't be reachable
    // from remote channels without explicit role grant per
    // Phase 62 Q2(a)).
    "web.search",
    "task.read",
    "task.write",
    "health.read",
    "health.write",
    // Phase 143 — Chapter G #2 (budget tracking,
    // aivyx-toolkit). Read+write split matches the
    // task.* / health.* shape:
    //   budget.read  — budget.summary.
    //   budget.write — budget.record.
    // Trusted-only default at the ceiling (personal-
    // finance data shouldn't leak through remote
    // channels without explicit operator grant —
    // same gating pattern as email.* / notify.send /
    // every other Chapter F/G third-party-tool-
    // process surface).
    "budget.read",
    "budget.write",
    // Chapter Abacus (AB.1) — pure-compute utilities pack in the
    // aivyx-toolkit tool process. Unlike every other toolkit base
    // above, these are side-effect-free and offline (no network, no
    // filesystem, no operator data), so they are the first toolkit
    // surface gated *below* Trusted: present in CEILING_SEMITRUSTED
    // (and therefore reachable at Trusted/Kernel too). See
    // docs/ABACUS.md §2.
    //   calc.eval     — calc.eval (arithmetic expression evaluator).
    //   convert.units — convert.units + convert.time (the convert
    //                   group; one base, AB.2).
    //   date.compute  — date.diff + date.add (the date group; one
    //                   base, AB.3).
    "calc.eval",
    "convert.units",
    "date.compute",
    // Calendar (Phase 128 — Chapter F #2, aivyx-calendar
    // third-party tool process). Two bases for the
    // five-tool surface (Q3b operator-picked):
    //   calendar.read  — calendar.list_events,
    //                    calendar.get_event.
    //   calendar.write — calendar.create_event,
    //                    calendar.update_event,
    //                    calendar.delete_event
    //                    (Trusted-tier only by default,
    //                    matching the email.* pattern
    //                    Phase 123 established for
    //                    third-party-tool-process write
    //                    surfaces).
    "calendar.read",
    "calendar.write",
    // Drive (Phase 129 — Chapter F #3, aivyx-drive
    // third-party tool process). Two bases for the
    // seven-tool surface (Q2b operator-picked over
    // Q2a's 5-tool default):
    //   drive.read  — drive.search, drive.get_metadata,
    //                 drive.list_folder,
    //                 drive.download_file.
    //   drive.write — drive.create_folder,
    //                 drive.upload_file,
    //                 drive.delete_file
    //                 (Trusted-tier only by default,
    //                 matching the email.* / calendar.*
    //                 third-party-tool-process gating
    //                 pattern).
    "drive.read",
    "drive.write",
    // Notion (Phase 130 — Chapter F #5, aivyx-notion
    // third-party tool process). Two bases for the
    // seven-tool surface (Q1a):
    //   notion.read  — notion.search,
    //                  notion.get_page,
    //                  notion.list_database.
    //   notion.write — notion.create_page,
    //                  notion.append_blocks,
    //                  notion.update_page_properties,
    //                  notion.archive_page
    //                  (Trusted-tier only by default,
    //                  matching the Chapter F write-tool
    //                  gating pattern).
    "notion.read",
    "notion.write",
    // Obsidian (Phase 130 — Chapter F #6, aivyx-obsidian
    // third-party tool process). Two bases for the
    // six-tool surface (Q2a):
    //   obsidian.read  — obsidian.search,
    //                    obsidian.get_note,
    //                    obsidian.list_folder.
    //   obsidian.write — obsidian.create_note,
    //                    obsidian.update_note,
    //                    obsidian.delete_note
    //                    (Trusted-tier only by default).
    "obsidian.read",
    "obsidian.write",
    // n8n (Phase 131 — Chapter F #7, aivyx-n8n
    // third-party tool process). Two bases for the
    // ten-tool surface (Q1c — operator picked over the
    // Recommended Q1b 7-tool default; n8n.create_workflow,
    // n8n.update_workflow, and n8n.delete_workflow ship
    // with the same Trusted-gating policy as every other
    // Chapter F write base):
    //   n8n.read  — n8n.list_workflows, n8n.get_workflow,
    //               n8n.list_executions, n8n.get_execution.
    //   n8n.write — n8n.execute_workflow,
    //               n8n.activate_workflow,
    //               n8n.deactivate_workflow,
    //               n8n.create_workflow,
    //               n8n.update_workflow,
    //               n8n.delete_workflow
    //               (Trusted-tier only by default).
    "n8n.read",
    "n8n.write",
    // Contacts (Chapter Contacts — aivyx-contacts third-party
    // tool process, the fifth Google integration). First
    // Broaden-track domain (closes the contacts/CRM slice of
    // backend-audit F4). Two bases for the six-tool People API
    // surface (3 read / 3 write, mirroring drive.*):
    //   contacts.read  — contacts.search, contacts.list,
    //                    contacts.get.
    //   contacts.write — contacts.create, contacts.update,
    //                    contacts.delete (contacts.delete is
    //                    irreversible — confirm-first per the
    //                    Documents-delete policy; Trusted-tier
    //                    only by default, matching the
    //                    email.* / calendar.* / drive.*
    //                    third-party-tool-process gating).
    "contacts.read",
    "contacts.write",
    // Open desktop applications (Chapter Deckhand — aivyx-apps
    // third-party tool process, opt-in via `[applications]`). Lets the
    // agent use the GUI apps already open on the operator's own hardware.
    // Three bases (observe / manage-windows / inject-input):
    //   app.read    — app.list (enumerate open windows),
    //                 app.screenshot (capture a window/screen).
    //   app.control — app.focus (raise/focus a window — reversible).
    //   app.input   — app.type / app.key / app.click (inject keystrokes /
    //                 clicks into the focused app — IRREVERSIBLE, confirm-first
    //                 via IRREVERSIBLE_BASES).
    // All three are Trusted-tier ONLY (in CEILING_TRUSTED, absent from
    // CEILING_SEMITRUSTED, like shell.exec / git.write): driving the GUI apps
    // on the operator's own machine reaches the whole desktop and can't be
    // sandboxed, so a remote/SemiTrusted adapter must never hold any of them.
    // Even `app.read` exposes whatever is on screen.
    "app.read",
    "app.control",
    "app.input",
    // mission (Phase 21 — PRODUCT.md P2, Phase 28 — list/status)
    "mission.create",
    "mission.gate",
    "mission.list",
    "mission.status",
    // Phase 14 Task 2 — Sub-Agent Role-Switching (PRODUCT.md P1).
    // `role.switch` gates the `role.switch` tool that opens a
    // bounded sub-session under a child role's attenuated
    // envelope. The qualifier (if present) is a role-name string
    // identifying the switch target: `role.switch:researcher`
    // grants switching into the `researcher` role specifically,
    // while unqualified `role.switch` is the "any target"
    // wildcard (subject to parent-chain attenuation — the
    // assemble_role_envelope walker ensures a child cannot
    // switch into a role the parent chain did not transitively
    // grant it).
    //
    // **Dispatch shape.** Role names are bare identifiers — no
    // `/`, no `://`, no `,` — so under `QualifierKind::of` they
    // fall through to `SimpleGlob`, which delegates to
    // `glob_matches`. For a glob-metacharacter-free needle like
    // `"researcher"`, `glob_matches("researcher", "researcher")`
    // is the degenerate exact-string-equality case. **No new
    // `QualifierKind` variant is needed**; the existing dispatch
    // already produces the right semantics. The Phase 14 plan
    // (PHASE_14.md Task 2 cut) anticipated a new qualifier kind;
    // the implementation correction is that the existing
    // `SimpleGlob` arm already matches role-name identifiers as
    // exact strings. This lets Task 2 ship a new scope base
    // without touching `QualifierKind` at all.
    //
    // **Wildcard form.** `role.switch:*` is deliberately
    // rejected by `Scope::parse` — the unqualified form already
    // is the wildcard under Rule 2 (held unqualified grants
    // anything same-base), so a literal `:*` qualifier is
    // redundant at best and ambiguous at worst (would it mean
    // "any role named `*`" or "any role"?). Reject at parse
    // time, not check time, per the v1 scope registry rule.
    "role.switch",
    // schedule (Phase 26 — PRODUCT.md G5)
    "schedule.create",
    "schedule.list",
    "schedule.delete",
    "schedule.update",
    // webhook (Phase 27 — PRODUCT.md G5 completion)
    "webhook.create",
    "webhook.list",
    "webhook.delete",
    // file_watch (Phase 27 — PRODUCT.md G5 completion)
    "file_watch.create",
    "file_watch.list",
    "file_watch.delete",
    // MCP bridge (Phase 23 Task 3 — PRODUCT_ROADMAP.md MCP Integration).
    // `mcp.call` gates invocation of tools discovered from MCP servers.
    // Qualifier format: `<server_name>:<tool_name>` — e.g.,
    // `mcp.call:github:create_issue`. Unqualified `mcp.call` grants
    // all MCP tools (Rule 2). Uses `SimpleGlob` dispatch (same as
    // `role.switch`), so `mcp.call:github:*` grants all tools on the
    // `github` server.
    "mcp.call",
    // reflection (Phase 29 — PRODUCT.md G3/P8)
    "reflection.propose",
    "reflection.apply",
    // persona proposal (Phase 59 — PRODUCT.md P14). Granted alongside
    // reflection.propose for roles authorized to extend the agent's
    // identity layer. Without this scope, a role's
    // reflection.propose calls cannot include `persona_deltas` —
    // mismatches are rejected at the per-tool gate. Gate-side
    // approval still goes through P2's mission machinery.
    "persona.propose",
    // Phase 110 — Skills Auto-Creation. `skills.propose` is the
    // sibling capability gate to `persona.propose` for proposals
    // whose `persona_deltas` array contains any
    // `PersonaDeltaCategory::LearnedSkill` entry. The dispatch
    // in `reflection.propose` checks `persona.propose` for
    // non-Skill categories and `skills.propose` for Skill
    // categories; mixed proposals require both. Q2(b) at Phase
    // 110 sign-off — operator picked per-category granularity
    // over the Q2(a) "extend persona.propose" recommendation.
    "skills.propose",
    // Phase 110 — `skills.list` and `skills.invoke` substrate
    // tools. Read-only enumeration (skills.list) and on-demand
    // procedure rendering (skills.invoke) of the operator-
    // approved skill set.
    "skills.list",
    "skills.invoke",
    // Chapter Lattice — `graph.read` gates the `graph.query`
    // tool: a read-only multi-hop traversal of the agent's own
    // typed knowledge graph (entities + directed relations
    // extracted from memory). Infrastructure, not substrate —
    // the agent querying its OWN derived self-knowledge, like
    // `skills.list` / `audit.read` — so it grows `KNOWN_BASES`
    // without a P10 substrate-count amendment (the graph is
    // derived from memory, not a new operator-owned resource).
    // Trusted-tier only (reflection-layer read).
    "graph.read",
    // Phase 184 — Conversational skill-teaching. `skills.write`
    // gates the operator-authored edit tools (`skills.teach` /
    // `skills.update` / `skills.forget`) that append LearnedSkill
    // deltas to the Persona chain after the agent confirms the
    // drafted skill with the operator. Channel-tier, Trusted-tier
    // (a remote adapter must not edit the skill set — it is
    // identity). Distinct from `skills.propose` (the gated
    // reflection path).
    "skills.write",
    // role mutation (Phase 30 — PRODUCT.md P8 completion)
    "role.update",
    // ollama model management (Phase 36 — local LLM story completion)
    "ollama.list",
    "ollama.show",
    "ollama.pull",
    // Phase 62 Task 2 — Agent-Initiated Outbound Notifications
    // (Reach Milestone, first phase past the closed
    // forward-commitment ledger). `notify.send` gates the
    // `notify.send` infrastructure tool that pushes a message to
    // an operator-configured `[[notify_target]]` (Telegram chat
    // or generic webhook URL). The qualifier (if present) is a
    // target-name string identifying which configured target the
    // role may reach: `notify.send:phone` grants pushing to the
    // `phone` target specifically, while unqualified
    // `notify.send` is the "any configured target" wildcard.
    //
    // **Dispatch shape.** Target names are bare identifiers
    // (the same shape as role names — no `/`, no `://`, no `,`),
    // so under `QualifierKind::of` they fall through to
    // `SimpleGlob`, which collapses to exact-string equality
    // for metacharacter-free needles. No new `QualifierKind`
    // variant is needed; the `role.switch` precedent applies.
    //
    // **Wildcard form.** `notify.send:*` is rejected at parse
    // time alongside `role.switch:*` per the same rationale —
    // the unqualified form already is the wildcard under Rule 2.
    //
    // **Tier restriction.** Present only in `CEILING_TRUSTED`
    // (Q2(a) at sign-off). SemiTrusted roles cannot notify
    // because notifications can leak data across trust
    // boundaries (a SemiTrusted Telegram operator must not be
    // able to coerce the agent into POSTing the address book to
    // a webhook). Relaxation to SemiTrusted is a Phase 63+
    // consideration if real use surfaces.
    "notify.send",
    // Phase 173 — Autonomous Loop (the Aivyx Ralph loop).
    // `loop.next` gates reading the next pending backlog story;
    // `loop.complete` gates marking a story Done. Both are
    // channel-tier substrate tools (like `mission.*`), granted
    // to the trust tier the loop driver's `TriggerSource::Loop`
    // turns run under (Trusted, like reflection). The
    // thirteen-tool substrate core (amendment A12) is untouched.
    "loop.next",
    "loop.complete",
    // Phase 175 — Loop progress log. `loop.note` gates appending
    // a learning to the reserved progress topic the driver
    // injects into each fresh iteration. Channel-tier, Trusted,
    // like the other loop tools.
    "loop.note",
    // Phase 183 — Reminders (everyday-PA breadth #1). `remind.read`
    // gates `remind.list`; `remind.write` gates `remind.set` /
    // `remind.cancel`. Channel-tier, Trusted, like the loop tools;
    // the reminder driver fires due reminders through the notify
    // dispatcher. The thirteen-tool substrate core is untouched.
    "remind.read",
    "remind.write",
    // Kitchen / Back-of-House — the first Aivyx **vertical pack**
    // (see docs/VERTICAL_PACKS.md). `kitchen.read` gates the read +
    // compute tool surface of the `aivyx-kitchen` tool process
    // (inventory / recipe / supplier / PO / alert reads over the
    // KitchenDB RPCs, plus the pure `kitchen.recipe.scale`).
    // Trusted-tier-only by default — same gating pattern as every
    // other third-party-tool-process surface (email.* / web.search /
    // drive.* …). The gated write bases (`kitchen.write`,
    // `kitchen.order.send`, `kitchen.haccp.log`) land with later
    // vertical-pack phases.
    "kitchen.read",
    // Kitchen vertical pack — gated write surface. `kitchen.write`
    // gates inventory counts / adjustments (and, later, production
    // batch lifecycle); `kitchen.order.send` gates dispatching a
    // purchase order to a supplier — money leaves the building, so it
    // is *additionally* confirm-first at the tool level (the
    // `skills.teach` `confirmed: true` pattern). Both Trusted-tier-only
    // at the ceiling, like every other mutating third-party surface.
    "kitchen.write",
    "kitchen.order.send",
    // Kitchen vertical pack — food-safety (HACCP) logging. Distinct
    // from `kitchen.write`: `kitchen.haccp.log` gates **append-only**
    // food-safety records (temperature checks, corrective actions,
    // cleaning / allergen / use-by). The compliance wedge — each call
    // lands on the tamper-evident HMAC audit chain (tool id, scope,
    // input hash, time, outcome), so the food-safety log is
    // cryptographically ordered + non-repudiable. Ungated (logging a
    // fridge temp must be friction-free) but immutable by construction.
    // Trusted-tier-only at the ceiling; operators can grant it to a
    // SemiTrusted line role (logging from the pass) via
    // `capability_scopes`.
    "kitchen.haccp.log",
    // The Nonagon multi-agent chapter (docs/NONAGON.md). `team.delegate`
    // is the *lead's* orchestration authority — the scope `delegate_task`
    // / `query_agent` require. It is deliberately NOT inherited by
    // specialists (they don't declare it, so the lead→specialist
    // attenuation drops it): a specialist cannot convene its own team.
    "team.delegate",
    // `team.message` is the team *dialogue* scope — held by EVERY member
    // (lead + specialists) so peers can message each other on the bus.
    // Distinct from `team.delegate`: a specialist may talk, but not convene
    // its own team.
    "team.message",
    // Chapter L (L.7) — `team.run` gates the daemon-side `team.run` tool:
    // delegating a free-text goal to a **durable** daemon team mission
    // (decomposed + checkpoint/resume-driven, gate-pausable, shown in the TUI
    // Missions panel). It lets an autonomous-loop iteration hand a large story
    // to a team rather than implementing it single-handed. Channel-tier,
    // Trusted (like the loop tools); distinct from `team.delegate` (the
    // in-assembly lead authority) — `team.run` *starts a whole mission*.
    "team.run",
    // team.run.channel — Chapter (Piece C, 2026-08-23). Narrow,
    // channel-only sibling to team.run: starts a new Nonagon team
    // mission from a chat command (`/team run <goal>`), never from
    // the model. Deliberately absent from every real trust-tier
    // ceiling (Trusted included) — this base is never granted via
    // the normal CapabilitySet/TrustTier intersection at all.
    // Authorization is a bespoke, daemon-side, per-channel-type
    // config check (see `daemon_server.rs`'s `ChannelTriggerAuthz`),
    // since the trust-tier ceiling mechanism has no per-channel-type
    // granularity to hang this on (every channel type hardcodes the
    // same SemiTrusted tier). Present in KNOWN_BASES purely for
    // audit-trail/drift-guard consistency with every other gated
    // capability surface in this codebase.
    "team.run.channel",
    // Phase 191 — Daemon-side automatic alert dispatch. `notify.dispatch`
    // gates a tool process's *own* ability to push a notification through
    // the daemon's `NotifyDispatcher` (e.g. a toolkit watcher detecting a
    // low-stock or overdue-order condition and alerting the operator
    // without a model round-trip). Distinct from `notify.send`: that one
    // gates the model-invoked `notify.send` infrastructure tool; this one
    // gates the daemon-side sink a tool process's `DispatchNotification`
    // wire frame is routed through. Unlike `notify.send`, this base is
    // currently unqualified-only (no per-target qualifier) — the
    // configured tool process has exactly one default notify target for
    // this phase (per-watcher targets are explicitly out of scope; see
    // Task 3/4 of the Phase 191 plan). Trusted-tier only by default,
    // matching `notify.send`'s own tier restriction (same data-exfil
    // rationale: a SemiTrusted tool process must not be able to push
    // arbitrary content to an operator-configured target).
    "notify.dispatch",
    // Aivyx-Vision Milestone 1 (2026-09-18) — vision.generate_svg tool
    // process. One base for the one tool this milestone adds; later
    // milestones' vision.generate_image / vision.generate_3d tools will
    // share this same base (nothing to read separately from what's
    // generated). Reachable at SemiTrusted: narrower and safer than
    // llm.call (constrained prompt, sanitized output), which is itself
    // already SemiTrusted-reachable. See aivyx-ecosystem/docs/superpowers/
    // specs/2026-09-18-aivyx-vision-v1-design.md.
    "vision.generate",
];

/// The capability bases whose actions are **irreversible, outbound, or
/// authority-changing** — deletion, arbitrary process execution, outbound
/// network/money, history rewrites, and self-governance. Chapter Reins (RN.3).
///
/// This is the **structural backstop** for the bounded `AutoApprove` posture
/// (`docs/AUTONOMY.md` §5.1): an unattended run may auto-approve a *reversible*
/// escalation that is on the operator's allowlist, but a scope whose base is in
/// this set must **never** auto-proceed — regardless of any allowlist. The list
/// is consulted alongside the allowlist, not instead of it; the allowlist is
/// already deny-by-default (an unlisted base never auto-approves), so this set
/// is the second, non-bypassable gate on the dangerous bases specifically.
///
/// Deliberately conservative, and deliberately **not** including ordinary
/// `fs.write`: writes within `fs_root` are an agent's bread-and-butter and the
/// checkpoint/rollback primitive (RN.5.2) is their intended safety net —
/// treating every write as irreversible would make `autonomous` useless. The
/// line is delete / exec / outbound / history / governance, not "any write".
const IRREVERSIBLE_BASES: &[&str] = &[
    "fs.delete",          // deletion
    "shell.exec",         // arbitrary command execution
    "shell.spawn",        // long-running process spawn
    "net.post",           // outbound HTTP (data / money)
    "git.write",          // rewrites version history
    "app.input",          // injects keystrokes/clicks into the live desktop
    "kitchen.order.send", // money leaves the building
    // Self-governance — already Kernel-tier (unreachable from an agent turn),
    // listed for defense-in-depth so no future wiring can auto-approve them.
    "config.write",
    "role.update",
    "role.switch",
    "tool.allowlist",
];

/// Whether a capability `base` names an irreversible / outbound /
/// authority-changing action that bounded `AutoApprove` must never auto-proceed
/// (Chapter Reins RN.3). Pure over the base string; callers pass
/// [`Scope::base`]. Unknown bases return `false` — they are gated by the
/// allowlist's deny-by-default instead, so an unclassified base still never
/// auto-approves.
pub fn is_irreversible_base(base: &str) -> bool {
    IRREVERSIBLE_BASES.contains(&base)
}

/// Third-party/OAuth integration write/send/delete/archive bases —
/// Task 4 (HIGH security fix, 2026-09-16 audit). Derived from the real
/// `Scope::parse(...)` call sites in `aivyx-gmail`, `aivyx-drive`,
/// `aivyx-notion`, `aivyx-obsidian`, `aivyx-n8n`, `aivyx-contacts`, and
/// `aivyx-calendar` — every base one of those crates' write/send/delete/
/// archive tools declares as its `required_scope()`. Deletion and
/// archival actions in these integrations share their service's single
/// `.write` base (e.g. `drive.delete_file` needs `drive.write`, same as
/// `drive.create_folder`) rather than a separate `.delete` base, so this
/// list has one entry per service except Gmail (which splits `email.write`
/// — drafts — from `email.send` — immediate, irreversible delivery).
///
/// Two independent consumers share this single list so it cannot drift
/// between them (mirrors the `IRREVERSIBLE_BASES` pattern just above):
/// `aivyx-tool::ToolProxy::auto_grantable_in_backcompat_floor` (withhold
/// these from the operator's backcompat floor grant — an operator must
/// explicitly declare them in a role's `capability_scopes`) and
/// `aivyx-core::agent`'s turn loop (even once explicitly granted, gate
/// each call behind `[access] confirm_destructive`).
const WITHHELD_INTEGRATION_BASES: &[&str] = &[
    "email.write",
    "email.send",
    "drive.write",
    "notion.write",
    "obsidian.write",
    "n8n.write",
    "contacts.write",
    "calendar.write",
];

/// Whether a capability `base` names a third-party/OAuth integration
/// write/send/delete/archive action that must stay withheld from the
/// backcompat floor grant and, once explicitly granted, still needs a
/// per-call `[access] confirm_destructive` gate. See
/// [`WITHHELD_INTEGRATION_BASES`]. Unknown bases return `false` — same
/// deny-by-default posture as [`is_irreversible_base`].
pub fn is_withheld_integration_base(base: &str) -> bool {
    WITHHELD_INTEGRATION_BASES.contains(&base)
}

// ---------------------------------------------------------------------------
// Scope
// ---------------------------------------------------------------------------

/// A capability scope.
///
/// Hierarchical string form: `base` or `base:qualifier`. The base is a dotted
/// identifier drawn from `KNOWN_BASES`; the qualifier is an optional free-form
/// attenuation string whose semantics are determined by the *needed* scope's
/// shape at check time (see `QualifierKind`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Scope(String);

impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Scope {
    /// Parse a scope string. Returns `None` if the base is not a known v1
    /// scope, per D4: "unknown scopes fail at parse time, not check time."
    pub fn parse(s: &str) -> Option<Self> {
        let (base, qualifier) = match s.find(':') {
            Some(idx) => (&s[..idx], Some(&s[idx + 1..])),
            None => (s, None),
        };
        if !KNOWN_BASES.contains(&base) {
            return None;
        }
        // Phase 14 Task 2 — `role.switch:*` is rejected at parse
        // time. The unqualified form (`role.switch` with no
        // qualifier) already is the wildcard under Rule 2, so a
        // literal `:*` qualifier is redundant and ambiguous. See
        // the `role.switch` entry in `KNOWN_BASES` for the full
        // rationale.
        //
        // Phase 62 Task 2 extends the same restriction to
        // `notify.send:*` for the same reason — the unqualified
        // form already grants "any configured target" under
        // Rule 2; a literal `:*` qualifier is redundant and
        // ambiguous (would it mean "any target named `*`" or
        // "any target"?). The list will likely grow with each
        // new target-name-qualified base added; the structural
        // pattern is "bases whose qualifier is an external-name
        // identifier reject literal `:*`."
        if qualifier == Some("*")
            && (base == "role.switch" || base == "notify.send")
        {
            return None;
        }
        Some(Scope(s.to_string()))
    }

    /// The base portion (everything before the first `:`, or the whole string).
    pub fn base(&self) -> &str {
        match self.0.find(':') {
            Some(idx) => &self.0[..idx],
            None => &self.0,
        }
    }

    /// The qualifier portion (everything after the first `:`), if any.
    pub fn qualifier(&self) -> Option<&str> {
        self.0.find(':').map(|idx| &self.0[idx + 1..])
    }

    /// The full string form, useful for display and error messages.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Prefix-attenuation check per D4 rules 1–4: `true` iff `self` (the
    /// needed scope) is granted by `other` (a held capability).
    pub fn is_granted_by(&self, other: &Scope) -> bool {
        // Rule 1: bases must match exactly.
        if self.base() != other.base() {
            return false;
        }

        match (self.qualifier(), other.qualifier()) {
            // Held unqualified grants anything with the same base (rule 2).
            (_, None) => true,

            // Rule 4: qualified held cannot grant unqualified needed.
            (None, Some(_)) => false,

            // Rule 3: both qualified — dispatch by the *needed* qualifier's
            // shape so the tool's declared intent drives matching semantics.
            // Exception: allowlists live on the *held* side by convention
            // (`shell.exec:git,ls,cat` grants `shell.exec:git`), so a comma
            // on either side flips us into allowlist mode.
            (Some(needed_q), Some(held_q)) => {
                QualifierKind::of(needed_q, held_q).check(needed_q, held_q)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// QualifierKind — how to interpret a qualifier string.
// ---------------------------------------------------------------------------

/// Determines how to compare a needed qualifier against a held one.
///
/// Dispatch order (per D4 rule 3 and the decision in PHASE_1.md):
/// URL → path → allowlist → simple-glob. Dispatch is driven by the *needed*
/// scope's qualifier shape so that the tool's declared intent determines
/// matching semantics.
#[derive(Debug, Clone, Copy)]
enum QualifierKind {
    /// Contains `://` — URL prefix match.
    UrlPrefix,
    /// Contains `/` (and no `://`) — glob semantics, `**` crosses `/`.
    PathGlob,
    /// Contains `,` — comma-separated allowlist; needed set must be a subset
    /// of held set.
    Allowlist,
    /// Otherwise — simple glob against the whole string.
    SimpleGlob,
}

impl QualifierKind {
    /// Classify the qualifier pair.
    ///
    /// Dispatch rules, in order:
    /// 1. `://` on the *needed* side → URL prefix (URLs are almost always
    ///    declared by the tool, not the capability set).
    /// 2. `/` on *either* side → path glob. Paths always win over allowlist
    ///    so that brace alternation like `/home/{julian,root}/**` is
    ///    correctly treated as a path despite containing a comma.
    /// 3. `,` on either side → allowlist (allowlists live on the held side
    ///    by convention: `shell.exec:git,ls,cat` grants `shell.exec:git`).
    /// 4. Otherwise → simple glob against the whole string.
    fn of(needed_q: &str, held_q: &str) -> Self {
        if needed_q.contains("://") {
            QualifierKind::UrlPrefix
        } else if needed_q.contains('/') || held_q.contains('/') {
            QualifierKind::PathGlob
        } else if needed_q.contains(',') || held_q.contains(',') {
            QualifierKind::Allowlist
        } else {
            QualifierKind::SimpleGlob
        }
    }

    /// Returns `true` iff `held_q` grants `needed_q` under this kind.
    fn check(self, needed_q: &str, held_q: &str) -> bool {
        match self {
            QualifierKind::UrlPrefix => url_prefix_grants(held_q, needed_q),
            QualifierKind::PathGlob => glob_matches(held_q, needed_q),
            QualifierKind::Allowlist => {
                let held: Vec<&str> = held_q.split(',').map(str::trim).collect();
                needed_q
                    .split(',')
                    .map(str::trim)
                    .all(|item| held.contains(&item))
            }
            QualifierKind::SimpleGlob => glob_matches(held_q, needed_q),
        }
    }
}

/// Decide whether a held URL-prefix qualifier grants a needed one.
///
/// ## Why not `starts_with`
///
/// Naive raw-byte prefix matching admits the classic attack:
/// held `https://example.com/` would grant needed
/// `https://example.com.evil.com/` because the needed string
/// literally starts with the held string. Phase 12 Task 2 replaces
/// the byte-prefix with an origin-aware matcher: the (scheme, host,
/// port) origin of `held` and `needed` must match exactly, and
/// `needed`'s path must start at a **component boundary** under
/// `held`'s path.
///
/// ## Rules
///
/// 1. **Origin match** — scheme, host, and port must be equal. A
///    missing port in either URL is normalized to its scheme's
///    default (80 for `http`, 443 for `https`). The host compare
///    is ASCII-lowercase (RFC 3986 §3.2.2: hosts are
///    case-insensitive).
/// 2. **Path prefix** — after trimming any trailing `/`, the held
///    path must equal `needed`'s path (exact match) OR the needed
///    path must start with `held + "/"` (subpath match). This
///    rejects `/users2/` under a held `/users` grant.
/// 3. **Query and fragment are ignored.** A held prefix grants any
///    query string; authorization is on the resource, not on how
///    it is parametrized.
///
/// ## Non-goals
///
/// - Not a general URL parser. This is a minimal scope-matching
///   primitive — no percent-decoding, no IDN, no userinfo. If an
///   attacker can insert `@` in the authority they've already
///   gotten past upstream input validation.
/// - Not `url`-crate-backed. Adding a new workspace dep just for
///   scope matching would break the zero-new-dep streak the
///   project has held since Phase 1. Every piece of the matcher
///   is `&str` arithmetic.
///
/// Returns `false` if either side fails to parse — a malformed
/// qualifier cannot grant anything, same invariant as
/// `glob_matches`.
fn url_prefix_grants(held: &str, needed: &str) -> bool {
    let Some(h) = parse_scope_url(held) else {
        return false;
    };
    let Some(n) = parse_scope_url(needed) else {
        return false;
    };
    if h.scheme != n.scheme || h.host != n.host || h.port != n.port {
        return false;
    }
    // Path-prefix on component boundaries. Trim one trailing `/`
    // from the held path so `/api` and `/api/` are equivalent
    // directory prefixes; a `/foo/bar` held path never admits
    // `/foo/barbaz` because the boundary check fires.
    let held_path = h.path.strip_suffix('/').unwrap_or(h.path);
    let needed_path = n.path.strip_suffix('/').unwrap_or(n.path);
    if needed_path == held_path {
        return true;
    }
    if let Some(rest) = needed_path.strip_prefix(held_path) {
        rest.starts_with('/')
    } else {
        false
    }
}

/// Minimal scope-URL parser: scheme, lowercased host, effective
/// port, path. Anything after `?` or `#` is discarded. Returns
/// `None` on any structural problem.
struct ScopeUrl<'a> {
    scheme: &'a str,
    host: String,
    port: u16,
    path: &'a str,
}

fn parse_scope_url(s: &str) -> Option<ScopeUrl<'_>> {
    let (scheme, rest) = s.split_once("://")?;
    if scheme.is_empty() {
        return None;
    }
    // Strip query/fragment — scope matching is on the resource path,
    // not on call-site parametrization.
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    // Authority ends at the first `/` (which then starts the path)
    // or the end of string.
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return None;
    }
    // Reject userinfo (`user@host`) — the scope-base matcher is not
    // a full URL parser and should not silently accept a shape it
    // does not validate.
    if authority.contains('@') {
        return None;
    }
    let (host_part, port) = match authority.rfind(':') {
        Some(i) => {
            // IPv6 literal `[::1]:8080` — the colon we want is the
            // one outside the brackets. If the authority starts
            // with `[`, the port colon must be after `]`.
            let colon_in_v6 = authority.starts_with('[') && !authority[..i].ends_with(']');
            if colon_in_v6 {
                (authority, default_port(scheme)?)
            } else {
                let p: u16 = authority[i + 1..].parse().ok()?;
                (&authority[..i], p)
            }
        }
        None => (authority, default_port(scheme)?),
    };
    if host_part.is_empty() {
        return None;
    }
    Some(ScopeUrl {
        scheme,
        host: host_part.to_ascii_lowercase(),
        port,
        path,
    })
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    }
}

/// Compile `pattern` as a glob and test it against `candidate`. Returns
/// `false` if the pattern fails to compile — a malformed qualifier cannot
/// grant anything.
fn glob_matches(pattern: &str, candidate: &str) -> bool {
    match globset::Glob::new(pattern) {
        Ok(g) => g.compile_matcher().is_match(candidate),
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// CapabilitySet
// ---------------------------------------------------------------------------

/// A set of held scopes.
///
/// Backed by a sorted, deduplicated `Vec<Scope>` so that serialization is
/// deterministic — required because `CapabilitySet` appears inside
/// `AuditEvent::TurnStarted`, and audit events are HMAC-chained.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitySet {
    scopes: Vec<Scope>,
}

impl CapabilitySet {
    pub fn empty() -> Self {
        CapabilitySet { scopes: Vec::new() }
    }

    pub fn from_scopes(scopes: impl IntoIterator<Item = Scope>) -> Self {
        let mut v: Vec<Scope> = scopes.into_iter().collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v.dedup();
        CapabilitySet { scopes: v }
    }

    /// Returns `true` iff any held scope grants `needed` per D4 rules 1–4.
    pub fn grants(&self, needed: &Scope) -> bool {
        self.scopes.iter().any(|held| needed.is_granted_by(held))
    }

    /// Intersection — every scope in the result is granted by *both* sides.
    ///
    /// Used exactly once per turn to cap agent capabilities by the trust tier
    /// ceiling: `effective = agent_caps.intersect(tier.default_ceiling())`.
    pub fn intersect(&self, other: &CapabilitySet) -> CapabilitySet {
        let mut out: Vec<Scope> = Vec::new();
        for s in &self.scopes {
            if other.grants(s) {
                out.push(s.clone());
            }
        }
        for s in &other.scopes {
            if self.grants(s) && !out.contains(s) {
                out.push(s.clone());
            }
        }
        CapabilitySet::from_scopes(out)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Scope> {
        self.scopes.iter()
    }
}

// ---------------------------------------------------------------------------
// TrustTier
// ---------------------------------------------------------------------------

/// The four trust tiers, per D5.
///
/// **Variant order is ascending trust**, so derived `Ord` gives
/// `Kernel > Trusted > SemiTrusted > Untrusted`. The display-label numbers
/// (Tier 0 = Kernel, Tier 3 = Untrusted) run *opposite* to `Ord` — those
/// labels are a naming convention; the ordering used in code is `Ord`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub enum TrustTier {
    /// Tier 3 — Untrusted. Public webhooks, anonymous HTTP, unknown senders.
    Untrusted,
    /// Tier 2 — SemiTrusted. Authenticated user on a remote channel.
    SemiTrusted,
    /// Tier 1 — Trusted. Authenticated user on an owned channel.
    Trusted,
    /// Tier 0 — Kernel. Unconditional. Never assigned to a user-facing channel.
    Kernel,
}

impl TrustTier {
    /// The default capability ceiling for this tier, per the D5 table.
    pub fn default_ceiling(self) -> &'static CapabilitySet {
        match self {
            TrustTier::Kernel => &CEILING_KERNEL,
            TrustTier::Trusted => &CEILING_TRUSTED,
            TrustTier::SemiTrusted => &CEILING_SEMITRUSTED,
            TrustTier::Untrusted => &CEILING_UNTRUSTED,
        }
    }

    pub fn is_more_trusted_than(self, other: TrustTier) -> bool {
        self > other
    }

    /// The least-trusted tier whose default ceiling grants `scope` — the
    /// minimum tier a channel needs before a tool requiring this scope
    /// becomes reachable. Checks tiers in ascending trust order
    /// (Untrusted → SemiTrusted → Trusted → Kernel); if no non-Kernel
    /// ceiling grants it, the scope is Kernel-only.
    pub fn min_for_scope(scope: &Scope) -> TrustTier {
        const ASCENDING: [TrustTier; 4] = [
            TrustTier::Untrusted,
            TrustTier::SemiTrusted,
            TrustTier::Trusted,
            TrustTier::Kernel,
        ];
        ASCENDING
            .into_iter()
            .find(|tier| tier.default_ceiling().grants(scope))
            .unwrap_or(TrustTier::Kernel)
    }
}

// ---------------------------------------------------------------------------
// Ceiling tables — one `LazyLock<CapabilitySet>` per tier, per D5.
// ---------------------------------------------------------------------------

/// Helper: parse a list of scope strings, panicking on unknown bases. Panic
/// here is correct — these are compile-time-authored constants from D5, and a
/// typo should fail loudly on first access, not silently drop a scope.
fn caps(scopes: &[&str]) -> CapabilitySet {
    CapabilitySet::from_scopes(
        scopes
            .iter()
            .map(|s| Scope::parse(s).expect("ceiling scope must be valid")),
    )
}

/// Tier 0 — Kernel. Unlimited: holds every v1 active scope unqualified.
/// Exists so internal operations pass capability checks without special-casing.
static CEILING_KERNEL: LazyLock<CapabilitySet> = LazyLock::new(|| {
    CapabilitySet::from_scopes(
        KNOWN_BASES
            .iter()
            .map(|b| Scope::parse(b).expect("known base must parse")),
    )
});

/// Tier 1 — Trusted. Near-total. Every v1 scope granted unqualified.
///
/// Phase 14 Task 2 added `role.switch` here (and only here among the
/// real tiers) because sub-agent role-switching is a Trusted-tier
/// primitive per PRODUCT.md P1. A SemiTrusted Telegram user must not
/// be able to escalate capability envelopes by switching into a
/// different role — omitting `role.switch` from `CEILING_SEMITRUSTED`
/// makes that impossible at the ceiling intersection step, before
/// the tool dispatch gate is even consulted. Kernel gets it
/// automatically via the `KNOWN_BASES` iteration below.
static CEILING_TRUSTED: LazyLock<CapabilitySet> = LazyLock::new(|| {
    caps(&[
        "fs.read",
        "fs.write",
        "fs.delete",
        "fs.metadata",
        // Chapter O — the agent's own workspace. Trusted-only (like
        // shell.exec / fs.delete): a Local-operator-driven agent gets its
        // private notebook; remote-channel agents do not by default.
        "workspace",
        "net.fetch",
        "net.post",
        "net.dns",
        "shell.exec",
        "shell.spawn",
        // Amendment A12 (Phase 109) — `git.read` gates `git.status` /
        // `git.diff`, framed in the amendment doc as "the same category
        // as fs.read, fs.write, and net.fetch": ordinary substrate
        // reads, Trusted-tier like its siblings. The A12 commit added
        // the base to KNOWN_BASES only and never added this ceiling
        // entry — found 2026-07-07 via Chapter Almanac's tier audit
        // (the FG.2 comment below used to claim this omission was
        // deliberate ["reachable at the operator/Kernel tier the Local
        // CLI runs under"], but `local.rs`'s real `trust_tier()` returns
        // `Trusted` for the Local CLI, and no real channel anywhere
        // ever returns `Kernel` — so that claim was itself mistaken,
        // not a verified design decision). Until this fix, no
        // Trusted-tier channel could invoke `git.status`/`git.diff`.
        "git.read",
        // Chapter Forge (FG.2) — `git.write` gates the destructive
        // `git.commit` tool. Trusted-tier only (like shell.exec /
        // fs.delete): writing repo history is sensitive, so a remote
        // SemiTrusted adapter must not hold it by default; the write
        // base is pinned here so a future role grant can never lift it
        // past Trusted.
        "git.write",
        "llm.call",
        "llm.embed",
        "memory.read",
        "memory.write",
        "memory.forget",
        "memory.gc",
        "channel.send",
        "channel.receive",
        "audit.read",
        "config.read",
        "config.write",
        "mission.create",
        "mission.gate",
        "mission.list",
        "mission.status",
        "schedule.create",
        "schedule.list",
        "schedule.delete",
        "schedule.update",
        "webhook.create",
        "webhook.list",
        "webhook.delete",
        "file_watch.create",
        "file_watch.list",
        "file_watch.delete",
        "role.switch",
        "mcp.call",
        "reflection.propose",
        "reflection.apply",
        "persona.propose",
        // Phase 173 — Autonomous Loop. Trusted-tier (the
        // loop driver fires local `TriggerSource::Loop` turns,
        // same envelope as reflection). SemiTrusted does not
        // get these: a remote adapter must not drive an
        // autonomous code-committing loop.
        "loop.next",
        "loop.complete",
        "loop.note",
        // Phase 183 — Reminders. Trusted-tier (like the loop
        // tools); the reminder driver delivers via notify.
        "remind.read",
        "remind.write",
        // Phase 110 — Skills Auto-Creation. Trusted tier gets
        // skills.* because skill proposal + listing + invocation
        // are inside the same reflection-layer envelope as
        // persona.propose / role.update. SemiTrusted does not
        // get these — proposing or rendering skills from a
        // remote adapter would cross trust boundaries the same
        // way notify.send does. Operators who want SemiTrusted
        // skill access can grant individual bases via role
        // capability_scopes.
        "skills.propose",
        "skills.list",
        "skills.invoke",
        // Chapter Lattice — `graph.read` (the `graph.query` tool).
        // Trusted-tier only, like the other reflection-layer reads;
        // SemiTrusted does not get it by default.
        "graph.read",
        // Phase 184 — operator-authored skill editing (Trusted
        // only; identity-modifying).
        "skills.write",
        "role.update",
        "ollama.list",
        "ollama.show",
        "ollama.pull",
        // Phase 62 Task 2 — Trusted-tier only (Q2(a) sign-off).
        // notifications can leak data across trust boundaries,
        // so SemiTrusted does not inherit this base.
        "notify.send",
        // Phase 123 — Gmail third-party tool process (Chapter
        // F #1). All three bases Trusted-tier only by default;
        // SemiTrusted operators reading a personal inbox
        // through a remote adapter would cross the same trust
        // boundary notify.send guards against. Operators who
        // explicitly want SemiTrusted email access can grant
        // narrow bases via a role's `capability_scopes`.
        "email.read",
        "email.write",
        "email.send",
        // Phase 125 — Personal assistant tool bundle (Chapter
        // G #1). Five bases for the aivyx-toolkit tool process
        // surface; same Trusted-only default as email.* and
        // notify.send. Operators who want narrow access from a
        // remote channel can grant individual bases via a
        // role's `capability_scopes`.
        "web.search",
        "task.read",
        "task.write",
        "health.read",
        "health.write",
        // Phase 143 — Chapter G budget tracking.
        // Same Trusted-only default as task.* /
        // health.* / etc.; operators who want
        // narrow access from a remote channel can
        // grant individual bases via a role's
        // `capability_scopes`.
        "budget.read",
        "budget.write",
        // Chapter Abacus (AB.1) — pure-compute utilities. Listed
        // here so Trusted (and Kernel) hold it too, but its real
        // home is CEILING_SEMITRUSTED below: a calculator touches no
        // network/data, so it is safe below the Trusted tier the
        // rest of the toolkit pins to. AB.2 adds convert.units (the
        // convert group: convert.units + convert.time); AB.3 adds
        // date.compute (the date group: date.diff + date.add).
        "calc.eval",
        "convert.units",
        "date.compute",
        // Aivyx-Vision Milestone 1 (2026-09-18) — vision.generate_svg tool
        // process. Listed here so Trusted (and Kernel) hold it too, but its
        // real home is CEILING_SEMITRUSTED below, same pattern as
        // calc.eval/convert.units/date.compute just above: a sanitized,
        // constrained LLM call is safe below the Trusted tier the rest of
        // the toolkit pins to.
        "vision.generate",
        // Phase 128 — Google Calendar third-party tool
        // process (Chapter F #2). Two bases for the
        // five-tool surface (Q3b); Trusted-only default
        // matches the email.* / web.search / etc.
        // third-party-tool-process gating pattern.
        "calendar.read",
        "calendar.write",
        // Phase 129 — Google Drive third-party tool
        // process (Chapter F #3). Two bases for the
        // seven-tool surface (Q2b); Trusted-only default
        // matches email.* / calendar.* / web.search
        // third-party-tool-process gating.
        "drive.read",
        "drive.write",
        // Phase 130 — Notion third-party tool process
        // (Chapter F #5). Two bases for the seven-tool
        // surface (Q1a); Trusted-only default matches
        // Chapter F precedent.
        "notion.read",
        "notion.write",
        // Phase 130 Task 10 — Obsidian vault third-party
        // tool process (Chapter F #6). Two bases for the
        // six-tool surface (Q2a); Trusted-only default.
        "obsidian.read",
        "obsidian.write",
        // Phase 131 — n8n workflow-automation third-party
        // tool process (Chapter F #7). Two bases for the
        // ten-tool surface (Q1c, operator-picked over Q1b
        // Recommended); Trusted-only default matching the
        // Chapter F precedent for write-capable bases.
        "n8n.read",
        "n8n.write",
        // Chapter Contacts — aivyx-contacts third-party tool
        // process (the fifth Google integration; first Broaden
        // domain). Two bases for the six-tool People API surface;
        // Trusted-only default matches the Chapter F precedent for
        // write-capable bases (contacts.write mutates the user's
        // address book; contacts.delete is irreversible).
        "contacts.read",
        "contacts.write",
        // Chapter Deckhand — aivyx-apps third-party tool process (opt-in
        // `[applications]`). All three bases Trusted-tier ONLY: driving the GUI
        // apps on the operator's own machine reaches the whole desktop and
        // is inherently un-sandboxable, so a remote/SemiTrusted adapter must
        // never hold them. `app.input` is also in IRREVERSIBLE_BASES
        // (confirm-first when `[access] confirm_destructive` is on).
        "app.read",
        "app.control",
        "app.input",
        // Kitchen / BOH vertical pack — `kitchen.read` (the read +
        // compute tool surface). Trusted-tier-only default, matching
        // the email.* / web.search / drive.* third-party-tool-process
        // gating pattern. See docs/VERTICAL_PACKS.md.
        "kitchen.read",
        // Kitchen vertical pack — gated write surface. Trusted-tier-
        // only; `kitchen.order.send` is additionally confirm-first at
        // the tool level.
        "kitchen.write",
        "kitchen.order.send",
        // Append-only food-safety logging (the compliance wedge); each
        // call lands on the HMAC audit chain. Trusted-tier-only default.
        "kitchen.haccp.log",
        // Nonagon team orchestration (docs/NONAGON.md): the lead's
        // authority to delegate to specialists. Trusted-tier default.
        "team.delegate",
        // Team dialogue (the message bus) — held by every member. Trusted.
        "team.message",
        // Chapter L (L.7) — `team.run` (starts a whole durable team
        // mission). The KNOWN_BASES doc comment has said "Channel-tier,
        // Trusted (like the loop tools)" since the base was added
        // (51a711d), but this ceiling entry was never actually added
        // alongside it — found 2026-07-07 via Chapter Almanac's Studio
        // Tools screen showing it as Kernel-only in the live catalog.
        // Until this fix, no Trusted-tier chat turn or autonomous-loop
        // iteration could ever actually invoke it (silently denied at
        // the capability gate before reaching the tool).
        "team.run",
        // Phase 191 — daemon-side automatic alert dispatch. Trusted-tier
        // only, matching notify.send's own tier restriction directly
        // above (same data-exfil rationale: a SemiTrusted tool process
        // must not be able to push arbitrary content to an
        // operator-configured notify target). Added alongside the
        // KNOWN_BASES entry itself — see the `team.run` comment just
        // above for what happens when a base's Trusted-tier doc claim
        // isn't backed by an actual ceiling entry: silently denied at
        // the capability gate, no matter what a role's config grants.
        "notify.dispatch",
    ])
});

/// Tier 2 — SemiTrusted. Per D5 table:
///
/// **⊘ rows** (hard-denied — no form survives intersection):
/// `fs.delete`, `shell.exec`, `shell.spawn`, `config.write`,
/// `mission.create`, `mission.gate`, `role.switch`.
///
/// **▲ rows** (conditionally granted — the *unqualified* form is omitted from
/// this ceiling, so a held *unqualified* scope like bare `fs.write` is denied.
/// A held *qualified* scope like `fs.write:/sandbox/**` survives intersection
/// only if the ceiling also holds a pattern that grants it per D4 rules 1–4.
/// Since this ceiling carries no entry for the ▲ bases at all, qualified forms
/// also fail intersection — the ▲ scopes are effectively denied unless a
/// future ceiling revision adds narrow qualified grants here):
/// `fs.read`, `fs.write`, `net.post`, `memory.forget`, `channel.send`,
/// `channel.receive`, `audit.read`.
static CEILING_SEMITRUSTED: LazyLock<CapabilitySet> = LazyLock::new(|| {
    caps(&[
        "fs.metadata",
        "net.fetch",
        "net.dns",
        "llm.call",
        "llm.embed",
        "memory.read",
        "memory.write",
        "config.read",
        // Chapter Abacus (AB.1) — pure-compute utilities. The first
        // toolkit base reachable at SemiTrusted: a calculator has no
        // side effects and exposes no operator data, so a
        // semi-trusted remote context may use it without a Trusted
        // role grant. See docs/ABACUS.md §2. AB.2 adds convert.units
        // (the convert group: convert.units + convert.time); AB.3
        // adds date.compute (the date group: date.diff + date.add).
        "calc.eval",
        "convert.units",
        "date.compute",
        // Aivyx-Vision Milestone 1 (2026-09-18) — vision.generate_svg tool
        // process. Reachable at SemiTrusted: narrower and safer than
        // llm.call (constrained prompt, sanitized output), which is
        // itself already SemiTrusted-reachable. See the KNOWN_BASES doc
        // comment for the full rationale.
        "vision.generate",
    ])
});

/// Tier 3 — Untrusted. Near-empty. The two ▲ rows (`memory.read:scope:public:*`,
/// `audit.read:public`) are not representable as an unqualified ceiling entry —
/// intersection with a held narrow qualified scope would not match since rule
/// 2 (unqualified held grants qualified needed) is what we want here. So we
/// grant the specific narrow qualified forms directly.
static CEILING_UNTRUSTED: LazyLock<CapabilitySet> = LazyLock::new(|| {
    caps(&["memory.read:scope:public:*", "audit.read:public"])
});

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn s(x: &str) -> Scope {
        Scope::parse(x).expect("test scope must parse")
    }

    #[test]
    fn irreversible_bases_classify_correctly() {
        // The dangerous bases are flagged.
        for base in ["fs.delete", "shell.exec", "net.post", "git.write", "kitchen.order.send"] {
            assert!(is_irreversible_base(base), "{base} must be irreversible");
        }
        // Ordinary reversible work is not — these are the ones AutoApprove may
        // approve when allowlisted (writes lean on checkpoint/rollback).
        for base in ["fs.read", "fs.write", "net.fetch", "memory.write", "data.csv"] {
            assert!(!is_irreversible_base(base), "{base} must be reversible-class");
        }
        // Unknown bases are not classified irreversible — the allowlist's
        // deny-by-default is what stops them auto-approving.
        assert!(!is_irreversible_base("totally.unknown"));
    }

    /// Drift guard: every irreversible base must be a real `KNOWN_BASES` entry,
    /// so a typo here is caught rather than silently never matching a scope.
    #[test]
    fn every_irreversible_base_is_a_known_base() {
        for base in IRREVERSIBLE_BASES {
            assert!(
                KNOWN_BASES.contains(base),
                "irreversible base `{base}` is not in KNOWN_BASES (typo?)"
            );
        }
    }

    // ---- Task 4 (HIGH, 2026-09-16 audit) — withheld integration bases ----

    #[test]
    fn withheld_integration_bases_classify_correctly() {
        for base in [
            "email.write",
            "email.send",
            "drive.write",
            "notion.write",
            "obsidian.write",
            "n8n.write",
            "contacts.write",
            "calendar.write",
        ] {
            assert!(is_withheld_integration_base(base), "{base} must be withheld");
        }
        // Read-only integration bases stay auto-grantable / unconfirmed.
        for base in [
            "email.read",
            "drive.read",
            "notion.read",
            "obsidian.read",
            "n8n.read",
            "contacts.read",
            "calendar.read",
        ] {
            assert!(
                !is_withheld_integration_base(base),
                "{base} must not be withheld"
            );
        }
        assert!(!is_withheld_integration_base("totally.unknown"));
    }

    /// Drift guard: every withheld integration base must be a real
    /// `KNOWN_BASES` entry, so a typo here is caught rather than silently
    /// never matching a scope (same rationale as the irreversible-bases
    /// guard just above).
    #[test]
    fn every_withheld_integration_base_is_a_known_base() {
        for base in WITHHELD_INTEGRATION_BASES {
            assert!(
                KNOWN_BASES.contains(base),
                "withheld integration base `{base}` is not in KNOWN_BASES (typo?)"
            );
        }
    }

    // ---- Chapter Atlas (AT.1) — tool-catalog drift guard ----

    /// `docs/TOOLS.md` is organized around `KNOWN_BASES`. This asserts every
    /// capability base is documented there, so adding a new base (a new tool
    /// surface) fails CI until the catalog is updated. The reverse direction
    /// (no stale bases) is covered implicitly: a removed base that's still
    /// documented is harmless prose, and removing one is rare + reviewed.
    #[test]
    fn tools_catalog_documents_every_known_base() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/TOOLS.md");
        let catalog = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {path}: {e}"));
        let missing: Vec<&str> = KNOWN_BASES
            .iter()
            .copied()
            .filter(|base| !catalog.contains(*base))
            .collect();
        assert!(
            missing.is_empty(),
            "docs/TOOLS.md is missing {} capability base(s): {missing:?}\n\
             Add them to the catalog (Chapter Atlas) when introducing a new tool surface.",
            missing.len(),
        );
    }

    // ---- Scope::parse ----

    #[test]
    fn parse_accepts_known_bare_base() {
        assert!(Scope::parse("fs.read").is_some());
        assert!(Scope::parse("memory.write").is_some());
    }

    #[test]
    fn parse_accepts_known_base_with_qualifier() {
        assert!(Scope::parse("fs.read:/home/julian/**").is_some());
        assert!(Scope::parse("net.fetch:https://example.com").is_some());
    }

    #[test]
    fn parse_rejects_unknown_base() {
        assert!(Scope::parse("nonsense.base").is_none());
        assert!(Scope::parse("display.window_close").is_none(),
                "Reserved scopes are not v1 active");
    }

    #[test]
    fn notify_dispatch_scope_parses() {
        assert!(Scope::parse("notify.dispatch").is_some());
    }

    #[test]
    fn mcp_server_wildcard_grants_that_servers_tools_only() {
        // An MCP tool call requires `mcp.call:<server>:<tool>`. The default-role
        // floor grants `mcp.call:<server>:*` per configured server so the agent
        // can actually invoke a configured server's tools (e.g. the bundled
        // web-search the wizard adds). Verify the per-server wildcard grants
        // that server's tools but NOT another server's.
        let needed = Scope::parse("mcp.call:web-search:web_search").unwrap();
        let held = Scope::parse("mcp.call:web-search:*").unwrap();
        assert!(
            needed.is_granted_by(&held),
            "mcp.call:web-search:* must grant mcp.call:web-search:web_search"
        );

        let other_server = Scope::parse("mcp.call:other:web_search").unwrap();
        assert!(
            !other_server.is_granted_by(&held),
            "a web-search grant must not authorize a different server's tools"
        );
    }

    #[test]
    fn base_and_qualifier_split() {
        let sc = s("fs.read:/etc/*");
        assert_eq!(sc.base(), "fs.read");
        assert_eq!(sc.qualifier(), Some("/etc/*"));

        let bare = s("fs.read");
        assert_eq!(bare.base(), "fs.read");
        assert_eq!(bare.qualifier(), None);
    }

    // ---- Attenuation rule 1: bases must match ----

    #[test]
    fn rule1_base_mismatch_denies() {
        assert!(!s("fs.read").is_granted_by(&s("fs.write")));
        assert!(!s("fs.read:/foo").is_granted_by(&s("fs.write:/foo")));
    }

    // ---- Attenuation rule 2: unqualified held grants qualified needed ----

    #[test]
    fn rule2_unqualified_grants_qualified() {
        assert!(s("fs.read:/any/path").is_granted_by(&s("fs.read")));
        assert!(s("net.fetch:https://example.com").is_granted_by(&s("net.fetch")));
    }

    #[test]
    fn rule2_unqualified_grants_unqualified() {
        assert!(s("fs.read").is_granted_by(&s("fs.read")));
    }

    // ---- Attenuation rule 3: qualified held grants qualified needed per kind ----

    #[test]
    fn rule3_path_glob_match() {
        assert!(s("fs.read:/home/julian/docs/note.md")
            .is_granted_by(&s("fs.read:/home/julian/**")));
        assert!(!s("fs.read:/etc/passwd")
            .is_granted_by(&s("fs.read:/home/julian/**")));
    }

    #[test]
    fn rule3_url_prefix_match() {
        assert!(s("net.fetch:https://api.example.com/v1/users")
            .is_granted_by(&s("net.fetch:https://api.example.com/")));
        assert!(!s("net.fetch:https://evil.example.com/")
            .is_granted_by(&s("net.fetch:https://api.example.com/")));
    }

    // ---- Phase 12 Task 2: URL-prefix hardening -----------------------

    // Classic hostile-suffix attack. Byte-prefix `starts_with` admits
    // `example.com.evil.com` because the needed string literally
    // starts with the held string; the origin-aware matcher must
    // reject it because the hosts differ.
    #[test]
    fn url_prefix_rejects_hostile_suffix_hostname() {
        assert!(
            !s("net.fetch:https://example.com.evil.com/")
                .is_granted_by(&s("net.fetch:https://example.com/")),
            "held example.com MUST NOT grant needed example.com.evil.com"
        );
        assert!(
            !s("net.fetch:https://example.com.evil.com/login")
                .is_granted_by(&s("net.fetch:https://example.com/")),
            "hostile suffix with path must also be denied"
        );
    }

    // Path-segment boundary, not byte boundary. A held `/users`
    // grant cannot leak into `/users2` — otherwise any path prefix
    // accidentally admits an adjacent path.
    #[test]
    fn url_prefix_rejects_non_boundary_path_prefix() {
        assert!(
            !s("net.fetch:https://api.example.com/users2/list")
                .is_granted_by(&s("net.fetch:https://api.example.com/users")),
            "path /users2 must not match held /users — no segment boundary"
        );
        // But the same held scope does grant a real sub-path.
        assert!(s("net.fetch:https://api.example.com/users/42")
            .is_granted_by(&s("net.fetch:https://api.example.com/users")));
    }

    // Schemes must match exactly. A held `https://` grant does not
    // cover plain `http://` — that would be a downgrade attack.
    #[test]
    fn url_prefix_rejects_scheme_mismatch() {
        assert!(
            !s("net.fetch:http://api.example.com/")
                .is_granted_by(&s("net.fetch:https://api.example.com/")),
            "http must not be granted by https hold"
        );
    }

    // Default-port normalization: `https://host` and
    // `https://host:443` are the same origin and grant each other.
    #[test]
    fn url_prefix_normalizes_default_ports() {
        assert!(s("net.fetch:https://api.example.com/")
            .is_granted_by(&s("net.fetch:https://api.example.com:443/")));
        assert!(s("net.fetch:https://api.example.com:443/")
            .is_granted_by(&s("net.fetch:https://api.example.com/")));
        // A non-default port is its own origin — 8443 does NOT
        // match the default 443.
        assert!(!s("net.fetch:https://api.example.com:8443/")
            .is_granted_by(&s("net.fetch:https://api.example.com/")));
    }

    // Host compare is ASCII case-insensitive (RFC 3986).
    #[test]
    fn url_prefix_host_compare_is_case_insensitive() {
        assert!(s("net.fetch:https://API.Example.Com/path")
            .is_granted_by(&s("net.fetch:https://api.example.com/path")));
    }

    // Trailing-slash equivalence. Held `/api` and held `/api/` both
    // cover needed `/api/v1`.
    #[test]
    fn url_prefix_trailing_slash_equivalence() {
        assert!(s("net.fetch:https://api.example.com/api/v1")
            .is_granted_by(&s("net.fetch:https://api.example.com/api")));
        assert!(s("net.fetch:https://api.example.com/api/v1")
            .is_granted_by(&s("net.fetch:https://api.example.com/api/")));
    }

    // Query-string in needed is ignored — authorization is on the
    // resource path, not on how the call parametrizes it.
    #[test]
    fn url_prefix_query_string_is_ignored() {
        assert!(s("net.fetch:https://api.example.com/search?q=hello")
            .is_granted_by(&s("net.fetch:https://api.example.com/search")));
    }

    // Malformed qualifier denies — same invariant as glob_matches.
    #[test]
    fn url_prefix_malformed_denies() {
        // `is_granted_by` never reaches the URL matcher for these
        // because Scope::parse accepts any known base — so the
        // malformed check is on the matcher itself. Use a held
        // scope that's well-formed and a needed scope that
        // technically parses (known base) but has no `://`. The
        // dispatch would not route this through UrlPrefix, so we
        // instead test the matcher directly via a URL-shaped
        // needed with a broken authority.
        assert!(!s("net.fetch:https:///no-host/path")
            .is_granted_by(&s("net.fetch:https://example.com/")),
            "empty authority must be rejected by the parser");
    }

    #[test]
    fn rule3_allowlist_subset_match() {
        // Needed is a single item present in held list.
        assert!(s("shell.exec:git").is_granted_by(&s("shell.exec:git,ls,cat")));
        // Needed as multi-item subset.
        assert!(s("shell.exec:git,ls").is_granted_by(&s("shell.exec:git,ls,cat")));
        // Needed item not in held list.
        assert!(!s("shell.exec:rm").is_granted_by(&s("shell.exec:git,ls,cat")));
    }

    #[test]
    fn rule3_simple_glob_match() {
        // model-name style qualifier — no slashes, no commas, no ://
        assert!(s("llm.call:claude-opus-4-6")
            .is_granted_by(&s("llm.call:claude-*")));
        assert!(!s("llm.call:gpt-5").is_granted_by(&s("llm.call:claude-*")));
    }

    // ---- Dispatch-collision regressions (Phase 1 Q1) ----

    #[test]
    fn dispatch_path_with_brace_alternation_beats_comma() {
        // Held qualifier uses globset brace alternation — contains both `/`
        // and `,`. Must dispatch as PathGlob, not Allowlist. The needed
        // scope here deliberately has no `/` so the *held* side is what
        // forces the path classification.
        let needed = s("fs.read:julian");
        let held = s("fs.read:/home/{julian,root}/**");
        // The glob won't actually match "julian" — that's fine; what we're
        // testing is that we hit the PathGlob branch, not Allowlist, which
        // would otherwise do a string-subset check and return a nonsense
        // answer. We assert PathGlob semantics: non-match.
        assert!(!needed.is_granted_by(&held));

        // And the glob *does* match a full path.
        assert!(s("fs.read:/home/julian/notes.md").is_granted_by(&held));
        assert!(s("fs.read:/home/root/.bashrc").is_granted_by(&held));
        assert!(!s("fs.read:/etc/passwd").is_granted_by(&held));
    }

    #[test]
    fn dispatch_url_with_comma_in_query_stays_url() {
        // A URL with a comma in a query parameter must remain URL-dispatched.
        // `://` on the needed side wins before the comma check fires.
        let needed = s("net.fetch:https://api.example.com/search?tags=a,b,c");
        let held = s("net.fetch:https://api.example.com/");
        assert!(needed.is_granted_by(&held));
    }

    #[test]
    fn dispatch_colon_in_qualifier_falls_through_to_simple_glob() {
        // A qualifier like `session:abc` (the memory-recall case from D4)
        // contains neither `/`, `,`, nor `://`. Scope::parse splits on the
        // first `:` only, so the qualifier is literally `session:abc`.
        // Must dispatch as SimpleGlob.
        let sc = s("memory.read:session:abc");
        assert_eq!(sc.qualifier(), Some("session:abc"));
        // Exact-match held grants exact needed.
        assert!(sc.is_granted_by(&s("memory.read:session:abc")));
        // Wildcard on the session suffix matches too.
        assert!(sc.is_granted_by(&s("memory.read:session:*")));
        // Different session is denied.
        assert!(!sc.is_granted_by(&s("memory.read:session:xyz")));
    }

    // ---- Attenuation rule 4: qualified held does NOT grant unqualified needed ----

    #[test]
    fn rule4_qualified_does_not_grant_unqualified() {
        assert!(!s("fs.read").is_granted_by(&s("fs.read:/home/julian/**")));
        assert!(!s("shell.exec").is_granted_by(&s("shell.exec:git,ls")));
    }

    // ---- CapabilitySet::grants / intersect ----

    #[test]
    fn capset_grants_any_held_match() {
        let held = CapabilitySet::from_scopes([
            s("fs.read:/home/julian/**"),
            s("net.fetch:https://api.example.com/"),
        ]);
        assert!(held.grants(&s("fs.read:/home/julian/notes.md")));
        assert!(held.grants(&s("net.fetch:https://api.example.com/v1")));
        assert!(!held.grants(&s("fs.write:/home/julian/notes.md")));
    }

    #[test]
    fn capset_from_scopes_sorts_and_dedupes() {
        let set = CapabilitySet::from_scopes([
            s("net.fetch"),
            s("fs.read"),
            s("fs.read"),
            s("llm.call"),
        ]);
        let collected: Vec<&str> = set.iter().map(|sc| sc.0.as_str()).collect();
        assert_eq!(collected, vec!["fs.read", "llm.call", "net.fetch"]);
    }

    #[test]
    fn capset_intersect_keeps_mutually_granted() {
        let agent = CapabilitySet::from_scopes([
            s("fs.read:/home/julian/**"),
            s("shell.exec"),
            s("llm.call"),
        ]);
        let ceiling = CapabilitySet::from_scopes([s("fs.read"), s("llm.call")]);

        let eff = agent.intersect(&ceiling);
        // fs.read:/home/julian/** is granted by unqualified fs.read, keep it.
        assert!(eff.grants(&s("fs.read:/home/julian/notes.md")));
        // llm.call is in both.
        assert!(eff.grants(&s("llm.call")));
        // shell.exec is not in the ceiling — dropped.
        assert!(!eff.grants(&s("shell.exec")));
    }

    #[test]
    fn capset_intersect_is_deterministic() {
        let a = CapabilitySet::from_scopes([s("fs.read"), s("llm.call")]);
        let b = CapabilitySet::from_scopes([s("llm.call"), s("fs.read")]);
        // Different insertion order, same resulting Vec order.
        assert_eq!(a, b);
    }

    // ---- TrustTier ordering and ceilings ----

    #[test]
    fn trust_tier_ord_matches_d5_note() {
        assert!(TrustTier::Kernel > TrustTier::Trusted);
        assert!(TrustTier::Trusted > TrustTier::SemiTrusted);
        assert!(TrustTier::SemiTrusted > TrustTier::Untrusted);
    }

    #[test]
    fn trust_tier_is_more_trusted_than() {
        assert!(TrustTier::Kernel.is_more_trusted_than(TrustTier::Untrusted));
        assert!(!TrustTier::Untrusted.is_more_trusted_than(TrustTier::Kernel));
    }

    #[test]
    fn min_for_scope_matches_the_documented_tiers() {
        // Chapter Almanac — cross-checked against docs/TOOLS.md's own
        // "how to read the tier column" prose, corrected in the same
        // chapter after this helper caught it drifting from the ceiling
        // tables (config.write / role.switch / role.update had been
        // documented as Kernel; the ceiling code has always held them
        // bare in CEILING_TRUSTED).
        assert_eq!(TrustTier::min_for_scope(&s("fs.read")), TrustTier::Trusted);
        assert_eq!(
            TrustTier::min_for_scope(&s("fs.metadata")),
            TrustTier::SemiTrusted
        );
        assert_eq!(
            TrustTier::min_for_scope(&s("memory.read")),
            TrustTier::SemiTrusted
        );
        assert_eq!(
            TrustTier::min_for_scope(&s("role.switch")),
            TrustTier::Trusted
        );
        assert_eq!(
            TrustTier::min_for_scope(&s("role.update")),
            TrustTier::Trusted
        );
        // The one genuinely Kernel-only base: no real tier's ceiling
        // holds it (`tool_allowlist_parses_and_is_absent_from_real_ceilings`).
        assert_eq!(
            TrustTier::min_for_scope(&s("tool.allowlist")),
            TrustTier::Kernel
        );
    }

    #[test]
    fn ceiling_kernel_grants_everything() {
        let kernel = TrustTier::Kernel.default_ceiling();
        for b in KNOWN_BASES {
            assert!(kernel.grants(&Scope::parse(b).unwrap()));
        }
    }

    #[test]
    fn ceiling_trusted_grants_shell_exec() {
        let trusted = TrustTier::Trusted.default_ceiling();
        assert!(trusted.grants(&s("shell.exec")));
        assert!(trusted.grants(&s("fs.delete:/tmp/scratch")));
    }

    #[test]
    fn ceiling_semitrusted_denies_shell_and_delete() {
        // Scenario 3 from D1: "Run rm -rf from Telegram"
        let semi = TrustTier::SemiTrusted.default_ceiling();
        assert!(!semi.grants(&s("shell.exec")));
        assert!(!semi.grants(&s("shell.exec:rm")));
        assert!(!semi.grants(&s("fs.delete:/home/julian/notes.md")));
        assert!(!semi.grants(&s("config.write")));
        // But memory.read is still fine.
        assert!(semi.grants(&s("memory.read")));
    }

    #[test]
    fn ceiling_semitrusted_requires_qualifier_for_triangle_rows() {
        // D5's ▲ semantic: holding *bare* fs.write does NOT satisfy a Tier 2
        // check, because rule 4 says qualified held can't grant unqualified
        // needed — and the ceiling at Tier 2 has no unqualified fs.write.
        let semi = TrustTier::SemiTrusted.default_ceiling();
        assert!(!semi.grants(&s("fs.write")));
        assert!(!semi.grants(&s("fs.read")));

        // But an agent holding `fs.write:/tmp/**` and intersected with the
        // Tier 2 ceiling gets nothing — ceiling has no fs.write at all.
        // The ▲ semantic is enforced by *building a narrower tool* (per the
        // D5 design principle), not by loosening the ceiling.
        let agent = CapabilitySet::from_scopes([s("fs.write:/tmp/**")]);
        let eff = agent.intersect(semi);
        assert!(!eff.grants(&s("fs.write:/tmp/x.txt")));
    }

    #[test]
    fn ceiling_untrusted_is_minimal() {
        let u = TrustTier::Untrusted.default_ceiling();
        assert!(!u.grants(&s("fs.read")));
        assert!(!u.grants(&s("shell.exec")));
        assert!(!u.grants(&s("memory.write")));
        // The one explicit narrow grant.
        assert!(u.grants(&s("memory.read:scope:public:feed")));
    }

    // ---- Phase 21: mission capability scopes ----

    #[test]
    fn mission_scopes_parse() {
        assert!(Scope::parse("mission.create").is_some());
        assert!(Scope::parse("mission.gate").is_some());
        assert!(Scope::parse("mission.create:my-mission").is_some());
        assert!(Scope::parse("mission.gate:gate-001").is_some());
    }

    #[test]
    fn mission_scopes_kernel_grants() {
        let kernel = TrustTier::Kernel.default_ceiling();
        assert!(kernel.grants(&s("mission.create")));
        assert!(kernel.grants(&s("mission.gate")));
    }

    #[test]
    fn mission_scopes_trusted_grants() {
        let trusted = TrustTier::Trusted.default_ceiling();
        assert!(trusted.grants(&s("mission.create")));
        assert!(trusted.grants(&s("mission.gate")));
        assert!(trusted.grants(&s("mission.create:my-mission")));
        assert!(trusted.grants(&s("mission.gate:gate-001")));
    }

    #[test]
    fn mission_scopes_semitrusted_denies() {
        let semi = TrustTier::SemiTrusted.default_ceiling();
        assert!(!semi.grants(&s("mission.create")));
        assert!(!semi.grants(&s("mission.gate")));
        assert!(!semi.grants(&s("mission.create:my-mission")));
        assert!(!semi.grants(&s("mission.gate:gate-001")));
    }

    #[test]
    fn mission_scopes_untrusted_denies() {
        let untrusted = TrustTier::Untrusted.default_ceiling();
        assert!(!untrusted.grants(&s("mission.create")));
        assert!(!untrusted.grants(&s("mission.gate")));
    }

    // ---- Phase 36: Ollama model management scopes ----

    #[test]
    fn ollama_scopes_parse() {
        assert!(Scope::parse("ollama.list").is_some());
        assert!(Scope::parse("ollama.show").is_some());
        assert!(Scope::parse("ollama.pull").is_some());
        // Qualified forms are valid (though tools use bare forms)
        assert!(Scope::parse("ollama.show:llama3.1").is_some());
    }

    #[test]
    fn ollama_scopes_trusted_grants() {
        let trusted = TrustTier::Trusted.default_ceiling();
        assert!(trusted.grants(&s("ollama.list")));
        assert!(trusted.grants(&s("ollama.show")));
        assert!(trusted.grants(&s("ollama.pull")));
    }

    #[test]
    fn ollama_scopes_semitrusted_denies() {
        let semi = TrustTier::SemiTrusted.default_ceiling();
        assert!(!semi.grants(&s("ollama.list")));
        assert!(!semi.grants(&s("ollama.show")));
        assert!(!semi.grants(&s("ollama.pull")));
    }

    #[test]
    fn ollama_scopes_untrusted_denies() {
        let untrusted = TrustTier::Untrusted.default_ceiling();
        assert!(!untrusted.grants(&s("ollama.list")));
        assert!(!untrusted.grants(&s("ollama.show")));
        assert!(!untrusted.grants(&s("ollama.pull")));
    }

    // ---- Phase 11 Task 4: synthetic `tool.allowlist` base ----

    #[test]
    fn tool_allowlist_parses_and_is_absent_from_real_ceilings() {
        // The Phase 11 Task 4 role-allowlist gate synthesizes
        // `tool.allowlist:<tool_name>` scopes at the dispatch layer
        // and routes them through `ToolOutcome::Denied { scope,
        // held }` so auditors can distinguish "capability denial"
        // from "role allowlist denial" by reading
        // `scope_requested.base()`. The base must parse cleanly,
        // and NO real-tier ceiling (Trusted, SemiTrusted,
        // Untrusted) may hold it — otherwise an audit event for a
        // role rejection would show a held set that contradicts
        // the denial.
        let synthetic = Scope::parse("tool.allowlist:shell.exec")
            .expect("tool.allowlist base must parse");
        assert_eq!(synthetic.base(), "tool.allowlist");
        assert_eq!(synthetic.qualifier(), Some("shell.exec"));

        let bare = s("tool.allowlist");
        assert!(
            !TrustTier::Trusted.default_ceiling().grants(&bare),
            "Trusted ceiling must NOT hold tool.allowlist — it's \
             a synthetic dispatch-layer base, not a real capability"
        );
        assert!(
            !TrustTier::SemiTrusted.default_ceiling().grants(&bare),
            "SemiTrusted ceiling must NOT hold tool.allowlist"
        );
        assert!(
            !TrustTier::Untrusted.default_ceiling().grants(&bare),
            "Untrusted ceiling must NOT hold tool.allowlist"
        );

        // Kernel DOES hold it unqualified — that's fine; kernel is
        // an internal-only tier, no real binary dispatches through
        // it. The `ceiling_kernel_grants_everything` test already
        // pins the "kernel holds every base" invariant.
        assert!(
            TrustTier::Kernel.default_ceiling().grants(&bare),
            "Kernel holds every KNOWN_BASES entry including the \
             synthetic one, per the ceiling-kernel-grants-everything \
             invariant"
        );
    }

    // ---- Phase 14 Task 2: `role.switch` scope base ----

    #[test]
    fn role_switch_parses_bare_and_qualified_forms() {
        let bare = Scope::parse("role.switch").expect("bare form must parse");
        assert_eq!(bare.base(), "role.switch");
        assert_eq!(bare.qualifier(), None);

        let qualified =
            Scope::parse("role.switch:researcher").expect("qualified form must parse");
        assert_eq!(qualified.base(), "role.switch");
        assert_eq!(qualified.qualifier(), Some("researcher"));
    }

    #[test]
    fn role_switch_rejects_wildcard_qualifier() {
        // `role.switch:*` is redundant — the unqualified form
        // already is the wildcard under Rule 2. Reject at parse
        // time per the Phase 14 Task 2 design note.
        assert!(
            Scope::parse("role.switch:*").is_none(),
            "role.switch:* must be rejected; use bare role.switch \
             for the any-target wildcard"
        );
    }

    #[test]
    fn role_switch_unqualified_grants_qualified_target() {
        // Rule 2 (unqualified held grants qualified needed) works
        // for role.switch for free — no dispatch changes needed.
        assert!(s("role.switch:researcher").is_granted_by(&s("role.switch")));
        assert!(s("role.switch:coder").is_granted_by(&s("role.switch")));
    }

    #[test]
    fn role_switch_qualified_does_not_grant_unqualified() {
        // Rule 4: a role holding only `role.switch:researcher`
        // cannot claim unqualified switch rights. This is the
        // structural guarantee behind PRODUCT.md P1.3 — a child
        // role attenuated down to a single target cannot widen
        // back to all targets by dropping the qualifier.
        assert!(!s("role.switch").is_granted_by(&s("role.switch:researcher")));
    }

    #[test]
    fn role_switch_qualified_grants_same_target_only() {
        // Rule 3 via SimpleGlob dispatch. Role-name qualifiers
        // contain no `/`, no `,`, no `://`, so they fall through
        // to the SimpleGlob arm — and glob_matches of two equal
        // glob-metacharacter-free strings is trivially true.
        assert!(s("role.switch:researcher").is_granted_by(&s("role.switch:researcher")));
        assert!(!s("role.switch:researcher").is_granted_by(&s("role.switch:coder")));
    }

    #[test]
    fn role_switch_reflexive_grants_itself_in_a_capset() {
        // The Phase 13 Task 4 url-prefix reflexivity bug is a
        // known hazard the Phase 14 Non-goals block names. For
        // `role.switch` specifically, reflexivity goes through
        // `SimpleGlob → glob_matches(q, q)`, which has no URL
        // parser and no asymmetric component — so the hazard
        // class does not transfer. This test pins that claim:
        // a CapabilitySet containing `role.switch:researcher`
        // must `grants` itself.
        let held = CapabilitySet::from_scopes([s("role.switch:researcher")]);
        assert!(
            held.grants(&s("role.switch:researcher")),
            "role.switch:researcher must self-grant; if this fails \
             the Phase 13 Task 4 url-prefix reflexivity bug has \
             leaked into SimpleGlob dispatch"
        );
    }

    #[test]
    fn role_switch_intersection_with_unqualified_holder_keeps_qualified() {
        // An agent declaring the qualified form whose parent
        // chain holds the unqualified form: after intersection,
        // the qualified form survives. This is what
        // `assemble_role_envelope` produces for a coder role
        // declaring `role.switch:researcher` under a default
        // root declaring unqualified `role.switch`.
        let coder = CapabilitySet::from_scopes([s("role.switch:researcher")]);
        let default = CapabilitySet::from_scopes([s("role.switch")]);
        let effective = coder.intersect(&default);
        assert!(effective.grants(&s("role.switch:researcher")));
        assert!(
            !effective.grants(&s("role.switch")),
            "bare role.switch must NOT survive intersection with a \
             qualified coder set — the intersection narrows to the \
             qualified form"
        );
        // And the coder role cannot switch into a sibling it did
        // not declare: `coder` asking for `role.switch:scribe`
        // is denied because its own declared set has only
        // `role.switch:researcher`, and the intersection's
        // SimpleGlob-equality check rejects the name mismatch.
        assert!(!effective.grants(&s("role.switch:scribe")));
    }

    #[test]
    fn role_switch_is_in_trusted_ceiling_only() {
        // Trusted holds it unqualified so a role declaring
        // `role.switch:*` under a Trusted tier survives the
        // ceiling intersection. SemiTrusted and Untrusted both
        // omit it — role-switching is a Trusted-tier primitive
        // per PRODUCT.md P1, and a SemiTrusted Telegram user
        // must not be able to escalate via a switch.
        assert!(TrustTier::Trusted.default_ceiling().grants(&s("role.switch")));
        assert!(TrustTier::Trusted
            .default_ceiling()
            .grants(&s("role.switch:researcher")));

        assert!(!TrustTier::SemiTrusted
            .default_ceiling()
            .grants(&s("role.switch")));
        assert!(!TrustTier::SemiTrusted
            .default_ceiling()
            .grants(&s("role.switch:researcher")));

        assert!(!TrustTier::Untrusted
            .default_ceiling()
            .grants(&s("role.switch")));

        // Kernel holds every KNOWN_BASES entry including this
        // one — the `ceiling_kernel_grants_everything` invariant.
        assert!(TrustTier::Kernel.default_ceiling().grants(&s("role.switch")));
    }

    #[test]
    fn team_run_is_in_trusted_ceiling_only() {
        // Regression for a real bug (found 2026-07-07 via Chapter
        // Almanac's Studio Tools screen): `team.run`'s KNOWN_BASES doc
        // comment always said "Channel-tier, Trusted (like the loop
        // tools)" (since 51a711d), but the base was never actually
        // added to CEILING_TRUSTED — so it silently resolved to
        // Kernel-only, and no Trusted-tier chat turn or autonomous-loop
        // iteration could ever invoke the tool it gates.
        assert!(TrustTier::Trusted.default_ceiling().grants(&s("team.run")));
        assert!(!TrustTier::SemiTrusted
            .default_ceiling()
            .grants(&s("team.run")));
        assert!(!TrustTier::Untrusted.default_ceiling().grants(&s("team.run")));
        assert!(TrustTier::Kernel.default_ceiling().grants(&s("team.run")));
        assert_eq!(
            TrustTier::min_for_scope(&s("team.run")),
            TrustTier::Trusted
        );
    }

    #[test]
    fn team_run_channel_is_known_but_granted_by_no_real_tier() {
        let s = |x: &str| Scope::parse(x).unwrap();
        assert!(KNOWN_BASES.contains(&"team.run.channel"));
        assert!(!TrustTier::SemiTrusted
            .default_ceiling()
            .grants(&s("team.run.channel")));
        assert!(!TrustTier::Trusted
            .default_ceiling()
            .grants(&s("team.run.channel")));
        assert!(!TrustTier::Untrusted
            .default_ceiling()
            .grants(&s("team.run.channel")));
        // Kernel grants every KNOWN_BASES entry unconditionally — pre-existing,
        // unrelated behavior this base does not special-case around.
        assert!(TrustTier::Kernel
            .default_ceiling()
            .grants(&s("team.run.channel")));
    }

    #[test]
    fn git_read_is_in_trusted_ceiling_only() {
        // Regression for a second real bug of the same shape (found
        // 2026-07-07 via Chapter Almanac's full-93-base tier audit,
        // triggered by the team.run fix above): Amendment A12
        // (fc5ec30) added `git.read` to KNOWN_BASES only, never to
        // CEILING_TRUSTED, so `git.status`/`git.diff` — two of the
        // "thirteen tools forever" locked substrate tools — were
        // unreachable from any Trusted-tier channel since Phase 109
        // (2026-05-28). docs/TOOLS.md always documented them as
        // Trusted with no caveat.
        assert!(TrustTier::Trusted.default_ceiling().grants(&s("git.read")));
        assert!(!TrustTier::SemiTrusted
            .default_ceiling()
            .grants(&s("git.read")));
        assert!(!TrustTier::Untrusted.default_ceiling().grants(&s("git.read")));
        assert!(TrustTier::Kernel.default_ceiling().grants(&s("git.read")));
        assert_eq!(
            TrustTier::min_for_scope(&s("git.read")),
            TrustTier::Trusted
        );
    }

    #[test]
    fn every_chapter_f_integration_base_reaches_its_documented_trusted_tier() {
        // Audit triggered by the team.run finding above: every Chapter F
        // third-party-tool-process base (Gmail/Calendar/Drive/Notion/
        // Obsidian/n8n/Contacts) carries a KNOWN_BASES doc comment
        // promising "Trusted-tier only by default" — the same promise
        // team.run's comment made and didn't keep. Checked each one
        // against `min_for_scope` (2026-07-07): all 15 land on Trusted
        // exactly as documented, so this chapter has no team.run-style
        // gap today. Kept as a permanent regression guard — a future
        // integration base that repeats the missing-ceiling-entry
        // mistake fails here immediately instead of silently landing at
        // Kernel until someone happens to look at the live catalog.
        for base in [
            "email.read",
            "email.write",
            "email.send",
            "calendar.read",
            "calendar.write",
            "drive.read",
            "drive.write",
            "notion.read",
            "notion.write",
            "obsidian.read",
            "obsidian.write",
            "n8n.read",
            "n8n.write",
            "contacts.read",
            "contacts.write",
        ] {
            assert_eq!(
                TrustTier::min_for_scope(&s(base)),
                TrustTier::Trusted,
                "{base} should reach Trusted tier per its KNOWN_BASES doc comment"
            );
        }
    }

    // ---- Phase 62 Task 2: `notify.send` scope base ----

    #[test]
    fn notify_send_parses_bare_and_qualified_forms() {
        let bare = Scope::parse("notify.send").expect("bare form must parse");
        assert_eq!(bare.base(), "notify.send");
        assert_eq!(bare.qualifier(), None);

        let qualified = Scope::parse("notify.send:phone").expect("qualified form must parse");
        assert_eq!(qualified.base(), "notify.send");
        assert_eq!(qualified.qualifier(), Some("phone"));
    }

    #[test]
    fn notify_send_rejects_wildcard_qualifier() {
        // Same Phase 14 / Phase 62 design rationale as
        // role.switch:* — the unqualified form is already the
        // wildcard under Rule 2, so a literal `:*` qualifier is
        // redundant and ambiguous. Reject at parse time.
        assert!(
            Scope::parse("notify.send:*").is_none(),
            "notify.send:* must be rejected; use bare notify.send \
             for the any-target wildcard"
        );
    }

    #[test]
    fn notify_send_unqualified_grants_qualified_target() {
        // Rule 2 (unqualified held grants qualified needed)
        // dispatches through SimpleGlob and works for free.
        assert!(s("notify.send:phone").is_granted_by(&s("notify.send")));
        assert!(s("notify.send:ops-webhook").is_granted_by(&s("notify.send")));
    }

    #[test]
    fn notify_send_qualified_does_not_grant_unqualified() {
        // Rule 4: a role holding only `notify.send:phone` cannot
        // claim unqualified notify rights. Matches the
        // `role.switch:researcher` attenuation guarantee.
        assert!(!s("notify.send").is_granted_by(&s("notify.send:phone")));
    }

    #[test]
    fn notify_send_qualified_grants_same_target_only() {
        // Rule 3 via SimpleGlob dispatch. Target-name qualifiers
        // are bare identifiers — exact-string equality after
        // glob_matches collapses on metacharacter-free needles.
        assert!(s("notify.send:phone").is_granted_by(&s("notify.send:phone")));
        assert!(!s("notify.send:phone").is_granted_by(&s("notify.send:laptop")));
    }

    #[test]
    fn notify_send_is_in_trusted_ceiling_only() {
        // Q2(a) at sign-off: Trusted only. SemiTrusted and
        // Untrusted both omit it — notifications can leak data
        // across trust boundaries (a SemiTrusted Telegram
        // operator must not be able to coerce the agent into
        // POSTing the address book to a webhook).
        assert!(TrustTier::Trusted.default_ceiling().grants(&s("notify.send")));
        assert!(TrustTier::Trusted
            .default_ceiling()
            .grants(&s("notify.send:phone")));

        assert!(!TrustTier::SemiTrusted
            .default_ceiling()
            .grants(&s("notify.send")));
        assert!(!TrustTier::SemiTrusted
            .default_ceiling()
            .grants(&s("notify.send:phone")));

        assert!(!TrustTier::Untrusted
            .default_ceiling()
            .grants(&s("notify.send")));

        // Kernel holds every KNOWN_BASES entry — the
        // `ceiling_kernel_grants_everything` invariant.
        assert!(TrustTier::Kernel.default_ceiling().grants(&s("notify.send")));
    }

    // ---- Reflexivity: grants(&self, &self) ----

    #[test]
    fn grants_is_reflexive_for_all_practical_scope_forms() {
        let cases = [
            "fs.read",
            "fs.write:/home/julian/**",
            "net.fetch:https://example.com/api",
            "shell.exec:git,ls,cat",
            "memory.read:scope:public:*",
            "llm.call",
            "audit.read:public",
            "mission.create",
            "mission.gate:gate-001",
        ];
        for raw in &cases {
            let scope = s(raw);
            let set = CapabilitySet::from_scopes([scope.clone()]);
            assert!(
                set.grants(&scope),
                "grants must be reflexive for {raw}",
            );
        }
    }

    // ---- End-to-end: D1 scenario 3 ("rm -rf from Telegram") ----

    #[test]
    fn d1_scenario3_rm_rf_from_telegram_is_denied() {
        // Agent is "near-full-power" — holds shell.exec.
        let agent = CapabilitySet::from_scopes([
            s("shell.exec"),
            s("fs.read"),
            s("fs.write"),
            s("fs.delete"),
            s("llm.call"),
        ]);
        // Channel reports SemiTrusted (Telegram).
        let tier = TrustTier::SemiTrusted;
        let effective = agent.intersect(tier.default_ceiling());

        // The attempted tool call: shell.exec:rm
        let needed = s("shell.exec:rm");
        assert!(
            !effective.grants(&needed),
            "Tier 2 ceiling must deny shell.exec regardless of agent caps"
        );
    }

    /// Phase 113 — pin the `KNOWN_BASES` count against the A3
    /// amendment file. If a future phase adds a base without
    /// updating the amendment, this test fails and forces the
    /// docs catch-up to ship in the same phase. The single
    /// source of truth is still `KNOWN_BASES` in this file;
    /// this test just keeps the operator-readable inventory
    /// honest.
    #[test]
    fn known_bases_count_matches_phase_143_a3_addendum() {
        // See `docs/amendments/2026-04-17-capability-taxonomy-growth.md`
        // — the latest addendum (Phase 143) lists every entry.
        // Phase 143 adds the budget.read + budget.write bases
        // for the Chapter G #2 third-party tool process
        // (aivyx-toolkit budget tracking — `budget.summary`
        // and `budget.record`). Phase 173 adds loop.next +
        // loop.complete for the Autonomous Loop (the Aivyx
        // Ralph loop) backlog tools. Phase 175 adds loop.note
        // for the loop progress log. Phase 183 adds remind.read +
        // remind.write for the reminder tools (everyday-PA #1).
        // Phase 184 adds skills.write for conversational
        // skill-teaching (skills.teach / update / forget).
        // The Kitchen/BOH vertical pack (docs/VERTICAL_PACKS.md)
        // adds kitchen.read, kitchen.write + kitchen.order.send for
        // the gated write surface, and kitchen.haccp.log for the
        // append-only food-safety (HACCP) compliance log.
        // Chapter O adds `workspace` (one base for the agent's own
        // workspace.* tools).
        // Chapter Contacts adds contacts.read + contacts.write for the
        // aivyx-contacts third-party tool process (Google People API;
        // first Broaden-track everyday-PA domain, audit F4).
        // Chapter Forge (FG.2) adds git.write — the destructive git
        // sibling A12 anticipated — gating the git.commit tool.
        // Chapter Lattice adds graph.read — the read gate for the
        // graph.query knowledge-graph traversal tool (infrastructure,
        // no P10 amendment).
        // Chapter Abacus (AB.1) adds calc.eval — the first pure-compute
        // utility in the aivyx-toolkit pack, and the first toolkit base
        // reachable at SemiTrusted (no I/O, no operator data). AB.2 adds
        // convert.units (the convert group: convert.units + convert.time);
        // AB.3 adds date.compute (the date group: date.diff + date.add).
        // Phase 191 adds notify.dispatch — the daemon-side sink gate for
        // a tool process's own unprompted DispatchNotification wire
        // frame (distinct from the model-invoked notify.send tool),
        // Trusted-tier-only at the ceiling like notify.send itself.
        // Aivyx-Vision Milestone 1 adds vision.generate — the
        // vision.generate_svg tool process's one base (SemiTrusted-reachable,
        // see the KNOWN_BASES doc comment for why).
        // Any change here means updating the addendum's
        // "Current full enumeration" section in the same PR.
        assert_eq!(
            KNOWN_BASES.len(),
            96,
            "If KNOWN_BASES grew, also update the A3 addendum's \
             latest count + per-base list."
        );
    }

    #[test]
    fn vision_generate_is_a_known_base_reachable_at_semitrusted() {
        // Aivyx-Vision Milestone 1 — vision.generate must parse (it's a
        // real KNOWN_BASES entry now) and must be reachable at SemiTrusted:
        // it's a narrower, sanitized form of llm.call, which is already
        // SemiTrusted-reachable. Assertion shape matches this file's own
        // precedent for other SemiTrusted-reachable toolkit bases (e.g.
        // `graph_read_base_parses_and_is_trusted_only`,
        // `contacts_bases_are_trusted_only`): `CapabilitySet::grants`, not
        // `Scope::is_granted_by` (a different, scope-vs-scope check) and
        // not a nonexistent `CapabilitySet::contains`.
        let scope = Scope::parse("vision.generate").expect("must be a known base");
        assert!(
            CEILING_SEMITRUSTED.grants(&scope),
            "vision.generate must be reachable at SemiTrusted -- it's a narrower, \
             sanitized form of llm.call, which is already SemiTrusted-reachable"
        );
    }

    #[test]
    fn vision_generate_is_absent_from_untrusted_ceiling() {
        let scope = Scope::parse("vision.generate").expect("must be a known base");
        assert!(
            !CEILING_UNTRUSTED.grants(&scope),
            "vision.generate must not be reachable at Untrusted by default"
        );
    }

    #[test]
    fn budget_read_and_write_bases_parse() {
        // Phase 143 — both new bases parse via
        // Scope::parse exactly as their siblings do.
        let r = Scope::parse("budget.read").expect("budget.read");
        assert_eq!(r.base(), "budget.read");
        let w = Scope::parse("budget.write").expect("budget.write");
        assert_eq!(w.base(), "budget.write");
    }

    #[test]
    fn graph_read_base_parses_and_is_trusted_only() {
        // Chapter Lattice — the knowledge-graph read base parses (bare,
        // like skills.list) and sits at Trusted+ only.
        let g = Scope::parse("graph.read").expect("graph.read");
        assert_eq!(g.base(), "graph.read");
        assert!(
            CEILING_TRUSTED.grants(&g),
            "Trusted ceiling must grant graph.read"
        );
        assert!(
            !CEILING_SEMITRUSTED.grants(&g),
            "SemiTrusted ceiling must deny graph.read by default"
        );
    }

    #[test]
    fn git_write_base_parses_and_is_trusted_only() {
        // Chapter Forge (FG.2) — the new destructive git base parses
        // (qualified by repo path, like git.read) and sits at Trusted+
        // only: held at the Trusted ceiling, denied at SemiTrusted.
        let w = Scope::parse("git.write").expect("git.write");
        assert_eq!(w.base(), "git.write");
        let repo = Scope::parse("git.write:/home/me/projects/aivyx")
            .expect("git.write with repo-path qualifier");
        assert_eq!(repo.base(), "git.write");

        let needed = Scope::parse("git.write:/home/me/projects/aivyx").unwrap();
        assert!(
            CEILING_TRUSTED.grants(&needed),
            "Trusted ceiling must grant git.write"
        );
        assert!(
            !CEILING_SEMITRUSTED.grants(&needed),
            "SemiTrusted ceiling must deny git.write (writing history is Trusted-only)"
        );
    }

    #[test]
    fn contacts_read_and_write_bases_parse() {
        // Chapter Contacts — both People API bases parse via
        // Scope::parse exactly as their drive.* siblings do.
        let r = Scope::parse("contacts.read").expect("contacts.read");
        assert_eq!(r.base(), "contacts.read");
        let w = Scope::parse("contacts.write").expect("contacts.write");
        assert_eq!(w.base(), "contacts.write");
    }

    #[test]
    fn contacts_bases_are_trusted_only() {
        // Chapter Contacts — contacts.* sits in the Trusted ceiling
        // only, matching every other Chapter F third-party-tool-process
        // base. SemiTrusted / Untrusted operators get zero contacts
        // access without an explicit per-role grant (Phase 62 Q2(a)).
        for base in ["contacts.read", "contacts.write"] {
            let scope = Scope::parse(base).expect("contacts base parses");
            assert!(
                TrustTier::Trusted.default_ceiling().grants(&scope),
                "Trusted ceiling must hold {base}"
            );
            assert!(
                !TrustTier::SemiTrusted.default_ceiling().grants(&scope),
                "SemiTrusted ceiling must NOT hold {base}"
            );
            assert!(
                !TrustTier::Untrusted.default_ceiling().grants(&scope),
                "Untrusted ceiling must NOT hold {base}"
            );
        }
    }
}
