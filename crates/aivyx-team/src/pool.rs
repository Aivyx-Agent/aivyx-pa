//! `SpecialistChannel` + `SpecialistPool` — run an attenuated specialist as
//! a **sub-turn** inside the lead's session (J.2.2).
//!
//! The `SpecialistChannel` is the mechanism that makes multi-agent work on
//! the single-agent loop: it's a thin [`ChannelContext`] that gives a
//! specialist the **lead's** session / cancellation / platform, but reports
//! a **floored trust tier** (`effective_trust(member.ceiling, lead_tier)`).
//! Because the core's turn loop already computes `caps ∩ tier.ceiling()`
//! every turn, the specialist ends up **double-bounded** — attenuated caps
//! *and* the floored tier ceiling — with zero new enforcement code.
//!
//! Output flows back via the returned [`TurnOutcome::Completed`], so the
//! channel's `stream_event` / `finalize` are no-ops (operator-facing
//! streaming of specialist progress is the J.7 Fleet panel).

use aivyx_capability::{CapabilitySet, TrustTier};
use aivyx_core::{
    Agent, CancellationToken, ChannelContext, ChannelError, ChannelPlatform, Message, SessionId,
    StreamEvent, TurnOutcome,
};
use async_trait::async_trait;

use crate::attenuation::effective_trust;
use crate::config::{TeamConfig, TeamError, TeamMember};
use crate::factory::SpecialistFactory;

/// A derived channel for one specialist sub-turn (see module docs).
pub struct SpecialistChannel {
    session_id: SessionId,
    trust_tier: TrustTier,
    platform: ChannelPlatform,
    cancellation: CancellationToken,
    /// Chapter Spyglass — the specialist's name, so its tool calls are
    /// legible in the journal (a mission's inner work used to be a black box).
    label: String,
}

impl SpecialistChannel {
    pub fn new(
        session_id: SessionId,
        trust_tier: TrustTier,
        platform: ChannelPlatform,
        cancellation: CancellationToken,
        label: impl Into<String>,
    ) -> Self {
        SpecialistChannel {
            session_id,
            trust_tier,
            platform,
            cancellation,
            label: label.into(),
        }
    }
}

#[async_trait]
impl ChannelContext for SpecialistChannel {
    fn channel_name(&self) -> &str {
        "specialist"
    }
    fn platform(&self) -> ChannelPlatform {
        self.platform
    }
    fn trust_tier(&self) -> TrustTier {
        self.trust_tier
    }
    fn session_id(&self) -> SessionId {
        self.session_id
    }
    async fn stream_event(&self, event: StreamEvent<'_>) -> Result<(), ChannelError> {
        // Chapter Spyglass — surface a specialist's TOOL activity in the journal
        // so a mission's inner work is observable (previously a black box that
        // made "reports done but produced nothing" hard to diagnose). Only the
        // tool start/finish pair is logged; token text stays quiet. The live
        // operator-facing Fleet panel (J.7) is still deferred.
        match event {
            StreamEvent::ToolCallStarted { tool_name, .. } => {
                eprintln!("aivyx-pa team: [{}] → {tool_name}", self.label);
            }
            StreamEvent::ToolCallFinished { tool_name, outcome_summary, .. } => {
                let summary: String = outcome_summary.chars().take(120).collect();
                eprintln!("aivyx-pa team: [{}] ← {tool_name} — {summary}", self.label);
            }
            _ => {}
        }
        Ok(())
    }
    async fn finalize(&self, _outcome: &TurnOutcome) -> Result<(), ChannelError> {
        Ok(()) // the lead reads the result from run()'s return value
    }
    fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }
}

/// A specialist's channel whose approval requests go to the lead's channel —
/// the specialist works on the lead's behalf, so the lead's channel decides
/// (an unattended mission's lead channel parks them for review; an
/// interactive one can't ask, as before). Everything else is the
/// specialist's own channel.
struct LeadApprovals<'a> {
    inner: SpecialistChannel,
    lead: &'a dyn ChannelContext,
}

