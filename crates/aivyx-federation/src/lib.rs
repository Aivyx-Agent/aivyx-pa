//! # Aivyx Federation — the Identity & Cross-Boundary Trust keystone
//!
//! Chapter **Passport** (FED.1–5). This crate is the open-core **trust
//! substrate** for agent-to-agent interaction across an operator boundary —
//! the one primitive `VISION.md` says to *get right early* because it cannot be
//! safely retrofitted. The design contract is `docs/FEDERATION.md` (FED.0).
//!
//! The central insight: **"a network of agents is a Nonagon team with the trust
//! boundary moved."** Same delegation, attenuation, gates, and audit; the only
//! new thing is *identity and trust at the boundary between operators*. So this
//! crate generalizes the local-team machinery
//! ([`aivyx_capability::Scope`] attenuation, NT-02) across that boundary.
//!
//! ## Scope of this crate (the discipline line)
//!
//! This is the **substrate only** — identity, per-peer trust + attenuation, the
//! relay protocol *shape*, peer-content provenance/sandboxing, and operator
//! consent gating. Nothing here assumes a transport or a topology. The
//! transport between one operator's own instances lives elsewhere (amendment
//! A19): the Noise link and peer cards in `aivyx-federation-net`, the
//! listener and outbound tools in `aivyx-channel`. The rest of what is
//! **network-shaped** — peer discovery, a relay server for other operators,
//! reputation, and the agent-hiring economics — is the *private Nexus
//! product*, built last on an installed base (`docs/FEDERATION.md` §10).
//!
//! ## Modernization (Chapter Passport PP.0)
//!
//! Salvaged and modernized from the pre-rebuild `aivyx-federation` onto the new
//! core:
//! - old `aivyx_core::AutonomyTier` → Reins [`AutonomyLevel`](aivyx_config) ceiling;
//! - old free-string scopes → [`aivyx_capability::Scope`];
//! - old HTTP relay/client → replaced by A19's Noise link between one
//!   operator's instances (`aivyx-federation-net`); a relay for other
//!   operators stays deferred (the Nexus product);
//! - every crossing routes onto `aivyx-audit` (PP.3).
//!
//! ## Module map (filled phase by phase)
//!
//! - [`identity`] — PP.1: operator-owned Ed25519 keypair + signed,
//!   replay-guarded request envelope.
//! - [`trust`] — PP.2: per-peer `TrustPolicy` (deny-by-default) + the
//!   cross-operator attenuation (`effective = asked ∩ policy ∩ ceiling ∩ cap`).
//! - [`relay`] — PP.3: compose the wasm-clean relay verbs
//!   ([`aivyx_ipc::federation`]) with the signing envelope; the auditable
//!   [`Crossing`](relay::Crossing) shape (emitted live since A19).
//! - [`received`] — PP.4: peer content as untrusted input —
//!   [`PeerContent`](received::PeerContent) binds a verified request to its
//!   provenance + attenuated authority; validates it; gates execution to
//!   attenuated caps only.
//! - [`consent`] — PP.5: classify whether a peer crossing needs the operator's
//!   confirm-first gate before it can have effect (a peer requests; only the
//!   operator's gate lets it act). No cross-boundary self-escalation.

#![forbid(unsafe_code)]

use thiserror::Error;

/// Errors from the federation substrate. Per-crate error (the new-core
/// convention) rather than a shared `AivyxError` — keeps the keystone's failure
/// surface explicit. **Never carries key material** (the identity invariant).
#[derive(Debug, Error)]
pub enum FederationError {
    /// An identity / signature / replay failure (PP.1).
    #[error("federation identity error: {0}")]
    Identity(String),

    /// A peer is not trusted for the requested action (PP.2). Deny-by-default:
    /// the absence of a `TrustPolicy`, or a request outside it, lands here.
    #[error("federation trust denied: {0}")]
    TrustDenied(String),

    /// Malformed input (instance id, key bytes, policy, …).
    #[error("federation validation error: {0}")]
    Validation(String),

    /// A hardware-backed signing operation failed (card absent, wrong card,
    /// touch not provided, PIN rejected). Wraps `aivyx_yubi::YubiError`'s
    /// own message rather than re-deriving a parallel taxonomy.
    #[error("YubiKey signing error: {0}")]
    Hardware(String),
}

pub mod consent;
pub mod identity;
pub mod received;
pub mod relay;
pub mod trust;
