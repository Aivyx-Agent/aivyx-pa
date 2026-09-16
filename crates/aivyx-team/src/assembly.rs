//! `TeamAssembly` — wire a [`TeamConfig`] + the daemon's shared deps into a
//! runnable team (J.5).
//!
//! This closes the J.3/J.4 "roster wiring" deferral: from a config it builds
//! the one shared [`MessageBus`], a [`SpecialistFactory`] with dialogue wired
//! (so every specialist sub-turn gets its own `send_message` / `read_message`
//! — see [`SpecialistFactory::with_dialogue`]), the [`SpecialistPool`], and a
//! [`TeamRuntime`]. [`lead_tools`](TeamAssembly::lead_tools) returns the tool
//! set the operator-facing **lead agent** is mounted with: orchestration
//! (`decompose_task` / `synthesize_results` / `verify_output`), delegation
//! (`delegate_task` / `query_agent`), and the lead's own message tools.
//!
//! The CLI (`aivyx-pa team run`) builds a lead `ConcreteAgent` over `lead_tools`
//! and the same `AuditHook`, so every delegation + specialist tool call lands
//! on the one HMAC chain.

use std::sync::Arc;

use aivyx_capability::CapabilitySet;
use aivyx_core::{AuditHook, Tool};
use aivyx_llm::LlmProvider;

use crate::config::{TeamConfig, TeamError};
use crate::factory::SpecialistFactory;
use crate::message_bus::MessageBus;
use crate::message_tools::{ReadMessagesTool, SendMessageTool};
use crate::orchestration::{DecomposeTaskTool, SynthesizeResultsTool, VerifyOutputTool};
use crate::pool::SpecialistPool;
use crate::runtime::TeamRuntime;
use crate::tools::{DelegateTaskTool, QueryAgentTool};

/// A wired, runnable team: the shared bus, pool, runtime, and the lead's
/// mounted tool set.
pub struct TeamAssembly {
    config: TeamConfig,
    bus: Arc<MessageBus>,
    pool: Arc<SpecialistPool>,
    runtime: Arc<TeamRuntime>,
    ceiling: CapabilitySet,
    /// The lead's own message tools, kept typed so a multi-turn driver can
    /// reset the per-turn send budget at turn boundaries.
    lead_send: Arc<SendMessageTool>,
}

