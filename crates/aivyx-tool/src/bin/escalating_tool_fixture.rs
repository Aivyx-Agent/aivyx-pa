//! Task 4 (HIGH, 2026-09-16 audit) conformance fixture — wraps a tool
//! whose `execute()` always returns `ToolOutcome::RequiresEscalation`
//! as a tool process, via [`run_multi_tool_subprocess`] (the real
//! `multi_harness.rs` dispatch path every third-party integration
//! crate — Gmail, Drive, Notion, … — actually uses).
//!
//! The `tests/escalation_propagation.rs` integration test spawns this
//! binary and drives it through the real `ToolProcessBridge` +
//! `ToolProxy`, proving the fix end to end: an out-of-process tool's
//! `RequiresEscalation` now survives the wire round trip as
//! `ToolOutcome::RequiresEscalation`, not a generic `Failed`. Built
//! only as a test fixture — operators do not run this directly.

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use aivyx_capability::Scope;
use aivyx_core::{Tool, ToolContext, ToolId, ToolOutcome};
use aivyx_tool::run_multi_tool_subprocess;

struct AlwaysEscalatesTool {
    id: ToolId,
    schema: Value,
}

#[async_trait]
impl Tool for AlwaysEscalatesTool {
    fn id(&self) -> ToolId {
        self.id
    }
    fn name(&self) -> &str {
        "escalating.fixture"
    }
    fn description(&self) -> &str {
        "always requires escalation, for conformance testing"
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("email.send").expect("email.send is in KNOWN_BASES")
    }
    async fn execute(&self, _input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        ToolOutcome::RequiresEscalation {
            reason: "fixture always escalates".to_string(),
            // Real tool processes never set this — the daemon-side
            // turn loop stamps the authoritative scope (RN.3). `None`
            // matches that real-world shape.
            scope: None,
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let tool: Arc<dyn Tool> = Arc::new(AlwaysEscalatesTool {
        id: ToolId::new(),
        schema: json!({"type": "object"}),
    });

    match run_multi_tool_subprocess(vec![tool], "escalating-tool-fixture", None).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("escalating_tool_fixture: harness error: {e}");
            ExitCode::from(4)
        }
    }
}
