//! `aivyx-proton` — the `proton.*` tool process. The daemon spawns it via
//! `[[tool_process]] name = "proton"` (sandboxed like Gmail); it reads the
//! Bridge credential `aivyx-pa connect proton` wrote and serves
//! `proton.search`, `proton.read`, `proton.draft` and `proton.send`.

use std::process::ExitCode;
use std::sync::Arc;

use aivyx_core::Tool;
use aivyx_instance::InstancePaths;
use aivyx_proton::credentials;
use aivyx_proton::tools::{Mail, ProtonDraft, ProtonRead, ProtonSearch, ProtonSend};
use aivyx_tool::multi_harness::run_multi_tool_subprocess;

#[tokio::main]
async fn main() -> ExitCode {
    if std::env::args().len() > 1 {
        eprintln!(
            "aivyx-proton takes no arguments: the aivyx-pa daemon runs it as a \
             [[tool_process]]. Set it up with `aivyx-pa connect proton`."
        );
        return ExitCode::from(2);
    }
    let Some(path) = InstancePaths::current().ok().and_then(|i| credentials::path_for(&i)) else {
        eprintln!("aivyx-proton: can't resolve the instance's folders (is HOME set?)");
        return ExitCode::from(2);
    };
    let creds = match credentials::load(&path) {
        Ok(Some(c)) => c,
        Ok(None) => {
            eprintln!("aivyx-proton: not connected — run `aivyx-pa connect proton` first");
            return ExitCode::from(2);
        }
        Err(e) => {
            eprintln!("aivyx-proton: {e}");
            return ExitCode::from(2);
        }
    };
    // The tools never file handled mail, so the folder is unused here.
    let mail: Mail = match creds.transport("Aivyx") {
        Ok(t) => Arc::new(t),
        Err(e) => {
            eprintln!("aivyx-proton: {e}");
            return ExitCode::from(2);
        }
    };
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(ProtonSearch::new(Arc::clone(&mail))),
        Arc::new(ProtonRead::new(Arc::clone(&mail))),
        Arc::new(ProtonDraft::new(Arc::clone(&mail), creds.from_address.clone())),
        Arc::new(ProtonSend::new(Arc::clone(&mail), creds.from_address.clone())),
    ];
    match run_multi_tool_subprocess(tools, "aivyx-proton", None).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aivyx-proton: harness exited with error: {e}");
            ExitCode::from(1)
        }
    }
}
