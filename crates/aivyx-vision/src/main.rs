//! `aivyx-vision` binary entry point.
//!
//! The daemon spawns this via `[[tool_process]]` in `aivyx-pa.toml`; on
//! startup: load config (`config.rs`), build the LLM provider it names
//! (`text_completer::build_provider`), wrap it in `LlmTextCompleter`,
//! register `vision.generate_svg` (`tools::GenerateSvgTool`), hand off
//! to the multi-tool IPC harness.

use std::process::ExitCode;
use std::sync::Arc;

use aivyx_core::Tool;
use aivyx_vision::config::{default_config_path, load_config};
use aivyx_vision::generation_tools::{GenerateImageTool, GenerateThreeDTool};
use aivyx_vision::text_completer::{LlmTextCompleter, build_provider};
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
    let completer = Arc::new(LlmTextCompleter::new(
        provider,
        config.model.clone(),
        MAX_TOKENS,
    ));
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
                tools.push(Arc::new(GenerateImageTool::new(
                    provider.clone(),
                    mold_settings.output_dir.clone(),
                )));
                tools.push(Arc::new(GenerateThreeDTool::new(provider)));
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
