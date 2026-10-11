//! `aivyx-matrix` — the Matrix channel. Same shape as `aivyx-discord`
//! (transport trait + scripted double, channel, session loop), plus the
//! trust policy (`policy`), the real `matrix-sdk` transport (`sdk`),
//! sealed credentials (`credentials`) and SAS verification (`verify`).
//! See `docs/superpowers/specs/2026-10-11-matrix-channel-design.md`.

// matrix-sdk's deep async nesting overflows the default limit when proving
// `Send` for the spawned sync task.
#![recursion_limit = "256"]

pub mod credentials;
pub mod matrix_channel;
pub mod policy;
pub mod sdk;
pub mod session;
pub mod transport;
pub mod verify;

pub use session::{run_matrix_session, run_matrix_session_with_transport, MatrixSessionConfig, MatrixSessionReport};

#[cfg(test)]
mod tests;
