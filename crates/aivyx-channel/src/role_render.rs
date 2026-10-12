//! Human-readable rendering of a role's effective capability
//! envelope — the library-side home for the `--print-role` debug
//! surface.
//!
//! ## History
//!
//! [`render_role_envelope`], [`ChannelKind`], and the two private
//! helpers below (`build_display_floor`, `drop_reason_for`) were
//! written in Phase 13 Task 4 (`--print-role` first landed) and
//! extended in Phase 14 Task 4 (reachable role.switch target
//! enumerator). They all originally lived inside
//! `crates/aivyx-channel/src/bin/aivyx.rs` as binary-private
//! items. Phase 14 Task 5 recorded the lift into the channel
//! library as an optional cleanup; Phase 15 Task 3 picks that
//! cleanup up and executes it.
//!
//! ## Why the lift
//!
//! Two reasons, neither structural on its own but both
//! compounding:
//!
//! 1. **Test reach.** While the renderer was binary-private, its
//!    tests had to live inside `src/bin/aivyx.rs`'s `mod tests`
//!    (integration tests cannot reach binary internals in Rust).
//!    The binary's test module has been the single longest section
//!    in the crate since Phase 10 and the renderer's seven
//!    functional tests made it noticeably worse. Lifting the
//!    renderer makes those tests reachable from
//!    `crates/aivyx-channel/tests/role_render_e2e.rs`, which
//!    trims the binary and puts the tests next to their siblings.
//!
//! 2. **Cross-crate reachability.** Future downstream consumers
//!    (the Daemon Migration milestone's IPC frontends, for one)
//!    will need the "explain a role's effective envelope" surface
//!    without depending on the `aivyx-pa` binary. Keeping it
//!    library-side from Phase 15 onward avoids a second lift later.
//!
//! ## Surface
//!
//! - [`ChannelKind`] — which adapter a session runs on. The
//!   renderer uses it to decide whether `shell.exec` goes into
//!   the displayed backcompat floor (Local: yes, Telegram: no),
//!   mirroring the registration-time gate
//!   `build_shell_exec_for_channel` applies in the binary.
//! - [`render_role_envelope`] — pure function that takes a role
//!   name, an [`AivyxConfig`], and a [`ChannelKind`], and returns
//!   a multi-section string suitable for `println!` or for the
//!   integration tests in `tests/role_render_e2e.rs`. Errors on
//!   unknown role names with a typed list of the known names.
//!
//! The two helpers are deliberately module-private: they are
//! implementation details of `render_role_envelope` and no
//! caller outside this file should be reaching for them.

use std::fmt::Write as _;
use std::path::Path;

use aivyx_capability::{CapabilitySet, Scope};
use aivyx_config::{AivyxConfig, Role};

use crate::assemble_role_envelope;

/// Which channel adapter a session runs on.
///
/// Phase 8 Task 4 introduced the first non-`Local` variant
/// (`Telegram`). Pre-Phase-8 configs only knew `Local`, and the
/// `Default` choice stays `Local` so a bare `aivyx-pa` invocation
/// opens the local REPL with no flags required.
///
/// Lifted from the binary to the channel library in Phase 15
/// Task 3 alongside [`render_role_envelope`]. The binary still
/// owns the CLI parsing that constructs a `ChannelKind` from
/// `--channel local|telegram`, but the type itself is
/// library-side so downstream callers can share the same
/// `local`/`telegram` discriminator without re-declaring it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelKind {
    Local,
    Telegram,
    /// Phase 107 — Discord adapter. Same `SemiTrusted` tier
    /// posture as `Telegram` for registration-time tool
    /// gating (`shell.exec` / `fs.delete` are not registered
    /// for Discord; `web.fetch` is) and the same
    /// non-`Local` rendering for role-envelope display.
    Discord,
    /// Phase 108 — Slack adapter. Same `SemiTrusted` tier
    /// posture as `Telegram` and `Discord` — the three
    /// remote-channel adapters share registration-time gates
    /// on destructive tools (`shell.exec`, `fs.delete`)
    /// symmetrically.
    Slack,
    /// Matrix adapter — same SemiTrusted registration posture as Telegram/Discord/Slack
    Matrix,
    /// Proton Mail (email) channel — same SemiTrusted registration posture
    Proton,
    /// Signal channel — same SemiTrusted registration posture
    Signal,
    /// Phase 135 — Voice I/O channel. Operator speaks
    /// through the microphone; agent speaks back through
    /// the speakers. Same `Trusted` tier posture as
    /// `Local` — voice runs in-process on the operator's
    /// own machine; the audio loop never leaves the host.
    Voice,
}

