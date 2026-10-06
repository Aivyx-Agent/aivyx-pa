//! Agent autonomy levels — Chapter Reins (RN.1).
//!
//! One end-user-facing dial, [`AutonomyLevel`], that composes the scattered
//! autonomy knobs (`[access]`, `confirm_destructive`, the headless gate
//! policy, the `[loop]` arming, the self-improvement adoption policy) into
//! named tiers. This module is the **pure composition layer**: the
//! `AutonomyLevel -> AutonomyPosture` expansion and nothing else. No TOML
//! parsing, no daemon wiring — those land in RN.2+.
//!
//! The contract anchor (see `docs/AUTONOMY.md` §4) is that **`Assisted` — the
//! default — expands to exactly today's behavior**, asserted byte-for-byte by
//! [`tests::assisted_expands_to_todays_behavior`]. A future wiring change that
//! quietly drifts the default trips that test.
//!
//! `AutonomyPosture` is intentionally config-native (plain bools + small
//! enums). It does **not** reference `aivyx_core::GatePolicy` — `aivyx-config`
//! and `aivyx-core` are siblings — so the wiring layer translates
//! [`GatePosture`] onto the runtime `GatePolicy` when it consumes the posture.

use serde::Deserialize;

/// How autonomous the agent is, as one word the end user owns. Mirrors
/// [`crate::AccessLevel`] in shape: a `#[default]` tier, lowercase wire names,
/// and a string⇄level round-trip ([`as_str`](Self::as_str) /
/// [`from_wire`](Self::from_wire)) shared by the CLI and the IPC handler.
///
/// `Assisted` is the default — an absent `[autonomy]` section resolves here,
/// so existing configs behave byte-for-byte as before.
/// Ordered by how much the agent may do on its own (declaration order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutonomyLevel {
    /// Confirm *every* action, even reversible ones. No loop, no
    /// self-improvement. Maximum oversight — a pure assistant.
    Manual,
    /// Today's behavior: reversible actions run free, irreversible ones
    /// confirm; no autonomous loop; self-improvement is propose-only. The
    /// default, so an absent `[autonomy]` section is unchanged.
    #[default]
    Assisted,
    /// Irreversible actions queue for batched approval; the autonomous loop is
    /// armed (capped, per-iteration gate); low-risk skill refinements
    /// auto-adopt; the agent proposes a backlog. A builder with a human nearby.
    Supervised,
    /// Unattended runs reject-and-abort at gates (Chapter H), with reversible
    /// work auto-approved within the allowlist; loop armed and capped; the
    /// agent proposes and pursues goals within budget. Eyes-on-dashboard.
    Autonomous,
    /// `confirm_destructive` off (explicit warning); broad auto-adoption;
    /// self-directed within budget. For a dedicated, isolated host — eyes-open.
    Unleashed,
}

impl AutonomyLevel {
    /// Lowercase wire/display name (matches the `[autonomy] level` value).
    pub fn as_str(&self) -> &'static str {
        match self {
            AutonomyLevel::Manual => "manual",
            AutonomyLevel::Assisted => "assisted",
            AutonomyLevel::Supervised => "supervised",
            AutonomyLevel::Autonomous => "autonomous",
            AutonomyLevel::Unleashed => "unleashed",
        }
    }

    /// Parse the lowercase wire/display name back into a level — the inverse of
    /// [`as_str`](Self::as_str). `None` for an unknown token. The single source
    /// of truth for the string⇄level mapping, shared by the future
    /// `aivyx-pa autonomy set` parser and the `SetAutonomyLevel` IPC handler.
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "manual" => Some(AutonomyLevel::Manual),
            "assisted" => Some(AutonomyLevel::Assisted),
            "supervised" => Some(AutonomyLevel::Supervised),
            "autonomous" => Some(AutonomyLevel::Autonomous),
            "unleashed" => Some(AutonomyLevel::Unleashed),
            _ => None,
        }
    }

    /// Whether this level reaches beyond the default `Assisted` posture (so the
    /// wizard's extra confirmation + the audited expansion apply).
    pub fn is_expanded(&self) -> bool {
        !matches!(self, AutonomyLevel::Assisted)
    }

    /// The **pure composition** — expand a level into the bundle of low-level
    /// knobs it sets. This is the heart of Chapter Reins: every tier is a
    /// named point in [`AutonomyPosture`] space. The wiring layer (RN.2) fills
    /// only the knobs the operator left *unset*, so an explicit knob always
    /// wins over the level's expansion.
    pub fn expand(self) -> AutonomyPosture {
        match self {
            AutonomyLevel::Manual => AutonomyPosture {
                gate: GatePosture::ConfirmAll,
                loop_enabled: false,
                confirm_destructive: true,
                growth: GrowthAdoption::None,
            },
            // The default — must equal today's behavior. See the byte-identical
            // test; do not change without updating `AutonomyPosture::todays_default`.
            AutonomyLevel::Assisted => AutonomyPosture {
                gate: GatePosture::ConfirmIrreversible,
                loop_enabled: false,
                confirm_destructive: true,
                growth: GrowthAdoption::ProposeOnly,
            },
            AutonomyLevel::Supervised => AutonomyPosture {
                gate: GatePosture::BatchIrreversible,
                loop_enabled: true,
                confirm_destructive: true,
                growth: GrowthAdoption::LowRiskAuto,
            },
            AutonomyLevel::Autonomous => AutonomyPosture {
                gate: GatePosture::RejectUnattended,
                loop_enabled: true,
                confirm_destructive: true,
                growth: GrowthAdoption::PolicyAuto,
            },
            AutonomyLevel::Unleashed => AutonomyPosture {
                gate: GatePosture::RejectUnattended,
                loop_enabled: true,
                confirm_destructive: false,
                growth: GrowthAdoption::BroadAuto,
            },
        }
    }
}

