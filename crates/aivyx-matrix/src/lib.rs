//! `aivyx-matrix` — the Matrix channel. Same shape as `aivyx-discord`
//! (transport trait + scripted double, channel, session loop), plus the
//! trust policy (`policy`), the real `matrix-sdk` transport (`sdk`),
//! sealed credentials (`credentials`) and SAS verification (`verify`).
//! See `docs/superpowers/specs/2026-10-11-matrix-channel-design.md`.

pub mod matrix_channel;
pub mod policy;
pub mod session;
pub mod transport;

pub use session::{run_matrix_session_with_transport, MatrixSessionConfig, MatrixSessionReport};

#[cfg(test)]
mod tests;
