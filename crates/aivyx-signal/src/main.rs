//! `aivyx-signal` — the `signal.send` tool process. The daemon spawns it via
//! `[[tool_process]] name = "signal"` (sandboxed like Gmail); it talks to
//! the operator's signal-cli daemon over the socket in the instance's
//! signal tool folder and reads `send_to` from `signal-tool.toml`.

use std::process::ExitCode;
use std::sync::Arc;

use aivyx_core::Tool;
use aivyx_instance::InstancePaths;
use aivyx_signal::toolconf;
use aivyx_signal::tools::SignalSend;
use aivyx_signal::transport::{CliTransport, SignalTransport};
use aivyx_tool::multi_harness::run_multi_tool_subprocess;

#[tokio::main]
async fn main() -> ExitCode {
    if std::env::args().len() > 1 {
        eprintln!(
            "aivyx-signal takes no arguments: the aivyx-pa daemon runs it as a \
             [[tool_process]]. Set it up with `aivyx-pa connect signal`."
        );
        return ExitCode::from(2);
    }
    let Ok(instance) = InstancePaths::current() else {
        eprintln!("aivyx-signal: can't resolve the instance's folders (is HOME set?)");
        return ExitCode::from(2);
    };
    let (Some(socket), Some(conf)) = (toolconf::socket_path(&instance), toolconf::tool_config_path(&instance)) else {
        eprintln!("aivyx-signal: can't resolve the instance's folders (is HOME set?)");
        return ExitCode::from(2);
    };
    let send_to = match toolconf::load(&conf) {
        Ok(Some(c)) => c.send_to,
        Ok(None) => {
            eprintln!("aivyx-signal: not set up — run `aivyx-pa connect signal` first");
            return ExitCode::from(2);
        }
        Err(e) => {
            eprintln!("aivyx-signal: {e}");
            return ExitCode::from(2);
        }
    };
    let transport: Arc<dyn SignalTransport> = Arc::new(CliTransport::new(socket));
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(SignalSend::new(transport, send_to))];
    match run_multi_tool_subprocess(tools, "aivyx-signal", None).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aivyx-signal: harness exited with error: {e}");
            ExitCode::from(1)
        }
    }
}
