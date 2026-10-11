//! The in-process Matrix session: one room may drive turns
//! (`MatrixPolicy::room_filter`), so one `MatrixChannel`, a pending queue
//! and `/cancel` during a turn. Everything else is decided by
//! `policy::decide`.

use std::collections::VecDeque;
use std::sync::Arc;

use aivyx_capability::CapabilitySet;
use aivyx_core::{
    agent::ConcreteAgent, llm_planner::LlmPlanner, planner::ToolRegistry, Agent, AgentId,
    AuditHook, CancellationToken, ChannelContext, LlmPlannerConfig, Message,
};
use aivyx_llm::LlmProvider;
use aivyx_storage::Storage;

use crate::matrix_channel::MatrixChannel;
use crate::policy::{decide, Action, MatrixPolicy, OncePerRoom, ASK_TO_VERIFY, CANT_DECRYPT};
use crate::transport::{IncomingMessage, MatrixTransport, OutgoingMessage, TransportError};

/// Same fields and meaning as `aivyx_discord::DiscordSessionConfig`.
#[derive(Clone)]
pub struct MatrixSessionConfig {
    pub model: String,
    pub system_prompt: String,
    pub max_tokens: u32,
    pub capabilities: CapabilitySet,
    pub tools: Arc<ToolRegistry>,
    pub storage: Arc<dyn Storage>,
    pub tool_allowlist: Option<std::collections::BTreeSet<String>>,
    pub memory_topic_prefix: Option<String>,
    pub turn_timeout_secs: Option<u64>,
    pub cycle_detection: Option<bool>,
    pub injection_scan_enabled: bool,
    pub confirm_integration_writes: bool,
    pub budget_gate: Option<Arc<dyn aivyx_core::BudgetGate>>,
    pub rate_gate: Option<Arc<dyn aivyx_core::RateGate>>,
    pub injection_scan_exempt: std::collections::BTreeSet<String>,
}

#[derive(Debug, Clone, Default)]
pub struct MatrixSessionReport {
    pub turns_run: usize,
}

fn build_agent(
    config: MatrixSessionConfig,
    provider: Arc<dyn LlmProvider>,
    audit: Arc<dyn AuditHook>,
    checkpointer: Option<Arc<aivyx_core::GitCheckpointer>>,
) -> impl Agent {
    let registry = config.tools;
    let registry_for_factory = Arc::clone(&registry);
    let planner_config = LlmPlannerConfig::new(config.model)
        .with_system_prompt(config.system_prompt)
        .with_max_tokens(config.max_tokens)
        .with_tool_allowlist(config.tool_allowlist.clone());
    let agent = ConcreteAgent::new(
        AgentId::new(),
        config.capabilities,
        registry,
        audit,
        move || {
            Box::new(LlmPlanner::new(
                Arc::clone(&provider),
                Arc::clone(&registry_for_factory),
                planner_config.clone(),
            ))
        },
    )
    .with_tool_allowlist(config.tool_allowlist)
    .with_memory_topic_prefix(config.memory_topic_prefix)
    .with_checkpointer(checkpointer)
    .with_confirm_integration_writes(config.confirm_integration_writes)
    .with_budget_gate(config.budget_gate)
    .with_rate_gate(config.rate_gate);
    aivyx_core::TurnSafety::interactive(
        config.turn_timeout_secs,
        config.cycle_detection,
        config.injection_scan_enabled,
        config.injection_scan_exempt,
    )
    .apply(agent)
}

/// What the routing step did with one inbound item.
enum Routed {
    Turn(IncomingMessage),
    Handled,
}

/// Applies `decide` to one inbound item: turns are returned; joins,
/// declines and once-per-room explanations are carried out here.
async fn route<T: MatrixTransport>(
    transport: &T,
    policy: &MatrixPolicy,
    asked: &mut OncePerRoom,
    told_undecryptable: &mut OncePerRoom,
    inbound: crate::transport::Inbound,
) -> Result<Routed, TransportError> {
    match decide(policy, inbound) {
        Action::Turn(m) => return Ok(Routed::Turn(m)),
        Action::AskToVerify { room_id } if asked.first(&room_id) => {
            transport
                .send_message(OutgoingMessage {
                    room_id,
                    text: ASK_TO_VERIFY.into(),
                })
                .await?;
        }
        Action::CantDecrypt { room_id } if told_undecryptable.first(&room_id) => {
            transport
                .send_message(OutgoingMessage {
                    room_id,
                    text: CANT_DECRYPT.into(),
                })
                .await?;
        }
        Action::Join { room_id } => transport.join_room(&room_id).await?,
        Action::Decline { room_id } => transport.decline_invite(&room_id).await?,
        Action::AskToVerify { .. } | Action::CantDecrypt { .. } | Action::Ignore => {}
    }
    Ok(Routed::Handled)
}

