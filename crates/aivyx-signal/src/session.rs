//! The in-process Signal session, and the pieces daemon mode shares
//! (`route`, `turn_text`, `send_reply`).
//!
//! One turn at a time; messages that arrive during a turn wait in a queue,
//! except `/cancel`, which cancels it. A typing indicator shows while a
//! turn runs; the reply goes out once complete, split into Signal-sized
//! parts.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use aivyx_capability::CapabilitySet;
use aivyx_core::{
    agent::ConcreteAgent, llm_planner::LlmPlanner, planner::ToolRegistry, Agent, AgentId,
    AuditHook, CancellationToken, ChannelContext, LlmPlannerConfig, Message, SessionId,
};
use aivyx_llm::LlmProvider;
use aivyx_storage::Storage;

use crate::envelope::{Inbound, TextMessage};
use crate::lock;
use crate::policy::{decide, Action, OncePerSender, SignalPolicy, ASK_TO_VERIFY};
use crate::signal_channel::SignalChannel;
use crate::transport::{SignalTransport, TransportError};

/// The most of a message a turn reads.
pub const MAX_INPUT: usize = 4 * 1024;
/// The longest single Signal message the assistant sends.
pub const MAX_PART: usize = 2000;
/// Added to the reply when the message was cut to [`MAX_INPUT`].
pub const CUT_NOTE: &str = "(Your message was longer than 4 KiB, so I read only the start.)";
/// Added to the reply when the message had attachments.
pub const ATTACHMENTS_NOTE: &str = "(I can't open attachments, so I ignored them.)";

/// What [`route`] did with one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Routed {
    Turn(TextMessage),
    Handled,
}

/// Applies the trust policy to one event: a turn is returned (after a
/// read receipt); the once-per-run "please verify" reply is sent here.
pub async fn route<T: SignalTransport + ?Sized>(
    transport: &T,
    policy: &SignalPolicy,
    asked: &mut OncePerSender,
    inbound: Inbound,
) -> Result<Routed, TransportError> {
    let trust = match &inbound {
        Inbound::Text(m) if policy.is_allowed(m) => {
            transport.identity(m.sender()).await?.map(|i| i.trust)
        }
        _ => None,
    };
    match decide(policy, inbound, trust) {
        Action::Turn(m) => {
            if let Err(e) = transport.read_receipt(m.sender(), m.timestamp).await {
                eprintln!("aivyx-signal: read receipt failed: {e}");
            }
            Ok(Routed::Turn(m))
        }
        Action::AskToVerify { to } => {
            if asked.first(&to) {
                transport.send_text(&to, ASK_TO_VERIFY).await?;
            }
            Ok(Routed::Handled)
        }
        Action::Ignore => Ok(Routed::Handled),
    }
}

/// What the agent reads (the text, cut to [`MAX_INPUT`]) and the notes to
/// add to the reply.
pub fn turn_text(m: &TextMessage) -> (String, Vec<String>) {
    let mut notes = Vec::new();
    let text = if m.text.len() > MAX_INPUT {
        notes.push(CUT_NOTE.to_string());
        let mut end = MAX_INPUT;
        while !m.text.is_char_boundary(end) {
            end -= 1;
        }
        m.text[..end].to_string()
    } else {
        m.text.clone()
    };
    if m.attachments > 0 {
        notes.push(ATTACHMENTS_NOTE.to_string());
    }
    (text, notes)
}

/// `text` in parts of at most [`MAX_PART`] characters, cut at a line break
/// (else a space) where one is close to the limit.
pub fn split_reply(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut rest = text.trim();
    while rest.chars().count() > MAX_PART {
        let limit = rest
            .char_indices()
            .nth(MAX_PART)
            .map_or(rest.len(), |(i, _)| i);
        let window = &rest[..limit];
        let cut = window
            .rfind('\n')
            .filter(|&i| i > limit / 2)
            .or_else(|| window.rfind(' ').filter(|&i| i > limit / 2))
            .unwrap_or(limit);
        parts.push(rest[..cut].trim_end().to_string());
        rest = rest[cut..].trim_start();
    }
    if !rest.is_empty() {
        parts.push(rest.to_string());
    }
    parts
}

