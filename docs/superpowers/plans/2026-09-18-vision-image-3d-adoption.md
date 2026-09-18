# Vision Image/3D Adoption (aivyx-pa) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `vision.generate_image` and `vision.generate_3d` to the
existing `crates/aivyx-vision` tool process, backed by the just-shipped
`aivyx-vision-core`/`aivyx-vision-mold` crates. This is `aivyx-pa`'s
adoption of Aivyx-Vision's Milestone 2 Pass A, alongside the already-shipped
`vision.generate_svg` (Milestone 1).

**Architecture:** Both new tools depend on `Arc<dyn
aivyx_vision_core::GenerationProvider>` (a trait object), exactly mirroring
`GenerateSvgTool`'s existing `Arc<dyn TextCompleter>` dependency — see
`crates/aivyx-vision/src/tools.rs`. `main.rs` constructs one concrete
`aivyx_vision_mold::MoldProvider` and hands the same `Arc` to both new
tools. Both reuse the already-shipped `vision.generate` capability base
(`CEILING_SEMITRUSTED`) — no `aivyx-capability` changes needed.
`reference_image` (image-to-image input) is restricted to a bare filename
resolved against the tool's own `output_dir` — this tool process has no
access to the daemon's fs sandbox, so accepting an arbitrary absolute path
would let a `vision.generate`-scoped agent read any file the OS user can
read and send its bytes to `mold_url`. See
`docs/superpowers/specs/2026-09-18-vision-image-3d-adoption-design.md` for
the full reasoning behind every decision below.

**Tech Stack:** Rust, edition 2024. `aivyx-vision-core`/`aivyx-vision-mold`
(new external git dependencies, same repo/rev as the existing
`aivyx-vision-svg` pin), `async-trait`, `serde`, `tokio` (all already
workspace dependencies).

## Global Constraints

- `cargo clippy --all-targets -- -D warnings` (default-members scope) must
  stay clean.
- `cargo test` (no `-p`) must stay green across the whole default-members
  workspace, not just `aivyx-vision`.
- No `aivyx-capability` changes — both tools use the existing
  `Scope::parse("vision.generate")`, already at `CEILING_SEMITRUSTED`. No
  `KNOWN_BASES` count-assertion bump needed.
- `aivyx-vision-svg`, `aivyx-vision-core`, and `aivyx-vision-mold` are all
  pinned to the same rev, `caed4c0` (Milestone 2 Pass A's merge commit in
  the `aivyx-vision` repo) — keep them in lockstep, don't let the existing
  `aivyx-vision-svg` pin drift stale relative to the two new ones.
- A missing `[mold]` section in `config.toml` must NOT prevent the process
  from starting — `vision.generate_svg` keeps working exactly as it does
  today; `vision.generate_image`/`vision.generate_3d` simply aren't
  registered. Every existing install stays backward compatible with zero
  changes required.
- `reference_image` must be a bare filename (no path separators, not
  `".."`), resolved and canonicalize-checked against the canonicalized
  `output_dir` — never accept or resolve an arbitrary absolute path.
- Re-read every file cited by line number below before editing — accurate
  as of this plan's own research (2026-09-18) but the repo moves.

---

## Task 1: Pin `aivyx-vision-core`/`aivyx-vision-mold`, bump `aivyx-vision-svg`

**Files:**
- Modify: `/home/julian/Projects/Rust/aivyx-pa/Cargo.toml` (root —
  `[workspace.dependencies]`, around the existing `aivyx-vision-svg` entry)
- Modify: `/home/julian/Projects/Rust/aivyx-pa/crates/aivyx-vision/Cargo.toml`

**Interfaces:** none new — dependency wiring only. Produces:
`aivyx_vision_core::{GenerationProvider, ImageRequest, ThreeDRequest,
GeneratedAsset, VisionError}` and
`aivyx_vision_mold::{MoldProvider, MoldConfig}` importable from
`aivyx-vision`.

This is not a TDD task (no new behavior yet) — the steps replace
write-test/verify-fail with record-baseline/verify-no-regression, same
shape as the `aivyx-vision` repo's own workspace-conversion task.

- [ ] **Step 1: Record the baseline**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
cargo test -p aivyx-vision 2>&1 | tail -20
```

Expected: all existing `aivyx-vision` tests pass (record the count).

- [ ] **Step 2: Bump the `aivyx-vision-svg` pin, add the two new pins**

In root `Cargo.toml`'s `[workspace.dependencies]` section, replace the
existing `aivyx-vision-svg` entry (currently pinned at
`a80be4709b41382ef62c20545bb706e18a1d5ee3`) with:

```toml
# Standalone generation backends for the aivyx-vision tool process's
# vision.generate_svg / vision.generate_image / vision.generate_3d
# (Chapter design docs:
# aivyx-ecosystem/docs/superpowers/specs/2026-09-18-aivyx-vision-v1-design.md,
# docs/superpowers/specs/2026-09-18-vision-image-3d-adoption-design.md).
# Not on crates.io — pinned by commit SHA, same as aivyx-checkpoint/
# aivyx-confine/aivyx-kvcache/aivyx-injection-guard above. All three pinned
# to the same rev (Milestone 2 Pass A's merge commit) so they never drift
# out of lockstep with each other. No platform-specific backend, so no
# target-gating needed.
# MUST stay a public repo — see the aivyx-confine entry above for why.
aivyx-vision-svg = { git = "https://github.com/Aivyx-Agent/aivyx-vision", rev = "caed4c0875a19ef5c7f4059ddda8cd6364f9fda4" }
aivyx-vision-core = { git = "https://github.com/Aivyx-Agent/aivyx-vision", rev = "caed4c0875a19ef5c7f4059ddda8cd6364f9fda4" }
aivyx-vision-mold = { git = "https://github.com/Aivyx-Agent/aivyx-vision", rev = "caed4c0875a19ef5c7f4059ddda8cd6364f9fda4" }
```

- [ ] **Step 3: Add the two new crates as dependencies of `aivyx-vision`**

In `crates/aivyx-vision/Cargo.toml`'s `[dependencies]` section, immediately
after the existing `aivyx-vision-svg = { workspace = true }` line, add:

```toml
aivyx-vision-core = { workspace = true }
aivyx-vision-mold = { workspace = true }
```

- [ ] **Step 4: Build and verify no regression**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
cargo build -p aivyx-vision
cargo test -p aivyx-vision 2>&1 | tail -20
cargo clippy -p aivyx-vision --all-targets -- -D warnings
cargo fmt --check
```