/// Construct the display-time backcompat floor for the given
/// channel.
///
/// This duplicates the floor-construction logic from the binary's
/// `run()` startup path. The duplication was accepted in Phase 13
/// Task 4 and persists here: factoring the production floor
/// builder and this display builder into a single helper would
/// require pulling channel kind, sandbox path, and shell.exec
/// gate together at a layer above both call sites, which is a
/// larger structural shift than either a Phase 13 or Phase 15
/// working session warrants.
///
/// Returns `(floor, canonicalized)`. `canonicalized == false`
/// means the configured sandbox directory did not exist (or the
/// canonicalization failed for another reason) and the floor's
/// `fs.read`/`fs.write` scopes use the as-written path. The
/// renderer uses the flag to print a footnote explaining the
/// difference between display and production floors.
fn build_display_floor(
    fs_root: &Path,
    channel_kind: ChannelKind,
) -> Result<(Vec<Scope>, bool), String> {
    let (root_string, canonicalized) = match std::fs::canonicalize(fs_root) {
        Ok(c) => (c.display().to_string(), true),
        Err(_) => (fs_root.display().to_string(), false),
    };
    let fs_read_scope = Scope::parse(&format!("fs.read:{root_string}/**"))
        .ok_or_else(|| format!("could not parse fs.read scope for sandbox {root_string}"))?;
    let fs_write_scope = Scope::parse(&format!("fs.write:{root_string}/**"))
        .ok_or_else(|| format!("could not parse fs.write scope for sandbox {root_string}"))?;
    let mut floor: Vec<Scope> = vec![
        Scope::parse("memory.read").unwrap(),
        Scope::parse("memory.write").unwrap(),
        Scope::parse("memory.forget").unwrap(),
        fs_read_scope,
        fs_write_scope,
        Scope::parse("net.fetch").unwrap(),
    ];
    if matches!(channel_kind, ChannelKind::Local) {
        floor.push(Scope::parse("shell.exec").unwrap());
    }
    Ok((floor, canonicalized))
}