/// Sends `answer` (plus `notes`) to `to`, in parts.
pub async fn send_reply<T: SignalTransport + ?Sized>(
    transport: &T,
    to: &str,
    answer: &str,
    notes: &[String],
) -> Result<(), TransportError> {
    let body = if notes.is_empty() {
        answer.to_string()
    } else {
        format!("{answer}\n\n{}", notes.join("\n"))
    };
    for part in split_reply(&body) {
        transport.send_text(to, &part).await?;
    }
    Ok(())
}

/// A sender's memory namespace: `signal-` plus the sender with anything
/// but letters, digits and `-` dropped.
pub fn partition(sender: &str) -> String {
    let id: String = sender
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    format!("signal-{id}")
}

/// Same fields and meaning as `aivyx_matrix::MatrixSessionConfig`.
#[derive(Clone)]
pub struct SignalSessionConfig {
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SignalSessionReport {
    pub turns_run: usize,
}

fn build_agent(
    config: SignalSessionConfig,
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

/// Runs the in-process session. `state_dir` is the instance's
/// `signal_dir()` (it holds the lock).
#[allow(clippy::too_many_arguments)]
pub async fn run_signal_session_with_transport<T: SignalTransport + ?Sized>(
    transport: &T,
    policy: &SignalPolicy,
    state_dir: &Path,
    config: SignalSessionConfig,
    provider: Arc<dyn LlmProvider>,
    audit: Arc<dyn AuditHook>,
    checkpointer: Option<Arc<aivyx_core::GitCheckpointer>>,
    shutdown: CancellationToken,
) -> Result<SignalSessionReport, String> {
    let _lock = lock::acquire(state_dir)?;
    let agent = build_agent(config, provider, audit, checkpointer);
    let mut sessions: HashMap<String, SessionId> = HashMap::new();
    let mut asked = OncePerSender::default();
    let mut pending: VecDeque<TextMessage> = VecDeque::new();
    let mut report = SignalSessionReport::default();
    let mut backoff = Duration::from_secs(1);

    loop {
        if pending.is_empty() {
            let event = tokio::select! {
                biased;
                _ = shutdown.cancelled() => return Ok(report),
                r = transport.next_event() => r,
            };
            let event = match event {
                Ok(e) => {
                    backoff = Duration::from_secs(1);
                    e
                }
                Err(e) if e.is_fatal() => return Err(e.to_string()),
                Err(e) => {
                    eprintln!("aivyx-signal: {e}; retrying in {}s", backoff.as_secs());
                    tokio::select! {
                        biased;
                        _ = shutdown.cancelled() => return Ok(report),
                        _ = tokio::time::sleep(backoff) => {}
                    }
                    backoff = (backoff * 2).min(Duration::from_secs(60));
                    continue;
                }
            };
            match route(transport, policy, &mut asked, event).await {
                Ok(Routed::Turn(m)) if m.text.trim() == "/cancel" => {}
                Ok(Routed::Turn(m)) => pending.push_back(m),
                Ok(Routed::Handled) => {}
                Err(e) if e.is_fatal() => return Err(e.to_string()),
                Err(e) => eprintln!("aivyx-signal: {e}"),
            }
            continue;
        }

        if shutdown.is_cancelled() {
            return Ok(report);
        }
        let m = pending.pop_front().expect("pending is non-empty here");
        let sender = m.sender().to_string();
        let session = *sessions.entry(sender.clone()).or_default();
        let channel = SignalChannel::new(session, partition(&sender));
        let (text, notes) = turn_text(&m);
        let _ = transport.typing(&sender, false).await;
        let turn = agent.turn(Message::text(session, text), &channel);
        tokio::pin!(turn);
        let mut fatal: Option<String> = None;
        let mut stopped = false;
        // Off after a transient error: it could return at once on every
        // poll. The outer loop backs off after the turn.
        let mut polling = true;
        let mut backoff_after_turn = false;
        loop {
            tokio::select! {
                biased;
                _ = &mut turn => break,
                _ = shutdown.cancelled() => {
                    channel.cancellation_token().cancel();
                    let _ = (&mut turn).await;
                    stopped = true;
                    break;
                }
                r = transport.next_event(), if polling => {
                    let event = match r {
                        Ok(e) => {
                            backoff = Duration::from_secs(1);
                            e
                        }
                        Err(e) if e.is_fatal() => {
                            fatal = Some(e.to_string());
                            channel.cancellation_token().cancel();
                            let _ = (&mut turn).await;
                            break;
                        }
                        Err(e) => {
                            eprintln!("aivyx-signal: {e}");
                            polling = false;
                            backoff_after_turn = true;
                            continue;
                        }
                    };
                    match route(transport, policy, &mut asked, event).await {
                        Ok(Routed::Turn(next)) if next.text.trim() == "/cancel" => {
                            channel.cancellation_token().cancel();
                        }
                        Ok(Routed::Turn(next)) => pending.push_back(next),
                        Ok(Routed::Handled) => {}
                        Err(e) if e.is_fatal() => {
                            fatal = Some(e.to_string());
                            channel.cancellation_token().cancel();
                            let _ = (&mut turn).await;
                            break;
                        }
                        Err(e) => eprintln!("aivyx-signal: {e}"),
                    }
                }
            }
        }
        let _ = transport.typing(&sender, true).await;
        report.turns_run += 1;
        if let Some(e) = fatal {
            return Err(e);
        }
        if stopped {
            return Ok(report);
        }
        if let Some(answer) = channel.take_reply() {
            match send_reply(transport, &sender, &answer, &notes).await {
                Ok(()) => {}
                Err(e) if e.is_fatal() => return Err(e.to_string()),
                Err(e) => eprintln!("aivyx-signal: couldn't send the reply: {e}"),
            }
        }
        if backoff_after_turn {
            eprintln!("aivyx-signal: retrying in {}s", backoff.as_secs());
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => return Ok(report),
                _ = tokio::time::sleep(backoff) => {}
            }
            backoff = (backoff * 2).min(Duration::from_secs(60));
        }
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn long_replies_split_at_line_breaks_within_the_limit() {
        assert_eq!(split_reply("short"), vec!["short"]);
        assert!(split_reply("  ").is_empty());
        let para = "x".repeat(1500);
        let text = format!("{para}\n{para}\n{para}");
        let parts = split_reply(&text);
        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|p| p.chars().count() <= MAX_PART && *p == para));
        // No break at all: hard cut at the limit, multibyte-safe.
        let parts = split_reply(&"é".repeat(4500));
        assert_eq!(parts.iter().map(|p| p.chars().count()).collect::<Vec<_>>(), vec![2000, 2000, 500]);
    }

    #[test]
    fn long_or_attached_input_is_noted() {
        let m = TextMessage {
            text: "é".repeat(3000),
            attachments: 1,
            ..Default::default()
        };
        let (text, notes) = turn_text(&m);
        assert!(text.len() <= MAX_INPUT);
        assert_eq!(notes, vec![CUT_NOTE.to_string(), ATTACHMENTS_NOTE.to_string()]);
        let short = TextMessage { text: "hi".into(), ..Default::default() };
        assert_eq!(turn_text(&short), ("hi".to_string(), vec![]));
    }

    #[test]
    fn partitions_are_scope_safe() {
        assert_eq!(partition("+15550001111"), "signal-15550001111");
        assert_eq!(partition("8f3c-AB"), "signal-8f3c-AB");
    }
}
