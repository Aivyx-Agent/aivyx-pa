# Operator Access Levels — how much of the machine the agent may reach (Chapter N)

> **Status:** design contract. This is the spec Chapter N scaffolds from
> (mirrors `docs/HEADLESS_MODE.md` / `docs/WEB_MISSION_CONTROL.md`).
>
> Today every filesystem and shell action runs inside a single sandbox root
> (`[fs] root`, default `~/aivyx-pa-sandbox`). That is correct for an *untrusted*
> agent but limiting for a **personal** assistant on the operator's own
> machine — asked to "list the folders in my home directory," the agent can
> only see its sandbox.
>
> **Access levels** let the operator *choose*, as an explicit and audited
> Setting, how far their agent reaches — from the current sandbox up to the
> whole machine — without hand-crafting capability scopes. The choice widens
> the **boundary**; it never removes the capability/audit machinery that keeps
> the expansion safe. Default stays `sandbox`: an existing install, or one with
> no `[access]` section, behaves byte-for-byte as today.

---

## 0. Decisions (locked at scope time)

| Decision | Choice | Why |
|---|---|---|
| Default level | **`sandbox`** | Absent `[access]` ⇒ today's behavior, unchanged. Expansion is strictly opt-in. |
| The reach lever | **`fs_root`** | `fs_root` already derives both the `fs.*:<root>/**` scopes and `shell.exec:cwd:<root>/**` (Local only). Widening it widens both; nothing else needs per-level plumbing. |
| Who an expanded level applies to | **Local (Trusted) operator only** | The trust-tier ceiling already attenuates remote channels — `shell.exec` is `Ok(None)` for Telegram/Discord/Slack/webhooks. "Full" for the operator is still sandboxed for the internet. This is inherent, not a new knob. |
| Safety posture | **`confirm_destructive` (default on at every level unless autonomy is `unleashed`)** | Broad reach + a hard confirm-first gate on *irreversible* ops (delete, overwrite, destructive shell, outbound). Protects the operator from the agent's own mistakes — a hallucinated `rm` asks first. Everything non-destructive runs friction-free. |
| Authority | **Boundary widens; machinery unchanged** | Same capability attenuation, same HMAC audit chain, same headless gate policy (Chapter H). A level grants *reach*, never *un-auditability* and never authority a remote channel could inherit. |

---

## 1. What exists today

- **`fs_root` is the single reach lever.** `build_shell_exec_for_channel`
  (`crates/aivyx-cli/src/bin/aivyx.rs`) builds `shell.exec:cwd:<fs_root>/**` and
  grants it **only to the Local (Trusted) channel**. fs tools are scoped
  `fs.read|write|metadata:<canonical fs_root>/**`; `fs.delete` sits behind a
  Local-only gate. So `fs_root` alone decides how far fs *and* shell reach.
- **Trust-tier ceiling auto-attenuates.** Effective grant =
  `(operator-held ∪ role capability_scopes) ∩ tier_ceiling`. `shell.exec` is not
  in the SemiTrusted/Untrusted ceilings, so remote channels never receive it,
  regardless of the operator's chosen level.
- **"Confirm destructive" is only a *soft* hint today** — a behavioral-constraint
  prompt string (`"always confirm destructive shell commands"`), not enforced.
  Delivering "full access + confirm irreversible" needs a real tool-level gate.
- **The confirm-first substrate already exists.** `ToolOutcome::RequiresEscalation`
  + the `confirmed: true` pattern (kitchen.order.send, skills.teach) park a turn
  behind an operator gate; Chapter H already makes headless runs *reject* such
  gates rather than hang. N.5 reuses this for destructive fs/shell ops.

---

## 2. The levels

| Level | `fs_root` | Granted to Local/Trusted | Confirm posture | Intended use |
|---|---|---|---|---|
| `sandbox` *(default)* | `~/aivyx-pa-sandbox` | `fs.read` `fs.write` `fs.metadata` | n/a | untrusted / shared agent — today's behavior |
| `workspace` | operator-chosen dir | + `shell.exec`, `fs.delete` (within dir) | on | a project / working tree |
| `home` | `$HOME` | `fs.*` (incl. delete) + `shell.exec` | on | the personal-assistant default |
| `full` | `/` | `fs.*` (incl. delete) + `shell.exec` | on | whole machine (explicit warning) |
| `custom` | operator-specified root(s) | operator-specified | operator-set | escape hatch |

`confirm_destructive` is orthogonal to the level (an operator can run `home`
with it off, accepting the risk). It defaults **on** for `home`/`full`/`workspace`
and is **n/a** for `sandbox` (nothing destructive escapes the sandbox anyway).

