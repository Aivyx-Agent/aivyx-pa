# Persistent Interactive Terminal Sessions (Chapter Tether)

> **Status:** 🔵 **SCOPED, NOT STARTED — deferred to post-v1.0.0.** Operator asked
> "should the agent have their own terminal?", clarified to mean a **real
> interactive session** (persistent pty, not one-shot `shell.exec`). This
> chapter is a scoping document written 2026-07-07 to capture the design
> questions before any implementation — see [`POST_V1_ROADMAP.md`](POST_V1_ROADMAP.md)
> for the parking-lot entry and trigger condition. Named for the same
> equestrian-restraint family as [Reins](AUTONOMY.md)/[Bridle](BRIDLE.md)/
> [Halter](HALTER.md): a tether keeps something bounded while it moves freely
> *within* that bound — the property a persistent-but-killable shell session needs.

## 1. Why this chapter

`shell.exec` is deliberately one-shot: spawn, run, capture stdout/stderr/exit
code, die. No cwd persists across calls beyond the sandbox root, no background
process outlives the call, no REPL can stay warm between turns. That's the
right default — it's why `shell.exec`'s guard story (Ward/Portcullis text-scan,
sandbox fence, Trusted-tier-only) is as simple as it is. But it can't serve a
real workflow shape: start a dev server and keep checking on it across a
conversation, tail a log while doing other work, keep a Python/node REPL warm
through a multi-step task, run `git rebase -i`-style interactive tools. A
one-shot tool structurally cannot do any of that — the process is gone the
moment the call returns.

## 2. What "real interactive" actually requires

Four things, in ascending order of how hard they are to get right.

### 2a. A daemon-owned, long-lived session registry
Mirrors the existing pattern: `TeamMissionService`/`LoopDriver`/the schedule
driver all live in `aivyx-channel` as daemon-owned services with their own
registries, outliving any single turn. A `TerminalDriver` would be the same
shape: `HashMap<TerminalId, TerminalSession>`, each wrapping a pty (e.g. via
the `portable-pty` crate) + its child shell process + an output ring-buffer
the tool reads from.

### 2b. A small tool family, not one tool
Mirrors `file_watch.*`/`webhook.*`'s create/list/delete shape:
- `terminal.open` — start a session (optional `cwd` inside the sandbox),
  returns a `terminal_id`.
- `terminal.send` — write a line/bytes to the pty's stdin.
- `terminal.read` — read output accumulated since the last read (or since a
  cursor), ANSI-stripped before the model sees it — raw escape codes and
  progress-bar redraws are noise, not signal, for an LLM.
- `terminal.close` — kill the session and its child process tree.
- `terminal.list` — enumerate live sessions (needed for recovery after a
  context compaction, or just multi-session awareness).

### 2c. A new capability base, not a shell.exec variant
The lifecycle semantics (a session you can keep talking to) are different
enough from shell.exec's one-shot semantics that this needs its own base —
same reasoning that gave `git.write` its own base distinct from `git.read`
(Chapter Forge), or `team.run` its own base distinct from `team.delegate`
(Chapter L.7). Under **P10** (substrate tool count, amendment-gated) this
tool family would need its own amendment, the same process A12/A13b went
through for `git.read`/`git.write`. Given the severity — a *live* shell you
can keep steering is strictly more powerful than a one-shot command — it
should sit at Trusted tier at minimum (same ceiling as `shell.exec`), and
`terminal.close`/session-kill is plausibly irreversible-classified the same
way `fs.delete`/`shell.exec` are.

### 2d. Lifecycle policy — the question that gates everything else
Four shapes, increasing in both usefulness and cost:

| Option | What it means | Cost |
|---|---|---|
| **Turn-scoped** | Dies at the end of the current turn | Safest, but defeats the actual ask — no "keep checking on the dev server later" |
| **Session-scoped** | Survives across turns within one chat session, dies when the session ends | The natural middle ground |
| **Daemon-restart-survivable** | Survives a daemon restart too (like missions/schedules, checkpointed) | Much harder — a live pty's process tree can't be checkpointed the way a mission's plan state can. Would need the child process owned by something outside the daemon's own process tree (a `tmux`/`screen`-style detached multiplexer the daemon reattaches to), not the daemon spawning it directly |
| **+ idle-timeout / max-lifetime backstop** | Applies regardless of the above — an abandoned session doesn't run forever | Mirrors Bridle's `turn_timeout_secs` philosophy, applied at the session level instead of the turn level |

