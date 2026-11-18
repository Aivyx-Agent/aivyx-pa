//! Per-role capability envelope assembly.
//!
//! This module hosts [`assemble_role_envelope`], the pure function
//! that walks a role's `parent_role` chain and produces the
//! effective `CapabilitySet` the runtime should grant when the
//! named role is active.
//!
//! ## History
//!
//! The function was written in Phase 13 Task 2 (the per-role
//! capability envelope migration) and originally lived inside
//! `src/bin/aivyx.rs` as a binary-private free fn. Phase 13 Task 3
//! recorded the lift into this lib module as a deferral, tagged
//! "clean cut, not a refactor" — the fn has no binary-specific
//! state, it takes only `aivyx_config::Role`, a `BTreeMap` role
//! table, and a backcompat floor scope slice.
//!
//! Phase 14 Task 1 consumed that deferral. The fn moved here
//! verbatim; the Phase 13 binary-internal regression tests against
//! `examples/aivyx-pa.toml` stayed in `src/bin/aivyx.rs` and now call
//! through to this module via `aivyx_channel::assemble_role_
//! envelope`. A small set of pure-fn unit tests against hand-rolled
//! `Role` values (no TOML loader in the call path) lives at the
//! bottom of this file.
//!
//! ## Why the lift mattered
//!
//! Phase 14's main deliverable is sub-agent role-switching
//! (**PRODUCT.md P1**), which needs the envelope assembler as a
//! library-reachable function: a `role.switch` tool constructing a
//! child agent at turn-runtime cannot reach into the binary's
//! private fn, and duplicating the walk inside the tool would
//! violate the "structurally impossible escalation" rule
//! (**PRODUCT.md P1.3**) by giving the tool a second place to
//! synthesize a `CapabilitySet`. The lift makes the walker the
//! single construction path for every downstream caller.

use std::collections::BTreeMap;

use aivyx_capability::{CapabilitySet, Scope};
use aivyx_config::Role;

/// Belt-and-suspenders bound — `aivyx_config::validate_role_
/// inheritance` already rejects cycles at config-load time, so
/// this can only fire if a regression in that validator lets one
/// through. Realistic role trees are 2–3 deep; 64 is comfortably
/// above any plausible operator config.
pub const MAX_INHERITANCE_DEPTH: usize = 64;

/// Phase 13 Task 2 — assemble the effective capability envelope
/// for an active role by walking its `parent_role` chain.
///
/// **Algorithm.** Starting from `active`, walk up the chain
/// through `roles`, collecting each role's declared
/// `capability_scopes`. At each level:
///
/// - If the role's `capability_scopes` is non-empty, use it as
///   declared.
/// - If empty, substitute `backcompat_floor`. This is the **Q6
///   minimal backcompat floor**: a role that declares no
///   envelope inherits whatever the binary used to grant
///   pre-Phase-13 (the hard-coded `aivyx.rs:907–934` vector,
///   shrunk to the Phase 1–10 zero-config defaults). The floor
///   substitution happens at every empty level, not just at
///   the root, so a chain of empty roles all see the same
///   floor and intersect to itself — preserving Phase 11
///   `tool_allowlist`-narrows-broad-floor backcompat exactly.
///
/// The resulting per-level scope sets are then folded
/// pairwise via `CapabilitySet::intersect` from leaf toward
/// root. Intersection under D4 prefix-attenuation keeps the
/// **narrower** of two scopes that share a base (the child's
/// `fs.read:/tmp/**` survives intersection with the parent's
/// `fs.read`), which is the structural meaning of P7's
/// "child can attenuate, never widen" rule.
///
/// **Why leaf-to-root, not root-to-leaf.** Both directions
/// produce the same final set under intersection (the operation
/// is commutative and associative), but the leaf-to-root walk
/// matches how an operator reads the config — "this role,
/// then its parent, then its grandparent" — and keeps the
/// "active role" the natural starting point.
///
/// **Trust ceiling intersection happens at the call site, not
/// here.** `assemble_role_envelope` is purely about scope-set
/// inheritance; the Q3 `trust_ceiling.default_ceiling()` layer
/// composes on top via a separate `intersect` call right
/// before the envelope is handed to the channel branch. This
/// keeps the function's contract narrow: "given a role tree
/// and a backcompat floor, what scopes does this role declare
/// it wants?"
///
/// **Cycle safety.** `aivyx_config::validate_role_inheritance`
/// has already rejected cycles by the time this function runs,
/// so an unbounded `while let Some(parent)` walk is safe. As
/// belt-and-suspenders, the loop carries a depth counter and
/// bails after [`MAX_INHERITANCE_DEPTH`] to make a future
/// validator regression loud rather than infinite-looping a
/// production process.
pub fn assemble_role_envelope(
    active: &Role,
    roles: &BTreeMap<String, Role>,
    backcompat_floor: &[Scope],
) -> CapabilitySet {
    let level_scopes = |role: &Role| -> Vec<Scope> {
        if role.capability_scopes.value.is_empty() {
            backcompat_floor.to_vec()
        } else {
            role.capability_scopes.value.clone()
        }
    };

    let mut effective = CapabilitySet::from_scopes(level_scopes(active));
    let mut cursor = active.parent_role.value.as_deref();
    let mut depth = 0;
    while let Some(parent_name) = cursor {
        depth += 1;
        if depth > MAX_INHERITANCE_DEPTH {
            // Validator regression — bail out with whatever we
            // have so far rather than loop forever. The next
            // turn's capability check will surface the
            // truncation as a denial, which is a louder failure
            // than an infinite loop and keeps the audit chain
            // honest.
            break;
        }
        let Some(parent) = roles.get(parent_name) else {
            // Validator already rejected unknown parents; this
            // branch is unreachable under a well-validated
            // config but kept for defensive composition.
            break;
        };
        let parent_set = CapabilitySet::from_scopes(level_scopes(parent));
        effective = effective.intersect(&parent_set);
        cursor = parent.parent_role.value.as_deref();
    }
    effective
}