impl std::fmt::Display for AutonomyLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A per-domain exception to the global level — `[[autonomy.override]]`. The
/// `domain` is a capability-domain label (e.g. `"shell"`, `"email"`); a call
/// whose scope maps to that domain uses `level` instead of the global one.
/// "Autonomous at coding, manual on money" is the real-world ask a flat dial
/// can't express. (Mapping a tool's scope → domain is the consumer's job in
/// RN.3; this layer just carries the keyed exceptions.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutonomyOverride {
    pub domain: String,
    pub level: AutonomyLevel,
}

/// Resolve the effective posture for a call in `domain`: the most specific
/// matching `[[autonomy.override]]` wins, else the global level — then expand.
/// Pure so the resolution order is testable without a loaded config. `domain =
/// None` (a call with no domain, or a global query) always uses the global
/// level.
pub fn resolve_posture(
    global: AutonomyLevel,
    overrides: &[AutonomyOverride],
    domain: Option<&str>,
) -> AutonomyPosture {
    let level = domain
        .map(normalize_area)
        .and_then(|d| overrides.iter().find(|o| o.domain == d).map(|o| o.level))
        .unwrap_or(global);
    level.expand()
}

/// The canonical spelling of an override area: `schedules` (the name the
/// routine-growth wiring and older configs use) is the `schedule` area.
pub fn normalize_area(domain: &str) -> &str {
    match domain {
        "schedules" => "schedule",
        d => d,
    }
}

/// The overrides that give their area *more* autonomy than the global level —
/// surfaced as a start-up warning and in `aivyx-pa autonomy show`.
pub fn looser_overrides(
    global: AutonomyLevel,
    overrides: &[AutonomyOverride],
) -> Vec<&AutonomyOverride> {
    overrides.iter().filter(|o| o.level > global).collect()
}

/// What a run does at an approval point. Config-native so it stays out of the
/// `aivyx-core` dependency; the wiring layer maps it onto the runtime
/// `aivyx_core::GatePolicy` (`ConfirmAll`/`ConfirmIrreversible`/`BatchIrreversible`
/// → `Interactive`; `RejectUnattended` → `RejectAndAbort` (+ the RN.3 bounded
/// `AutoApprove` of reversible scopes)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatePosture {
    /// Confirm every action, including reversible ones.
    ConfirmAll,
    /// Confirm only irreversible/outbound actions; reversible runs free. The
    /// interactive default today.
    ConfirmIrreversible,
    /// Irreversible actions queue for batched operator approval.
    BatchIrreversible,
    /// Unattended: reject-and-abort at any gate; reversible work may be
    /// auto-approved within the `[autonomy.auto_approve]` allowlist (RN.3).
    RejectUnattended,
}

