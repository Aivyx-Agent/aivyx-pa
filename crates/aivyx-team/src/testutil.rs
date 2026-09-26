//! Shared test fixtures: a working fake `LlmProvider` that drives one
//! tool-less turn, a fake lead `ChannelContext`, and team/pool builders.
//! Compiled only under `#[cfg(test)]`; used by the `pool` and `tools`
//! test modules.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use aivyx_capability::{CapabilitySet, Scope, TrustTier};
use aivyx_core::{
    CancellationToken, ChannelContext, ChannelError, ChannelPlatform, NullAuditHook, SessionId,
    StreamEvent, TurnOutcome,
};
use aivyx_llm::{
    LlmError, LlmProvider, LlmRequest, LlmStepEnd, LlmStream, LlmStreamEvent, NameResolution,
    ToolCallEnd,
};
use async_trait::async_trait;

use crate::config::{DialogueConfig, TeamConfig, TeamMember};
use crate::factory::SpecialistFactory;
use crate::pool::SpecialistPool;

// --- a working fake provider: one tool-less turn that says a fixed line ---

struct FakeStep {
    events: Vec<LlmStreamEvent>,
    terminal: LlmStepEnd,
}

pub struct FakeProvider {
    script: Mutex<VecDeque<FakeStep>>,
    /// When set, every `chat_stream` returns this text (never exhausts) —
    /// needed for multi-step missions where one provider serves many turns.
    repeat: Option<String>,
}

fn one_shot(text: &str) -> FakeStep {
    FakeStep {
        events: vec![LlmStreamEvent::TextChunk(text.to_string())],
        terminal: LlmStepEnd::FinalMessage {
            text: text.to_string(),
            usage: aivyx_llm::LlmUsage::default(),
        },
    }
}

/// One step that emits a single tool call to `name` with `input`.
fn tool_step(name: &str, call_id: &str, input: serde_json::Value) -> FakeStep {
    FakeStep {
        events: vec![],
        terminal: LlmStepEnd::ToolCalls {
            calls: vec![ToolCallEnd {
                call_id: call_id.to_string(),
                tool_name: name.to_string(),
                input,
                name_resolution: NameResolution::Known,
            }],
            text_so_far: String::new(),
            usage: aivyx_llm::LlmUsage::default(),
        },
    }
}

impl FakeProvider {
    /// A provider that completes one turn with `text` as the final message.
    pub fn says(text: &str) -> Arc<Self> {
        Arc::new(FakeProvider {
            script: Mutex::new(VecDeque::from(vec![one_shot(text)])),
            repeat: None,
        })
    }

    /// A provider that returns `text` on *every* turn — for missions whose
    /// many delegated sub-turns share one provider.
    pub fn always(text: &str) -> Arc<Self> {
        Arc::new(FakeProvider {
            script: Mutex::new(VecDeque::new()),
            repeat: Some(text.to_string()),
        })
    }

    /// A provider that emits one single-tool call per turn, taking `names` in
    /// order then exhausting. Drives a scripted tool-call loop — e.g.
    /// `["a","b","a","b","a","b"]` to exercise the small-cycle breaker.
    pub fn tool_loop(names: &[&str]) -> Arc<Self> {
        let steps: VecDeque<FakeStep> = names
            .iter()
            .enumerate()
            .map(|(i, n)| tool_step(n, &format!("c{i}"), serde_json::json!({})))
            .collect();
        Arc::new(FakeProvider {
            script: Mutex::new(steps),
            repeat: None,
        })
    }

    /// One turn: a single tool call to `name` with `input`, then a final
    /// message closing the turn. For tests that need a real (non-empty)
    /// tool-call payload — e.g. `fs.write`'s `{"path", "content"}`.
    pub fn tool_call_then_done(name: &str, input: serde_json::Value) -> Arc<Self> {
        Arc::new(FakeProvider {
            script: Mutex::new(VecDeque::from(vec![
                tool_step(name, "c0", input),
                one_shot("done"),
            ])),
            repeat: None,
        })
    }
}

#[async_trait]
impl LlmProvider for FakeProvider {
    async fn chat_stream(
        &self,
        _: LlmRequest<'_>,
        _: &CancellationToken,
    ) -> Result<Box<dyn LlmStream>, LlmError> {
        let step = match &self.repeat {
            Some(text) => one_shot(text),
            None => self
                .script
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| LlmError::Config("fake provider exhausted".into()))?,
        };
        Ok(Box::new(FakeStream {
            events: step.events.into_iter(),
            terminal: Some(step.terminal),
        }))
    }
}