/// A20 — what each role's board run may do: its resolved envelope (the
/// parent chain, and the floor for a level that declares no scopes)
/// capped by the role's own trust ceiling, the same set the `role.switch`
/// child factory builds. The board adds `board.write` on top as the run's
/// ceiling.
pub fn board_role_scopes(
    roles: &BTreeMap<String, Role>,
    backcompat_floor: &[Scope],
) -> std::collections::HashMap<String, CapabilitySet> {
    roles
        .iter()
        .map(|(name, role)| {
            let envelope = assemble_role_envelope(role, roles, backcompat_floor);
            let capped = envelope.intersect(role.trust_ceiling.value.default_ceiling());
            (name.clone(), capped)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    //! Pure-fn unit tests against hand-rolled `Role` values. These
    //! deliberately bypass the TOML loader so the walker's
    //! contract can be exercised without a config-file round-trip
    //! in the call path — the binary-internal regression tests in
    //! `src/bin/aivyx.rs` pin the integration, these pin the
    //! pure-fn contract.
    //!
    //! Three shapes covered:
    //!
    //! 1. **Leaf-only** — `active` has no parent; the walker
    //!    returns `active`'s declared set unchanged.
    //! 2. **Clean parent + child attenuation** — child drops one
    //!    scope the parent declares; the walker returns the
    //!    child's declared set (intersection narrows, not
    //!    widens).
    //! 3. **Empty-child surprise** — child declares
    //!    `capability_scopes = []`; the walker substitutes the
    //!    backcompat floor for the child level and intersects
    //!    against the parent. This is the Phase 13 Task 2
    //!    correction-block case; pinning it here (not just in
    //!    the binary) means a future refactor that accidentally
    //!    aligned the empty-child and parent envelopes would
    //!    break this test first, before the binary's
    //!    `examples/aivyx-pa.toml` regression ever runs.

    use super::*;
    use aivyx_config::{FieldSource, Sourced, ToolAllowlist};
    use aivyx_capability::TrustTier;

    fn scope(s: &str) -> Scope {
        Scope::parse(s).expect("test scope must parse")
    }

    /// Build a `Role` with the minimum fields each test needs.
    /// Phase 13 Task 1 added six `Sourced<T>` fields on `Role`;
    /// this helper fills each with `FieldSource::Default` and
    /// values the test picks.
    fn role(
        name: &str,
        capability_scopes: Vec<Scope>,
        parent: Option<&str>,
    ) -> Role {
        Role {
            name: Sourced {
                value: name.to_string(),
                source: FieldSource::Default,
            },
            system_prompt: Sourced {
                value: String::new(),
                source: FieldSource::Default,
            },
            tool_allowlist: Sourced {
                value: ToolAllowlist::AllowAll,
                source: FieldSource::Default,
            },
            memory_topic_prefix: Sourced {
                value: None,
                source: FieldSource::Default,
            },
            capability_scopes: Sourced {
                value: capability_scopes,
                source: FieldSource::Default,
            },
            trust_ceiling: Sourced {
                value: TrustTier::Trusted,
                source: FieldSource::Default,
            },
            parent_role: Sourced {
                value: parent.map(str::to_string),
                source: FieldSource::Default,
            },
        }
    }

    fn roles_map(rs: impl IntoIterator<Item = Role>) -> BTreeMap<String, Role> {
        rs.into_iter()
            .map(|r| (r.name.value.clone(), r))
            .collect()
    }

    #[test]
    fn leaf_only_role_returns_declared_set_verbatim() {
        let leaf = role(
            "leaf",
            vec![scope("fs.read"), scope("memory.read")],
            None,
        );
        let roles = roles_map([leaf.clone()]);
        let floor: Vec<Scope> = vec![];

        let envelope = assemble_role_envelope(&leaf, &roles, &floor);
        let got: Vec<String> = envelope.iter().map(|s| s.as_str().to_string()).collect();

        assert_eq!(
            got,
            vec!["fs.read".to_string(), "memory.read".to_string()]
        );
    }

    #[test]
    fn child_attenuates_parent_by_dropping_a_scope() {
        let parent = role(
            "parent",
            vec![scope("fs.read"), scope("fs.write"), scope("memory.read")],
            None,
        );
        let child = role(
            "child",
            vec![scope("fs.read"), scope("memory.read")],
            Some("parent"),
        );
        let roles = roles_map([parent.clone(), child.clone()]);
        let floor: Vec<Scope> = vec![];

        let envelope = assemble_role_envelope(&child, &roles, &floor);
        let mut got: Vec<String> =
            envelope.iter().map(|s| s.as_str().to_string()).collect();
        got.sort();

        assert_eq!(
            got,
            vec!["fs.read".to_string(), "memory.read".to_string()],
            "child's declared set survives intersection with parent's broader \
             declared set; parent's fs.write does NOT leak into the child \
             (that would widen the envelope, which P7 forbids)"
        );
    }

    #[test]
    fn empty_child_substitutes_backcompat_floor_and_intersects_with_parent() {
        let parent = role(
            "parent",
            vec![scope("fs.read"), scope("memory.read"), scope("net.fetch")],
            None,
        );
        let child = role("child", vec![], Some("parent"));
        let roles = roles_map([parent.clone(), child.clone()]);
        let floor: Vec<Scope> = vec![
            scope("fs.read"),
            scope("memory.read"),
            scope("shell.exec"),
        ];

        let envelope = assemble_role_envelope(&child, &roles, &floor);
        let mut got: Vec<String> =
            envelope.iter().map(|s| s.as_str().to_string()).collect();
        got.sort();

        // Floor ∩ parent: fs.read (both), memory.read (both),
        // shell.exec (floor only — dropped), net.fetch (parent
        // only — dropped). Final: [fs.read, memory.read].
        assert_eq!(
            got,
            vec!["fs.read".to_string(), "memory.read".to_string()],
            "empty-child surprise: the walker substitutes the floor \
             for the child level and intersects upward. shell.exec is \
             in the floor but not the parent, so it drops. net.fetch \
             is in the parent but not the floor-substituted child, \
             so it drops. Only the intersection of floor ∩ parent \
             survives."
        );
    }
    fn sorted(set: &CapabilitySet) -> Vec<String> {
        let mut v: Vec<String> = set.iter().map(|s| s.as_str().to_string()).collect();
        v.sort();
        v
    }

    #[test]
    fn a_board_role_run_gets_its_parents_narrowed_fs_read() {
        let parent = role("parent", vec![scope("fs.read:/data/**")], None);
        let child = role("child", vec![scope("fs.read"), scope("memory.read")], Some("parent"));
        let roles = roles_map([parent, child]);
        let map = board_role_scopes(&roles, &[scope("fs.read"), scope("memory.read")]);
        assert_eq!(sorted(&map["child"]), ["fs.read:/data/**"]);
        assert!(!map["child"].grants(&scope("fs.read:/etc/passwd")));
        assert!(map["child"].grants(&scope("fs.read:/data/q4.csv")));
    }

    #[test]
    fn a_board_role_with_no_scopes_gets_the_floor() {
        let roles = roles_map([role("plain", vec![], None)]);
        let floor = vec![scope("fs.read"), scope("memory.read"), scope("board.write")];
        let map = board_role_scopes(&roles, &floor);
        assert_eq!(sorted(&map["plain"]), ["board.write", "fs.read", "memory.read"]);
    }

    #[test]
    fn a_semi_trusted_board_role_loses_trusted_only_bases() {
        let mut r = role("helper", vec![scope("memory.read"), scope("shell.exec")], None);
        r.trust_ceiling.value = TrustTier::SemiTrusted;
        let tier = TrustTier::SemiTrusted.default_ceiling();
        assert!(!tier.grants(&scope("shell.exec")), "shell.exec must be Trusted-only");
        assert!(tier.grants(&scope("memory.read")));
        let map = board_role_scopes(&roles_map([r]), &[]);
        assert!(!map["helper"].grants(&scope("shell.exec")));
        assert_eq!(sorted(&map["helper"]), ["memory.read"]);
    }
}
