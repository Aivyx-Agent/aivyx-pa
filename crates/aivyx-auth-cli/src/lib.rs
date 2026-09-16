//! # aivyx-auth-cli
//!
//! Shared auth-CLI substrate for Aivyx Chapter F
//! third-party tool processes. Lifted in Phase 132
//! from the near-identical auth_cli surfaces in
//! `aivyx-notion`, `aivyx-obsidian`, and `aivyx-n8n`.
//!
//! ## What this crate owns
//!
//! - [`BinaryMode`] / [`AuthMode`] enums — the four CLI
//!   modes every third-party tool process supports.
//! - [`parse_cli_args`] — CLI argument parsing,
//!   parameterised on the binary name so error messages
//!   carry the right `aivyx-<service>` prefix.
//! - [`ConfigFileError`] — IO + parse error variants
//!   every consumer hits. Service-specific validation
//!   errors (empty token, non-absolute path, etc.)
//!   stay on the consumer side as a separate enum.
//! - [`default_config_path`] — computes
//!   `$HOME/.aivyx-pa/tool-processes/<service>/config.toml`.
//! - [`load_toml`] — generic TOML file load. Also
//!   tightens the file to `0600` on Unix on every
//!   successful load (Task 5, 2026-09-16 security
//!   audit fix — see [`enforce_secure_permissions`]).
//! - [`enforce_secure_permissions`] — the standalone
//!   0600-tightening step `load_toml` uses internally,
//!   exposed for consumers (`aivyx-gmail`) that parse
//!   `config.toml` without going through `load_toml`.
//! - [`StatusReport`] / [`CheckReport`] — Display-aware
//!   report types every consumer's `auth status` and
//!   `auth check` subcommands return.
//!
//! ## What this crate does NOT own
//!
//! - Service-specific config struct fields.
//! - Service-specific `validate_config` logic (e.g.
//!   "token must be non-empty," "vault path must be
//!   absolute").
//! - The `help_text()` string — every service has a
//!   distinct help block (Notion's "share with
//!   integration" UX note, Obsidian's vault-path
//!   note, n8n's API-key location, etc.).
//! - The `check()` body — Notion + n8n hit an HTTP
//!   endpoint; Obsidian probes the local filesystem.
//!   The shape is too divergent to lift behind a
//!   trait without forcing awkward uniformity.
//!
//! Per Phase 132 Q1a sign-off, the lift granularity
//! is "shared types + reusable helpers," not a full
//! trait that consumers implement.

mod cli;
mod config_file;
mod report;

pub use cli::{parse_cli_args, AuthMode, BinaryMode};
pub use config_file::{default_config_path, enforce_secure_permissions, load_toml, ConfigFileError};
pub use report::{CheckReport, StatusReport};
