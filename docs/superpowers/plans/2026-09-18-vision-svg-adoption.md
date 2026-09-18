# Vision SVG Adoption (aivyx-pa) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire `aivyx-vision-svg`'s `generate_svg` into `aivyx-pa` as a real,
agent-invokable tool — `vision.generate_svg` — via a new tool-process
crate, `aivyx-vision`. This is `aivyx-pa`'s adoption half of Aivyx-Vision's
Milestone 1 (the other half, `aivyx-coder`'s adoption, is a separate plan
in that repo).

**Architecture:** A new tool-process crate, `crates/aivyx-vision`
(binary `aivyx-vision`), following the `aivyx-toolkit` shape (no OAuth,
no `auth_cli` module — just an operator config file naming an LLM
provider). It depends on the standalone `aivyx-vision-svg` crate (pinned
git dependency, matching `aivyx-confine`/`aivyx-checkpoint`/`aivyx-kvcache`/
`aivyx-injection-guard`'s own pattern) for the actual `generate_svg`
logic, plus `aivyx-llm` directly to construct its own LLM provider
connection — **this is a deliberate, reasoned deviation from a
same-process-reuse design**: since a tool-process is a separate OS
process, it cannot share the daemon's in-memory `Arc<dyn LlmProvider>`,
so this crate gets its own, separately-configured one (its own
`config.toml`, naming a provider/model), the same way `aivyx-gmail` has
its own OAuth config rather than sharing the daemon's. A small adapter,
`LlmTextCompleter`, implements `aivyx-vision-svg`'s `TextCompleter` trait
by wrapping whichever `aivyx_llm::LlmProvider` the config selects.

**Tech Stack:** Rust, edition 2024 (matching this workspace). `aivyx-core`,
`aivyx-tool`, `aivyx-llm`, `aivyx-capability` (all workspace-internal),
`aivyx-vision-svg` (new external git dependency), `async-trait`, `serde`,
`tokio`.

## Global Constraints

- `cargo clippy --all-targets -- -D warnings` (default-members scope) must
  stay clean.
- `cargo test` (no `-p`) must stay green across the whole default-members
  workspace, not just the new crate.
- The new capability base is `vision.generate` — one base for the one
  tool this plan adds (`vision.generate_svg`); later milestones' tools
  (`vision.generate_image`, `vision.generate_3d`) will share this same
  base when they land, per the design spec's own reasoning (nothing to
  read separately from what's generated).
- Ceiling placement: `CEILING_SEMITRUSTED` (and therefore reachable at
  Trusted/Kernel too), **not** Trusted-only — confirmed against real
  precedent: `llm.call`/`llm.embed` are themselves already at
  `CEILING_SEMITRUSTED`, and `vision.generate_svg` is a narrower,
  sanitized version of an LLM call (constrained prompt, sanitized
  output) — arguably lower risk than a raw `llm.call`, not higher.
- Every `KNOWN_BASES` addition needs the count assertion
  (`known_bases_count_matches_phase_143_a3_addendum`) bumped in the same
  commit, plus a one-line addition to that test's own inline enumeration
  comment — this is the actual, currently-followed convention (the
  separate `docs/amendments/2026-04-17-capability-taxonomy-growth.md`
  file's own "Total: ... = 59" arithmetic has already drifted stale
  across many prior additions that only updated the test; don't attempt
  to reconcile that file's stale total in this plan — out of scope, a
  pre-existing gap this plan didn't create).
- Re-read the current file before editing anything cited by line number
  below — line numbers are accurate as of this plan's own research
  (2026-09-18) but may drift.

---

## Task 1: Add the `vision.generate` capability base

**Files:**
- Modify: `crates/aivyx-capability/src/lib.rs` (`KNOWN_BASES`,
  `CEILING_SEMITRUSTED`, the count-assertion test)

**Interfaces:**
- Produces: `Scope::parse("vision.generate")` succeeds; `CEILING_TRUSTED`
  and `CEILING_SEMITRUSTED` both grant it (SemiTrusted grants it, and
  Trusted's ceiling is a superset per this crate's own tier model, so no
  separate Trusted-list edit is needed — confirm this by reading how
  `calc.eval` is only added to `CEILING_SEMITRUSTED` in Step 1, not
  `CEILING_TRUSTED`, before writing this task's diff).

- [ ] **Step 1: Read the current `KNOWN_BASES` and `CEILING_SEMITRUSTED` lists**

```bash
grep -n "\"calc.eval\"\|\"convert.units\"\|\"date.compute\"" crates/aivyx-capability/src/lib.rs
```

Confirm these three appear in both `KNOWN_BASES` (with the Chapter Abacus
doc-comment block) and `CEILING_SEMITRUSTED`'s list — this is your
insertion template.

- [ ] **Step 2: Write the failing test**

Add to `crates/aivyx-capability/src/lib.rs`'s test module:

```rust
#[test]
fn vision_generate_is_a_known_base_reachable_at_semitrusted() {
    let scope = Scope::parse("vision.generate").expect("must be a known base");
    assert!(
        CEILING_SEMITRUSTED.is_granted_by(&scope) || CEILING_SEMITRUSTED.contains(&scope),
        "vision.generate must be reachable at SemiTrusted -- it's a narrower, \
         sanitized form of llm.call, which is already SemiTrusted-reachable"
    );
}

#[test]
fn vision_generate_is_absent_from_untrusted_ceiling() {
    let scope = Scope::parse("vision.generate").expect("must be a known base");
    assert!(
        !(CEILING_UNTRUSTED.is_granted_by(&scope) || CEILING_UNTRUSTED.contains(&scope)),
        "vision.generate must not be reachable at Untrusted by default"
    );
}
```

(Match whichever of `is_granted_by`/`contains`/a direct set-membership
check this file's own existing SemiTrusted-reachability tests for
`calc.eval` actually use — grep for a test named something like
`calc_eval_is_reachable_at_semitrusted` or similar and copy its exact
assertion shape rather than guessing at the `CapabilitySet` API here.)

- [ ] **Step 3: Run to verify it fails**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
cargo test -p aivyx-capability vision_generate -- --nocapture
```

Expected: `Scope::parse("vision.generate")` panics/errors — not a known
base yet.

- [ ] **Step 4: Add the base**

In `KNOWN_BASES`, add `"vision.generate"` with a doc comment following the
Chapter Abacus example's shape:

```rust
    // Aivyx-Vision Milestone 1 (2026-09-18) — vision.generate_svg tool
    // process. One base for the one tool this milestone adds; later
    // milestones' vision.generate_image / vision.generate_3d tools will
    // share this same base (nothing to read separately from what's
    // generated). Reachable at SemiTrusted: narrower and safer than
    // llm.call (constrained prompt, sanitized output), which is itself
    // already SemiTrusted-reachable. See aivyx-ecosystem/docs/superpowers/
    // specs/2026-09-18-aivyx-vision-v1-design.md.
    "vision.generate",
```

In `CEILING_SEMITRUSTED`'s list, add `"vision.generate"` alongside
`"calc.eval"`/`"convert.units"`/`"date.compute"`.

- [ ] **Step 5: Update the count assertion**

In `known_bases_count_matches_phase_143_a3_addendum`, bump the count from
95 to 96, and add one line to its inline enumeration comment:

```rust
    // Aivyx-Vision Milestone 1 adds vision.generate — the
    // vision.generate_svg tool process's one base (SemiTrusted-reachable,
    // see the KNOWN_BASES doc comment for why).
```

(Confirm 95 is still the actual current value by running Step 3's test
suite once before editing — if it's drifted from 95, use the real current
value, not the number written here.)

- [ ] **Step 6: Run to verify all three tests pass**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
cargo test -p aivyx-capability vision_generate known_bases_count -- --nocapture
```

- [ ] **Step 7: Run full crate check**

```bash
cargo test -p aivyx-capability
cargo clippy -p aivyx-capability --all-targets -- -D warnings
```

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat: add vision.generate capability base (SemiTrusted-reachable)

First step of Aivyx-Vision Milestone 1's aivyx-pa adoption. One base for
vision.generate_svg (Task 2-4 of this plan); later milestones' image/3D
tools will share it. Placed at CEILING_SEMITRUSTED per real precedent:
llm.call/llm.embed are already there, and this is a narrower, sanitized
form of an LLM call, not a broader one."
```

---

## Task 2: `aivyx-vision` crate scaffold + LLM provider config

**Files:**
- Create: `crates/aivyx-vision/Cargo.toml`
- Create: `crates/aivyx-vision/src/lib.rs`
- Create: `crates/aivyx-vision/src/config.rs`
- Modify: `Cargo.toml` (workspace root — add the `aivyx-vision-svg`
  pinned git dependency to `[workspace.dependencies]`, and add
  `crates/aivyx-vision` to workspace `members`)

**Interfaces:**
- Produces: `VisionConfig { provider: ProviderChoice, model: String }`
  where `ProviderChoice` is an enum (`Ollama { base_url: Option<String> }`,
  `Anthropic { api_key: SecretString }`, `OpenAi { api_key: SecretString }`)
  deserializable from TOML; `load_config(path: &Path) -> Result<VisionConfig, ConfigFileError>`;
  `default_config_path() -> Result<PathBuf, ConfigFileError>` (returns
  `~/.aivyx-pa/tool-processes/vision/config.toml`, matching every sibling
  tool-process's path shape — confirm the exact sibling path via
  `grep -n "tool-processes" crates/aivyx-toolkit/src/config.rs` before
  writing this).

- [ ] **Step 1: Read the current workspace root `Cargo.toml`'s dependency-pin block and an existing simple tool-process's `Cargo.toml`**

```bash
sed -n '270,325p' /home/julian/Projects/Rust/aivyx-pa/Cargo.toml
cat /home/julian/Projects/Rust/aivyx-pa/crates/aivyx-toolkit/Cargo.toml
```

- [ ] **Step 2: Add the `aivyx-vision-svg` pin to the workspace root `Cargo.toml`**

Insert alongside the existing `aivyx-confine`/`aivyx-checkpoint`/
`aivyx-kvcache`/`aivyx-injection-guard` entries in
`[workspace.dependencies]`:

```toml
# Not on crates.io — pinned by commit SHA, same as aivyx-checkpoint/
# aivyx-confine/aivyx-kvcache/aivyx-injection-guard above. No
# platform-specific backend, so no target-gating needed.
# MUST stay a public repo — see the aivyx-confine entry above for why.
aivyx-vision-svg = { git = "https://github.com/Aivyx-Agent/aivyx-vision", rev = "a80be4709b41382ef62c20545bb706e18a1d5ee3" }
```

(Verify `a80be4709b41382ef62c20545bb706e18a1d5ee3` is still `aivyx-vision`'s
real current `main` HEAD before using it —
`git ls-remote https://github.com/Aivyx-Agent/aivyx-vision main` — and use
the real current SHA if it has moved.)

Add `"crates/aivyx-vision"` to the workspace `members` list, in the same
style/position as `"crates/aivyx-toolkit"`.

- [ ] **Step 3: Write `crates/aivyx-vision/Cargo.toml`**

```toml
[package]
name = "aivyx-vision"
version = "0.1.0"
edition = "2024"
publish = false

[[bin]]
name = "aivyx-vision"
path = "src/main.rs"

[dependencies]
aivyx-core = { workspace = true }
aivyx-tool = { workspace = true }
aivyx-llm = { workspace = true }
aivyx-vision-svg = { workspace = true }
async-trait = { workspace = true }
serde = { workspace = true, features = ["derive"] }
serde_json = { workspace = true }
thiserror = { workspace = true }
tokio = { workspace = true, features = ["rt-multi-thread", "macros"] }
toml = { workspace = true }
secrecy = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
```

(Confirm every `{ workspace = true }` crate above already has a
`[workspace.dependencies]` entry at the version/feature-set this crate
needs — `grep -n "^aivyx-core \|^aivyx-tool \|^aivyx-llm \|^async-trait \|^serde \|^serde_json \|^thiserror \|^tokio \|^toml \|^secrecy " Cargo.toml`
— and adjust feature lists to match what's already pinned rather than
introducing a second, conflicting feature set for a shared dependency.)

- [ ] **Step 4: Write `src/config.rs`**

```rust
//! Operator-supplied LLM provider configuration for `aivyx-vision`.
//!
//! Loaded once at tool-process startup from
//! `~/.aivyx-pa/tool-processes/vision/config.toml`. This tool process
//! runs as a separate OS process from the daemon, so it cannot share
//! the daemon's own in-memory `Arc<dyn LlmProvider>` -- it gets its own,
//! separately-configured one, the same way `aivyx-gmail` has its own
//! OAuth config rather than sharing the daemon's.
//!
//! ## File format
//!
//! ```toml
//! # ~/.aivyx-pa/tool-processes/vision/config.toml
//!
//! provider = "ollama"   # "ollama" | "anthropic" | "openai"
//! model = "qwen3:8b"
//!
//! # Only read when provider = "ollama"; omit for the default localhost.
//! # base_url = "http://127.0.0.1:11434"
//!
//! # Only read when provider = "anthropic" or "openai"; required for those.
//! # api_key = "sk-..."
//! ```

use std::io;
use std::path::{Path, PathBuf};

use secrecy::SecretString;
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigFileError {
    #[error("config file I/O failed at {path:?}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("config file at {path:?} not found -- create it with an LLM provider/model (see aivyx-vision's own README)")]
    NotFound { path: PathBuf },
    #[error("config file at {path:?} failed to parse as TOML: {reason}")]
    Parse { path: PathBuf, reason: String },
    #[error("$HOME is unset; cannot resolve default config path")]
    NoHome,
    #[error("provider {provider:?} requires an api_key, but none was set")]
    MissingApiKey { provider: String },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RawProvider {
    Ollama,
    Anthropic,
    Openai,
}

#[derive(Debug, Deserialize)]
struct RawVisionConfig {
    provider: RawProvider,
    model: String,
    base_url: Option<String>,
    api_key: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ProviderChoice {
    Ollama { base_url: Option<String> },
    Anthropic { api_key: SecretString },
    Openai { api_key: SecretString },
}

#[derive(Debug, Clone)]
pub struct VisionConfig {
    pub provider: ProviderChoice,
    pub model: String,
}

pub fn default_config_path() -> Result<PathBuf, ConfigFileError> {
    let home = std::env::var_os("HOME").ok_or(ConfigFileError::NoHome)?;
    Ok(PathBuf::from(home)
        .join(".aivyx-pa")
        .join("tool-processes")
        .join("vision")
        .join("config.toml"))
}

pub fn load_config(path: &Path) -> Result<VisionConfig, ConfigFileError> {
    let raw_text = std::fs::read_to_string(path).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            ConfigFileError::NotFound {
                path: path.to_path_buf(),
            }
        } else {
            ConfigFileError::Io {
                path: path.to_path_buf(),
                source,
            }
        }
    })?;
    let raw: RawVisionConfig = toml::from_str(&raw_text).map_err(|e| ConfigFileError::Parse {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })?;

    let provider = match raw.provider {
        RawProvider::Ollama => ProviderChoice::Ollama {
            base_url: raw.base_url,
        },
        RawProvider::Anthropic => ProviderChoice::Anthropic {
            api_key: raw
                .api_key
                .map(SecretString::from)
                .ok_or_else(|| ConfigFileError::MissingApiKey {
                    provider: "anthropic".to_string(),
                })?,
        },
        RawProvider::Openai => ProviderChoice::Openai {
            api_key: raw
                .api_key
                .map(SecretString::from)
                .ok_or_else(|| ConfigFileError::MissingApiKey {
                    provider: "openai".to_string(),
                })?,
        },
    };

    Ok(VisionConfig {
        provider,
        model: raw.model,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_a_valid_ollama_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "provider = \"ollama\"\nmodel = \"qwen3:8b\"\n").unwrap();
        let config = load_config(&path).unwrap();
        assert_eq!(config.model, "qwen3:8b");
        assert!(matches!(config.provider, ProviderChoice::Ollama { base_url: None }));
    }

    #[test]
    fn loads_a_valid_ollama_config_with_base_url() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "provider = \"ollama\"\nmodel = \"qwen3:8b\"\nbase_url = \"http://127.0.0.1:9999\"\n",
        )
        .unwrap();
        let config = load_config(&path).unwrap();
        assert!(matches!(
            config.provider,
            ProviderChoice::Ollama { base_url: Some(ref u) } if u == "http://127.0.0.1:9999"
        ));
    }

    #[test]
    fn anthropic_without_api_key_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "provider = \"anthropic\"\nmodel = \"claude-3-5-sonnet-20241022\"\n").unwrap();
        let err = load_config(&path).unwrap_err();
        assert!(matches!(err, ConfigFileError::MissingApiKey { .. }));
    }

    #[test]
    fn missing_file_is_a_clear_not_found_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.toml");
        let err = load_config(&path).unwrap_err();
        assert!(matches!(err, ConfigFileError::NotFound { .. }));
    }

    #[test]
    fn malformed_toml_is_a_clear_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "this is not valid toml {{{").unwrap();
        let err = load_config(&path).unwrap_err();
        assert!(matches!(err, ConfigFileError::Parse { .. }));
    }
}
```

- [ ] **Step 5: Write a minimal `src/lib.rs`**

```rust
//! `aivyx-vision` — the aivyx-pa tool process exposing
//! `vision.generate_svg`, backed by the standalone `aivyx-vision-svg`
//! crate. See `crates/aivyx-vision/README.md` (added in a later task)
//! and `aivyx-ecosystem/docs/superpowers/specs/
//! 2026-09-18-aivyx-vision-v1-design.md` for the full design.