#[async_trait]
impl ChannelContext for LeadApprovals<'_> {
    fn channel_name(&self) -> &str {
        self.inner.channel_name()
    }
    fn platform(&self) -> ChannelPlatform {
        self.inner.platform()
    }
    fn trust_tier(&self) -> TrustTier {
        self.inner.trust_tier()
    }
    fn session_id(&self) -> SessionId {
        self.inner.session_id()
    }
    async fn stream_event(&self, event: StreamEvent<'_>) -> Result<(), ChannelError> {
        self.inner.stream_event(event).await
    }
    async fn finalize(&self, outcome: &TurnOutcome) -> Result<(), ChannelError> {
        self.inner.finalize(outcome).await
    }
    fn cancellation_token(&self) -> CancellationToken {
        self.inner.cancellation_token()
    }
    async fn request_approval(&self, request: &aivyx_core::ApprovalRequest) -> aivyx_core::Approval {
        self.lead.request_approval(request).await
    }
}

/// Constructs + runs attenuated specialists for one team.
pub struct SpecialistPool {
    factory: SpecialistFactory,
    config: TeamConfig,
    ceiling: CapabilitySet,
    /// `System` when this mission's own provenance is a trigger this
    /// codebase treats as unattended (a schedule, or any of the other
    /// `TriggerSource` shapes) -- every specialist run through
    /// [`SpecialistPool::run`] gets a `System`-origin `Message` for every
    /// turn, so the schedule.* write-tool guard (aivyx-channel's
    /// schedule_tool.rs) refuses them. `Operator` for an interactively-
    /// started mission or a channel-triggered one (a real person sent
    /// the command, authenticated via the channel's own sender
    /// allowlist) -- deliberately excluded from `System` classification.
    ///
    /// Note this field's reach: in the current architecture, the daemon
    /// mission-driving path (`assemble_runtime`/`TeamRuntime`) has no
    /// lead-agent turn of its own at all -- only specialists run through
    /// `SpecialistPool::run` ([`SpecialistPool::resolve`] explicitly
    /// errors if asked to resolve the lead's own name/role, "is the lead,
    /// not a delegable specialist"), so this field's practical effect
    /// today is scoped to specialist turns. The CLI's own separate
    /// lead-agent construction path (`aivyx-pa team run`) builds its lead
    /// with `Operator` origin directly, outside this mechanism entirely.
    message_origin: aivyx_core::MessageOrigin,
}

impl SpecialistPool {
    pub fn new(
        factory: SpecialistFactory,
        config: TeamConfig,
        ceiling: CapabilitySet,
        message_origin: aivyx_core::MessageOrigin,
    ) -> Self {
        SpecialistPool {
            factory,
            config,
            ceiling,
            message_origin,
        }
    }

