//! Conformance fixture for operator confirmation across the tool-process
//! bridge: a confirm-first tool (its schema declares `confirmed`) that runs
//! only with `confirmed: true` and otherwise escalates, like
//! `kitchen.order.send`. `tests/confirm_first_proxy.rs` drives it through
//! the real `ToolProcessBridge` + `ToolProxy`. Test fixture only.

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use aivyx_capability::Scope;
use aivyx_core::{Tool, ToolContext, ToolId, ToolOutcome, Verification};
use aivyx_tool::run_multi_tool_subprocess;

struct ConfirmFirstTool {
    id: ToolId,
    schema: Value,
}

#[async_trait]
impl Tool for ConfirmFirstTool {
    fn id(&self) -> ToolId {
        self.id
    }
    fn name(&self) -> &str {
        "confirm.fixture"
    }
    fn description(&self) -> &str {
        "runs only with confirmed: true, for conformance testing"
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("email.send").expect("email.send is in KNOWN_BASES")
    }
    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        if input.get("confirmed").and_then(Value::as_bool) == Some(true) {
            ToolOutcome::Completed {
                output: json!({ "ran": true }),
                verified: Verification::Unverified,
            }
        } else {
            ToolOutcome::RequiresEscalation {
                reason: "needs the operator's approval".to_string(),
                scope: None,
            }
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let tool: Arc<dyn Tool> = Arc::new(ConfirmFirstTool {
        id: ToolId::new(),
        schema: json!({
            "type": "object",
            "properties": {
                "target": { "type": "string" },
                "confirmed": { "type": "boolean" }
            }
        }),
    });
    match run_multi_tool_subprocess(vec![tool], "confirm-first-tool-fixture", None).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("confirm_first_tool_fixture: harness error: {e}");
            ExitCode::from(4)
        }
    }
}
