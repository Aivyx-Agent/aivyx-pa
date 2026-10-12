//! `aivyx-signal` — Signal for Aivyx PA, through the operator's own
//! signal-cli daemon (JSON-RPC on a Unix socket): the channel
//! (`--channel signal`) and `signal.send` (the `aivyx-signal` tool
//! process). signal-cli holds the account; pa holds no Signal credential.
//!
//! Shape: like `aivyx-proton` — a JSON-RPC client (`rpc`), envelope
//! parsing (`envelope`), a transport trait + scripted double
//! (`transport`), the pure trust policy (`policy`), a channel
//! (`signal_channel`) and the receive loop (`session`). See
//! `docs/superpowers/specs/2026-10-12-signal-channel-design.md`.

pub mod envelope;
pub mod lock;
pub mod policy;
pub mod rpc;
pub mod transport;