impl TeamAssembly {
    /// Validate `config` and wire the team against the daemon's shared deps.
    /// `ceiling` is the operator's real, un-narrowed authority — every
    /// specialist is attenuated to a subset of it (NT-02). This is
    /// deliberately NOT the lead's own (possibly narrower)
    /// `capability_scopes` field: a purely-orchestration lead that
    /// declares no domain scopes for itself must still be able to grant
    /// its specialists whatever the operator's real floor allows, or
    /// every specialist collapses to near-nothing (the "missions report
    /// done but do nothing" failure `bind_lead_scopes` exists to
    /// prevent). The lead's own `ConcreteAgent`, built separately by
    /// this function's caller, uses the lead's own narrower field — it
    /// must still grant `team.delegate` + `team.message` for the
    /// orchestration/dialogue tools to be callable.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        config: TeamConfig,
        provider: Arc<dyn LlmProvider>,
        model: impl Into<String>,
        max_tokens: u32,
        audit: Arc<dyn AuditHook>,
        base_tools: Vec<Arc<dyn Tool>>,
        ceiling: CapabilitySet,
        member_backends: std::collections::HashMap<
            String,
            crate::factory::SpecialistBackend,
        >,
        checkpointer: Option<Arc<aivyx_core::GitCheckpointer>>,
        kv_cache_handles: Option<(
            Arc<aivyx_llm::KvSlotPool>,
            Arc<aivyx_kvcache::LlamaServerSlotStore>,
            String,
        )>,
        broker_slot_hint_mode: bool,
        message_origin: aivyx_core::MessageOrigin,
        injection_scan_enabled: bool,
        injection_scan_exempt: std::collections::BTreeSet<String>,
        // Task 4 fix round 1 — the operator's `[access] confirm_destructive`
        // posture, threaded into `SpecialistFactory::with_confirm_destructive`
        // so every specialist (and the lead, when built through this same
        // factory) honors it, same as every other agent construction path.
        confirm_destructive: bool,
    ) -> Result<Self, TeamError> {
        config.validate()?;
        let dialogue = config.dialogue.clone();
        let bus = MessageBus::new(dialogue.message_bus_capacity);

        let factory = SpecialistFactory::new(provider, model, max_tokens, audit, base_tools)
            .with_dialogue(Arc::clone(&bus), dialogue.clone())
            .with_member_backends(member_backends)
            .with_checkpointer(checkpointer)
            .with_kv_cache(kv_cache_handles)
            .with_broker_slot_hint_mode(broker_slot_hint_mode)
            .with_injection_scan_enabled(injection_scan_enabled)
            .with_injection_scan_exempt(injection_scan_exempt)
            // Task 4 fix round 1 — same `[access] confirm_destructive`
            // posture as every other agent construction path.
            .with_confirm_destructive(confirm_destructive);
        let pool = Arc::new(SpecialistPool::new(
            factory,
            config.clone(),
            ceiling.clone(),
            message_origin,
        ));
        let runtime = Arc::new(TeamRuntime::new(Arc::clone(&pool)));

        // is_lead = true: the lead may always send, even with peer dialogue off.
        let lead_send = Arc::new(SendMessageTool::new(
            &config.lead,
            Arc::clone(&bus),
            &dialogue,
            true,
        ));

        Ok(TeamAssembly {
            config,
            bus,
            pool,
            runtime,
            ceiling,
            lead_send,
        })
    }

    pub fn pool(&self) -> Arc<SpecialistPool> {
        Arc::clone(&self.pool)
    }
    pub fn runtime(&self) -> Arc<TeamRuntime> {
        Arc::clone(&self.runtime)
    }
    pub fn bus(&self) -> Arc<MessageBus> {
        Arc::clone(&self.bus)
    }
    pub fn ceiling(&self) -> &CapabilitySet {
        &self.ceiling
    }
    pub fn config(&self) -> &TeamConfig {
        &self.config
    }

    /// The tool set the lead agent is mounted with — orchestration, delegation,
    /// and the lead's own message tools.
    pub fn lead_tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(DecomposeTaskTool::new(Arc::clone(&self.runtime))),
            Arc::new(SynthesizeResultsTool::new()),
            Arc::new(VerifyOutputTool::new(Arc::clone(&self.pool))),
            Arc::new(DelegateTaskTool::new(Arc::clone(&self.pool))),
            Arc::new(QueryAgentTool::new(Arc::clone(&self.pool))),
            Arc::clone(&self.lead_send) as Arc<dyn Tool>,
            Arc::new(ReadMessagesTool::new(&self.bus, &self.config.lead)),
        ]
    }

    /// Reset the lead's per-turn message budget. A multi-turn driver (the
    /// loop / TUI) calls this at each turn boundary; specialists are rebuilt
    /// per sub-turn, so their budgets refresh on their own.
    pub fn reset_lead_turn(&self) {
        self.lead_send.reset_turn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DialogueConfig, TeamMember};
    use crate::mission::{MissionPlan, Step};
    use crate::testutil::{FakeLeadChannel, FakeProvider};
    use aivyx_capability::{Scope, TrustTier};
    use aivyx_core::{
        AgentId, CancellationToken, ChannelContext, NullAuditHook, ToolContext, TurnId,
    };
    use serde_json::{json, Value};

    fn member(name: &str, scopes: &[&str]) -> TeamMember {
        TeamMember {
            name: name.into(),
            role: "R".into(),
            soul: "You are a specialist.".into(),
            tool_allowlist: vec![],
            capability_scopes: scopes.iter().map(|s| s.to_string()).collect(),
            trust_ceiling: TrustTier::Trusted,
            model: None,
            base_url: None,
        }
    }

    fn config() -> TeamConfig {
        TeamConfig {
            name: "t".into(),
            description: String::new(),
            lead: "lead".into(),
            members: vec![
                member("lead", &["team.delegate", "team.message"]),
                member("worker", &["team.message"]),
                member("reviewer", &["team.message"]),
            ],
            dialogue: DialogueConfig::default(),
        }
    }

    fn lead_caps() -> CapabilitySet {
        CapabilitySet::from_scopes([
            Scope::parse("team.delegate").unwrap(),
            Scope::parse("team.message").unwrap(),
        ])
    }

    fn assembly(provider: Arc<dyn LlmProvider>) -> TeamAssembly {
        TeamAssembly::build(
            config(),
            provider,
            "test-model",
            4096,
            Arc::new(NullAuditHook),
            vec![],
            lead_caps(),
            std::collections::HashMap::new(),
            None,
            None,
            false,
            aivyx_core::MessageOrigin::Operator,
            true,
            std::collections::BTreeSet::new(),
            false,
        )
        .expect("valid team")
    }

    #[test]
    fn build_rejects_an_invalid_config() {
        let mut bad = config();
        bad.lead = "ghost".into(); // not a member
        let result = TeamAssembly::build(
            bad,
            FakeProvider::always("x"),
            "m",
            4096,
            Arc::new(NullAuditHook),
            vec![],
            lead_caps(),
            std::collections::HashMap::new(),
            None,
            None,
            false,
            aivyx_core::MessageOrigin::Operator,
            true,
            std::collections::BTreeSet::new(),
            false,
        );
        assert!(matches!(result, Err(TeamError::Config(m)) if m.contains("lead")));
    }

    #[test]
    fn lead_tools_expose_the_full_orchestration_surface() {
        let a = assembly(FakeProvider::always("x"));
        let tools = a.lead_tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        for expected in [
            "decompose_task",
            "synthesize_results",
            "verify_output",
            "delegate_task",
            "query_agent",
            "send_message",
            "read_message",
        ] {
            assert!(names.contains(&expected), "lead is missing {expected}");
        }
    }

    #[test]
    fn lead_tools_carry_the_right_scopes() {
        let a = assembly(FakeProvider::always("x"));
        for t in a.lead_tools() {
            let base = t.required_scope(&Value::Null).base().to_string();
            match t.name() {
                "send_message" | "read_message" => assert_eq!(base, "team.message"),
                _ => assert_eq!(base, "team.delegate", "{} should be lead-only", t.name()),
            }
        }
    }

    #[tokio::test]
    async fn assembly_runs_a_mission_through_the_runtime() {
        let a = assembly(FakeProvider::always("done"));
        let plan = MissionPlan::new(
            "demo",
            vec![
                Step::delegate("a", "worker", "do it"),
                Step::gate("g", "reviewer", "ok?").after(["a"]),
            ],
        );
        let lead = FakeLeadChannel::at(TrustTier::Trusted);
        let report = a.runtime().run(&plan, &lead).await.unwrap();
        assert!(report.succeeded());
        assert_eq!(report.outputs.len(), 2);
    }

    #[tokio::test]
    async fn reset_lead_turn_refreshes_the_send_budget() {
        let mut cfg = config();
        cfg.dialogue.max_messages_per_turn = 1;
        let a = TeamAssembly::build(
            cfg,
            FakeProvider::always("x"),
            "m",
            4096,
            Arc::new(NullAuditHook),
            vec![],
            lead_caps(),
            std::collections::HashMap::new(),
            None,
            None,
            false,
            aivyx_core::MessageOrigin::Operator,
            true,
            std::collections::BTreeSet::new(),
            false,
        )
        .unwrap();

        // Pull the lead's send tool out of the mounted set.
        let tools = a.lead_tools();
        let send = tools.iter().find(|t| t.name() == "send_message").unwrap();

        let ch = FakeLeadChannel::at(TrustTier::Trusted);
        let audit = NullAuditHook;
        let tok = CancellationToken::new();
        let ctx = ToolContext {
            agent_id: AgentId::new(),
            session_id: ch.session_id(),
            turn_id: TurnId::new(),
            channel: &ch,
            audit: &audit,
            cancellation: &tok,
            message_origin: aivyx_core::MessageOrigin::Operator,
        };

        // Budget of 1: first send ok, second over budget.
        assert!(matches!(
            send.execute(json!({ "content": "1" }), &ctx).await,
            aivyx_core::ToolOutcome::Completed { .. }
        ));
        assert!(matches!(
            send.execute(json!({ "content": "2" }), &ctx).await,
            aivyx_core::ToolOutcome::Failed(_)
        ));
        // Turn boundary → budget refreshed via the assembly's handle.
        a.reset_lead_turn();
        assert!(matches!(
            send.execute(json!({ "content": "3" }), &ctx).await,
            aivyx_core::ToolOutcome::Completed { .. }
        ));
    }

    // A tool that requires a withheld-by-default destructive scope
    // (`email.send` — see `aivyx_capability::WITHHELD_INTEGRATION_BASES`).
    // Its `execute` would happily complete; the point of the test below is
    // that `confirm_destructive` must stop the call before `execute` ever
    // runs.
    struct EmailSendTool(aivyx_core::ToolId);
    #[async_trait::async_trait]
    impl Tool for EmailSendTool {
        fn id(&self) -> aivyx_core::ToolId {
            self.0
        }
        fn name(&self) -> &str {
            "send"
        }
        fn description(&self) -> &str {
            "send an email"
        }
        fn input_schema(&self) -> &Value {
            use std::sync::OnceLock;
            static S: OnceLock<Value> = OnceLock::new();
            S.get_or_init(|| json!({ "type": "object" }))
        }
        fn required_scope(&self, _: &Value) -> Scope {
            Scope::parse("email.send").unwrap()
        }
        async fn execute(
            &self,
            _: Value,
            _: &aivyx_core::ToolContext<'_>,
        ) -> aivyx_core::ToolOutcome {
            aivyx_core::ToolOutcome::Completed {
                output: json!({ "sent": true }),
                verified: aivyx_core::Verification::NotApplicable,
            }
        }
    }

    /// Task 4 fix round 3, I2 — `TeamAssembly::build` takes 15 positional
    /// parameters, three of them bare `bool`s (`broker_slot_hint_mode`,
    /// `injection_scan_enabled`, `confirm_destructive`); a reviewer flagged
    /// that shape as a real argument-swap risk. This proves
    /// `confirm_destructive: true` reaches a real specialist's gate through
    /// the whole production path — `TeamAssembly::build` ->
    /// `SpecialistPool::run` -> `SpecialistFactory::with_confirm_destructive`
    /// -> `ConcreteAgent::with_confirm_destructive` — not just that the
    /// field is stored somewhere. Corrected (Task 4 final review, Minor):
    /// a swap with `broker_slot_hint_mode` (`false` in this call) would
    /// make this test fail, since `confirm_destructive` would then receive
    /// `false` and the call would complete instead of escalating. A swap
    /// with `injection_scan_enabled` specifically would NOT be caught by
    /// this test — both are `true` in this call, so either ordering passes
    /// the same value to `with_confirm_destructive`; only a value-level
    /// assertion on which knob fired (not attempted here) would catch that
    /// particular pair.
    #[tokio::test]
    async fn confirm_destructive_threads_from_team_assembly_build_to_a_specialist_gate() {
        let tool: Arc<dyn Tool> = Arc::new(EmailSendTool(aivyx_core::ToolId::new()));

        let mailer = TeamMember {
            name: "mailer".into(),
            role: "R".into(),
            soul: "You send email.".into(),
            tool_allowlist: vec!["send".to_string()],
            capability_scopes: vec!["email.send".to_string()],
            trust_ceiling: TrustTier::Trusted,
            model: None,
            base_url: None,
        };
        let mut cfg = config();
        cfg.members.push(mailer);

        let caps = CapabilitySet::from_scopes([
            Scope::parse("team.delegate").unwrap(),
            Scope::parse("team.message").unwrap(),
            Scope::parse("email.send").unwrap(),
        ]);

        let a = TeamAssembly::build(
            cfg,
            FakeProvider::tool_call_then_done("send", json!({})),
            "test-model",
            4096,
            Arc::new(NullAuditHook),
            vec![tool],
            caps,
            std::collections::HashMap::new(),
            None,
            None,
            false,
            aivyx_core::MessageOrigin::Operator,
            true,
            std::collections::BTreeSet::new(),
            true, // confirm_destructive
        )
        .expect("valid team");

        let lead = FakeLeadChannel::at(TrustTier::Trusted);
        let err = a
            .pool()
            .run("mailer", "send the email", None, &lead)
            .await
            .expect_err("a withheld destructive scope must escalate, not complete");

        match err {
            TeamError::Config(msg) => assert!(
                msg.contains("escalated for approval"),
                "expected an escalation error naming the pending approval, got: {msg}"
            ),
            other => panic!("expected TeamError::Config carrying the escalation, got {other:?}"),
        }
    }
}
