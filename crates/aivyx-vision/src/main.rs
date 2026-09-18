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