Expected: builds clean (pulling the two new git deps), identical test
count/pass rate to Step 1's baseline (no code uses the new crates yet, so
nothing new to test), clippy/fmt clean.

- [ ] **Step 5: Commit**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
git add Cargo.toml Cargo.lock crates/aivyx-vision/Cargo.toml
git commit -m "chore: pin aivyx-vision-core/aivyx-vision-mold, bump aivyx-vision-svg to the same rev

All three Aivyx-Vision crates now pinned to caed4c0 (Milestone 2 Pass A's
merge commit), keeping them in lockstep. No behavior change yet -- this
is dependency wiring ahead of Task 3's actual tool code."
```

---

## Task 2: `[mold]` config section

**Files:**
- Modify: `/home/julian/Projects/Rust/aivyx-pa/crates/aivyx-vision/src/config.rs`

**Interfaces:**
- Produces: `MoldSettings { broker_url: String, mold_url: String, api_key: Option<String>, output_dir: PathBuf }`,
  `VisionConfig.mold: Option<MoldSettings>`, `default_output_dir() -> Result<PathBuf, ConfigFileError>`.

- [ ] **Step 1: Write the failing tests**

Add to `config.rs`'s existing `#[cfg(test)] mod tests` block:

```rust
#[test]
fn loads_a_config_with_no_mold_section() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "provider = \"ollama\"\nmodel = \"qwen3:8b\"\n").unwrap();
    let config = load_config(&path).unwrap();
    assert!(config.mold.is_none());
}

#[test]
fn loads_a_config_with_a_full_mold_section() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "provider = \"ollama\"\nmodel = \"qwen3:8b\"\n\n\
         [mold]\n\
         broker_url = \"http://127.0.0.1:8899\"\n\
         mold_url = \"http://127.0.0.1:7680\"\n\
         api_key = \"secret\"\n\
         output_dir = \"/tmp/vision-out\"\n",
    )
    .unwrap();
    let config = load_config(&path).unwrap();
    let mold = config.mold.expect("mold section must be present");
    assert_eq!(mold.broker_url, "http://127.0.0.1:8899");
    assert_eq!(mold.mold_url, "http://127.0.0.1:7680");
    assert_eq!(mold.api_key.as_deref(), Some("secret"));
    assert_eq!(mold.output_dir, PathBuf::from("/tmp/vision-out"));
}

#[test]
fn loads_a_mold_section_with_no_api_key_or_output_dir() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "provider = \"ollama\"\nmodel = \"qwen3:8b\"\n\n\
         [mold]\n\
         broker_url = \"http://127.0.0.1:8899\"\n\
         mold_url = \"http://127.0.0.1:7680\"\n",
    )
    .unwrap();
    let config = load_config(&path).unwrap();
    let mold = config.mold.expect("mold section must be present");
    assert_eq!(mold.api_key, None);
    // Falls back to default_output_dir() -- just confirm it's the same
    // value that function computes, not a hardcoded literal here (this
    // test would need updating if $HOME weren't stable within one test
    // run, which it is).
    assert_eq!(mold.output_dir, default_output_dir().unwrap());
}

#[test]
fn default_output_dir_lands_under_home_local_share_aivyx_pa_vision() {
    let dir = default_output_dir().unwrap();
    assert!(dir.ends_with("aivyx-pa/vision"));
    assert!(dir.starts_with(std::env::var("HOME").unwrap()));
}
```

