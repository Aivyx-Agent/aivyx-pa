//! Supervised batching — a tool call an unattended run parked for the
//! operator's review (its area is at the `supervised` autonomy level).
//! Wasm-clean: plain data shared by the daemon, the CLI and the Studio.

use serde::{Deserialize, Serialize};

/// Where a parked step is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParkedState {
    /// Waiting for the operator.
    Pending,
    /// Approved and run (its result is in `result`).
    Approved,
    /// Turned down; never ran.
    Denied,
    /// Not reviewed in time; never ran.
    Lapsed,
    /// Approved, but it couldn't run or the tool failed.
    Failed,
}

impl ParkedState {
    /// The lowercase word (`pending`, `approved`, …).
    pub fn as_str(self) -> &'static str {
        match self {
            ParkedState::Pending => "pending",
            ParkedState::Approved => "approved",
            ParkedState::Denied => "denied",
            ParkedState::Lapsed => "lapsed",
            ParkedState::Failed => "failed",
        }
    }
}

/// One parked step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParkedStep {
    /// Short id (8 hex characters) the operator types or clicks.
    pub id: String,
    /// The tool's name, e.g. `fs.delete`.
    pub tool: String,
    /// The exact arguments that run if approved (never a model-supplied
    /// `confirmed`).
    pub input: serde_json::Value,
    /// One line: the tool and what it touches.
    pub summary: String,
    /// Why it needed approval.
    pub reason: String,
    /// The autonomy area (first word of the capability base).
    pub area: String,
    /// Which run parked it: `routine digest`, `webhook deploy`,
    /// `team mission tm-…`.
    pub origin: String,
    /// The trust tier the call ran at; an approved step runs at it again.
    pub trust_tier: aivyx_capability::TrustTier,
    pub parked_at: i64,
    pub state: ParkedState,
    pub resolved_at: Option<i64>,
    /// A short result: the tool's output or error, or why it lapsed.
    pub result: Option<String>,
    /// What the step touches as it is now — filled when listed, never
    /// stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{QueryPayload, QueryResponsePayload};

    fn sample() -> ParkedStep {
        ParkedStep {
            id: "ab12cd34".into(),
            tool: "fs.delete".into(),
            input: serde_json::json!({"path": "old.txt"}),
            summary: "fs.delete old.txt".into(),
            reason: "deleting can't be undone".into(),
            area: "fs".into(),
            origin: "routine tidy".into(),
            trust_tier: aivyx_capability::TrustTier::Trusted,
            parked_at: 1_000,
            state: ParkedState::Pending,
            resolved_at: None,
            result: None,
            preview: Some("Now: old.txt (3 bytes)".into()),
        }
    }

    #[test]
    fn parked_queries_round_trip() {
        for p in [
            QueryPayload::GetParkedSteps,
            QueryPayload::ResolveParkedStep { id: "ab12cd34".into(), approve: true },
        ] {
            let back: QueryPayload =
                serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
            assert_eq!(back, p);
        }
        for r in [
            QueryResponsePayload::ParkedSteps { steps: vec![sample()] },
            QueryResponsePayload::ParkedStepResolved { step: sample() },
        ] {
            let back: QueryResponsePayload =
                serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
            assert_eq!(back, r);
        }
    }

    #[test]
    fn state_is_a_lowercase_word_and_preview_is_optional() {
        let mut v = serde_json::to_value(sample()).unwrap();
        assert_eq!(v["state"], "pending");
        v.as_object_mut().unwrap().remove("preview");
        let back: ParkedStep = serde_json::from_value(v).unwrap();
        assert_eq!(back.preview, None);
        assert_eq!(ParkedState::Lapsed.as_str(), "lapsed");
    }
}