pub mod config;
```

- [ ] **Step 6: Run the new tests**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
cargo test -p aivyx-vision
```

Expected: all 5 tests pass.

- [ ] **Step 7: Run full workspace build to confirm the new crate + git dependency resolve cleanly**

```bash
cargo build
```

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat: scaffold aivyx-vision tool-process crate + LLM provider config

New crate, crates/aivyx-vision, following aivyx-toolkit's shape (no
OAuth). Depends on the standalone aivyx-vision-svg crate (pinned git
dependency) and aivyx-llm directly -- this tool process needs its own,
separately-configured LLM connection since it can't share the daemon's
in-memory provider across the process boundary."
```

---

## Task 3: `LlmTextCompleter` adapter

**Files:**
- Create: `crates/aivyx-vision/src/text_completer.rs`
- Modify: `crates/aivyx-vision/src/lib.rs` (add `pub mod text_completer;`)

**Interfaces:**
- Consumes: `aivyx_llm::LlmProvider`, `aivyx_llm::LlmRequest`,
  `aivyx_llm::LlmMessage`, `aivyx_llm::LlmStreamEvent`,
  `aivyx_vision_svg::{TextCompleter, TextCompleterError}`,
  `ProviderChoice`/`VisionConfig` (Task 2).
- Produces: `LlmTextCompleter::new(provider: Arc<dyn LlmProvider>, model: String, max_tokens: u32) -> Self`
  implementing `TextCompleter`; `build_provider(config: &VisionConfig) -> Result<Arc<dyn LlmProvider>, BuildProviderError>`.

**Before writing code:** read `aivyx-llm`'s actual current
`LlmProvider`/`LlmRequest`/`LlmMessage`/`LlmStreamEvent`/`LlmStream`
definitions yourself — `grep -n "pub trait LlmProvider\|pub struct LlmRequest\|pub enum LlmMessage\|pub enum LlmStreamEvent\|pub trait LlmStream" crates/aivyx-llm/src/lib.rs` — and the three concrete provider constructors — `grep -n "pub fn new\|pub struct.*Config" crates/aivyx-llm/src/ollama/provider.rs crates/aivyx-llm/src/anthropic/provider.rs crates/aivyx-llm/src/openai/provider.rs`. This plan's code sketch below reflects the shape found during this plan's own research (2026-09-18); adjust field names/signatures to match what you actually find if anything has drifted.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_llm::{LlmError, LlmProvider, LlmRequest, LlmStepEnd, LlmStream, LlmStreamEvent, LlmUsage};
    use tokio_util::sync::CancellationToken;

    /// A fake `LlmProvider` returning one canned text response, for
    /// testing the adapter without a real backend.
    struct FakeProvider {
        text: String,
    }

    struct FakeStream {
        remaining: Option<String>,
    }

    #[async_trait::async_trait]
    impl LlmStream for FakeStream {
        async fn next_event(&mut self) -> Result<Option<LlmStreamEvent>, LlmError> {
            Ok(self.remaining.take().map(LlmStreamEvent::TextChunk))
        }

        async fn finish(self: Box<Self>) -> Result<LlmStepEnd, LlmError> {
            Ok(LlmStepEnd::Text {
                usage: LlmUsage::default(),
            })
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for FakeProvider {
        async fn chat_stream(
            &self,
            _request: LlmRequest<'_>,
            _cancellation: &CancellationToken,
        ) -> Result<Box<dyn LlmStream>, LlmError> {
            Ok(Box::new(FakeStream {
                remaining: Some(self.text.clone()),
            }))
        }
    }

    #[tokio::test]
    async fn complete_returns_the_provider_s_full_text() {
        let provider: Arc<dyn LlmProvider> = Arc::new(FakeProvider {
            text: "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>".to_string(),
        });
        let completer = LlmTextCompleter::new(provider, "test-model".to_string(), 2048);
        let result = completer.complete("draw a circle").await.unwrap();
        assert_eq!(result, "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>");
    }
}
```