/// How far self-improvement may adopt without a human. The growth gradient of
/// `docs/AUTONOMY.md` §7 — identity (persona/access/trust/the level itself) is
/// *never* self-adoptable at any value, so this only governs skill + goal
/// adoption, never who the agent is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrowthAdoption {
    /// No self-improvement at all.
    None,
    /// Everything surfaces as a governed proposal (today's behavior).
    ProposeOnly,
    /// Low-risk skill refinements above an effectiveness threshold auto-adopt;
    /// everything else stays a proposal.
    LowRiskAuto,
    /// Skill refinements + self-authored goals auto-adopt within policy.
    PolicyAuto,
    /// Broad auto-adoption (still never identity).
    BroadAuto,
}

/// The bundle of low-level knobs a level expands into. The wiring layer (RN.2)
/// fills only the knobs the operator left unset, so an explicit knob always
/// wins. Equality is derived so the byte-identical contract test can assert
/// `Assisted.expand() == AutonomyPosture::todays_default()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutonomyPosture {
    /// What happens at an approval point.
    pub gate: GatePosture,
    /// Whether the autonomous loop is armed. (Caps remain a separate `[loop]`
    /// concern; arming alone does not set them.)
    pub loop_enabled: bool,
    /// Whether irreversible/outbound actions escalate to a confirm-first gate.
    pub confirm_destructive: bool,
    /// How far self-improvement may adopt without a human.
    pub growth: GrowthAdoption,
}

impl AutonomyPosture {
    /// Whether the autonomous loop should be armed, given whether the operator
    /// *explicitly* enabled it via `[loop] enabled` (Chapter Reins RN.5). The
    /// composition is **additive**: the loop arms if `[loop] enabled` is set
    /// **or** the autonomy level grants it — the level never *disarms* an
    /// explicitly-enabled loop. At `assisted` (the default), `loop_enabled` is
    /// false, so this returns `explicit_loop_enabled` unchanged — today's
    /// behavior, byte-for-byte. (Arming only *makes the loop available*; a run
    /// still requires an explicit `aivyx-pa loop start`. And it only takes effect
    /// when a `[loop]` section exists, since that is where the caps live.)
    pub fn arms_loop(&self, explicit_loop_enabled: bool) -> bool {
        explicit_loop_enabled || self.loop_enabled
    }