    /// Resolve a specialist reference — must be a member, and not the lead.
    ///
    /// Matching is **case-insensitive** and accepts EITHER the roster `name`
    /// id (`ops`) OR the human-readable `role` label (`Operations`). The LLM
    /// planner refers to specialists by the role labels it's shown, not the
    /// internal ids, so a name-only match errored ("no specialist
    /// \"Operations\"") → the mission failed → the loop's auto-delegation
    /// retried and skipped a doable story. #15 first made this
    /// case-insensitive (fixing "Researcher" vs "researcher", where name and
    /// role differ only in case); this extends it to name↔role mismatches
    /// (`ops`/`Operations`, `coordinator`/`Lead`) which case-folding alone
    /// never covered. Both fields are short ASCII, so case-insensitive
    /// matching is safe.
    fn resolve(&self, specialist: &str) -> Result<&TeamMember, TeamError> {
        let matched = self.config.members.iter().find(|m| {
            m.name.eq_ignore_ascii_case(specialist)
                || m.role.eq_ignore_ascii_case(specialist)
        });
        match matched {
            // The lead is identified by its roster id in `config.lead`; a
            // reference resolving to that member (by name or role) is the
            // lead, not a delegable specialist.
            Some(m) if m.name.eq_ignore_ascii_case(&self.config.lead) => {
                Err(TeamError::Config(format!(
                    "{specialist:?} is the lead, not a delegable specialist"
                )))
            }
            Some(m) => Ok(m),
            // Graceful fallback: the LLM planner sometimes names a specialist
            // that isn't in the roster (a generic role word like "Operations"
            // or "QA Engineer"). Hard-failing the whole delegation there just
            // skips a doable story (seen live in the loop dogfood, sharpened by
            // the ops→verifier rename). Instead, map the requested word to a
            // capability and route to the best-fit specialist — the acceptance
            // gate (Keystone) still catches a bad result, so a best-effort
            // attempt strictly beats an abandoned story.
            None => match self.fallback_specialist(specialist) {
                Some(m) => {
                    eprintln!(
                        "aivyx-pa team: planner named unknown specialist \
                         {specialist:?}; routing to best-fit {:?}",
                        m.name
                    );
                    Ok(m)
                }
                None => Err(TeamError::Config(format!(
                    "no specialist {specialist:?} in team {:?}",
                    self.config.name
                ))),
            },
        }
    }

    /// Best-effort recovery when an exact name/role match fails: map a generic
    /// role word the planner reached for to a capability, then pick the roster
    /// member that best provides it. Capability-based rather than name-based so
    /// it works for ANY roster (incl. a vertical pack's), and prefers the
    /// least-privilege fit (e.g. a run-only "operations" step goes to a
    /// shell-but-not-write specialist over the coder). `None` when nothing
    /// sensible fits — the caller then errors as before.
    fn fallback_specialist(&self, specialist: &str) -> Option<&TeamMember> {
        let q = specialist.to_ascii_lowercase();
        let hit = |kws: &[&str]| kws.iter().any(|kw| q.contains(kw));
        // A member's declared scope bases (bare or qualified, `base` or
        // `base:qualifier`).
        let has = |m: &&TeamMember, base: &str| {
            m.capability_scopes
                .iter()
                .any(|s| s.split(':').next() == Some(base))
        };
        let specialists = || {
            self.config
                .members
                .iter()
                .filter(|m| !m.name.eq_ignore_ascii_case(&self.config.lead))
        };

        // Order matters: more specific capability wants are checked first so
        // e.g. "developer" routes to the coder, not merely any writer/runner.
        if hit(&["cod", "develop", "engineer", "program", "implement", "build"]) {
            if let Some(m) = specialists().find(|m| has(m, "fs.write") && has(m, "shell.exec")) {
                return Some(m);
            }
        }
        if hit(&["writ", "author", "scribe", "editor", "document", "content", "note"]) {
            // Prefer a pure writer (can write, no shell) over the coder, both
            // for semantic fit and least privilege; else any writer.
            if let Some(m) = specialists()
                .find(|m| (has(m, "fs.write") || has(m, "workspace")) && !has(m, "shell.exec"))
            {
                return Some(m);
            }
            if let Some(m) = specialists().find(|m| has(m, "fs.write") || has(m, "workspace")) {
                return Some(m);
            }
        }
        if hit(&[
            "operation", "ops", "devops", "sysadmin", "sre", "infra", "execut", "deploy", "run",
            "qa", "test", "verif", "validat",
        ]) {
            // Prefer a least-privilege runner (shell without write), else any.
            if let Some(m) = specialists().find(|m| has(m, "shell.exec") && !has(m, "fs.write")) {
                return Some(m);
            }
            if let Some(m) = specialists().find(|m| has(m, "shell.exec")) {
                return Some(m);
            }
        }
        if hit(&["research", "investigat", "gather", "search", "analy", "data", "fetch"]) {
            if let Some(m) = specialists().find(|m| has(m, "net.fetch") || has(m, "web.search")) {
                return Some(m);
            }
        }
        if hit(&["review", "critic", "audit", "inspect", "read"]) {
            if let Some(m) = specialists().find(|m| has(m, "fs.read")) {
                return Some(m);
            }
        }
        None
    }