(Check the real `LlmStepEnd` enum shape and `LlmUsage`'s `Default` impl —
`grep -n "pub enum LlmStepEnd\|pub struct LlmUsage" crates/aivyx-llm/src/lib.rs`
— before finalizing this test; adjust the `finish` implementation above
to construct whatever the real terminal-value shape requires.)

- [ ] **Step 2: Run to verify it fails**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
cargo test -p aivyx-vision complete_returns -- --nocapture
```

Expected: compile error — `LlmTextCompleter` doesn't exist yet.

- [ ] **Step 3: Implement `LlmTextCompleter` and `build_provider`**

```rust
//! Adapts `aivyx-llm`'s `LlmProvider` to `aivyx-vision-svg`'s
//! `TextCompleter` seam, and builds the real provider from this
//! tool-process's own config (Task 2) -- deliberately separate from
//! whichever provider the daemon itself is using, since a tool process
//! runs in its own OS process and can't share the daemon's in-memory
//! provider.

use std::sync::Arc;

use aivyx_llm::{LlmError, LlmMessage, LlmProvider, LlmRequest, LlmStreamEvent};
use aivyx_vision_svg::{TextCompleter, TextCompleterError};
use tokio_util::sync::CancellationToken;

use crate::config::{ProviderChoice, VisionConfig};

