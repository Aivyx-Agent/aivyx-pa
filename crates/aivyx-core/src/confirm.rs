//! Operator confirmation for confirm-first tools.
//!
//! A confirm-first tool (a delete, an overwrite, a commit, a real purchase
//! order) runs only with `confirmed: true`, which the model is told to set
//! after the operator approves. Nothing stopped a model from setting it on
//! its first call. [`OperatorConfirmations`] closes that: `confirmed: true`
//! only counts for the exact call (tool + arguments + resolved target) that
//! was refused in the turn just before, in the same session. The tool can't
//! read the operator's answer, but the model can't skip asking — it has to
//! end its turn, and only the operator's next message starts a turn in which
//! that same call goes through, once.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{SessionId, TurnId};

/// Appended to a confirm-first refusal so the model knows the rule.
pub const ASK_THEN_END_TURN: &str = "Ask the operator and END YOUR TURN; only if they agree \
     in their next message, call again with `confirmed: true`. A confirmation in the same \
     turn as the refusal is not accepted.";

/// How long a remembered refusal stays approvable, whatever happens.
pub const APPROVAL_WINDOW: Duration = Duration::from_secs(10 * 60);

/// What a refused call is remembered by: the session, the tool, and a digest
/// of the exact call (its canonical arguments without `confirmed`, plus the
/// resolved scope the tool derived from them).
type CallKey = (SessionId, String, [u8; 32]);

#[derive(Debug)]
struct Pending {
    /// The session's turn index when the refusal was recorded.
    epoch: u64,
    turn: TurnId,
    at: Instant,
}

#[derive(Debug, Default)]
struct Inner {
    /// Per session: the latest turn seen and how many turns have begun.
    epochs: HashMap<SessionId, (TurnId, u64)>,
    refused: HashMap<CallKey, Pending>,
}

/// Memory of refused calls that cannot be asked about in-line, so the
/// operator's next message may approve exactly that call.
///
/// An entry is bound to (session, tool, digest of the call) and:
/// - counts only in the turn right after the one that recorded it (the
///   operator's reply), never in the same turn and never later;
/// - expires after [`APPROVAL_WINDOW`];
/// - is consumed by use: one approval, one run;
/// - is forgotten when the operator was asked in-line (an explicit "no" must
///   never pre-approve anything, and neither may an answered "yes").
///
/// A different call to the same tool has a different digest and asks again.
#[derive(Debug, Default)]
pub struct OperatorConfirmations(Mutex<Inner>);