Remote channels are unaffected by the level — they always see the
tier-ceiling-attenuated set (no shell, narrow fs), whatever the operator picks.

---

## 3. The `[access]` Settings section

```toml
[access]
level = "home"            # sandbox | workspace | home | full | custom
root  = "/home/julian"    # required for workspace/custom; derived for the rest
confirm_destructive = true
```

**Resolution rules** (`aivyx-config`):
- `level` selects the default `fs_root`: `sandbox`→`~/aivyx-pa-sandbox`,
  `home`→`$HOME`, `full`→`/`, `workspace`/`custom`→the declared `root`.
- An explicit `[fs] root` (or `[access] root`) overrides the derived default.
- **No `[access]` section ⇒ `sandbox`** ⇒ existing configs unchanged.
- `confirm_destructive` defaults on at every level, `sandbox` included (the operator's own files live there), unless the autonomy level is `unleashed`; an explicit `[access] confirm_destructive` wins. For `fs.delete` and overwrites the confirmation must come from the operator: `confirmed: true` only counts in a turn *after* the tool refused that same path, so the model has to ask and wait for the reply.

`[access]` is sugar over the `fs_root` + posture the binary already consumes —
the operator sets one stanza instead of hand-writing capability scopes.

---

## 4. Surfaces (the Settings System)

1. **Config** — the `[access]` section above (hand-edit or via the command).
2. **`aivyx-pa init` wizard** — an access-level chooser replaces the bare
   "Sandbox root" prompt: it names each level and its risk, and requires an
   explicit extra confirmation for `home`/`full`.
3. **`aivyx-pa access` command** — `show` (current level + resolved roots/scopes)
   and `set <level> [--root P] [--yes]` (rewrites `aivyx-pa.toml`; re-confirms
   `home`/`full`).

---

## 5. Phase plan

| Phase | Deliverable |
|---|---|
| **N.0** | This design contract. |
| **N.1** | `AccessLevel` + `[access]` section + resolution in `aivyx-config` (default `sandbox` = unchanged). Unit tests. |
| **N.2** | Binary wiring: resolved access feeds `fs_root` + the operator/role grant set for the level; remote-channel attenuation verified. |
| **N.3** | `aivyx-pa init` access-level chooser (warning + confirm for `home`/`full`). |
| **N.4** | `aivyx-pa access show` / `set <level>` command. |
| **N.5** | Hard confirm-first gate for irreversible ops (`fs.delete`, `fs.write` overwrite, destructive `shell.exec`) when `confirm_destructive` is on. |
| **N.6** | `examples/templates/aivyx-full-access.toml`, docs polish, memory note. |

**Status: N.0–N.6 complete and verified live** (qwen3.6:27b — `home` lists the
real `$HOME`; the confirm-first delete gate refuses → operator approves →
re-run `confirmed: true` → deleted).

**Default-role vs explicit role.** The level table's "granted to Local/Trusted"
column describes the *default role* (no `[[role]]`), where the `backcompat_floor`
grants the full fs + shell set at `fs_root`. An explicit `[[role]]` with
`capability_scopes` is the operator deliberately NARROWING the tool surface —
the level still sets the `fs_root` boundary, but the role's declared scopes win
over the default grant. So `level = "home"` + a narrow role = home-rooted but
tool-restricted.

**Deferred follow-ons.** (1) Destructive `shell.exec` detection — gating
arbitrary shell commands needs fragile command parsing; today only `fs.delete`
and `fs.write`-overwrite gate. (2) A policy-aware operator GATE with auto-resume
+ headless-always-block for confirm-first — the current gate is the
conversational `confirmed: true` checkpoint (model shows the op, operator
approves, model re-calls confirmed); a true `RequiresEscalation` gate that
survives headless needs the single-agent gate-resume machinery Chapter H
deferred.

---

## 6. Invariants

- **Default is sandbox.** No `[access]` section ⇒ byte-identical to today;
  expansion is always opt-in.
- **Levels apply to the local operator only.** Remote/networked channels stay
  tier-attenuated — granting yourself `full` never hands shell/full-fs to a
  webhook or messaging bot. The classic RCE surface stays closed.
- **No authority widening beyond reach.** A level changes the `fs_root`
  boundary and the operator-held fs/shell bases; it does not raise trust tiers,
  bypass the budget gate, or remove anything from the HMAC audit chain. Every
  action — at every level — stays auditable.
- **Irreversible actions stay gated.** With `confirm_destructive` on, delete /
  overwrite / destructive shell / outbound ops ask first (interactively) or
  reject (headless, per Chapter H). The seatbelt is on by default for broad
  levels.
- **`home`/`full` are deliberate.** Selecting them requires an explicit
  confirmation in the wizard and in `aivyx-pa access set`.