#[derive(Debug, thiserror::Error)]
pub enum BuildProviderError {
    #[error("failed to construct the {provider} provider: {source}")]
    Provider { provider: String, source: LlmError },
}

/// Construct the real `LlmProvider` this config selects.
pub fn build_provider(config: &VisionConfig) -> Result<Arc<dyn LlmProvider>, BuildProviderError> {
    match &config.provider {
        ProviderChoice::Ollama { base_url } => {
            let mut ollama_config = aivyx_llm::ollama::OllamaConfig::default_local();
            if let Some(url) = base_url {
                ollama_config = ollama_config.with_base_url(url.clone());
            }
            let provider = aivyx_llm::ollama::OllamaProvider::new(ollama_config).map_err(|e| {
                BuildProviderError::Provider {
                    provider: "ollama".to_string(),
                    source: e,
                }
            })?;
            Ok(Arc::new(provider))
        }
        ProviderChoice::Anthropic { api_key } => {
            let anthropic_config = aivyx_llm::anthropic::AnthropicConfig::new(api_key.clone());
            let provider = aivyx_llm::anthropic::AnthropicProvider::new(anthropic_config).map_err(|e| {
                BuildProviderError::Provider {
                    provider: "anthropic".to_string(),
                    source: e,
                }
            })?;
            Ok(Arc::new(provider))
        }
        ProviderChoice::Openai { api_key } => {
            let openai_config = aivyx_llm::openai::OpenAiConfig::new(api_key.clone());
            let provider = aivyx_llm::openai::OpenAiProvider::new(openai_config).map_err(|e| {
                BuildProviderError::Provider {
                    provider: "openai".to_string(),
                    source: e,
                }
            })?;
            Ok(Arc::new(provider))
        }
    }
}