/// Render the named role's effective capability envelope into a
/// human-readable string suitable for printing. Pure function —
/// returns the string instead of writing to stdout — so tests
/// can assert on its content without redirecting global state.
///
/// Output structure:
///
/// ```text
/// role: <name>
/// channel: <local|telegram>
/// parent chain: <leaf> -> <mid> -> <root>
///
/// level 1 — <name> (declared)
///   capability_scopes: <list, or "<empty - backcompat floor will be substituted>">
///   trust_ceiling: <tier>
///
/// level 2 — <parent> (declared)
///   ...
///
/// backcompat floor (substituted for empty levels):
///   <list>
/// (optional footnote about non-canonical fs root)
///
/// effective envelope:
///   <list of surviving scopes>
///
/// dropped:
///   <scope> [from <where>]  reason: <short reason>
///   ...
///
/// reachable role.switch targets (from effective envelope):
///   <targets or "<none - this role cannot start a sub-session>">
/// ```
///
/// The "dropped" section is the load-bearing teaching feature.
/// For each level transition, it walks both sides of the
/// intersection and lists scopes that did not survive, with a
/// short reason: either "not granted by <other side>" (the base
/// is missing entirely) or "qualifier mismatch (D4 Rule 4 —
/// qualified-held cannot grant unqualified-needed)" (the base is
/// present but the qualifier shape blocks it). This is
/// deliberately less precise than reimplementing the full D4
/// rule dispatch; it is precise enough for an operator to
/// understand why a declared scope evaporated.
pub fn render_role_envelope(
    role_name: &str,
    cfg: &AivyxConfig,
    channel_kind: ChannelKind,
) -> Result<String, String> {
    let role = cfg.roles.get(role_name).ok_or_else(|| {
        let known: Vec<&str> = cfg.roles.keys().map(String::as_str).collect();
        format!(
            "role `{role_name}` is not declared in this config. Known roles: {known:?}"
        )
    })?;

    let (floor, canonicalized) = build_display_floor(&cfg.fs_root.value, channel_kind)?;

    let mut out = String::new();
    writeln!(out, "role: {role_name}").unwrap();
    writeln!(
        out,
        "channel: {}",
        match channel_kind {
            ChannelKind::Local => "local",
            ChannelKind::Telegram => "telegram",
            ChannelKind::Discord => "discord",
            ChannelKind::Slack => "slack",
            ChannelKind::Matrix => "matrix",
            ChannelKind::Proton => "proton",
            ChannelKind::Signal => "signal",
            ChannelKind::Voice => "voice",
        }
    )
    .unwrap();

    // Walk parent chain leaf-to-root and collect each level's
    // role + the level number. The chain is bounded by the
    // validator; an unbounded loop here would be a regression
    // hazard if the validator ever ships broken, so we bound it
    // explicitly the same way `assemble_role_envelope` does.
    let mut chain: Vec<&Role> = vec![role];
    {
        let mut cursor = role.parent_role.value.as_deref();
        let mut depth = 0;
        while let Some(parent_name) = cursor {
            depth += 1;
            if depth > 64 {
                break;
            }
            let Some(parent) = cfg.roles.get(parent_name) else {
                break;
            };
            chain.push(parent);
            cursor = parent.parent_role.value.as_deref();
        }
    }
    let chain_names: Vec<&str> = chain.iter().map(|r| r.name.value.as_str()).collect();
    writeln!(out, "parent chain: {}", chain_names.join(" -> ")).unwrap();
    writeln!(out).unwrap();

    for (i, level) in chain.iter().enumerate() {
        let level_num = i + 1;
        let level_name = level.name.value.as_str();
        writeln!(out, "level {level_num} - {level_name} (declared)").unwrap();
        if level.capability_scopes.value.is_empty() {
            writeln!(
                out,
                "  capability_scopes: <empty - backcompat floor will be substituted at runtime>"
            )
            .unwrap();
        } else {
            writeln!(out, "  capability_scopes:").unwrap();
            for scope in &level.capability_scopes.value {
                writeln!(out, "    {}", scope.as_str()).unwrap();
            }
        }
        writeln!(
            out,
            "  trust_ceiling: {:?}",
            level.trust_ceiling.value
        )
        .unwrap();
        writeln!(out).unwrap();
    }

    writeln!(out, "backcompat floor (substituted for empty levels):").unwrap();
    for scope in &floor {
        writeln!(out, "  {}", scope.as_str()).unwrap();
    }
    if !canonicalized {
        writeln!(
            out,
            "  (note: fs sandbox at {} did not exist or could not be canonicalized; the floor's fs.read/fs.write scopes use the as-written path. A live binary would canonicalize through any symlinks at startup.)",
            cfg.fs_root.value.display()
        )
        .unwrap();
    }
    writeln!(out).unwrap();

    // Compute the effective envelope by reusing the production
    // assembly fn, then folding in the role's declared trust
    // ceiling — exactly the same composition the production
    // `run()` path uses at the role-resolution site.
    let role_envelope = assemble_role_envelope(role, &cfg.roles, &floor);
    let role_tier_ceiling = role.trust_ceiling.value.default_ceiling();
    let effective = role_envelope.intersect(role_tier_ceiling);

    writeln!(
        out,
        "effective envelope (after intersection chain + role tier ceiling {:?}):",
        role.trust_ceiling.value
    )
    .unwrap();
    let effective_strs: Vec<&str> = effective.iter().map(|s| s.as_str()).collect();
    if effective_strs.is_empty() {
        writeln!(out, "  <empty - this role has no live capabilities>").unwrap();
    } else {
        for s in &effective_strs {
            writeln!(out, "  {s}").unwrap();
        }
    }
    writeln!(out).unwrap();

    // Compute dropped scopes — but only those that represent a
    // *surprise* for the operator, not those they declared away
    // on purpose by attenuating an ancestor. Two surfaces qualify
    // as "surprise":
    //
    // 1. Scopes the **active (leaf) role itself** declared but
    //    that didn't survive intersection. This is the
    //    SemiTrusted-fs.read footgun: the operator wrote
    //    `["fs.read", ...]` and the ceiling silently stripped
    //    it. They asked for it explicitly; getting nothing back
    //    deserves a loud explanation.
    //
    // 2. **Backcompat floor** scopes that didn't survive — but
    //    only when at least one level in the chain is empty,
    //    because that's the only path that pulls the floor into
    //    the assembly. This is the empty-child surprise: the
    //    operator wrote `parent_role = "researcher"` thinking
    //    they'd get researcher's view, and instead the floor
    //    leaked in and got partially stripped.
    //
    // Drops from *ancestor* levels (level >= 2) are *not*
    // reported. Those are by-design attenuations: if `coder`
    // omits `net.fetch` and `default` declares it, that's coder
    // narrowing the envelope on purpose. Listing it as "dropped"
    // would conflate intent with surprise and bury the actual
    // surprises in noise.
    writeln!(out, "dropped (surprises only — scopes the active role or backcompat floor declared that did not survive intersection; intentional ancestor-level attenuations are not listed):").unwrap();
    let mut any_dropped = false;

    // Surface (1): drops from the active/leaf role.
    let leaf = chain[0];
    let leaf_name = leaf.name.value.as_str();
    for scope in &leaf.capability_scopes.value {
        if effective.iter().any(|s| s == scope) || effective.grants(scope) {
            continue;
        }
        any_dropped = true;
        let reason = drop_reason_for(scope, &effective);
        writeln!(
            out,
            "  {} [active role {leaf_name}]  reason: {reason}",
            scope.as_str()
        )
        .unwrap();
    }

    // Surface (2): drops from the floor (only when at least one
    // chain level is empty — otherwise the floor was never
    // substituted in and listing its drops would be misleading).
    let any_empty = chain.iter().any(|r| r.capability_scopes.value.is_empty());
    if any_empty {
        for scope in &floor {
            if effective.grants(scope) || effective.iter().any(|s| s.is_granted_by(scope)) {
                continue;
            }
            any_dropped = true;
            let reason = drop_reason_for(scope, &effective);
            writeln!(
                out,
                "  {} [floor]  reason: {reason}",
                scope.as_str()
            )
            .unwrap();
        }
    }
    if !any_dropped {
        writeln!(out, "  <none - every scope the active role declared survived intersection>").unwrap();
    }
    writeln!(out).unwrap();

    // Phase 14 Task 4 — reachable role.switch targets.
    //
    // Enumerate the named roles this role can switch into via the
    // role.switch tool. The source of truth is `effective`, *not*
    // `leaf.capability_scopes`: a `role.switch:scribe` the leaf
    // declared but the parent chain stripped must not show up as
    // reachable. That mirrors PRODUCT.md P1.3 ("structurally
    // impossible escalation") at the debug surface — the
    // enumerator and the production sub-session dispatcher answer
    // "can this role switch into X?" through the same envelope.
    //
    // Three shapes the operator can see:
    //
    // 1. No `role.switch` scope in `effective` at all. The role
    //    cannot start a sub-session. Print a single-line note.
    //
    // 2. Unqualified `role.switch` in `effective`. Under D4 Rule 2
    //    the bare base grants any qualifier, so every other role
    //    in this config is reachable. Print "(any role: <list>)"
    //    where `<list>` is the other role names — actionable, not
    //    abstract.
    //
    // 3. One or more `role.switch:<target>` qualifiers in
    //    `effective`. Print each surviving target on its own line,
    //    annotated with `<unknown role>` if the target name is not
    //    declared in the current config (indicates a typo or a
    //    config drift the operator should know about).
    writeln!(out, "reachable role.switch targets (from effective envelope):").unwrap();
    let switch_scopes: Vec<&Scope> = effective
        .iter()
        .filter(|s| s.base() == "role.switch")
        .collect();
    if switch_scopes.is_empty() {
        writeln!(out, "  <none - this role cannot start a sub-session>").unwrap();
    } else {
        let unqualified_present = switch_scopes.iter().any(|s| s.qualifier().is_none());
        if unqualified_present {
            let mut others: Vec<&str> = cfg
                .roles
                .keys()
                .map(String::as_str)
                .filter(|n| *n != role_name)
                .collect();
            others.sort_unstable();
            if others.is_empty() {
                writeln!(out, "  (any role - unqualified role.switch held; no other roles declared in this config)").unwrap();
            } else {
                writeln!(out, "  (any role - unqualified role.switch held)").unwrap();
                for name in &others {
                    writeln!(out, "    {name}").unwrap();
                }
            }
        } else {
            let mut targets: Vec<(&str, bool)> = switch_scopes
                .iter()
                .filter_map(|s| s.qualifier().map(|q| (q, cfg.roles.contains_key(q))))
                .collect();
            targets.sort_unstable_by(|a, b| a.0.cmp(b.0));
            for (target, known) in &targets {
                if *known {
                    writeln!(out, "  {target}").unwrap();
                } else {
                    writeln!(out, "  {target}  <unknown role - not declared in this config>").unwrap();
                }
            }
        }
    }

    Ok(out)
}

