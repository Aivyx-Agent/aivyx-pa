//! `aivyx-n8n` binary entry point.
//!
//! Phase 131 Task 2 scaffold. Per-tool implementations
//! land in Tasks 3-12.

use std::process::ExitCode;
use std::sync::Arc;

use aivyx_core::Tool;
use aivyx_n8n::auth_cli::{
    cli::{help_text, parse_cli_args_from, AuthMode, BinaryMode},
    config_file::{default_config_path, load_config},
    status::{run_auth_check, run_auth_status},
};
use aivyx_n8n::tools::{
    N8nActivateWorkflow, N8nCreateWorkflow, N8nDeactivateWorkflow, N8nDeleteWorkflow,
    N8nExecuteWorkflow, N8nGetExecution, N8nGetWorkflow, N8nListExecutions, N8nListWorkflows,
    N8nUpdateWorkflow,
};
use aivyx_n8n::{run_multi_tool_subprocess, N8nClient};

#[tokio::main]
async fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().collect();
    let mode = match parse_cli_args_from(&argv) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("aivyx-n8n: {e}");
            eprintln!("Run `aivyx-n8n help` for usage.");
            return ExitCode::from(2);
        }
    };
    match mode {
        BinaryMode::Help => {
            println!("{}", help_text());
            ExitCode::SUCCESS
        }
        BinaryMode::Auth(AuthMode::Status) => run_status_cmd(),
        BinaryMode::Auth(AuthMode::Check) => run_check_cmd().await,
        BinaryMode::IpcLoop => run_ipc_loop().await,
    }
}

fn run_status_cmd() -> ExitCode {
    let config_path = match default_config_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("aivyx-n8n auth status: {e}");
            return ExitCode::from(2);
        }
    };
    match run_auth_status(&config_path) {
        Ok(report) => {
            print!("{report}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("aivyx-n8n auth status: {e}");
            ExitCode::from(1)
        }
    }
}

async fn run_check_cmd() -> ExitCode {
    let config_path = match default_config_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("aivyx-n8n auth check: {e}");
            return ExitCode::from(2);
        }
    };
    let config = match load_config(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("aivyx-n8n auth check: {e}");
            return ExitCode::from(2);
        }
    };
    let client = N8nClient::new(http_client_for(&config.n8n_base_url), config);
    let report = run_auth_check(&client).await;
    let ok = report.ok;
    print!("{report}");
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

async fn run_ipc_loop() -> ExitCode {
    let config_path = match default_config_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("aivyx-n8n (ipc): {e}");
            return ExitCode::from(2);
        }
    };
    let config = match load_config(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("aivyx-n8n (ipc): {e}");
            return ExitCode::from(2);
        }
    };
    let client = Arc::new(N8nClient::new(http_client_for(&config.n8n_base_url), config));

    // Phase 131 Q1c — 10-tool surface. Tasks 3-12 populate
    // this incrementally; Tasks 3-4 wire the first two
    // read tools.
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(N8nListWorkflows::new(client.clone())),
        Arc::new(N8nGetWorkflow::new(client.clone())),
        Arc::new(N8nListExecutions::new(client.clone())),
        Arc::new(N8nGetExecution::new(client.clone())),
        Arc::new(N8nExecuteWorkflow::new(client.clone())),
        Arc::new(N8nActivateWorkflow::new(client.clone())),
        Arc::new(N8nDeactivateWorkflow::new(client.clone())),
        Arc::new(N8nCreateWorkflow::new(client.clone())),
        Arc::new(N8nUpdateWorkflow::new(client.clone())),
        Arc::new(N8nDeleteWorkflow::new(client.clone())),
    ];
    match run_multi_tool_subprocess(tools, "aivyx-n8n", None).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aivyx-n8n (ipc): harness exited with error: {e}");
            ExitCode::from(1)
        }
    }
}

/// The client for the operator's n8n: direct (no proxy) when it is on a
/// local address, as a self-hosted n8n usually is; otherwise honouring a
/// configured proxy. reqwest would send even loopback requests -- with the
/// n8n API key -- to `HTTP_PROXY`/`ALL_PROXY` (aivyx-vision audit
/// 2026-11-11, X1).
fn http_client_for(base_url: &str) -> reqwest::Client {
    let builder = if aivyx_route::url_locality(base_url) == aivyx_route::Locality::Local {
        reqwest::Client::builder().no_proxy()
    } else {
        reqwest::Client::builder()
    };
    builder.build().unwrap_or_else(|_| reqwest::Client::new())
}