/// Adapts a real `Arc<dyn LlmProvider>` to `TextCompleter` by issuing a
/// single-turn, tool-free, non-cancellable completion and buffering the
/// full text response.
pub struct LlmTextCompleter {
    provider: Arc<dyn LlmProvider>,
    model: String,
    max_tokens: u32,
}

impl LlmTextCompleter {
    pub fn new(provider: Arc<dyn LlmProvider>, model: String, max_tokens: u32) -> Self {
        Self {
            provider,
            model,
            max_tokens,
        }
    }
}

#[async_trait::async_trait]
impl TextCompleter for LlmTextCompleter {
    async fn complete(&self, prompt: &str) -> Result<String, TextCompleterError> {
        let messages = vec![LlmMessage::user_text(prompt)];
        let request = LlmRequest {
            model: &self.model,
            system: None,
            messages: &messages,
            tools: &[],
            max_tokens: self.max_tokens,
            temperature: None,
            id_slot: None,
            slot_hint: None,
        };
        // One-shot, non-interactive call -- nothing to cancel from a
        // human-facing loop, so a token that's never triggered is
        // correct here, not a placeholder.
        let cancellation = CancellationToken::new();
        let mut stream = self
            .provider
            .chat_stream(request, &cancellation)
            .await
            .map_err(|e| TextCompleterError(e.to_string()))?;

        let mut text = String::new();
        while let Some(event) = stream
            .next_event()
            .await
            .map_err(|e| TextCompleterError(e.to_string()))?
        {
            if let LlmStreamEvent::TextChunk(chunk) = event {
                text.push_str(&chunk);
            }
        }
        stream
            .finish()
            .await
            .map_err(|e| TextCompleterError(e.to_string()))?;

        Ok(text)
    }
}
```

(The `aivyx_llm::ollama::OllamaConfig`/`OllamaProvider`,
`aivyx_llm::anthropic::AnthropicConfig`/`AnthropicProvider`,
`aivyx_llm::openai::OpenAiConfig`/`OpenAiProvider` paths above reflect
this plan's own research into `crates/aivyx-llm/src/{ollama,anthropic,openai}/provider.rs`
— confirm these are actually `pub` and re-exported at those exact paths
from `aivyx-llm`'s crate root or submodules before using them verbatim;
adjust import paths to match reality.)

- [ ] **Step 4: Run to verify it passes**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
cargo test -p aivyx-vision
```