    /// Build the derived channel for a specialist sub-turn — trust floored
    /// to `effective_trust(member.ceiling, lead_tier)`.
    pub fn specialist_channel(
        &self,
        member: &TeamMember,
        lead_channel: &dyn ChannelContext,
    ) -> SpecialistChannel {
        SpecialistChannel::new(
            lead_channel.session_id(),
            effective_trust(member.trust_ceiling, lead_channel.trust_tier()),
            lead_channel.platform(),
            lead_channel.cancellation_token(),
            member.name.clone(),
        )
    }

    /// Run a specialist on `task`, attenuated + trust-floored against the
    /// lead's live channel. Returns the specialist's final message.
    pub async fn run(
        &self,
        specialist: &str,
        task: &str,
        memory_topic: Option<&str>,
        lead_channel: &dyn ChannelContext,
    ) -> Result<String, TeamError> {
        let member = self.resolve(specialist)?;
        let agent = self.factory.build(member, &self.ceiling, memory_topic)?;
        let channel = LeadApprovals {
            inner: self.specialist_channel(member, lead_channel),
            lead: lead_channel,
        };
        let mut msg = Message::text(channel.session_id(), task);
        if self.message_origin == aivyx_core::MessageOrigin::System {
            msg = msg.system_originated();
        }

        match agent.turn(msg, &channel).await {
            TurnOutcome::Completed { final_message, .. } => Ok(final_message),
            TurnOutcome::Escalated { reason, .. } => Err(TeamError::Config(format!(
                "specialist {specialist:?} escalated for approval: {reason}"
            ))),
            TurnOutcome::TimedOut { .. } => Err(TeamError::Config(format!(
                "specialist {specialist:?} timed out"
            ))),
            TurnOutcome::Cancelled { .. } => Err(TeamError::Config(format!(
                "specialist {specialist:?} was cancelled"
            ))),
            _ => Err(TeamError::Config(format!(
                "specialist {specialist:?} did not complete its turn"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DialogueConfig;

    #[tokio::test]
    async fn spyglass_channel_logs_tool_events_without_erroring() {
        // Chapter Spyglass — stream_event now surfaces specialist tool calls;
        // the channel accepts tool start/finish (and ignores token text) cleanly.
        let ch = SpecialistChannel::new(
            SessionId::new(),
            TrustTier::Trusted,
            ChannelPlatform::Local,
            CancellationToken::new(),
            "writer",
        );
        let id = aivyx_core::ToolId::new();
        let input = serde_json::json!({"path": "foo.md"});
        assert!(ch
            .stream_event(StreamEvent::ToolCallStarted {
                tool: id,
                tool_name: "workspace.write",
                input: &input,
            })
            .await
            .is_ok());
        assert!(ch
            .stream_event(StreamEvent::ToolCallFinished {
                tool: id,
                tool_name: "workspace.write",
                outcome_summary: "wrote foo.md",
            })
            .await
            .is_ok());
        assert!(ch.stream_event(StreamEvent::Text("thinking…")).await.is_ok());
    }
    use aivyx_capability::Scope;
    use aivyx_core::NullAuditHook;
    use aivyx_llm::{LlmError, LlmProvider, LlmRequest, LlmStepEnd, LlmStream, LlmStreamEvent};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    // --- a working fake provider that drives one tool-less turn ----------

    struct FakeStep {
        events: Vec<LlmStreamEvent>,
        terminal: LlmStepEnd,
    }
    struct FakeProvider {
        script: Mutex<VecDeque<FakeStep>>,
    }
    impl FakeProvider {
        fn says(text: &str) -> Arc<Self> {
            let step = FakeStep {
                events: vec![LlmStreamEvent::TextChunk(text.to_string())],
                terminal: LlmStepEnd::FinalMessage {
                    text: text.to_string(),
                    usage: aivyx_llm::LlmUsage::default(),
                },
            };
            Arc::new(FakeProvider {
                script: Mutex::new(VecDeque::from(vec![step])),
            })
        }

        /// Scripts a tool call on the first step, then a plain final
        /// message on the second (after the tool result is fed back).
        fn calls_tool(tool_name: &str, input: serde_json::Value) -> Arc<Self> {
            let call_step = FakeStep {
                events: vec![],
                terminal: LlmStepEnd::ToolCalls {
                    calls: vec![aivyx_llm::ToolCallEnd {
                        call_id: "call-1".to_string(),
                        tool_name: tool_name.to_string(),
                        input,
                        name_resolution: aivyx_llm::NameResolution::default(),
                    }],
                    text_so_far: String::new(),
                    usage: aivyx_llm::LlmUsage::default(),
                },
            };
            let final_step = FakeStep {
                events: vec![LlmStreamEvent::TextChunk("done".to_string())],
                terminal: LlmStepEnd::FinalMessage {
                    text: "done".to_string(),
                    usage: aivyx_llm::LlmUsage::default(),
                },
            };
            Arc::new(FakeProvider {
                script: Mutex::new(VecDeque::from(vec![call_step, final_step])),
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
            let step = self
                .script
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| LlmError::Config("fake provider exhausted".into()))?;
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

    // --- a fake lead channel --------------------------------------------

    struct FakeLeadChannel {
        session: SessionId,
        tier: TrustTier,
        token: CancellationToken,
    }
    impl FakeLeadChannel {
        fn at(tier: TrustTier) -> Self {
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

    /// A lead channel that parks every approval request.
    struct ParkingLead(FakeLeadChannel);
    #[async_trait]
    impl ChannelContext for ParkingLead {
        fn channel_name(&self) -> &str {
            self.0.channel_name()
        }
        fn platform(&self) -> ChannelPlatform {
            self.0.platform()
        }
        fn trust_tier(&self) -> TrustTier {
            self.0.trust_tier()
        }
        fn session_id(&self) -> SessionId {
            self.0.session_id()
        }
        async fn stream_event(&self, e: StreamEvent<'_>) -> Result<(), ChannelError> {
            self.0.stream_event(e).await
        }
        async fn finalize(&self, o: &TurnOutcome) -> Result<(), ChannelError> {
            self.0.finalize(o).await
        }
        fn cancellation_token(&self) -> CancellationToken {
            self.0.cancellation_token()
        }
        async fn request_approval(&self, _: &aivyx_core::ApprovalRequest) -> aivyx_core::Approval {
            aivyx_core::Approval::Parked { id: "lead-parked".into() }
        }
    }

    #[tokio::test]
    async fn a_specialists_approval_request_goes_to_the_lead() {
        let p = pool(
            FakeProvider::says("x"),
            vec![
                member("lead", &[], TrustTier::Trusted),
                member("spec", &["fs.read"], TrustTier::SemiTrusted),
            ],
            "lead",
            vec![],
            aivyx_core::MessageOrigin::Operator,
        );
        let lead = ParkingLead(FakeLeadChannel::at(TrustTier::Trusted));
        let ch = LeadApprovals { inner: p.specialist_channel(p.resolve("spec").unwrap(), &lead), lead: &lead };
        assert_eq!(ch.trust_tier(), TrustTier::SemiTrusted, "keeps the specialist's floored tier");
        let answer = ch
            .request_approval(&aivyx_core::ApprovalRequest {
                tool: "fs.delete".into(),
                summary: "fs.delete a.txt".into(),
                input: serde_json::json!({"path": "a.txt"}),
                reason: "deleting can't be undone".into(),
                scope_base: "fs.delete".into(),
                trust_tier: TrustTier::SemiTrusted,
            })
            .await;
        assert_eq!(answer, aivyx_core::Approval::Parked { id: "lead-parked".into() });
    }

    // --- a tool that records the MessageOrigin it observed ---------------

    struct OriginCapturingTool {
        id: aivyx_core::ToolId,
        schema: serde_json::Value,
        observed: Arc<Mutex<Option<aivyx_core::MessageOrigin>>>,
    }
    impl OriginCapturingTool {
        fn new(observed: Arc<Mutex<Option<aivyx_core::MessageOrigin>>>) -> Self {
            OriginCapturingTool {
                id: aivyx_core::ToolId::new(),
                schema: serde_json::json!({}),
                observed,
            }
        }
    }
    #[async_trait]
    impl aivyx_core::Tool for OriginCapturingTool {
        fn id(&self) -> aivyx_core::ToolId {
            self.id
        }
        fn name(&self) -> &str {
            "origin.capture"
        }
        fn description(&self) -> &str {
            "test-only: records ctx.message_origin"
        }
        fn input_schema(&self) -> &serde_json::Value {
            &self.schema
        }
        fn required_scope(&self, _input: &serde_json::Value) -> Scope {
            Scope::parse("fs.read").unwrap()
        }
        async fn execute(
            &self,
            _input: serde_json::Value,
            ctx: &aivyx_core::ToolContext<'_>,
        ) -> aivyx_core::ToolOutcome {
            *self.observed.lock().unwrap() = Some(ctx.message_origin);
            aivyx_core::ToolOutcome::Completed {
                output: serde_json::json!({"ok": true}),
                verified: aivyx_core::Verification::NotApplicable,
            }
        }
    }

    // --- fixtures --------------------------------------------------------

    fn member(name: &str, scopes: &[&str], tier: TrustTier) -> TeamMember {
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

    fn pool(
        provider: Arc<dyn LlmProvider>,
        members: Vec<TeamMember>,
        lead: &str,
        base_tools: Vec<Arc<dyn aivyx_core::Tool>>,
        message_origin: aivyx_core::MessageOrigin,
    ) -> SpecialistPool {
        let config = TeamConfig {
            name: "t".into(),
            description: String::new(),
            lead: lead.into(),
            members,
            dialogue: DialogueConfig::default(),
        };
        let factory = SpecialistFactory::new(provider, "test-model", 4096, Arc::new(NullAuditHook), base_tools);
        let lead_caps = CapabilitySet::from_scopes([Scope::parse("fs.read").unwrap()]);
        SpecialistPool::new(factory, config, lead_caps, message_origin)
    }

    // --- tests -----------------------------------------------------------

    #[test]
    fn specialist_channel_floors_trust_to_the_lead() {
        // Member declares Kernel; lead channel is Trusted → floored to Trusted.
        let p = pool(
            FakeProvider::says("x"),
            vec![
                member("lead", &[], TrustTier::Trusted),
                member("spec", &["fs.read"], TrustTier::Kernel),
            ],
            "lead",
            vec![],
            aivyx_core::MessageOrigin::Operator,
        );
        let lead_ch = FakeLeadChannel::at(TrustTier::Trusted);
        let m = p.resolve("spec").unwrap();
        let ch = p.specialist_channel(m, &lead_ch);
        assert_eq!(ch.trust_tier(), TrustTier::Trusted, "Kernel floored to the lead");
        assert_eq!(ch.session_id(), lead_ch.session_id(), "shares the lead's session");
    }

    #[test]
    fn channel_floor_keeps_a_lower_member_ceiling() {
        let p = pool(
            FakeProvider::says("x"),
            vec![
                member("lead", &[], TrustTier::Trusted),
                member("spec", &["fs.read"], TrustTier::Trusted),
            ],
            "lead",
            vec![],
            aivyx_core::MessageOrigin::Operator,
        );
        // Lead on a less-trusted channel → specialist floored below its ceiling.
        let lead_ch = FakeLeadChannel::at(TrustTier::SemiTrusted);
        let ch = p.specialist_channel(p.resolve("spec").unwrap(), &lead_ch);
        assert_eq!(ch.trust_tier(), TrustTier::SemiTrusted);
    }

    #[test]
    fn resolve_matches_specialist_names_case_insensitively() {
        // #15 — the planner emits "Researcher"/"RESEARCHER"; the roster is
        // "researcher". All must resolve to the member (else the mission errors).
        let p = pool(
            FakeProvider::says("x"),
            vec![
                member("lead", &[], TrustTier::Trusted),
                member("researcher", &["fs.read"], TrustTier::Trusted),
            ],
            "lead",
            vec![],
            aivyx_core::MessageOrigin::Operator,
        );
        assert_eq!(p.resolve("researcher").unwrap().name, "researcher");
        assert_eq!(p.resolve("Researcher").unwrap().name, "researcher");
        assert_eq!(p.resolve("RESEARCHER").unwrap().name, "researcher");
        // The lead guard is case-insensitive too.
        assert!(p.resolve("Lead").is_err());
    }

    #[test]
    fn resolve_matches_by_role_label_not_just_name() {
        // The planner refers to specialists by their ROLE label ("Operations"),
        // but the roster id is "ops" — a real dogfood failure where the mission
        // errored "no specialist \"Operations\"" and the loop skipped a doable
        // story. Both the role AND the id must resolve.
        let mut ops = member("ops", &["shell.exec"], TrustTier::Trusted);
        ops.role = "Operations".into();
        let mut lead = member("coordinator", &[], TrustTier::Trusted);
        lead.role = "Lead".into();
        let p = pool(
            FakeProvider::says("x"),
            vec![lead, ops],
            "coordinator",
            vec![],
            aivyx_core::MessageOrigin::Operator,
        );

        // By role label (what the planner emits):
        assert_eq!(p.resolve("Operations").unwrap().name, "ops");
        assert_eq!(p.resolve("operations").unwrap().name, "ops");
        // By roster id still works:
        assert_eq!(p.resolve("ops").unwrap().name, "ops");
        // The lead is rejected whether referenced by id OR role label:
        assert!(matches!(
            p.resolve("coordinator"),
            Err(TeamError::Config(m)) if m.contains("is the lead")
        ));
        assert!(matches!(
            p.resolve("Lead"),
            Err(TeamError::Config(m)) if m.contains("is the lead")
        ));
    }

    #[test]
    fn cannot_delegate_to_the_lead_or_an_unknown_member() {
        let p = pool(
            FakeProvider::says("x"),
            vec![member("lead", &[], TrustTier::Trusted)],
            "lead",
            vec![],
            aivyx_core::MessageOrigin::Operator,
        );
        assert!(matches!(p.resolve("lead"), Err(TeamError::Config(m)) if m.contains("is the lead")));
        assert!(matches!(p.resolve("ghost"), Err(TeamError::Config(m)) if m.contains("no specialist")));
    }

    #[tokio::test]
    async fn run_executes_a_specialist_sub_turn_and_returns_its_output() {
        let p = pool(
            FakeProvider::says("inventory looks healthy"),
            vec![
                member("lead", &[], TrustTier::Trusted),
                member("inventory", &["fs.read"], TrustTier::Trusted),
            ],
            "lead",
            vec![],
            aivyx_core::MessageOrigin::Operator,
        );
        let lead_ch = FakeLeadChannel::at(TrustTier::Trusted);
        let out = p.run("inventory", "check stock", None, &lead_ch).await.unwrap();
        assert_eq!(out, "inventory looks healthy");
    }

    #[tokio::test]
    async fn run_rejects_delegating_to_the_lead() {
        let p = pool(
            FakeProvider::says("x"),
            vec![member("lead", &[], TrustTier::Trusted)],
            "lead",
            vec![],
            aivyx_core::MessageOrigin::Operator,
        );
        let lead_ch = FakeLeadChannel::at(TrustTier::Trusted);
        assert!(p.run("lead", "task", None, &lead_ch).await.is_err());
    }

    #[test]
    fn resolve_falls_back_to_best_fit_for_an_unknown_specialist() {
        // The planner sometimes names a specialist that isn't in the roster (a
        // generic role word). Rather than hard-fail the story, resolve routes to
        // the best-fit member BY CAPABILITY. Uses the real default Nonagon.
        let p = pool(
            FakeProvider::says("x"),
            crate::roster::default_nonagon().members,
            "coordinator",
            vec![],
            aivyx_core::MessageOrigin::Operator,
        );

        // "Operations" is no longer a role (ops→verifier); a run/inspect word
        // maps to a shell-capable, least-privilege (non-writing) runner.
        assert_eq!(p.resolve("Operations").unwrap().name, "verifier");
        assert_eq!(p.resolve("Tester").unwrap().name, "verifier");
        // A code word → the write+shell coder.
        assert_eq!(p.resolve("Developer").unwrap().name, "coder");
        // A writing word → a pure writer (not the coder), least-privilege.
        assert_eq!(p.resolve("Technical Author").unwrap().name, "writer");
        // A research word → a fetch-capable member.
        assert_eq!(p.resolve("Investigator").unwrap().name, "researcher");
        // No sensible capability fit still errors (unchanged behaviour).
        assert!(matches!(
            p.resolve("Astrologer"),
            Err(TeamError::Config(m)) if m.contains("no specialist")
        ));
        // The lead guard is untouched — naming the lead is still an error, not
        // a fallback.
        assert!(p.resolve("coordinator").is_err());
    }

    /// Like `member()`, but with `tool_allowlist` set so the returned member
    /// actually gets `origin.capture` mounted on its `ToolRegistry` --
    /// `filter_tools` (aivyx-team/src/factory.rs) gives an EMPTY allowlist NO
    /// tools by design, so the plain `member()` fixture alone would leave the
    /// specialist unable to call the tool this test needs to observe.
    fn member_with_tool(name: &str, tool_name: &str, tier: TrustTier) -> TeamMember {
        let mut m = member(name, &["fs.read"], tier);
        m.tool_allowlist = vec![tool_name.to_string()];
        m
    }

    #[tokio::test]
    async fn specialist_message_is_system_originated_when_pool_is_triggered() {
        let observed = Arc::new(Mutex::new(None));
        let tool: Arc<dyn aivyx_core::Tool> =
            Arc::new(OriginCapturingTool::new(Arc::clone(&observed)));
        let p = pool(
            FakeProvider::calls_tool("origin.capture", serde_json::json!({})),
            vec![
                member("lead", &[], TrustTier::Trusted),
                member_with_tool("worker", "origin.capture", TrustTier::Trusted),
            ],
            "lead",
            vec![tool],
            aivyx_core::MessageOrigin::System,
        );
        let lead_ch = FakeLeadChannel::at(TrustTier::Trusted);
        p.run("worker", "capture your origin", None, &lead_ch)
            .await
            .expect("specialist turn completes");
        assert_eq!(*observed.lock().unwrap(), Some(aivyx_core::MessageOrigin::System));
    }

    #[tokio::test]
    async fn specialist_message_is_operator_originated_when_pool_is_interactive() {
        // Companion to the test above -- proves the pool doesn't
        // over-tag every specialist turn as System.
        let observed = Arc::new(Mutex::new(None));
        let tool: Arc<dyn aivyx_core::Tool> =
            Arc::new(OriginCapturingTool::new(Arc::clone(&observed)));
        let p = pool(
            FakeProvider::calls_tool("origin.capture", serde_json::json!({})),
            vec![
                member("lead", &[], TrustTier::Trusted),
                member_with_tool("worker", "origin.capture", TrustTier::Trusted),
            ],
            "lead",
            vec![tool],
            aivyx_core::MessageOrigin::Operator,
        );
        let lead_ch = FakeLeadChannel::at(TrustTier::Trusted);
        p.run("worker", "capture your origin", None, &lead_ch)
            .await
            .expect("specialist turn completes");
        assert_eq!(*observed.lock().unwrap(), Some(aivyx_core::MessageOrigin::Operator));
    }
}