- [ ] **Step 2: Run to verify they fail**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
cargo test -p aivyx-vision config:: -- --nocapture
```

Expected: compile error — `VisionConfig` has no `mold` field, and
`default_output_dir` doesn't exist yet.

- [ ] **Step 3: Implement**

Add near the top of `config.rs`, alongside the existing `use` statements
(no new imports needed — `PathBuf` is already imported at line 26).

Add a new struct after `RawVisionConfig` (currently ending around line 62):

```rust
#[derive(Debug, Deserialize)]
struct RawMoldConfig {
    broker_url: String,
    mold_url: String,
    api_key: Option<String>,
    output_dir: Option<String>,
}
```

Modify `RawVisionConfig` (currently lines 56-62) to add one field:

```rust
#[derive(Debug, Deserialize)]
struct RawVisionConfig {
    provider: RawProvider,
    model: String,
    base_url: Option<String>,
    api_key: Option<String>,
    mold: Option<RawMoldConfig>,
}
```

Add a new public struct after `ProviderChoice` (currently lines 64-69):

```rust
/// Image-generation backend config -- present only when the operator has
/// added a `[mold]` section to `config.toml`. Absent by default, so an
/// existing install with only the LLM-provider fields (for
/// `vision.generate_svg`) keeps working unchanged; `vision.generate_image`/
/// `vision.generate_3d` simply aren't registered until this is configured.
/// See `docs/superpowers/specs/2026-09-18-vision-image-3d-adoption-design.md`.
#[derive(Debug, Clone)]
pub struct MoldSettings {
    pub broker_url: String,
    pub mold_url: String,
    pub api_key: Option<String>,
    pub output_dir: PathBuf,
}
```

Modify `VisionConfig` (currently lines 71-75) to add the new field:

```rust
#[derive(Debug, Clone)]
pub struct VisionConfig {
    pub provider: ProviderChoice,
    pub model: String,
    pub mold: Option<MoldSettings>,
}
```

Add a new public function after `default_config_path` (currently lines
77-84):

```rust
/// Where generated image/3D-model files land when the operator doesn't
/// override `[mold] output_dir` explicitly -- matches the ecosystem
/// spec's own storage convention
/// (`~/.local/share/aivyx-pa/vision/<uuid>.<ext>`;
/// `aivyx-vision-mold`'s own `MoldProvider` generates the `<uuid>.<ext>`
/// filename itself, so this only needs to point at the right directory).
pub fn default_output_dir() -> Result<PathBuf, ConfigFileError> {
    let home = std::env::var_os("HOME").ok_or(ConfigFileError::NoHome)?;
    Ok(PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("aivyx-pa")
        .join("vision"))
}
```

Modify `load_config` (currently lines 86-128) — insert the `mold`
resolution after `provider` is built (after the existing `let provider =
match raw.provider { ... };` block, before the final `Ok(VisionConfig {
... })`), and add `mold` to that final struct literal:

```rust
    let mold = raw
        .mold
        .map(|m| -> Result<MoldSettings, ConfigFileError> {
            let output_dir = match m.output_dir {
                Some(d) => PathBuf::from(d),
                None => default_output_dir()?,
            };
            Ok(MoldSettings {
                broker_url: m.broker_url,
                mold_url: m.mold_url,
                api_key: m.api_key,
                output_dir,
            })
        })
        .transpose()?;

    Ok(VisionConfig {
        provider,
        model: raw.model,
        mold,
    })
```

- [ ] **Step 4: Run to verify they pass**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
cargo test -p aivyx-vision config:: -- --nocapture
```

Expected: all 4 new tests pass, plus every pre-existing `config.rs` test
still passes (none of them set `[mold]`, so `mold` should be `None` for
all of them too — this is implicit in `Option`'s serde behavior, but worth
confirming the existing tests didn't need any change).

- [ ] **Step 5: Run full crate check**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
cargo test -p aivyx-vision
cargo clippy -p aivyx-vision --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 6: Commit**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
git add crates/aivyx-vision/src/config.rs
git commit -m "feat: add optional [mold] config section for image/3D generation

VisionConfig gains an Option<MoldSettings> field, populated only when the
operator adds a [mold] section to config.toml. Absent by default, so
every existing install keeps working with just vision.generate_svg --
vision.generate_image/vision.generate_3d simply aren't registered until
this is configured (wired in Task 4)."
```

---

## Task 3: `GenerateImageTool` + `GenerateThreeDTool`

**Files:**
- Create: `/home/julian/Projects/Rust/aivyx-pa/crates/aivyx-vision/src/generation_tools.rs`
- Modify: `/home/julian/Projects/Rust/aivyx-pa/crates/aivyx-vision/src/tools.rs`
  (mark two existing private helpers `pub(crate)` for reuse)
- Modify: `/home/julian/Projects/Rust/aivyx-pa/crates/aivyx-vision/src/lib.rs`

**Interfaces:**
- Consumes: `aivyx_vision_core::{GenerationProvider, ImageRequest,
  ThreeDRequest, GeneratedAsset, VisionError}` (Task 1's new dependency),
  `crate::tools::{required_string, failed}` (this task promotes both to
  `pub(crate)`).
- Produces: `GenerateImageTool::new(provider: Arc<dyn GenerationProvider>, output_dir: PathBuf) -> Self`,
  `GenerateThreeDTool::new(provider: Arc<dyn GenerationProvider>) -> Self`,
  both implementing `aivyx_core::Tool`.

- [ ] **Step 1: Write the failing tests**

```rust
use std::sync::Arc;

use aivyx_core::ToolOutcome;
use aivyx_vision_core::{FakeGenerationProvider, GeneratedAsset, VisionError};
use serde_json::json;

use super::*;
use crate::tools::tests::dummy_context; // see note below on making this reusable

fn sample_asset(path: &str) -> GeneratedAsset {
    GeneratedAsset {
        path: path.into(),
        backend: "mold".to_string(),
        seed_used: Some(42),
        generated_at: std::time::SystemTime::now(),
    }
}

#[tokio::test]
async fn generate_image_tool_metadata_is_sound() {
    let dir = tempfile::tempdir().unwrap();
    let provider = FakeGenerationProvider::with_image_result(Ok(sample_asset("/tmp/x.png")));
    let tool = GenerateImageTool::new(Arc::new(provider), dir.path().to_path_buf());
    assert_eq!(tool.name(), "vision.generate_image");
    assert!(!tool.description().is_empty());
    assert_eq!(tool.input_schema()["type"], "object");
    assert_eq!(
        tool.required_scope(&json!({"prompt": "anything"})),
        aivyx_capability::Scope::parse("vision.generate").unwrap()
    );
}

#[tokio::test]
async fn generate_image_returns_the_generated_asset_on_success() {
    let dir = tempfile::tempdir().unwrap();
    let provider =
        FakeGenerationProvider::with_image_result(Ok(sample_asset("/tmp/out/abc.png")));
    let tool = GenerateImageTool::new(Arc::new(provider), dir.path().to_path_buf());
    let outcome = tool
        .execute(json!({"prompt": "a red circle"}), &dummy_context())
        .await;
    match outcome {
        ToolOutcome::Completed { output, .. } => {
            assert_eq!(output["path"], "/tmp/out/abc.png");
            assert_eq!(output["backend"], "mold");
            assert_eq!(output["seed_used"], 42);
        }
        other => panic!("expected Completed, got {other:?}"),
    }
}

#[tokio::test]
async fn generate_image_passes_width_height_seed_style_hint_through() {
    let dir = tempfile::tempdir().unwrap();
    let provider = FakeGenerationProvider::with_image_result(Ok(sample_asset("/tmp/x.png")));
    let provider = Arc::new(provider);
    let tool = GenerateImageTool::new(provider.clone(), dir.path().to_path_buf());
    tool.execute(
        json!({
            "prompt": "a mountain",
            "width": 512,
            "height": 768,
            "seed": 7,
            "style_hint": "watercolor"
        }),
        &dummy_context(),
    )
    .await;
    let captured = provider
        .captured_image_request()
        .expect("must capture the request");
    assert_eq!(captured.prompt, "a mountain");
    assert_eq!(captured.width, Some(512));
    assert_eq!(captured.height, Some(768));
    assert_eq!(captured.seed, Some(7));
    assert_eq!(captured.style_hint.as_deref(), Some("watercolor"));
}

#[tokio::test]
async fn generate_image_fails_clearly_on_a_missing_prompt_field() {
    let dir = tempfile::tempdir().unwrap();
    let provider = FakeGenerationProvider::with_image_result(Ok(sample_asset("/tmp/x.png")));
    let tool = GenerateImageTool::new(Arc::new(provider), dir.path().to_path_buf());
    let outcome = tool.execute(json!({}), &dummy_context()).await;
    assert!(matches!(outcome, ToolOutcome::Failed(_)));
}

#[tokio::test]
async fn generate_image_fails_clearly_when_generation_errors() {
    let dir = tempfile::tempdir().unwrap();
    let provider = FakeGenerationProvider::with_image_result(Err(VisionError::GpuLockTimeout));
    let tool = GenerateImageTool::new(Arc::new(provider), dir.path().to_path_buf());
    let outcome = tool
        .execute(json!({"prompt": "anything"}), &dummy_context())
        .await;
    assert!(matches!(outcome, ToolOutcome::Failed(_)));
}

#[tokio::test]
async fn generate_image_accepts_a_reference_image_that_exists_in_output_dir() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("prior.png"), b"fake png bytes").unwrap();
    let provider = FakeGenerationProvider::with_image_result(Ok(sample_asset("/tmp/x.png")));
    let provider = Arc::new(provider);
    let tool = GenerateImageTool::new(provider.clone(), dir.path().to_path_buf());
    let outcome = tool
        .execute(
            json!({"prompt": "edit this", "reference_image": "prior.png"}),
            &dummy_context(),
        )
        .await;
    assert!(matches!(outcome, ToolOutcome::Completed { .. }));
    let captured = provider.captured_image_request().unwrap();
    assert_eq!(
        captured.reference_image,
        Some(dir.path().join("prior.png").canonicalize().unwrap())
    );
}