- [ ] **Step 5: Run full crate check**

```bash
cargo clippy -p aivyx-vision --all-targets -- -D warnings
cargo fmt --check -p aivyx-vision
```

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat: add LlmTextCompleter adapter + provider construction

Bridges aivyx-llm's LlmProvider to aivyx-vision-svg's TextCompleter seam
via a single-turn, tool-free, buffered completion. build_provider
constructs whichever of Ollama/Anthropic/OpenAI the tool process's own
config selects."
```

---

## Task 4: `GenerateSvgTool` + `main.rs` wiring

**Files:**
- Create: `crates/aivyx-vision/src/tools.rs`
- Create: `crates/aivyx-vision/src/main.rs`
- Modify: `crates/aivyx-vision/src/lib.rs` (add `pub mod tools;`)

**Interfaces:**
- Consumes: `aivyx_core::Tool`, `aivyx_core::ToolContext`,
  `aivyx_core::ToolOutcome`, `aivyx_capability::Scope`,
  `aivyx_vision_svg::generate_svg`, `LlmTextCompleter` (Task 3),
  `aivyx_tool::run_multi_tool_subprocess` (or whatever the real, current
  harness entry point is named — confirm via
  `grep -n "pub async fn run_multi_tool_subprocess" crates/aivyx-tool/src/multi_harness.rs`
  and read its full signature before writing `main.rs`).
- Produces: `GenerateSvgTool` implementing `aivyx_core::Tool`, registered
  as `"vision.generate_svg"`, with `required_scope` returning
  `Scope::parse("vision.generate").unwrap()` for every input (no
  input-dependent scope escalation — this tool has one fixed capability
  need regardless of prompt content).

**Grounding for the code below:** `crates/aivyx-toolkit/src/tools/calc.rs`
(the `calc.eval` tool) is the exact template this task's code follows —
pure input-in/output-out, no OAuth, `SemiTrusted`-reachable, same
`ToolId::new()` / `ToolOutcome::Completed { output, verified:
Verification::NotApplicable }` / `ToolOutcome::Failed(AivyxError::Tool {
tool, detail })` shapes used verbatim below. Re-read that file yourself
before starting in case it has drifted since this plan's own research
(2026-09-18).

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use aivyx_vision_svg::TextCompleterError;

    /// A fake `TextCompleter` returning a canned response, mirroring
    /// `aivyx-vision-svg`'s own test-double shape.
    struct FakeCompleter {
        response: Mutex<Option<Result<String, TextCompleterError>>>,
    }

    impl FakeCompleter {
        fn returning(response: Result<String, TextCompleterError>) -> Self {
            Self {
                response: Mutex::new(Some(response)),
            }
        }
    }

    #[async_trait::async_trait]
    impl aivyx_vision_svg::TextCompleter for FakeCompleter {
        async fn complete(&self, _prompt: &str) -> Result<String, TextCompleterError> {
            self.response
                .lock()
                .unwrap()
                .take()
                .expect("FakeCompleter.complete called more than once")
        }
    }

    fn dummy_context() -> aivyx_core::TestToolContext {
        // Match whichever real, already-established test-fixture
        // constructor this workspace's own Tool tests use to build a
        // `ToolContext` for a unit test not going through the IPC
        // harness -- grep `crates/aivyx-toolkit/src/tools/calc.rs`'s own
        // test module (it calls `execute` with `_ctx` unused, since
        // calc.eval needs nothing from the context) and any nearby
        // tool test that DOES construct a real `ToolContext` for its
        // `execute()` call, and copy that exact helper.
        unimplemented!()
    }

    #[tokio::test]
    async fn tool_metadata_is_sound() {
        let tool = GenerateSvgTool::new(Arc::new(FakeCompleter::returning(Ok(String::new()))));
        assert_eq!(tool.name(), "vision.generate_svg");
        assert!(!tool.description().is_empty());
        assert_eq!(tool.input_schema()["type"], "object");
        assert_eq!(
            tool.required_scope(&json!({"prompt": "anything"})),
            Scope::parse("vision.generate").unwrap()
        );
    }

    #[tokio::test]
    async fn execute_returns_the_sanitized_svg_on_success() {
        let completer = FakeCompleter::returning(Ok(
            "<svg xmlns=\"http://www.w3.org/2000/svg\"><circle r=\"5\"/></svg>".to_string(),
        ));
        let tool = GenerateSvgTool::new(Arc::new(completer));
        let outcome = tool
            .execute(json!({"prompt": "a small circle"}), &dummy_context())
            .await;
        match outcome {
            ToolOutcome::Completed { output, .. } => {
                let svg = output["svg"].as_str().expect("output must have a svg string field");
                assert!(svg.contains("<circle"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_fails_clearly_on_a_missing_prompt_field() {
        let tool = GenerateSvgTool::new(Arc::new(FakeCompleter::returning(Ok(String::new()))));
        let outcome = tool.execute(json!({}), &dummy_context()).await;
        assert!(matches!(outcome, ToolOutcome::Failed(_)));
    }

    #[tokio::test]
    async fn execute_fails_clearly_when_generation_errors() {
        let completer = FakeCompleter::returning(Err(TextCompleterError("backend down".into())));
        let tool = GenerateSvgTool::new(Arc::new(completer));
        let outcome = tool
            .execute(json!({"prompt": "anything"}), &dummy_context())
            .await;
        assert!(matches!(outcome, ToolOutcome::Failed(_)));
    }
}
```