**Recommendation to record (not locked — this is a scoping doc):**
session-scoped lifetime + an idle-timeout backstop is the natural first cut.
Daemon-restart-survivability is a real stretch goal worth naming but not
required for a first version.

## 3. The guard gap this chapter cannot fully close

Ward/Portcullis today are a **one-shot pre-execution text scan** of a
complete command string. That doesn't extend cleanly to a persistent shell:

- `terminal.open` can still gate on `cwd` (the same fs.metadata-style
  sandbox-root check `shell.exec` already does).
- `terminal.send` should still scan the line being written, mirroring
  `shell.exec`'s existing text-scan, applied per write instead of once.
- But once inside an arbitrary REPL (`python3`, `node`, …), "sensitive path
  reference in shell command text" stops being a coherent concept —
  `open('/home/user/.ssh/id_rsa').read()` inside a Python REPL bypasses a
  shell-command-shaped text scan entirely, and there is no realistic way to
  block "python3" from being a valid line to send to a shell.

This is a genuine, honestly-named residual risk, not an implementation
detail to paper over. It's the same class of risk `shell.exec` already
carries today (a Trusted-tier channel can already open a REPL via
`shell.exec` and this problem already exists in miniature — a one-shot
`shell.exec("python3 -c \"...\"")` call has the identical bypass), so Tether
doesn't *introduce* a new category of risk so much as it makes the existing
one persistently exploitable across an arbitrarily long session instead of
one call. That distinction matters when this chapter is actually scoped for
real: the honest framing is "no worse in kind, but the exposure window is
now open-ended instead of one call," and whatever mitigates that (the
idle-timeout backstop from §2d is the main lever) should be treated as load-bearing, not cosmetic.

## 4. Output handling (Bulwark)

Terminal output needs the same "fence untrusted output as DATA, not
instructions" treatment `web.fetch`/`web.extract` output already gets —
`cat`-ing an attacker-influenced file, or reading `git log` on an untrusted
repo, can carry the same prompt-injection payloads a fetched web page can.

## 5. Draft scope

**In (first cut):** pty-backed session driver; `terminal.open/send/read/close/list`
tools; a new Trusted-tier capability base (amendment-gated per P10); a
per-line Portcullis-style scan on `terminal.send`; ANSI-stripped +
Bulwark-fenced output on `terminal.read`; an idle-timeout + max-lifetime
backstop.

**Out (first cut):** daemon-restart-survivable sessions (the tmux-style
detach); more than one terminal per session (start with a single default,
like Herald's single default notify target); any capability escalation
beyond what `shell.exec` already has — this chapter widens *shape*
(persistence), not *reach*.

## 6. Open questions (resolve when this chapter actually starts)

- **OQ-1 — lifetime.** Session-scoped + idle-timeout (leaning this) vs.
  turn-scoped vs. daemon-restart-survivable.
- **OQ-2 — the REPL guard gap (§3).** Accept the residual risk as
  qualitatively unchanged from `shell.exec`'s existing exposure (leaning
  this, given §3's reasoning) vs. attempt some form of input classification
  vs. restrict to shell-only with no REPL affordance at all (doesn't
  actually close the gap — nothing stops `python3 -c "..."` as a single
  line — so likely not worth the restriction).
- **OQ-3 — one terminal or many.** Start with a single default session per
  chat session (simplest) vs. named multi-terminal from the start.
- **OQ-4 — Studio/TUI live view.** Does watching output stream in live
  belong in this chapter, or is it a separate follow-on the way the Studio
  Tools screen followed Atlas's catalog by a separate chapter (Almanac)?
  Leaning follow-on — get the daemon-side session model right first.

## 7. Trigger condition

Post-v1.0.0. Concrete trigger: a real dogfood workflow that one-shot
`shell.exec` genuinely cannot serve (e.g., the operator wants Jarvis to
start and keep monitoring a long-running dev server or watch-build across
a conversation), or a vertical pack's real need makes the case concretely —
not speculative "this would be nice."

---

*Chapter Tether is a scoping document, not a build plan. The lifecycle
question (§2d/OQ-1) is the one everything else hangs off of — resolve that
first, for real, before writing any pty-handling code.*