/// Produce a short human-readable reason for why `dropped` is
/// not in `effective`. Three cases, in priority order:
///
/// 1. The base does not appear at all in `effective`. Reason:
///    "no scope with this base in the effective envelope". This
///    is the common case (the intersection chain stripped every
///    scope sharing the base).
/// 2. The base appears, but only in a *more* qualified form than
///    `dropped`, i.e. `dropped` is unqualified and the effective
///    set has a path-qualified or url-prefix-qualified
///    counterpart. Reason: "qualified-held cannot grant
///    unqualified-needed (D4 Rule 4)".
/// 3. Fallback: "qualifier shape mismatch with surviving scopes"
///    — covers the rare cases where two qualified scopes share a
///    base but have different qualifier kinds.
///
/// This is deliberately less precise than full D4 rule dispatch.
/// The goal is to give an operator a hint, not a formal proof.
fn drop_reason_for(dropped: &Scope, effective: &CapabilitySet) -> String {
    let base = dropped.base();
    let same_base: Vec<&Scope> = effective.iter().filter(|s| s.base() == base).collect();
    if same_base.is_empty() {
        return format!("no scope with base `{base}` in the effective envelope");
    }
    if dropped.qualifier().is_none() {
        return format!(
            "qualified-held cannot grant unqualified-needed (D4 Rule 4); effective envelope has only qualified `{base}` scopes: {:?}",
            same_base.iter().map(|s| s.as_str()).collect::<Vec<_>>()
        );
    }
    format!(
        "qualifier shape mismatch with surviving `{base}` scopes: {:?}",
        same_base.iter().map(|s| s.as_str()).collect::<Vec<_>>()
    )
}