    /// Today's shipped defaults, spelled out independently of [`AutonomyLevel`]
    /// so the byte-identical test compares two *separately authored* values
    /// rather than a tautology. If the daemon's real defaults ever change, this
    /// constant — and the test — must change with them, deliberately.
    ///
    /// - gate: `ConfirmIrreversible` — `gate_policy` defaults to `Interactive`
    ///   and `confirm_destructive` gates only irreversible ops.
    /// - loop: disarmed — `[loop] enabled` defaults to `false`.
    /// - confirm_destructive: on — the default for an expanded access level;
    ///   n/a (and harmless) at `sandbox`, where nothing destructive escapes.
    /// - growth: propose-only — every self-improvement path is governed today.
    pub const fn todays_default() -> Self {
        AutonomyPosture {
            gate: GatePosture::ConfirmIrreversible,
            loop_enabled: false,
            confirm_destructive: true,
            growth: GrowthAdoption::ProposeOnly,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE contract anchor for Chapter Reins: the default level reproduces
    /// today's behavior exactly. RN.2's wiring must keep this true.
    #[test]
    fn assisted_expands_to_todays_behavior() {
        assert_eq!(
            AutonomyLevel::Assisted.expand(),
            AutonomyPosture::todays_default(),
            "the default `assisted` level must expand to today's shipped posture \
             byte-for-byte — an absent `[autonomy]` section changes nothing"
        );
    }

    #[test]
    fn default_level_is_assisted() {
        assert_eq!(AutonomyLevel::default(), AutonomyLevel::Assisted);
    }

    #[test]
    fn wire_names_round_trip() {
        for level in [
            AutonomyLevel::Manual,
            AutonomyLevel::Assisted,
            AutonomyLevel::Supervised,
            AutonomyLevel::Autonomous,
            AutonomyLevel::Unleashed,
        ] {
            assert_eq!(AutonomyLevel::from_wire(level.as_str()), Some(level));
            assert_eq!(level.to_string(), level.as_str());
        }
        assert_eq!(AutonomyLevel::from_wire("nonsense"), None);
    }

    #[test]
    fn serde_reads_lowercase_wire_names() {
        // The `[autonomy] level = "supervised"` shape RN.2 will parse.
        #[derive(Deserialize)]
        struct Wrap {
            level: AutonomyLevel,
        }
        let parsed: Wrap =
            toml::from_str("level = \"supervised\"").expect("lowercase parses");
        assert_eq!(parsed.level, AutonomyLevel::Supervised);
    }

    #[test]
    fn only_assisted_is_unexpanded() {
        assert!(!AutonomyLevel::Assisted.is_expanded());
        for level in [
            AutonomyLevel::Manual,
            AutonomyLevel::Supervised,
            AutonomyLevel::Autonomous,
            AutonomyLevel::Unleashed,
        ] {
            assert!(level.is_expanded(), "{level} must count as expanded");
        }
    }

    #[test]
    fn arms_loop_is_additive_and_assisted_is_byte_identical() {
        // assisted (default): never adds arming — pure passthrough of [loop].
        let assisted = AutonomyLevel::Assisted.expand();
        assert!(!assisted.arms_loop(false), "assisted + [loop] off ⇒ off (today)");
        assert!(assisted.arms_loop(true), "assisted + [loop] on ⇒ on (today)");

        // autonomous: arms the loop even when [loop] enabled is unset.
        let autonomous = AutonomyLevel::Autonomous.expand();
        assert!(autonomous.arms_loop(false), "autonomous arms an unset loop");
        assert!(autonomous.arms_loop(true), "explicit on stays on");

        // manual never disarms an explicitly-enabled loop (additive only).
        let manual = AutonomyLevel::Manual.expand();
        assert!(manual.arms_loop(true), "manual must not disarm [loop] enabled=true");
        assert!(!manual.arms_loop(false));
    }

    #[test]
    fn override_resolution_is_most_specific_then_global() {
        let overrides = vec![
            AutonomyOverride {
                domain: "email".into(),
                level: AutonomyLevel::Manual,
            },
            AutonomyOverride {
                domain: "shell".into(),
                level: AutonomyLevel::Autonomous,
            },
        ];
        let global = AutonomyLevel::Supervised;

        // A domain with an override uses it.
        assert_eq!(
            resolve_posture(global, &overrides, Some("email")),
            AutonomyLevel::Manual.expand()
        );
        assert_eq!(
            resolve_posture(global, &overrides, Some("shell")),
            AutonomyLevel::Autonomous.expand()
        );
        // A domain without one falls back to the global level.
        assert_eq!(
            resolve_posture(global, &overrides, Some("fs")),
            AutonomyLevel::Supervised.expand()
        );
        // No domain ⇒ global.
        assert_eq!(
            resolve_posture(global, &overrides, None),
            AutonomyLevel::Supervised.expand()
        );
    }

    /// The expansion table is the design contract (`docs/AUTONOMY.md` §4) —
    /// pin every cell so an accidental edit to one tier is a visible diff.
    #[test]
    fn expansion_table_matches_the_contract() {
        // Loop is armed exactly at supervised+.
        assert!(!AutonomyLevel::Manual.expand().loop_enabled);
        assert!(!AutonomyLevel::Assisted.expand().loop_enabled);
        assert!(AutonomyLevel::Supervised.expand().loop_enabled);
        assert!(AutonomyLevel::Autonomous.expand().loop_enabled);
        assert!(AutonomyLevel::Unleashed.expand().loop_enabled);

        // confirm_destructive is on everywhere except unleashed.
        assert!(AutonomyLevel::Manual.expand().confirm_destructive);
        assert!(AutonomyLevel::Autonomous.expand().confirm_destructive);
        assert!(!AutonomyLevel::Unleashed.expand().confirm_destructive);

        // Gate posture climbs with the tier.
        assert_eq!(AutonomyLevel::Manual.expand().gate, GatePosture::ConfirmAll);
        assert_eq!(
            AutonomyLevel::Supervised.expand().gate,
            GatePosture::BatchIrreversible
        );
        assert_eq!(
            AutonomyLevel::Autonomous.expand().gate,
            GatePosture::RejectUnattended
        );

        // Growth widens with the tier; only manual disables it.
        assert_eq!(AutonomyLevel::Manual.expand().growth, GrowthAdoption::None);
        assert_eq!(
            AutonomyLevel::Assisted.expand().growth,
            GrowthAdoption::ProposeOnly
        );
        assert_eq!(
            AutonomyLevel::Unleashed.expand().growth,
            GrowthAdoption::BroadAuto
        );
    }
}