/// Runs the in-process session over `transport`. Production passes the one
/// `sdk::SdkTransport` built for the run (the same instance the daemon-mode
/// frontend would have used, so a fallback loses nothing queued).
pub async fn run_matrix_session_with_transport<T: MatrixTransport + 'static>(
    transport: Arc<T>,
    policy: MatrixPolicy,
    config: MatrixSessionConfig,
    provider: Arc<dyn LlmProvider>,
    audit: Arc<dyn AuditHook>,
    checkpointer: Option<Arc<aivyx_core::GitCheckpointer>>,
    shutdown: CancellationToken,
) -> Result<MatrixSessionReport, String> {
    if !transport
        .room_is_encrypted(&policy.room_filter)
        .await
        .map_err(|e| e.to_string())?
    {
        return Err(format!(
            "the Matrix room {} isn't encrypted — the assistant only talks in \
             end-to-end-encrypted rooms. Turn on encryption for it, or use another room.",
            policy.room_filter
        ));
    }
    let agent = build_agent(config, provider, audit, checkpointer);
    let channel = Arc::new(MatrixChannel::new(
        "aivyx-matrix",
        policy.room_filter.clone(),
        Arc::clone(&transport),
    ));
    let (mut asked, mut told) = (OncePerRoom::new(), OncePerRoom::new());
    let mut pending: VecDeque<IncomingMessage> = VecDeque::new();
    let mut turns_run = 0usize;
    let mut backoff = std::time::Duration::from_secs(1);

    loop {
        if pending.is_empty() {
            let inbound = tokio::select! {
                biased;
                _ = shutdown.cancelled() => return Ok(MatrixSessionReport { turns_run }),
                res = transport.next_inbound() => res,
            };
            let inbound = match inbound {
                Ok(i) => {
                    backoff = std::time::Duration::from_secs(1);
                    i
                }
                Err(TransportError::Unauthorized(e)) => {
                    return Err(TransportError::Unauthorized(e).to_string());
                }
                Err(e) => {
                    eprintln!("aivyx-matrix: {e}; retrying in {}s", backoff.as_secs());
                    tokio::select! {
                        biased;
                        _ = shutdown.cancelled() => {
                            return Ok(MatrixSessionReport { turns_run });
                        }
                        _ = tokio::time::sleep(backoff) => {}
                    }
                    backoff = (backoff * 2).min(std::time::Duration::from_secs(60));
                    continue;
                }
            };
            match route(transport.as_ref(), &policy, &mut asked, &mut told, inbound).await {
                Ok(Routed::Turn(m)) if m.text.trim() == "/cancel" => continue,
                Ok(Routed::Turn(m)) => pending.push_back(m),
                Ok(Routed::Handled) => continue,
                Err(e @ TransportError::Unauthorized(_)) => return Err(e.to_string()),
                Err(e) => {
                    eprintln!("aivyx-matrix: {e}");
                    continue;
                }
            }
        }

        let m = pending.pop_front().expect("pending is non-empty here");
        channel.reset_cancellation();
        let turn = agent.turn(
            Message::text(channel.session_id(), &m.text),
            channel.as_ref(),
        );
        tokio::pin!(turn);
        let mut fatal: Option<String> = None;
        loop {
            tokio::select! {
                biased;
                _ = &mut turn => break,
                _ = shutdown.cancelled() => {
                    // Cancel the turn, let it wind down, then stop; the
                    // check after this loop returns.
                    channel.cancellation_token().cancel();
                    let _ = (&mut turn).await;
                    break;
                }
                res = transport.next_inbound() => {
                    let inbound = match res {
                        Ok(i) => i,
                        Err(e @ TransportError::Unauthorized(_)) => {
                            // Logged out mid-turn: finish this turn, then fail.
                            fatal = Some(e.to_string());
                            let _ = (&mut turn).await;
                            break;
                        }
                        // Any other error (e.g. a dead sync task) would
                        // return instantly on every poll: stop polling for
                        // this turn; the outer loop applies its backoff.
                        Err(_) => {
                            let _ = (&mut turn).await;
                            break;
                        }
                    };
                    match route(transport.as_ref(), &policy, &mut asked, &mut told, inbound).await {
                        Ok(Routed::Turn(next)) if next.text.trim() == "/cancel" => {
                            channel.cancellation_token().cancel();
                        }
                        Ok(Routed::Turn(next)) => pending.push_back(next),
                        Ok(Routed::Handled) => {}
                        Err(e @ TransportError::Unauthorized(_)) => {
                            fatal = Some(e.to_string());
                            let _ = (&mut turn).await;
                            break;
                        }
                        Err(e) => eprintln!("aivyx-matrix: {e}"),
                    }
                }
            }
        }
        turns_run += 1;
        if let Some(e) = fatal {
            return Err(e);
        }
        if shutdown.is_cancelled() {
            return Ok(MatrixSessionReport { turns_run });
        }
    }
}