**`dummy_context()`'s `unimplemented!()` is intentional and must be
resolved in Step 3, before this task is done** — it is not a placeholder
left in committed code; it exists so Step 2 fails for the *test-helper*
reason first, then gets replaced with a real `ToolContext` constructor
found by reading this workspace's own existing test fixtures, the same
way every implementer this cycle has been asked to verify an external
API before trusting a sketch. If no existing tool test constructs a
`ToolContext` directly for a unit test outside the IPC harness (possible,
since `calc.eval`'s own tests never call `execute()` at all — they test
`evaluate()` directly and check tool metadata separately), building one
from `aivyx_core::ToolContext`'s real, current field list is this step's
job; do not leave `unimplemented!()` in the final commit.

- [ ] **Step 2: Run to verify they fail**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
cargo test -p aivyx-vision generate_svg_tool tool_metadata_is_sound execute_ -- --nocapture
```

Expected: compile error — `GenerateSvgTool` doesn't exist yet.

- [ ] **Step 3: Implement `GenerateSvgTool`, then resolve `dummy_context()`**

```rust
//! `vision.generate_svg` -- the one tool this milestone adds.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use aivyx_capability::Scope;
use aivyx_core::{AivyxError, Tool, ToolContext, ToolId, ToolOutcome, Verification};
use aivyx_vision_svg::TextCompleter;

pub struct GenerateSvgTool {
    id: ToolId,
    schema: Value,
    completer: Arc<dyn TextCompleter>,
}

impl GenerateSvgTool {
    pub fn new(completer: Arc<dyn TextCompleter>) -> Self {
        Self {
            id: ToolId::new(),
            schema: input_schema(),
            completer,
        }
    }
}

#[async_trait]
impl Tool for GenerateSvgTool {
    fn id(&self) -> ToolId {
        self.id
    }
    fn name(&self) -> &str {
        "vision.generate_svg"
    }
    fn description(&self) -> &str {
        "Generate a sanitized SVG image from a text prompt. Input: \
         `{prompt: string (required)}`. Returns `{svg: string}` -- the \
         sanitized SVG markup; the caller decides whether/where to save \
         it. Scope: `vision.generate`."
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("vision.generate")
            .expect("vision.generate must parse -- it is in KNOWN_BASES (Task 1)")
    }
    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let prompt = match required_string(&input, "prompt") {
            Ok(s) => s,
            Err(e) => return failed(self.id, format!("vision.generate_svg: {e}")),
        };
        match aivyx_vision_svg::generate_svg(self.completer.as_ref(), &prompt).await {
            Ok(svg) => ToolOutcome::Completed {
                output: json!({ "svg": svg }),
                // Generation + sanitization already happened; there is
                // nothing further to verify against an external source
                // of truth, same reasoning as calc.eval's own pure
                // computation.
                verified: Verification::NotApplicable,
            },
            Err(e) => failed(self.id, format!("vision.generate_svg: {e}")),
        }
    }
}

fn input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "prompt": {
                "type": "string",
                "minLength": 1,
                "description": "Description of the SVG image to generate, \
                                e.g. \"a small red circle icon\"."
            }
        },
        "required": ["prompt"],
        "additionalProperties": false
    })
}

fn required_string(input: &Value, field: &str) -> Result<String, String> {
    let s = input
        .get(field)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("input must include a `{field}` string field"))?;
    if s.trim().is_empty() {
        return Err(format!("`{field}` must not be empty"));
    }
    Ok(s.to_string())
}