struct FakeStream {
    events: std::vec::IntoIter<LlmStreamEvent>,
    terminal: Option<LlmStepEnd>,
}

#[async_trait]
impl LlmStream for FakeStream {
    async fn next_event(&mut self) -> Result<Option<LlmStreamEvent>, LlmError> {
        Ok(self.events.next())
    }
    async fn finish(mut self: Box<Self>) -> Result<LlmStepEnd, LlmError> {
        Ok(self.terminal.take().expect("finish once"))
    }
}

/// Model routing Part 3a — a one-shot fake that records the request's
/// `route` hint, so `planner.rs`'s tests can assert the mission planner's
/// decomposition request is tagged `TaskKind::Plan`.
pub struct RouteCapturingFakeProvider {
    reply: String,
    route_seen: Arc<Mutex<Option<aivyx_llm::RouteHint>>>,
}

impl RouteCapturingFakeProvider {
    pub fn says(text: &str, route_seen: Arc<Mutex<Option<aivyx_llm::RouteHint>>>) -> Self {
        RouteCapturingFakeProvider { reply: text.to_string(), route_seen }
    }
}

#[async_trait]
impl LlmProvider for RouteCapturingFakeProvider {
    async fn chat_stream(
        &self,
        request: LlmRequest<'_>,
        _cancel: &CancellationToken,
    ) -> Result<Box<dyn LlmStream>, LlmError> {
        *self.route_seen.lock().unwrap() = request.route.clone();
        Ok(Box::new(one_shot_stream(&self.reply)))
    }
}

fn one_shot_stream(text: &str) -> FakeStream {
    let step = one_shot(text);
    FakeStream {
        events: step.events.into_iter(),
        terminal: Some(step.terminal),
    }
}

// --- a fake lead channel ---------------------------------------------------

pub struct FakeLeadChannel {
    session: SessionId,
    tier: TrustTier,
    token: CancellationToken,
}

impl FakeLeadChannel {
    pub fn at(tier: TrustTier) -> Self {
        FakeLeadChannel {
            session: SessionId::new(),
            tier,
            token: CancellationToken::new(),
        }
    }
}

#[async_trait]
impl ChannelContext for FakeLeadChannel {
    fn channel_name(&self) -> &str {
        "fake-lead"
    }
    fn platform(&self) -> ChannelPlatform {
        ChannelPlatform::Local
    }
    fn trust_tier(&self) -> TrustTier {
        self.tier
    }
    fn session_id(&self) -> SessionId {
        self.session
    }
    async fn stream_event(&self, _: StreamEvent<'_>) -> Result<(), ChannelError> {
        Ok(())
    }
    async fn finalize(&self, _: &TurnOutcome) -> Result<(), ChannelError> {
        Ok(())
    }
    fn cancellation_token(&self) -> CancellationToken {
        self.token.clone()
    }
}

// --- builders --------------------------------------------------------------

/// A team member with declared scopes + a trust ceiling (no tools).
pub fn member(name: &str, scopes: &[&str], tier: TrustTier) -> TeamMember {
    TeamMember {
        name: name.into(),
        role: "R".into(),
        soul: "You are a specialist.".into(),
        tool_allowlist: vec![],
        capability_scopes: scopes.iter().map(|s| s.to_string()).collect(),
        trust_ceiling: tier,
        model: None,
        base_url: None,
    }
}

/// A pool over `members` led by `lead`, with the given provider and a lead
/// capability set holding `lead_scopes`.
pub fn team_pool(
    provider: Arc<dyn LlmProvider>,
    members: Vec<TeamMember>,
    lead: &str,
    lead_scopes: &[&str],
) -> SpecialistPool {
    let config = TeamConfig {
        name: "t".into(),
        description: String::new(),
        lead: lead.into(),
        members,
        dialogue: DialogueConfig::default(),
    };
    let factory =
        SpecialistFactory::new(provider, "test-model", 4096, Arc::new(NullAuditHook), vec![]);
    let lead_caps =
        CapabilitySet::from_scopes(lead_scopes.iter().map(|s| Scope::parse(s).unwrap()));
    // None of this fixture's callers test trigger-origin propagation (that's
    // covered directly against `pool.rs`'s own `pool()` fixture) -- Operator
    // preserves every existing test's tested behavior.
    SpecialistPool::new(factory, config, lead_caps, aivyx_core::MessageOrigin::Operator)
}
