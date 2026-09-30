//! Operator confirmation for confirm-first tools.
//!
//! A confirm-first tool (a delete, an overwrite, a commit, a real purchase
//! order) runs only with `confirmed: true`, which the model is told to set
//! after the operator approves. Nothing stopped a model from setting it on
//! its first call. [`OperatorConfirmations`] closes that: `confirmed: true`
//! only counts for a target the tool refused in an EARLIER turn of the same
//! session. The tool can't read the operator's answer, but the model can't
//! skip asking — it has to end its turn, and only the operator's next
//! message starts a turn in which the confirmed call goes through.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::Value;

use crate::{SessionId, ToolContext, TurnId};

/// Appended to a confirm-first refusal so the model knows the rule.
pub const ASK_THEN_END_TURN: &str = "Ask the operator and END YOUR TURN; only if they agree \
     in their next message, call again with `confirmed: true`. A confirmation in the same \
     turn as the refusal is not accepted.";

/// Per-tool memory of refused confirm-first calls, keyed by session and a
/// tool-chosen target (a resolved path, a repo, a skill name, …).
#[derive(Debug, Default)]
pub struct OperatorConfirmations(Mutex<HashMap<(SessionId, String), TurnId>>);

impl OperatorConfirmations {
    /// Whether this call may run. `confirmed` is the model's flag; it only
    /// counts after a refusal of the same `target` in an earlier turn. A
    /// call that doesn't run is remembered (the first refusal wins), so the
    /// operator's next turn can confirm it.
    pub fn allows(&self, ctx: &ToolContext<'_>, target: &str, confirmed: bool) -> bool {
        let Ok(mut refused) = self.0.lock() else {
            return false;
        };
        let key = (ctx.session_id, target.to_string());
        match refused.get(&key) {
            Some(turn) if confirmed && *turn != ctx.turn_id => {
                refused.remove(&key);
                true
            }
            _ => {
                // Bounded: stale refusals are cheap to forget — the cost is
                // one more "please confirm" round, never a silent run.
                if refused.len() > 1024 {
                    refused.clear();
                }
                refused.entry(key).or_insert(ctx.turn_id);
                false
            }
        }
    }
}

/// Whether a tool's input schema declares the `confirmed` flag — how a
/// bridged (out-of-process) tool says it is confirm-first.
pub fn declares_confirmed(schema: &Value) -> bool {
    schema
        .get("properties")
        .and_then(|p| p.get("confirmed"))
        .is_some()
}

/// The model's `confirmed` flag on an input.
pub fn is_confirmed(input: &Value) -> bool {
    input.get("confirmed").and_then(Value::as_bool) == Some(true)
}

/// The input without its `confirmed` flag — the stable "what is being
/// confirmed" key for a tool with no better target.
pub fn target_of(input: &Value) -> String {
    let mut v = input.clone();
    if let Some(obj) = v.as_object_mut() {
        obj.remove("confirmed");
    }
    v.to_string()
}