/// The digest that identifies a call: tool name, the arguments with
/// `confirmed` removed (keys sorted, so it is canonical), and the resolved
/// scope string.
pub fn call_digest(tool: &str, input: &Value, resolved_scope: &str) -> [u8; 32] {
    fn canon(v: &Value, out: &mut String) {
        match v {
            Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                out.push('{');
                for k in keys {
                    out.push_str(&Value::String(k.clone()).to_string());
                    out.push(':');
                    canon(&m[k], out);
                    out.push(',');
                }
                out.push('}');
            }
            Value::Array(a) => {
                out.push('[');
                for x in a {
                    canon(x, out);
                    out.push(',');
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut args = input.clone();
    if let Some(obj) = args.as_object_mut() {
        obj.remove("confirmed");
    }
    let mut text = String::new();
    canon(&args, &mut text);
    let mut h = Sha256::new();
    h.update(tool.as_bytes());
    h.update([0]);
    h.update(resolved_scope.as_bytes());
    h.update([0]);
    h.update(text.as_bytes());
    h.finalize().into()
}

impl OperatorConfirmations {
    /// A turn begins in `session`. Entries recorded in the previous turn
    /// become approvable (this one is "the operator's reply"); older ones
    /// are dead.
    ///
    /// `approves` says whether the operator's message that starts this turn
    /// is a short affirmative ([`is_affirmative`]). Any other message drops
    /// the session's pending entries: a "no", an unrelated question or a
    /// scheduled turn never approves a refused call.
    pub fn begin_turn(&self, session: SessionId, turn: TurnId, approves: bool) {
        let Ok(mut inner) = self.0.lock() else { return };
        if !approves {
            inner.refused.retain(|(s, _, _), _| *s != session);
        }
        if inner.epochs.len() > 1024 {
            // Forget everything: the cost is one more "please confirm".
            inner.epochs.clear();
            inner.refused.clear();
        }
        let e = inner.epochs.entry(session).or_insert((turn, 0));
        if e.0 != turn {
            *e = (turn, e.1 + 1);
        }
        let epochs = inner.epochs.clone();
        inner.refused.retain(|(s, _, _), p| {
            epochs.get(s).is_some_and(|(_, now)| *now <= p.epoch + 1)
        });
    }

    fn epoch(inner: &Inner, session: SessionId, turn: TurnId) -> u64 {
        match inner.epochs.get(&session) {
            Some((t, n)) if *t == turn => *n,
            // A turn that never called `begin_turn` (a resumed approved
            // call): it is not a reply to anything.
            Some((_, n)) => n + 1,
            None => 0,
        }
    }

    /// Remember that this exact call was refused in `turn` (the first refusal
    /// of a call wins, so a repeat in the same turn doesn't move it forward).
    pub fn record_refusal(&self, session: SessionId, turn: TurnId, tool: &str, digest: [u8; 32]) {
        let Ok(mut inner) = self.0.lock() else { return };
        if inner.refused.len() > 1024 {
            inner.refused.clear();
        }
        let epoch = Self::epoch(&inner, session, turn);
        inner
            .refused
            .entry((session, tool.to_string(), digest))
            .or_insert(Pending { epoch, turn, at: Instant::now() });
    }

    /// Whether this exact call was refused in the PREVIOUS turn of `session`,
    /// within the window: the operator has replied since. Consumes the entry.
    pub fn take_refusal(&self, session: SessionId, turn: TurnId, tool: &str, digest: [u8; 32]) -> bool {
        let Ok(mut inner) = self.0.lock() else { return false };
        let key = (session, tool.to_string(), digest);
        let now = Self::epoch(&inner, session, turn);
        match inner.refused.get(&key) {
            Some(p) if p.epoch + 1 == now && p.turn != turn && p.at.elapsed() <= APPROVAL_WINDOW => {
                inner.refused.remove(&key);
                true
            }
            Some(p) if p.epoch + 1 < now || p.at.elapsed() > APPROVAL_WINDOW => {
                inner.refused.remove(&key);
                false
            }
            _ => false,
        }
    }

    /// The operator was asked in-line about a call of `tool` in `turn` and
    /// answered (yes, no, timeout, parked): drop what this turn recorded for
    /// that tool, so neither a "no" nor a used "yes" approves a later call.
    pub fn forget_turn(&self, session: SessionId, turn: TurnId, tool: &str) {
        let Ok(mut inner) = self.0.lock() else { return };
        inner
            .refused
            .retain(|(s, t, _), p| !(*s == session && t == tool && p.turn == turn));
    }
}

/// Whether an operator's reply is a short "yes": `yes`, `y`, `ok`, `okay`,
/// `approve(d)`, `confirm(ed)`, `go ahead`, `do it`, `sure`, `yep`, `yeah`,
/// trimmed and case-insensitive, optionally followed by a few more words after
/// a comma or full stop ("yes, send it"). Anything else, "no" included, is
/// not an approval.
pub fn is_affirmative(reply: &str) -> bool {
    const YES: &[&str] = &[
        "yes", "y", "ok", "okay", "approve", "approved", "confirm", "confirmed", "go ahead",
        "do it", "sure", "yep", "yeah",
    ];
    let lower = reply.trim().to_lowercase();
    if lower.is_empty() || lower.split_whitespace().count() > 6 {
        return false;
    }
    let head = lower
        .split([',', '.', '!', ';', ':', '\n'])
        .next()
        .unwrap_or("")
        .trim();
    YES.contains(&head)
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
    use serde_json::json;

    fn d(input: Value) -> [u8; 32] {
        call_digest("fs.delete", &input, "fs.write")
    }

    /// Begin turns the way the agent does and return the new turn id.
    fn turn(l: &OperatorConfirmations, s: SessionId) -> TurnId {
        let t = TurnId::new();
        l.begin_turn(s, t, true);
        t
    }

    #[test]
    fn a_refusal_approves_only_the_same_call_in_the_next_turn_once() {
        let l = OperatorConfirmations::default();
        let s = SessionId::new();
        let (a, b) = (d(json!({"path": "a"})), d(json!({"path": "b"})));
        let t1 = turn(&l, s);
        assert!(!l.take_refusal(s, t1, "fs.delete", a), "nothing recorded yet");
        l.record_refusal(s, t1, "fs.delete", a);
        assert!(!l.take_refusal(s, t1, "fs.delete", a), "same turn doesn't count");
        let t2 = turn(&l, s);
        assert!(!l.take_refusal(s, t2, "fs.delete", b), "other arguments ask again");
        assert!(!l.take_refusal(SessionId::new(), t2, "fs.delete", a), "other session");
        assert!(l.take_refusal(s, t2, "fs.delete", a), "the operator's next turn");
        assert!(!l.take_refusal(s, t2, "fs.delete", a), "consumed: one run");
    }

    #[test]
    fn confirmed_flag_and_key_order_do_not_change_the_digest() {
        let a = d(json!({"path": "x", "mode": 1}));
        assert_eq!(a, d(json!({"mode": 1, "path": "x", "confirmed": true})));
        assert_ne!(a, d(json!({"path": "y", "mode": 1})));
        assert_ne!(a, call_digest("fs.delete", &json!({"path": "x", "mode": 1}), "fs.read"));
    }

    #[test]
    fn a_refusal_expires_after_one_turn() {
        let l = OperatorConfirmations::default();
        let s = SessionId::new();
        let a = d(json!({"path": "a"}));
        let t1 = turn(&l, s);
        l.record_refusal(s, t1, "fs.delete", a);
        let _unrelated = turn(&l, s);
        let t3 = turn(&l, s);
        assert!(!l.take_refusal(s, t3, "fs.delete", a), "two turns later is too late");
    }

    #[test]
    fn an_answered_prompt_forgets_the_refusal() {
        let l = OperatorConfirmations::default();
        let s = SessionId::new();
        let a = d(json!({"path": "a"}));
        let t1 = turn(&l, s);
        l.record_refusal(s, t1, "fs.delete", a);
        l.forget_turn(s, t1, "fs.delete");
        let t2 = turn(&l, s);
        assert!(!l.take_refusal(s, t2, "fs.delete", a));
    }

    #[test]
    fn only_a_short_yes_is_an_approval() {
        for yes in ["yes", " Yes ", "y", "OK", "okay.", "approve", "go ahead", "Yes, send it", "do it!"] {
            assert!(is_affirmative(yes), "{yes:?}");
        }
        for no in [
            "no", "n", "no, yes", "nope", "don't", "what's the weather", "", "yesterday was fine",
            "yes but first delete everything else in the downloads folder please",
        ] {
            assert!(!is_affirmative(no), "{no:?}");
        }
    }

    #[test]
    fn a_reply_that_is_not_a_yes_drops_the_pending_entries() {
        let l = OperatorConfirmations::default();
        let s = SessionId::new();
        let a = d(json!({"path": "a"}));
        let t1 = turn(&l, s);
        l.record_refusal(s, t1, "fs.delete", a);
        let t2 = TurnId::new();
        l.begin_turn(s, t2, false);
        assert!(!l.take_refusal(s, t2, "fs.delete", a));
    }
}
