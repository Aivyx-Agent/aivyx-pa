//! `aivyx-proton` — Proton Mail for Aivyx PA, through Proton Mail Bridge's
//! local IMAP/SMTP: the email channel (`--channel proton`) and the
//! `proton.*` tools (the `aivyx-proton` tool process). Mail is decrypted
//! only by Bridge, on this machine.
//!
//! Shape: like `aivyx-matrix` — a transport trait + scripted double
//! (`transport`), the pure trust policy (`policy`), a channel
//! (`proton_channel`) and the receive loop (`session`), plus the real
//! Bridge transport (`bridge`, `smtp`, pinned `tls`). See
//! `docs/superpowers/specs/2026-10-12-proton-mail-design.md`.

pub mod bridge;
pub mod message;
pub mod outgoing;
pub mod policy;
pub mod smtp;
pub mod tls;
pub mod transport;
