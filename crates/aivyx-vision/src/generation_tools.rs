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
use aivyx_vision_core::{
    GeneratedAsset, GenerationProvider, ImageRequest, MAX_VARIATIONS, ThreeDRequest,
    UpscaleRequest, generate_variations,
};

use crate::tools::{failed, required_string};

pub struct GenerateImageTool {
    id: ToolId,
    schema: Value,
    provider: Arc<dyn GenerationProvider>,
    output_dir: PathBuf,
    previews: bool,
}

impl GenerateImageTool {
    pub fn new(provider: Arc<dyn GenerationProvider>, output_dir: PathBuf) -> Self {
        Self {
            id: ToolId::new(),
            schema: image_input_schema(),
            provider,
            output_dir,
            previews: true,
        }
    }

    /// `[mold] show_model_previews` (default on): attach a small preview
    /// of each image for a model that can see images.
    pub fn with_model_previews(mut self, on: bool) -> Self {
        self.previews = on;
        self
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
         reference_image?: string, strength?: number, count?: integer}`. `reference_image` \
         must be a bare filename previously returned by this same tool \
         (image-to-image) -- arbitrary filesystem paths are rejected; \
         without width/height the result keeps the reference's size. \
         `strength` (0.0-1.0, image-to-image only) is how far the result \
         may move from the reference: low keeps it, high changes more. \
         `count` (1-4) makes that many variations with consecutive seeds \
         and returns `{images: [...], failed: [string]}` instead. Returns `{path: \
         string, backend: string, seed_used: integer|null, warnings: [string]}`. Scope: \
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
            Some(filename) => {
                match resolve_generated_file(&self.output_dir, "reference_image", filename) {
                    Ok(path) => Some(path),
                    Err(e) => return failed(self.id, format!("vision.generate_image: {e}")),
                }
            }
            None => None,
        };
        let req = ImageRequest {
            prompt,
            reference_image,
            width: optional_u32(&input, "width"),
            height: optional_u32(&input, "height"),
            seed: optional_u64(&input, "seed"),
            style_hint: optional_string(&input, "style_hint"),
            strength: optional_f32(&input, "strength"),
            ..Default::default()
        };
        let count = optional_u32(&input, "count").unwrap_or(1) as usize;
        if count != 1 {
            return match generate_variations(self.provider.as_ref(), req, count).await {
                Err(e) => failed(self.id, format!("vision.generate_image: {e}")),
                Ok(results) => {
                    let (images, errors): (Vec<_>, Vec<_>) =
                        results.into_iter().partition(|r| r.is_ok());
                    if images.is_empty() {
                        let first = errors.into_iter().find_map(|r| r.err());
                        return failed(
                            self.id,
                            format!(
                                "vision.generate_image: every variation failed: {}",
                                first.map(|e| e.to_string()).unwrap_or_default()
                            ),
                        );
                    }
                    let assets: Vec<GeneratedAsset> = images.into_iter().flatten().collect();
                    let output = json!({
                        "images": assets.iter().map(asset_to_json).collect::<Vec<_>>(),
                        "failed": errors
                            .into_iter()
                            .filter_map(|r| r.err())
                            .map(|e| e.to_string())
                            .collect::<Vec<_>>(),
                    });
                    ToolOutcome::Completed {
                        output: with_previews(output, self.previews, assets.iter().map(|a| a.path.clone()))
                            .await,
                        verified: Verification::NotApplicable,
                    }
                }
            };
        }
        match self.provider.generate_image(req).await {
            Ok(asset) => ToolOutcome::Completed {
                output: with_previews(asset_to_json(&asset), self.previews, [asset.path.clone()]).await,
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

pub struct UpscaleImageTool {
    id: ToolId,
    schema: Value,
    provider: Arc<dyn GenerationProvider>,
    output_dir: PathBuf,
    previews: bool,
}

impl UpscaleImageTool {
    pub fn new(provider: Arc<dyn GenerationProvider>, output_dir: PathBuf) -> Self {
        Self {
            id: ToolId::new(),
            schema: upscale_input_schema(),
            provider,
            output_dir,
            previews: true,
        }
    }

    /// `[mold] show_model_previews` (default on): attach a small preview
    /// of each image for a model that can see images.
    pub fn with_model_previews(mut self, on: bool) -> Self {
        self.previews = on;
        self
    }
}

#[async_trait]
impl Tool for UpscaleImageTool {
    fn id(&self) -> ToolId {
        self.id
    }
    fn name(&self) -> &str {
        "vision.upscale_image"
    }
    fn description(&self) -> &str {
        "Enlarge an image made by vision.generate_image four times with the \
         local backend's upscaler, saving it as a new file. Input: `{image: \
         string (required)}`, a bare filename previously returned by \
         vision.generate_image, up to 1 megapixel (for example 1024x1024). \
         Returns `{path: string, backend: string, seed_used: null, warnings: \
         [string]}`. Scope: `vision.generate`."
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("vision.generate")
            .expect("vision.generate must parse -- it is in aivyx-capability's KNOWN_BASES")
    }
    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let filename = match required_string(&input, "image") {
            Ok(s) => s,
            Err(e) => return failed(self.id, format!("vision.upscale_image: {e}")),
        };
        let image = match resolve_generated_file(&self.output_dir, "image", &filename) {
            Ok(path) => path,
            Err(e) => return failed(self.id, format!("vision.upscale_image: {e}")),
        };
        match self.provider.upscale_image(UpscaleRequest { image }).await {
            Ok(asset) => ToolOutcome::Completed {
                output: with_previews(asset_to_json(&asset), self.previews, [asset.path.clone()]).await,
                verified: Verification::NotApplicable,
            },
            Err(e) => failed(self.id, format!("vision.upscale_image: {e}")),
        }
    }
}

pub struct GenerateThreeDTool {
    id: ToolId,
    schema: Value,
    provider: Arc<dyn GenerationProvider>,
    output_dir: PathBuf,
}

impl GenerateThreeDTool {
    pub fn new(provider: Arc<dyn GenerationProvider>, output_dir: PathBuf) -> Self {
        Self {
            id: ToolId::new(),
            schema: threed_input_schema(),
            provider,
            output_dir,
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
        "Generate a 3D model (a .glb mesh, untextured) of one object via a \
         local generation backend; takes about half a minute. Input: \
         `{prompt: string (required), reference_image?: string, \
         style_hint?: string, seed?: integer}`. Without `reference_image` \
         an image of the object is generated first and kept (its path is \
         in `warnings`). `reference_image` must be a bare filename \
         vision.generate_image returned, showing one whole object on a \
         plain background (a busy background becomes stray fragments). \
         Returns `{path: string, backend: string, seed_used: integer|null, \
         warnings: [string]}`. Scope: `vision.generate`."
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
        let reference_image = match input.get("reference_image").and_then(|v| v.as_str()) {
            Some(filename) => {
                match resolve_generated_file(&self.output_dir, "reference_image", filename) {
                    Ok(path) => Some(path),
                    Err(e) => return failed(self.id, format!("vision.generate_3d: {e}")),
                }
            }
            None => None,
        };
        let req = ThreeDRequest {
            prompt,
            reference_image,
            style_hint: optional_string(&input, "style_hint"),
            seed: optional_u64(&input, "seed"),
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

/// Adds a small JPEG of each image under `aivyx_core::TOOL_IMAGES_KEY`, so
/// a model that can see images is shown what it made. An image that can't
/// be previewed is left out; the result still names its file.
async fn with_previews(
    mut output: Value,
    on: bool,
    paths: impl IntoIterator<Item = PathBuf>,
) -> Value {
    use base64::Engine;
    if !on {
        return output;
    }
    let paths: Vec<PathBuf> = paths.into_iter().take(aivyx_core::MAX_TOOL_IMAGES).collect();
    let previews = tokio::task::spawn_blocking(move || {
        paths
            .iter()
            .filter_map(|path| aivyx_vision_mold::preview::model_preview(path).ok())
            .map(|(media_type, jpeg)| {
                json!({
                    "media_type": media_type,
                    "data": base64::engine::general_purpose::STANDARD.encode(jpeg),
                })
            })
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    if !previews.is_empty()
        && let Some(object) = output.as_object_mut()
    {
        object.insert(aivyx_core::TOOL_IMAGES_KEY.to_string(), Value::Array(previews));
    }
    output
}

fn asset_to_json(asset: &GeneratedAsset) -> Value {
    json!({
        "path": asset.path.display().to_string(),
        "backend": asset.backend,
        "seed_used": asset.seed_used,
        // The backend's advisories (for example a size it adjusted), so the
        // agent reports what it got rather than what it asked for.
        "warnings": asset.warnings,
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

fn optional_f32(input: &Value, field: &str) -> Option<f32> {
    input.get(field).and_then(|v| v.as_f64()).map(|v| v as f32)
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
/// A bare filename this tool process generated earlier, resolved inside
/// `output_dir`; `field` names the input it came from in errors.
fn resolve_generated_file(
    output_dir: &Path,
    field: &str,
    filename: &str,
) -> Result<PathBuf, String> {
    if filename.is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    if filename.contains('/') || filename.contains('\\') || filename == ".." {
        return Err(format!(
            "{field} must be a bare filename (no path separators), \
                 referencing a file this tool itself previously generated"
        ));
    }
    let canonical_output_dir = std::fs::canonicalize(output_dir)
        .map_err(|e| format!("cannot canonicalize output_dir {output_dir:?}: {e}"))?;
    let candidate = output_dir.join(filename);
    let canonical_candidate = std::fs::canonicalize(&candidate)
        .map_err(|e| format!("{field} {filename:?} not found in output_dir: {e}"))?;
    if !canonical_candidate.starts_with(&canonical_output_dir) {
        return Err(format!("{field} {filename:?} resolved outside output_dir"));
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
                                generated, for image-to-image generation. \
                                Without width/height the result keeps its size."
            },
            "strength": {
                "type": "number",
                "minimum": 0,
                "maximum": 1,
                "description": "Image-to-image only: how far the result may \
                                move from reference_image, from 0 (keep it) \
                                to 1 (ignore it). Default 0.75."
            },
            "count": {
                "type": "integer",
                "minimum": 1,
                "maximum": MAX_VARIATIONS,
                "description": "How many variations to make, with consecutive \
                                seeds. Default 1."
            }
        },
        "required": ["prompt"],
        "additionalProperties": false
    })
}

fn upscale_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "image": {
                "type": "string",
                "minLength": 1,
                "description": "Bare filename (no path separators) of an \
                                image vision.generate_image made, up to 1 \
                                megapixel."
            }
        },
        "required": ["image"],
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
                "description": "Description of the object to model."
            },
            "reference_image": {
                "type": "string",
                "description": "Bare filename (no path separators) of an \
                                image vision.generate_image made, showing one \
                                whole object on a plain background."
            },
            "style_hint": {
                "type": "string",
                "description": "A style nudge for the generated image, e.g. \
                                \"low poly\"."
            },
            "seed": {"type": "integer", "minimum": 0}
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
            warnings: Vec::new(),
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
    async fn generate_image_passes_its_optional_fields_through() {
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
                "style_hint": "watercolor",
                "strength": 0.5
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
        assert_eq!(captured.strength, Some(0.5));
    }

    /// Answers every call with the seed it was asked for, failing the
    /// call numbered `fail`.
    struct Seeds {
        calls: std::sync::Mutex<u64>,
        fail: Option<u64>,
    }

    #[async_trait]
    impl GenerationProvider for Seeds {
        async fn generate_image(&self, req: ImageRequest) -> Result<GeneratedAsset, VisionError> {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            if self.fail == Some(*calls) {
                return Err(VisionError::GenerationTimeout);
            }
            Ok(GeneratedAsset {
                seed_used: req.seed,
                ..sample_asset(&format!("/out/{calls}.png"))
            })
        }
        async fn generate_3d(&self, _: ThreeDRequest) -> Result<GeneratedAsset, VisionError> {
            Err(VisionError::Unsupported("3D"))
        }
    }

    #[tokio::test]
    async fn generate_image_with_count_returns_every_variation_and_each_failure() {
        let dir = tempfile::tempdir().unwrap();
        let provider = Arc::new(Seeds {
            calls: std::sync::Mutex::new(0),
            fail: Some(2),
        });
        let tool = GenerateImageTool::new(provider, dir.path().to_path_buf());
        let outcome = tool
            .execute(
                json!({"prompt": "a cat", "seed": 5, "count": 3}),
                &dummy_context(),
            )
            .await;
        match outcome {
            ToolOutcome::Completed { output, .. } => {
                let seeds: Vec<_> = output["images"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|i| i["seed_used"].clone())
                    .collect();
                assert_eq!(seeds, [json!(5), json!(7)]);
                assert_eq!(output["failed"].as_array().unwrap().len(), 1);
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn generated_images_carry_a_preview_for_the_model() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("a.png");
        let mut bytes = Vec::new();
        image::RgbImage::from_pixel(1024, 512, image::Rgb([10, 20, 30]))
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        std::fs::write(&png, bytes).unwrap();
        let provider = FakeGenerationProvider::with_image_result(Ok(sample_asset(
            png.to_str().unwrap(),
        )));
        let tool = GenerateImageTool::new(Arc::new(provider), dir.path().to_path_buf());
        let outcome = tool
            .execute(json!({"prompt": "a dark band"}), &dummy_context())
            .await;
        let ToolOutcome::Completed { output, .. } = outcome else {
            panic!("expected Completed, got {outcome:?}")
        };
        let previews = output[aivyx_core::TOOL_IMAGES_KEY].as_array().unwrap();
        assert_eq!(previews.len(), 1);
        assert_eq!(previews[0]["media_type"], "image/jpeg");
        use base64::Engine;
        let jpeg = base64::engine::general_purpose::STANDARD
            .decode(previews[0]["data"].as_str().unwrap())
            .unwrap();
        let preview = image::load_from_memory(&jpeg).unwrap();
        assert_eq!((preview.width(), preview.height()), (768, 384));
    }

    #[tokio::test]
    async fn previews_switched_off_are_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("a.png");
        image::RgbImage::from_pixel(8, 8, image::Rgb([1, 2, 3])).save(&png).unwrap();
        let provider = FakeGenerationProvider::with_image_result(Ok(sample_asset(
            png.to_str().unwrap(),
        )));
        let tool = GenerateImageTool::new(Arc::new(provider), dir.path().to_path_buf())
            .with_model_previews(false);
        let outcome = tool.execute(json!({"prompt": "x"}), &dummy_context()).await;
        let ToolOutcome::Completed { output, .. } = outcome else {
            panic!("expected Completed, got {outcome:?}")
        };
        assert!(output.get(aivyx_core::TOOL_IMAGES_KEY).is_none(), "{output}");
    }

    #[tokio::test]
    async fn a_result_without_a_readable_image_has_no_preview() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FakeGenerationProvider::with_image_result(Ok(sample_asset("/tmp/missing.png")));
        let tool = GenerateImageTool::new(Arc::new(provider), dir.path().to_path_buf());
        let outcome = tool
            .execute(json!({"prompt": "x"}), &dummy_context())
            .await;
        let ToolOutcome::Completed { output, .. } = outcome else {
            panic!("expected Completed, got {outcome:?}")
        };
        assert!(output.get(aivyx_core::TOOL_IMAGES_KEY).is_none(), "{output}");
    }

    #[tokio::test]
    async fn generate_image_refuses_a_count_over_four() {
        let dir = tempfile::tempdir().unwrap();
        let provider = Arc::new(Seeds {
            calls: std::sync::Mutex::new(0),
            fail: None,
        });
        let tool = GenerateImageTool::new(provider, dir.path().to_path_buf());
        let outcome = tool
            .execute(json!({"prompt": "a cat", "count": 9}), &dummy_context())
            .await;
        assert!(matches!(outcome, ToolOutcome::Failed(_)), "{outcome:?}");
    }

    #[tokio::test]
    async fn upscale_image_takes_a_generated_file_and_refuses_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.png"), b"x").unwrap();
        let provider = Arc::new(FakeGenerationProvider::with_upscale_result(Ok(
            sample_asset("/tmp/out/big.png"),
        )));
        let tool = UpscaleImageTool::new(provider.clone(), dir.path().to_path_buf());
        assert_eq!(tool.name(), "vision.upscale_image");
        let outcome = tool
            .execute(json!({"image": "a.png"}), &dummy_context())
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Completed { .. }),
            "{outcome:?}"
        );
        assert_eq!(
            provider.captured_upscale_request().unwrap().image,
            dir.path().join("a.png").canonicalize().unwrap()
        );

        let outcome = tool
            .execute(json!({"image": "../secret.png"}), &dummy_context())
            .await;
        match outcome {
            ToolOutcome::Failed(e) => assert!(e.to_string().contains("bare filename"), "{e}"),
            other => panic!("expected Failed, got {other:?}"),
        }
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
        let tool = GenerateThreeDTool::new(Arc::new(provider), std::env::temp_dir());
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
        let tool = GenerateThreeDTool::new(Arc::new(provider), std::env::temp_dir());
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
    async fn generate_3d_passes_a_generated_reference_and_refuses_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lantern.png"), b"x").unwrap();
        let provider = Arc::new(FakeGenerationProvider::with_3d_result(Ok(sample_asset(
            "/tmp/out/x.glb",
        ))));
        let tool = GenerateThreeDTool::new(provider.clone(), dir.path().to_path_buf());
        let outcome = tool
            .execute(
                json!({"prompt": "a lantern", "reference_image": "lantern.png", "seed": 3}),
                &dummy_context(),
            )
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Completed { .. }),
            "{outcome:?}"
        );
        let captured = provider.captured_3d_request().unwrap();
        assert_eq!(
            captured.reference_image,
            Some(dir.path().join("lantern.png").canonicalize().unwrap())
        );
        assert_eq!(captured.seed, Some(3));

        let outcome = tool
            .execute(
                json!({"prompt": "a lantern", "reference_image": "/etc/passwd"}),
                &dummy_context(),
            )
            .await;
        assert!(matches!(outcome, ToolOutcome::Failed(_)), "{outcome:?}");
    }

    #[tokio::test]
    async fn generate_3d_fails_clearly_when_unsupported() {
        let provider = FakeGenerationProvider::with_3d_result(Err(VisionError::Unsupported(
            "3D generation via mold (Pass B is not yet implemented)",
        )));
        let tool = GenerateThreeDTool::new(Arc::new(provider), std::env::temp_dir());
        let outcome = tool
            .execute(json!({"prompt": "a small statue"}), &dummy_context())
            .await;
        assert!(matches!(outcome, ToolOutcome::Failed(_)));
    }

    #[test]
    fn resolve_reference_image_accepts_a_bare_filename_that_exists() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.png"), b"x").unwrap();
        let resolved = resolve_generated_file(dir.path(), "reference_image", "a.png").unwrap();
        assert_eq!(resolved, dir.path().join("a.png").canonicalize().unwrap());
    }

    #[test]
    fn resolve_reference_image_rejects_a_forward_slash() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve_generated_file(dir.path(), "reference_image", "sub/a.png").is_err());
    }

    #[test]
    fn resolve_reference_image_rejects_a_backslash() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve_generated_file(dir.path(), "reference_image", "sub\\a.png").is_err());
    }

    #[test]
    fn resolve_reference_image_rejects_bare_dotdot() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve_generated_file(dir.path(), "reference_image", "..").is_err());
    }

    #[test]
    fn resolve_reference_image_rejects_an_empty_filename() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve_generated_file(dir.path(), "reference_image", "").is_err());
    }

    #[test]
    fn resolve_reference_image_rejects_a_filename_that_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve_generated_file(dir.path(), "reference_image", "missing.png").is_err());
    }
}
