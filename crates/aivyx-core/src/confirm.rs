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

use crate::{SessionId, TurnId};

/// Appended to a confirm-first refusal so the model knows the rule.
pub const ASK_THEN_END_TURN: &str = "Ask the operator and END YOUR TURN; only if they agree \
     in their next message, call again with `confirmed: true`. A confirmation in the same \
     turn as the refusal is not accepted.";

/// Per-tool memory of refused confirm-first calls, keyed by session and a
/// tool-chosen target (a resolved path, a repo, a skill name, …).
#[derive(Debug, Default)]
pub struct OperatorConfirmations(Mutex<HashMap<(SessionId, String), TurnId>>);

impl OperatorConfirmations {
    /// Remember that `target` was refused in `turn` (the first refusal wins,
    /// so a later refusal in the same turn doesn't move it forward).
    pub fn record_refusal(&self, session: SessionId, turn: TurnId, target: &str) {
        let Ok(mut refused) = self.0.lock() else { return };
        // Bounded: stale refusals are cheap to forget — the cost is one more
        // "please confirm" round, never a silent run.
        if refused.len() > 1024 {
            refused.clear();
        }
        refused.entry((session, target.to_string())).or_insert(turn);
    }

    /// Whether `target` was refused in an EARLIER turn of `session` — i.e. the
    /// operator has replied since. Consumes the record: one approval, one run.
    pub fn take_refusal(&self, session: SessionId, turn: TurnId, target: &str) -> bool {
        let Ok(mut refused) = self.0.lock() else { return false };
        let key = (session, target.to_string());
        match refused.get(&key) {
            Some(t) if *t != turn => {
                refused.remove(&key);
                true
            }
            _ => false,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_counts_only_in_a_later_turn_of_the_same_session() {
        let ledger = OperatorConfirmations::default();
        let (s, t1, t2) = (SessionId::new(), TurnId::new(), TurnId::new());
        assert!(!ledger.take_refusal(s, t1, "fs.delete"), "nothing recorded yet");
        ledger.record_refusal(s, t1, "fs.delete");
        assert!(!ledger.take_refusal(s, t1, "fs.delete"), "same turn doesn't count");
        assert!(!ledger.take_refusal(SessionId::new(), t2, "fs.delete"), "other session");
        assert!(ledger.take_refusal(s, t2, "fs.delete"), "the operator's next turn");
        assert!(!ledger.take_refusal(s, TurnId::new(), "fs.delete"), "consumed: one run");
    }
}