#[tokio::test]
async fn generate_image_rejects_a_reference_image_with_a_path_separator() {
    let dir = tempfile::tempdir().unwrap();
    let provider = FakeGenerationProvider::with_image_result(Ok(sample_asset("/tmp/x.png")));
    let tool = GenerateImageTool::new(Arc::new(provider), dir.path().to_path_buf());
    let outcome = tool
        .execute(
            json!({"prompt": "edit this", "reference_image": "../../etc/passwd"}),
            &dummy_context(),
        )
        .await;
    assert!(matches!(outcome, ToolOutcome::Failed(_)));
}

#[tokio::test]
async fn generate_image_rejects_a_reference_image_that_does_not_exist_in_output_dir() {
    let dir = tempfile::tempdir().unwrap();
    let provider = FakeGenerationProvider::with_image_result(Ok(sample_asset("/tmp/x.png")));
    let tool = GenerateImageTool::new(Arc::new(provider), dir.path().to_path_buf());
    let outcome = tool
        .execute(
            json!({"prompt": "edit this", "reference_image": "nonexistent.png"}),
            &dummy_context(),
        )
        .await;
    assert!(matches!(outcome, ToolOutcome::Failed(_)));
}

#[tokio::test]
async fn generate_3d_tool_metadata_is_sound() {
    let provider = FakeGenerationProvider::with_3d_result(Ok(sample_asset("/tmp/x.glb")));
    let tool = GenerateThreeDTool::new(Arc::new(provider));
    assert_eq!(tool.name(), "vision.generate_3d");
    assert!(!tool.description().is_empty());
    assert_eq!(
        tool.required_scope(&json!({"prompt": "anything"})),
        aivyx_capability::Scope::parse("vision.generate").unwrap()
    );
}

#[tokio::test]
async fn generate_3d_returns_the_generated_asset_on_success() {
    let provider = FakeGenerationProvider::with_3d_result(Ok(sample_asset("/tmp/out/x.glb")));
    let tool = GenerateThreeDTool::new(Arc::new(provider));
    let outcome = tool
        .execute(json!({"prompt": "a small statue"}), &dummy_context())
        .await;
    match outcome {
        ToolOutcome::Completed { output, .. } => {
            assert_eq!(output["path"], "/tmp/out/x.glb");
        }
        other => panic!("expected Completed, got {other:?}"),
    }
}

