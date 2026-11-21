//! OpenAI-compatible provider implementation of [`LlmProvider`].
//!
//! Gated behind the `provider-openai` Cargo feature. Targets the
//! `/v1/chat/completions` streaming endpoint, compatible with OpenAI,
//! Ollama, and any OpenAI-API-compatible service.
//!
//! ## Layout
//!
//! - [`provider`] — the `OpenAiProvider` struct, wire-format types,
//!   and the streaming state machine that turns SSE `data:` lines
//!   into `LlmStreamEvent` / `LlmStepEnd` values.
//!
//! Reuses the `HttpTransport` and `ByteStream` types from
//! `crate::transport` — the transport seam is
//! provider-agnostic by design.

pub mod provider;

pub use provider::{OpenAiConfig, OpenAiProvider, DEFAULT_OLLAMA_BASE_URL};