fn failed(id: ToolId, detail: String) -> ToolOutcome {
    ToolOutcome::Failed(AivyxError::Tool { tool: id, detail })
}
```

Then resolve `dummy_context()` from Step 1's test module: read
`aivyx_core::ToolContext`'s real, current definition
(`grep -n "pub struct ToolContext" -A 30 crates/aivyx-core/src/lib.rs`)
and search the workspace for any existing unit test building one
directly (`grep -rln "ToolContext {" crates/*/src/**/*.rs crates/*/tests/*.rs 2>/dev/null`)
to find a real, minimal construction to copy — or, if every existing
`execute()` test in this workspace instead goes through a full
`ConcreteAgent`/IPC harness rather than constructing a bare
`ToolContext`, that's a real finding: report it in this task's own report
file and either follow that heavier pattern for these three tests, or
justify why a lighter fixture is safe here (this tool's `execute()`
ignores `_ctx` entirely, same as `calc.eval`'s, which is exactly the kind
of thing that makes a minimal/dummy context legitimate rather than a
shortcut — but confirm that reasoning against the real
`Tool::execute`/`ToolContext` contract before relying on it, since a
future tool added to this same file might not be able to ignore context
the way this one does).

- [ ] **Step 4: Write `src/main.rs`**

```rust
//! `aivyx-vision` binary entry point.
//!
//! The daemon spawns this via `[[tool_process]]` in `aivyx-pa.toml`; on
//! startup: load config (Task 2), build the LLM provider it names
//! (Task 3), wrap it in `LlmTextCompleter`, register `vision.generate_svg`
//! (Task 4), hand off to the multi-tool IPC harness.

use std::process::ExitCode;
use std::sync::Arc;

use aivyx_core::Tool;
use aivyx_vision::config::{default_config_path, load_config};
use aivyx_vision::text_completer::{build_provider, LlmTextCompleter};
use aivyx_vision::tools::GenerateSvgTool;

const MAX_TOKENS: u32 = 4096;

#[tokio::main]
async fn main() -> ExitCode {
    let config_path = match default_config_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("aivyx-vision: {e}");
            return ExitCode::FAILURE;
        }
    };
    let config = match load_config(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("aivyx-vision: {e}");
            return ExitCode::FAILURE;
        }
    };
    let provider = match build_provider(&config) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("aivyx-vision: {e}");
            return ExitCode::FAILURE;
        }
    };
    let completer = Arc::new(LlmTextCompleter::new(provider, config.model.clone(), MAX_TOKENS));
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(GenerateSvgTool::new(completer))];

    // Matches aivyx-toolkit's own main.rs call shape exactly (verified
    // against its real source during this plan's own research):
    // run_multi_tool_subprocess(tools, name, notify_receiver). No
    // DispatchNotification wire frame needed here, unlike aivyx-toolkit's
    // health-check-alert use of the third argument, hence `None`.
    match aivyx_tool::run_multi_tool_subprocess(tools, "vision", None).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aivyx-vision: {e}");
            ExitCode::FAILURE
        }
    }
}
```

- [ ] **Step 5: Run to verify all tests pass**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
cargo test -p aivyx-vision
```

- [ ] **Step 6: Run full crate + build check**

```bash
cargo build -p aivyx-vision
cargo clippy -p aivyx-vision --all-targets -- -D warnings
cargo fmt --check -p aivyx-vision
```

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat: add GenerateSvgTool + main.rs wiring for vision.generate_svg

The one tool this milestone adds. Input: {\"prompt\": string}. Output:
{\"svg\": string} -- the sanitized SVG markup, caller decides whether/
where to save it. required_scope always returns vision.generate,
regardless of prompt content (no input-dependent escalation needed)."
```

---

## Task 5: Docs + final verification

**Files:**
- Create: `crates/aivyx-vision/README.md`
- Modify: `docs/TOOLS.md` (add a `## Vision (tool process \`aivyx-vision\`)` section)
- Modify: `docs/TOOL_SDK.md` (if it documents `[[tool_process]]` entries by
  example — add `aivyx-vision`'s entry alongside the existing ones; check
  whether this is actually needed by reading how `aivyx-toolkit` or
  `aivyx-contacts` is documented there first)

**Interfaces:** none — documentation only.

- [ ] **Step 1: Read `docs/TOOLS.md`'s existing tool-process sections (e.g. the Kitchen or Contacts one) for the exact format to match**

```bash
grep -n "^## " docs/TOOLS.md
sed -n '/## Contacts/,/^## /p' docs/TOOLS.md | head -30
```

- [ ] **Step 2: Write `crates/aivyx-vision/README.md`**

```markdown
# aivyx-vision

The `aivyx-pa` tool process exposing `vision.generate_svg` — LLM-prompted,
sanitized SVG generation. Part of Aivyx-Vision's Milestone 1; wraps the
standalone [`aivyx-vision-svg`](https://github.com/Aivyx-Agent/aivyx-vision)
crate.

This tool process runs as a separate OS process from the daemon and
cannot share the daemon's own LLM provider — it has its own,
separately-configured one via `~/.aivyx-pa/tool-processes/vision/config.toml`:

```toml
provider = "ollama"   # "ollama" | "anthropic" | "openai"
model = "qwen3:8b"
# base_url = "http://127.0.0.1:11434"   # ollama only, optional
# api_key = "sk-..."                     # required for anthropic/openai
```

Add the corresponding `[[tool_process]]` entry to `aivyx-pa.toml`:

```toml
[[tool_process]]
name = "vision"
command = "aivyx-vision"
```

`vision.generate_svg` takes `{"prompt": "a small red circle icon"}` and
returns `{"svg": "<svg ...>...</svg>"}` — the sanitized SVG markup; the
caller decides whether and where to save it. Gated by the
`vision.generate` capability (`SemiTrusted`-and-above by default).

See `aivyx-ecosystem/docs/superpowers/specs/2026-09-18-aivyx-vision-v1-design.md`
for the full design.
```

- [ ] **Step 3: Add the `docs/TOOLS.md` catalog entry**

Following the exact format of an existing tool-process section (Step 1),
add a new section documenting `vision.generate_svg`: scope
(`vision.generate`), min trust tier (SemiTrusted), delivery mechanism
(tool process `aivyx-vision`), input/output shape.

- [ ] **Step 4: Final verification**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Expected: all green across the whole default-members workspace, not just
`aivyx-vision` — this is the first time this plan's changes are checked
against everything else in the workspace at once.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "docs: document the aivyx-vision tool process and vision.generate_svg"
```

---

## Final verification (after all 5 tasks land)

- [ ] Run the complete workspace test suite once, not per-crate:

```bash
cd /home/julian/Projects/Rust/aivyx-pa
cargo test 2>&1 | tail -30
```

- [ ] Run the documented clippy command once more:

```bash
cargo clippy --all-targets -- -D warnings
```

- [ ] This plan does not decide whether to push a branch / open a PR for
  `aivyx-pa` — follow `superpowers:finishing-a-development-branch` once
  all tasks are individually reviewed and a final whole-branch review has
  passed, same as every other plan executed against this repo this
  cycle.