#[tokio::test]
async fn generate_3d_fails_clearly_when_unsupported() {
    let provider = FakeGenerationProvider::with_3d_result(Err(VisionError::Unsupported(
        "3D generation via mold (Pass B is not yet implemented)",
    )));
    let tool = GenerateThreeDTool::new(Arc::new(provider));
    let outcome = tool
        .execute(json!({"prompt": "a small statue"}), &dummy_context())
        .await;
    assert!(matches!(outcome, ToolOutcome::Failed(_)));
}

#[test]
fn resolve_reference_image_accepts_a_bare_filename_that_exists() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.png"), b"x").unwrap();
    let resolved = resolve_reference_image(dir.path(), "a.png").unwrap();
    assert_eq!(resolved, dir.path().join("a.png").canonicalize().unwrap());
}

#[test]
fn resolve_reference_image_rejects_a_forward_slash() {
    let dir = tempfile::tempdir().unwrap();
    assert!(resolve_reference_image(dir.path(), "sub/a.png").is_err());
}

#[test]
fn resolve_reference_image_rejects_a_backslash() {
    let dir = tempfile::tempdir().unwrap();
    assert!(resolve_reference_image(dir.path(), "sub\\a.png").is_err());
}

#[test]
fn resolve_reference_image_rejects_bare_dotdot() {
    let dir = tempfile::tempdir().unwrap();
    assert!(resolve_reference_image(dir.path(), "..").is_err());
}

#[test]
fn resolve_reference_image_rejects_an_empty_filename() {
    let dir = tempfile::tempdir().unwrap();
    assert!(resolve_reference_image(dir.path(), "").is_err());
}

