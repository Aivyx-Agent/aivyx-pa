//! `aivyx-vertical-sdk` — the **contract surface for vertical packs**.
//!
//! A vertical pack (the kitchen pack is the open reference example) is *config
//! and tools over the free engine*, never a fork. But until now packs reached
//! straight into the engine internals (`aivyx-core`, `aivyx-team`,
//! `aivyx-capability`, `aivyx-tool`) so every core refactor risked breaking
//! every paid pack. This crate is the seam that fixes that: it re-exports a
//! **curated subset** of the engine, and a pack depends on this crate alone.
//!
//! ## The contract
//!
//! Everything a pack is allowed to touch lives under one of three modules:
//!
//! - [`capability`] — declare the scopes a tool requires ([`Scope`],
//!   [`TrustTier`]).
//! - [`team`] — the shape of a pack's Nonagon: [`TeamConfig`] + members +
//!   the [`MissionPlan`] it runs, plus [`attenuate_for_member`] for NT-02
//!   least-privilege.
//! - [`tool`] — implement the [`Tool`] trait and run a tool-process via
//!   [`run_multi_tool_subprocess`] (the Chapter F/G substrate pattern).
//!
//! Or pull the lot in with `use aivyx_vertical_sdk::prelude::*;`.
//!
//! ## Stability
//!
//! Not stable in pa 1.0 (docs/COMPATIBILITY.md): this crate and the tool
//! protocol it speaks may change in a 1.x minor release, so a tool built
//! against them may need a rebuild. Format 1 (tool) pack *bundles* are
//! stable. Both get stabilised in a 1.x minor once a real tool pack exists.
//!
//! Items re-exported here are the pack-facing API. They change only with a
//! deliberate SDK version bump, even when the underlying engine crate moves.
//! Anything *not* re-exported here is engine-internal and off-limits to packs —
//! if a pack needs something new from the engine, add it to this facade first
//! (one reviewed place) rather than depending on the engine crate directly.
//!
//! [`Scope`]: capability::Scope
//! [`TrustTier`]: capability::TrustTier
//! [`TeamConfig`]: team::TeamConfig
//! [`MissionPlan`]: team::MissionPlan
//! [`attenuate_for_member`]: team::attenuate_for_member
//! [`Tool`]: tool::Tool
//! [`run_multi_tool_subprocess`]: tool::run_multi_tool_subprocess

/// Capability + trust primitives — a pack declares what authority each tool
/// needs. [`Scope`](capability::Scope) is what a `Tool::required_scope` returns;
/// [`TrustTier`](capability::TrustTier) tiers a team member's ceiling.
pub mod capability {
    pub use aivyx_capability::{Scope, TrustTier};
}

/// Team + mission config — the shape of a vertical pack's Nonagon. A pack ships
/// a [`TeamConfig`](self::TeamConfig) (lead + least-privileged specialists) and
/// the [`MissionPlan`](self::MissionPlan) it runs; [`attenuate_for_member`] caps
/// each specialist to a subset of the lead's authority (NT-02).
pub mod team {
    pub use aivyx_team::attenuate_for_member;
    pub use aivyx_team::config::{DialogueConfig, TeamConfig, TeamMember};
    pub use aivyx_team::mission::{MissionPlan, Step};
}

/// Tool-process building blocks — implement [`Tool`](self::Tool) for each
/// `domain.*` tool and hand the set to [`run_multi_tool_subprocess`] to become
/// a daemon-spawned tool process (Chapter F/G substrate pattern).
pub mod tool {
    pub use aivyx_core::{AivyxError, Tool, ToolContext, ToolId, ToolOutcome, Verification};
    pub use aivyx_tool::run_multi_tool_subprocess;
}

/// Everything in one glob — `use aivyx_vertical_sdk::prelude::*;`.
pub mod prelude {
    pub use super::capability::*;
    pub use super::team::*;
    pub use super::tool::*;
}
