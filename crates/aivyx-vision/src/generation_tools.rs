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
}