#[test]
fn resolve_reference_image_rejects_a_filename_that_does_not_exist() {
    let dir = tempfile::tempdir().unwrap();
    assert!(resolve_reference_image(dir.path(), "missing.png").is_err());
}
```

Note on `dummy_context`: `tools.rs`'s existing test module
(`crates/aivyx-vision/src/tools.rs:141-189`) already defines a private
`dummy_context()` helper. Rather than duplicating it, this task makes it
reusable: change `mod tests {` (line 100) to `pub(crate) mod tests {` and
`fn dummy_context<'a>()` (line 141) to `pub(crate) fn dummy_context<'a>()`
in `tools.rs`, so `generation_tools.rs`'s own test module can
`use crate::tools::tests::dummy_context;` instead of copying ~50 lines of
fixture code. This is the one targeted improvement to existing code this
task makes — it's directly in service of this task's own tests, not
unrelated cleanup.

- [ ] **Step 2: Run to verify they fail**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
cargo test -p aivyx-vision generation_tools:: -- --nocapture
```

Expected: compile error — `generation_tools` module, `GenerateImageTool`,
`GenerateThreeDTool`, `resolve_reference_image` don't exist yet.

- [ ] **Step 3: Promote the two `tools.rs` helpers this task reuses**

In `crates/aivyx-vision/src/tools.rs`:
- Change `fn required_string(` (currently line 84) to
  `pub(crate) fn required_string(`.
- Change `fn failed(` (currently line 95) to `pub(crate) fn failed(`.
- Change `mod tests {` (currently line 100) to `pub(crate) mod tests {`.
- Change `fn dummy_context<'a>()` (currently line 141) to
  `pub(crate) fn dummy_context<'a>()`.

No other changes to `tools.rs` — `GenerateSvgTool` itself is untouched.

- [ ] **Step 4: Implement `generation_tools.rs`**

```rust
//! `vision.generate_image` and `vision.generate_3d` -- both backed by the
//! same `Arc<dyn GenerationProvider>` (concretely `aivyx-vision-mold`'s
//! `MoldProvider` today; see `main.rs`). `generate_3d` always fails today
//! with `VisionError::Unsupported` -- mold's Pass B (async 3D generation)
//! isn't built yet -- but the tool ships now so it's discoverable, and it
//! starts working with zero changes here once a real backend lands. See
//! `docs/superpowers/specs/2026-09-18-vision-image-3d-adoption-design.md`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use aivyx_capability::Scope;
use aivyx_core::{Tool, ToolContext, ToolId, ToolOutcome, Verification};
use aivyx_vision_core::{GeneratedAsset, GenerationProvider, ImageRequest, ThreeDRequest};

use crate::tools::{failed, required_string};

pub struct GenerateImageTool {
    id: ToolId,
    schema: Value,
    provider: Arc<dyn GenerationProvider>,
    output_dir: PathBuf,
}

impl GenerateImageTool {
    pub fn new(provider: Arc<dyn GenerationProvider>, output_dir: PathBuf) -> Self {
        Self {
            id: ToolId::new(),
            schema: image_input_schema(),
            provider,
            output_dir,
        }
    }
}

#[async_trait]
impl Tool for GenerateImageTool {
    fn id(&self) -> ToolId {
        self.id
    }
    fn name(&self) -> &str {
        "vision.generate_image"
    }
    fn description(&self) -> &str {
        "Generate an image from a text prompt via a local image-generation \
         backend. Input: `{prompt: string (required), width?: integer, \
         height?: integer, seed?: integer, style_hint?: string, \
         reference_image?: string}`. `reference_image` must be a bare \
         filename previously returned by this same tool (image-to-image) \
         -- arbitrary filesystem paths are rejected. Returns `{path: \
         string, backend: string, seed_used: integer|null}`. Scope: \
         `vision.generate`."
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("vision.generate")
            .expect("vision.generate must parse -- it is in aivyx-capability's KNOWN_BASES")
    }
    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let prompt = match required_string(&input, "prompt") {
            Ok(s) => s,
            Err(e) => return failed(self.id, format!("vision.generate_image: {e}")),
        };
        let reference_image = match input.get("reference_image").and_then(|v| v.as_str()) {
            Some(filename) => match resolve_reference_image(&self.output_dir, filename) {
                Ok(path) => Some(path),
                Err(e) => return failed(self.id, format!("vision.generate_image: {e}")),
            },
            None => None,
        };
        let req = ImageRequest {
            prompt,
            reference_image,
            width: optional_u32(&input, "width"),
            height: optional_u32(&input, "height"),
            seed: optional_u64(&input, "seed"),
            style_hint: optional_string(&input, "style_hint"),
        };
        match self.provider.generate_image(req).await {
            Ok(asset) => ToolOutcome::Completed {
                output: asset_to_json(&asset),
                // Generation already happened and the file is on disk;
                // there is nothing further to verify against an external
                // source of truth, same reasoning as vision.generate_svg's
                // own NotApplicable.
                verified: Verification::NotApplicable,
            },
            Err(e) => failed(self.id, format!("vision.generate_image: {e}")),
        }
    }
}

pub struct GenerateThreeDTool {
    id: ToolId,
    schema: Value,
    provider: Arc<dyn GenerationProvider>,
}

impl GenerateThreeDTool {
    pub fn new(provider: Arc<dyn GenerationProvider>) -> Self {
        Self {
            id: ToolId::new(),
            schema: threed_input_schema(),
            provider,
        }
    }
}

#[async_trait]
impl Tool for GenerateThreeDTool {
    fn id(&self) -> ToolId {
        self.id
    }
    fn name(&self) -> &str {
        "vision.generate_3d"
    }
    fn description(&self) -> &str {
        "Generate a 3D model from a text prompt via a local generation \
         backend. Input: `{prompt: string (required)}`. Returns `{path: \
         string, backend: string, seed_used: integer|null}`. **Not yet \
         implemented** -- every call currently fails with a clear error; \
         the tool exists now so it's discoverable ahead of a future \
         backend that implements it. Scope: `vision.generate`."
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("vision.generate")
            .expect("vision.generate must parse -- it is in aivyx-capability's KNOWN_BASES")
    }
    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let prompt = match required_string(&input, "prompt") {
            Ok(s) => s,
            Err(e) => return failed(self.id, format!("vision.generate_3d: {e}")),
        };
        let req = ThreeDRequest {
            prompt,
            reference_image: None,
            style_hint: None,
        };
        match self.provider.generate_3d(req).await {
            Ok(asset) => ToolOutcome::Completed {
                output: asset_to_json(&asset),
                verified: Verification::NotApplicable,
            },
            Err(e) => failed(self.id, format!("vision.generate_3d: {e}")),
        }
    }
}

fn asset_to_json(asset: &GeneratedAsset) -> Value {
    json!({
        "path": asset.path.display().to_string(),
        "backend": asset.backend,
        "seed_used": asset.seed_used,
    })
}

fn optional_u32(input: &Value, field: &str) -> Option<u32> {
    input
        .get(field)
        .and_then(|v| v.as_u64())
        .and_then(|v| u32::try_from(v).ok())
}

fn optional_u64(input: &Value, field: &str) -> Option<u64> {
    input.get(field).and_then(|v| v.as_u64())
}

fn optional_string(input: &Value, field: &str) -> Option<String> {
    input
        .get(field)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Resolves a caller-supplied `reference_image` filename against
/// `output_dir`, rejecting anything that isn't a bare filename that
/// already exists inside it. This is the load-bearing security check
/// this tool has -- see the module doc comment and this crate's own
/// design doc for why: this tool process has no access to the daemon's
/// fs sandbox, so `reference_image` must never resolve outside
/// `output_dir`.
fn resolve_reference_image(output_dir: &Path, filename: &str) -> Result<PathBuf, String> {
    if filename.is_empty() {
        return Err("reference_image must not be empty".to_string());
    }
    if filename.contains('/') || filename.contains('\\') || filename == ".." {
        return Err(
            "reference_image must be a bare filename (no path separators), \
             referencing a file this tool itself previously generated"
                .to_string(),
        );
    }
    let canonical_output_dir = std::fs::canonicalize(output_dir)
        .map_err(|e| format!("cannot canonicalize output_dir {output_dir:?}: {e}"))?;
    let candidate = output_dir.join(filename);
    let canonical_candidate = std::fs::canonicalize(&candidate)
        .map_err(|e| format!("reference_image {filename:?} not found in output_dir: {e}"))?;
    if !canonical_candidate.starts_with(&canonical_output_dir) {
        return Err(format!(
            "reference_image {filename:?} resolved outside output_dir"
        ));
    }
    Ok(canonical_candidate)
}

fn image_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "prompt": {
                "type": "string",
                "minLength": 1,
                "description": "Description of the image to generate, \
                                e.g. \"a small red circle icon\"."
            },
            "width": {"type": "integer", "minimum": 1},
            "height": {"type": "integer", "minimum": 1},
            "seed": {"type": "integer", "minimum": 0},
            "style_hint": {
                "type": "string",
                "description": "A free-text style nudge, e.g. \"watercolor\", \"pixel art\"."
            },
            "reference_image": {
                "type": "string",
                "description": "Bare filename (no path separators) of a \
                                file this tool itself previously \
                                generated, for image-to-image generation."
            }
        },
        "required": ["prompt"],
        "additionalProperties": false
    })
}

fn threed_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "prompt": {
                "type": "string",
                "minLength": 1,
                "description": "Description of the 3D model to generate."
            }
        },
        "required": ["prompt"],
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    // (Step 1's tests land here.)
}
```

Save as `crates/aivyx-vision/src/generation_tools.rs`.

- [ ] **Step 5: Wire the module into `lib.rs`**

Current full content of `crates/aivyx-vision/src/lib.rs`:

```rust
//! `aivyx-vision` — the aivyx-pa tool process exposing
//! `vision.generate_svg`, backed by the standalone `aivyx-vision-svg`
//! crate. See `README.md` and `aivyx-ecosystem/docs/superpowers/specs/
//! 2026-09-18-aivyx-vision-v1-design.md` for the full design.

pub mod config;
pub mod text_completer;
pub mod tools;
```

Replace it with:

```rust
//! `aivyx-vision` — the aivyx-pa tool process exposing
//! `vision.generate_svg` (Milestone 1) and
//! `vision.generate_image`/`vision.generate_3d` (Milestone 2 Pass A),
//! backed by the standalone `aivyx-vision-svg`/`aivyx-vision-core`/
//! `aivyx-vision-mold` crates. See `README.md` and
//! `docs/superpowers/specs/2026-09-18-vision-image-3d-adoption-design.md`
//! (this repo) and `aivyx-ecosystem/docs/superpowers/specs/
//! 2026-09-18-aivyx-vision-v1-design.md` for the full design.

pub mod config;
pub mod generation_tools;
pub mod text_completer;
pub mod tools;
```

- [ ] **Step 6: Run to verify they pass**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
cargo test -p aivyx-vision -- --nocapture
```

Expected: all 16 new tests pass (12 tool-level + 6 `resolve_reference_image`
unit tests — recount from Step 1's actual test list rather than trusting
this number), plus every pre-existing `aivyx-vision` test still green.

- [ ] **Step 7: Run full crate check**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
cargo test -p aivyx-vision
cargo clippy -p aivyx-vision --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 8: Commit**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
git add crates/aivyx-vision/src/generation_tools.rs \
        crates/aivyx-vision/src/tools.rs \
        crates/aivyx-vision/src/lib.rs
git commit -m "feat: add GenerateImageTool + GenerateThreeDTool

Both depend on Arc<dyn GenerationProvider>, mirroring GenerateSvgTool's
own Arc<dyn TextCompleter> dependency -- wired to a concrete MoldProvider
in main.rs (Task 4). reference_image is restricted to a bare filename
resolved against output_dir (canonicalize-and-prefix-check, the same
idiom aivyx-core's fs.read/fs.write already use) since this tool process
has no access to the daemon's own fs sandbox. generate_3d always fails
with VisionError::Unsupported today -- ships now so it's discoverable,
starts working once a real 3D backend lands with no changes here."
```

---

## Task 4: Wire into `main.rs`, update docs, final verification

**Files:**
- Modify: `/home/julian/Projects/Rust/aivyx-pa/crates/aivyx-vision/src/main.rs`
- Modify: `/home/julian/Projects/Rust/aivyx-pa/docs/TOOLS.md`
- Modify: `/home/julian/Projects/Rust/aivyx-pa/crates/aivyx-vision/README.md`

**Interfaces:** none new — wiring + documentation only.

- [ ] **Step 1: Wire `main.rs`**

The current `main.rs` (read in full — 60 lines, shown in this plan's own
research) ends with:

```rust
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(GenerateSvgTool::new(completer))];

    // Matches aivyx-toolkit's own main.rs call shape exactly:
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

Replace the `let tools: Vec<...> = vec![...]` line and everything between
it and the `match aivyx_tool::run_multi_tool_subprocess(...)` call with:

```rust
    let mut tools: Vec<Arc<dyn Tool>> = vec![Arc::new(GenerateSvgTool::new(completer))];

    if let Some(mold_settings) = &config.mold {
        let mold_config = aivyx_vision_mold::MoldConfig {
            broker_url: mold_settings.broker_url.clone(),
            mold_url: mold_settings.mold_url.clone(),
            api_key: mold_settings.api_key.clone(),
            output_dir: mold_settings.output_dir.clone(),
        };
        match aivyx_vision_mold::MoldProvider::new(mold_config) {
            Ok(provider) => {
                let provider: Arc<dyn aivyx_vision_core::GenerationProvider> = Arc::new(provider);
                tools.push(Arc::new(aivyx_vision::generation_tools::GenerateImageTool::new(
                    provider.clone(),
                    mold_settings.output_dir.clone(),
                )));
                tools.push(Arc::new(aivyx_vision::generation_tools::GenerateThreeDTool::new(
                    provider,
                )));
            }
            Err(e) => {
                // [mold] is present but the provider failed to construct
                // (e.g. HTTP client build failure) -- degrade the same way
                // a missing [mold] section does, rather than failing the
                // whole process over an optional capability.
                eprintln!(
                    "aivyx-vision: failed to construct the mold provider, \
                     vision.generate_image/vision.generate_3d will be \
                     unavailable: {e}"
                );
            }
        }
    }
```

Add one import near the top, alongside the existing `use aivyx_vision::...`
lines:

```rust
use aivyx_vision::generation_tools::{GenerateImageTool, GenerateThreeDTool};
```

(If you added this import, simplify the two `tools.push(Arc::new(...))`
calls above to drop the `aivyx_vision::generation_tools::` prefix,
matching how `GenerateSvgTool` is already imported unqualified.)

- [ ] **Step 2: Manual smoke check (no `[mold]` configured)**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
cargo build -p aivyx-vision
```

Expected: builds clean. (A full runtime smoke test needs a real config
file + Ollama, out of scope for this step — the point here is just
confirming the new code compiles and the `if let Some(...)` branch is
inert when `config.mold` is `None`, which Task 2's own tests already
cover at the `config.rs` level.)

- [ ] **Step 3: Update `docs/TOOLS.md`**

Find the existing section (search for `## Aivyx-Vision`):

```markdown
## Aivyx-Vision (tool process `aivyx-vision`, Milestone 1 — 2026-09-18)

One base for the one tool this milestone adds; later milestones'
`vision.generate_image` / `vision.generate_3d` tools will share the same
base (nothing to read separately from what's generated). SemiTrusted:
narrower and safer than `llm.call` (constrained prompt, sanitized output),
which is itself already SemiTrusted-reachable.

| Tool | Scope | Min tier | Notes |
|---|---|---|---|
| `vision.generate_svg` | `vision.generate` | SemiTrusted | LLM-generated SVG from a text prompt (constrained prompt, sanitized output) |
```

Replace it with:

```markdown
## Aivyx-Vision (tool process `aivyx-vision`, Milestone 1 — 2026-09-18; Milestone 2 Pass A — 2026-09-18)

One base shared by all three generation domains (vector/SVG, image, 3D)
— nothing to read separately from what's generated. SemiTrusted: narrower
and safer than `llm.call` (constrained prompt / bounded local generation,
sanitized or locally-written output), which is itself already
SemiTrusted-reachable.

| Tool | Scope | Min tier | Notes |
|---|---|---|---|
| `vision.generate_svg` | `vision.generate` | SemiTrusted | LLM-generated SVG from a text prompt (constrained prompt, sanitized output) |
| `vision.generate_image` | `vision.generate` | SemiTrusted | Local image generation via `mold serve` (requires `[mold]` config; writes a file under `output_dir`, returns its path) |
| `vision.generate_3d` | `vision.generate` | SemiTrusted | Local 3D model generation — **not yet implemented** (mold's Pass B); always fails today with a clear error |
```

- [ ] **Step 4: Update `crates/aivyx-vision/README.md`**

The current file (34 lines, read in full during this plan's own research)
ends with a pointer to the design spec. Insert a new section right before
that final pointer line (`See aivyx-ecosystem/docs/superpowers/specs/...`):

```markdown
## Image and 3D generation (Milestone 2 Pass A)

Add a `[mold]` section to the same `config.toml` to enable
`vision.generate_image`/`vision.generate_3d`:

```toml
[mold]
broker_url = "http://127.0.0.1:8899"    # aivyx-broker
mold_url = "http://127.0.0.1:7680"      # mold serve
# api_key = "..."                        # optional, only if mold serve sets MOLD_API_KEY
# output_dir = "..."                     # optional, defaults to ~/.local/share/aivyx-pa/vision/
```

Without `[mold]`, the process still starts with just `vision.generate_svg`
registered — this is fully backward compatible with an existing install.

`vision.generate_image` takes
`{"prompt": "...", "width"?: int, "height"?: int, "seed"?: int,
"style_hint"?: string, "reference_image"?: string}` and returns
`{"path": "...", "backend": "mold", "seed_used": int|null}`.
`reference_image`, if given, must be a **bare filename** (no path
separators) previously returned by this same tool — arbitrary filesystem
paths are rejected, since this tool process has no access to the daemon's
own fs sandbox.

`vision.generate_3d` takes `{"prompt": "..."}` and returns the same shape
on success — but every call fails today with a clear error message; mold's
3D generation ("Pass B") isn't built yet.

Both gated by the same `vision.generate` capability (`SemiTrusted`-and-above)
as `vision.generate_svg`.
```

- [ ] **Step 5: Final full-workspace verification**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
cargo build -p aivyx-vision
cargo test -p aivyx-vision 2>&1 | tail -40
cargo clippy -p aivyx-vision --all-targets -- -D warnings
cargo fmt --check
```

Expected: all green.

- [ ] **Step 6: Commit**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
git add crates/aivyx-vision/src/main.rs docs/TOOLS.md crates/aivyx-vision/README.md
git commit -m "feat: wire vision.generate_image/vision.generate_3d into main.rs, update docs

Registers both new tools only when [mold] is configured, degrading
gracefully (process still starts, just without them) if it's absent or
the provider fails to construct. Completes aivyx-pa's adoption of
Aivyx-Vision Milestone 2 Pass A."
```

---

## Final verification (after all 4 tasks land)

- [ ] Run the complete crate test suite once, not per-task:

```bash
cd /home/julian/Projects/Rust/aivyx-pa
cargo test -p aivyx-vision 2>&1 | tail -40
```

- [ ] Run the default-members-wide checks once more (this plan only
  touches `crates/aivyx-vision`, but confirm nothing else regressed):

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace 2>&1 | tail -20
cargo fmt --check
```

- [ ] This plan does not decide whether to push a branch / open a PR —
  follow `superpowers:finishing-a-development-branch` once all tasks are
  individually reviewed and a final whole-branch review has passed, same
  as every other plan executed this cycle.

## Explicitly out of scope for this plan

(Copied forward from the design spec's own "What this design does not
decide" section, so a future reader doesn't mistake this plan's silence on
these for an oversight.)

- Pass B itself (a real `generate_3d` implementation) — separate,
  not-yet-scoped future work in the `aivyx-vision` repo.
- Budget/rate-limiting integration with `aivyx-cost`'s Chapter K —
  deferred, not resolved.
- Any cross-process fs-sandbox-forwarding mechanism — `reference_image`'s
  output-dir restriction is this pass's answer; a more general mechanism,
  if ever needed, is separate future work.
- Retention/cleanup of generated files — operator-managed, no automatic
  sweep, matching the ecosystem spec's own v1 default.
- `aivyx-coder`'s equivalent adoption — a separate plan in that repo.
