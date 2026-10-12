//! The mail loop both modes share, and the in-process session.
//!
//! [`run_mail_loop`] owns everything mode-independent: the per-instance
//! lock, the resume cursor, backoff, the trust check ([`screen`]) and the
//! reply ([`answer`]). A [`TurnRunner`] runs each approved email: the
//! agent here ([`run_proton_session_with_transport`]), or the daemon
//! (`aivyx_channel::proton_daemon_frontend`).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sha2::{Digest, Sha256};

use aivyx_capability::CapabilitySet;
use aivyx_core::{
    agent::ConcreteAgent, llm_planner::LlmPlanner, planner::ToolRegistry, Agent, AgentId,
    AuditHook, CancellationToken, ChannelContext, LlmPlannerConfig, Message, SessionId,
};
use aivyx_llm::LlmProvider;
use aivyx_storage::Storage;

use crate::message::{cap, without_quoted_history, MailMessage, TURN_TEXT_CAP};
use crate::outgoing::OutgoingMail;
use crate::policy::{decide, Decision, ProtonPolicy, Sender};
use crate::proton_channel::ProtonChannel;
use crate::transport::{MailTransport, TransportError};
use crate::{lock, state};

/// Added to the reply when the email was cut to [`TURN_TEXT_CAP`].
pub const CUT_NOTE: &str = "(Your email was longer than 32 KiB, so I read only the start.)";

/// Added to the reply when the email had attachments.
pub fn attachments_note(names: &[String]) -> String {
    format!("(I can't open attachments, so I ignored: {}.)", names.join(", "))
}

/// One approved email, ready to run.
#[derive(Debug, Clone)]
pub struct Turn {
    pub message: MailMessage,
    pub sender: Sender,
    /// What the agent reads: sender, subject and the new part of the body.
    pub text: String,
    /// The body's first non-empty line — where `/team …` is read.
    pub first_line: String,
    /// Said at the end of the reply (cut, attachments ignored).
    pub notes: Vec<String>,
}

impl Turn {
    /// The conversation key: the lowercased From and the thread's root
    /// `Message-ID`, so a sender who knows another thread's root (one of
    /// yours, say) still lands in a conversation — and memory partition —
    /// of their own.
    pub fn thread(&self) -> String {
        let from = self.message.from.as_deref().unwrap_or("").to_ascii_lowercase();
        format!("{from}\n{}", self.message.thread_root())
    }
}

/// The trust check (`policy::decide`, consulting Sent for mail claiming to
/// be from you). `Ok(None)`: ignored silently.
pub async fn screen<T: MailTransport + ?Sized>(
    transport: &T,
    policy: &ProtonPolicy,
    m: MailMessage,
) -> Result<Option<Turn>, TransportError> {
    let in_sent = match (policy.claims_operator(&m), m.internal_id.as_deref()) {
        (true, Some(id)) => transport.in_sent(id).await?,
        _ => false,
    };
    let sender = match decide(policy, &m, in_sent) {
        Decision::Turn(s) => s,
        Decision::Ignore(_) => return Ok(None),
    };
    let body = without_quoted_history(&m.text);
    let (body, cut) = cap(&body, TURN_TEXT_CAP);
    let mut notes = Vec::new();
    if cut {
        notes.push(CUT_NOTE.to_string());
    }
    if !m.attachments.is_empty() {
        notes.push(attachments_note(&m.attachments));
    }
    let first_line = body
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string();
    let text = format!(
        "Email from {}\nSubject: {}\n\n{body}",
        m.from.as_deref().unwrap_or(""),
        m.subject
    );
    Ok(Some(Turn {
        message: m,
        sender,
        text,
        first_line,
        notes,
    }))
}

pub fn reply_body(answer: &str, notes: &[String]) -> String {
    if notes.is_empty() {
        answer.to_string()
    } else {
        format!("{answer}\n\n{}", notes.join("\n"))
    }
}

/// Emails `answer` to the sender from `channel_address`, in the thread,
/// then files the original with a copy of the reply. `Err` only when the
/// send failed (the original then stays unread in the inbox).
pub async fn answer<T: MailTransport + ?Sized>(
    transport: &T,
    policy: &ProtonPolicy,
    turn: &Turn,
    answer: &str,
) -> Result<(), TransportError> {
    let to = turn.message.from.clone().into_iter().collect();
    let mut reply = OutgoingMail::reply(
        &policy.channel_address,
        to,
        &turn.message,
        reply_body(answer, &turn.notes),
    );
    reply.auto_submitted = true;
    transport.send(&reply).await?;
    if let Err(e) = transport.file_handled(turn.message.uid, &reply).await {
        eprintln!("aivyx-proton: replied, but couldn't file the email: {e}");
    }
    Ok(())
}

/// Runs one approved email.
#[async_trait]
pub trait TurnRunner: Send {
    /// `Ok(Some(text))`: the reply. `Ok(None)`: no reply (the email stays
    /// in the inbox). `Err`: the runner can't go on; the loop stops.
    async fn run(&mut self, turn: &Turn) -> Result<Option<String>, String>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProtonSessionReport {
    pub turns_run: usize,
    pub answered: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopError {
    /// Bridge rejected the login or its certificate changed, or the
    /// channel can't start (another one is running): stop.
    Fatal(String),
    /// The runner failed (e.g. the daemon went away): the binary may fall
    /// back to in-process.
    Runner(String),
}

impl std::fmt::Display for LoopError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoopError::Fatal(e) | LoopError::Runner(e) => f.write_str(e),
        }
    }
}

/// Times one email may fail to be checked (`screen`, in practice the Sent
/// lookup) in a row before it is skipped.
const MAX_ATTEMPTS: u32 = 5;

fn show(d: Duration) -> String {
    if d >= Duration::from_secs(1) {
        format!("{}s", d.as_secs())
    } else {
        format!("{}ms", d.as_millis())
    }
}

/// The receive loop. `state_dir` is the instance's `proton_dir()`: it
/// holds the lock and the cursor.
pub async fn run_mail_loop<T: MailTransport + ?Sized, R: TurnRunner>(
    transport: &T,
    policy: &ProtonPolicy,
    state_dir: &Path,
    runner: &mut R,
    shutdown: CancellationToken,
) -> Result<ProtonSessionReport, LoopError> {
    run_mail_loop_with(transport, policy, state_dir, runner, shutdown, Duration::from_secs(1)).await
}

/// [`run_mail_loop`] with the first retry delay given (tests pass 1 ms;
/// it doubles per retry, up to 60 s).
pub(crate) async fn run_mail_loop_with<T: MailTransport + ?Sized, R: TurnRunner>(
    transport: &T,
    policy: &ProtonPolicy,
    state_dir: &Path,
    runner: &mut R,
    shutdown: CancellationToken,
    first_backoff: Duration,
) -> Result<ProtonSessionReport, LoopError> {
    let _lock = lock::acquire(state_dir).map_err(LoopError::Fatal)?;
    let mut report = ProtonSessionReport::default();
    let mut backoff = first_backoff;
    // Bridge may not be up yet: retry anything but a fatal error.
    let status = loop {
        match transport.inbox_status().await {
            Ok(status) => break status,
            Err(e) if e.is_fatal() => return Err(LoopError::Fatal(e.to_string())),
            Err(e) => {
                eprintln!("aivyx-proton: {e}; retrying in {}", show(backoff));
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => return Ok(report),
                    _ = tokio::time::sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
        }
    };
    backoff = first_backoff;
    let mut cursor = state::resume(state::load(state_dir).map_err(LoopError::Fatal)?, status);
    state::save(state_dir, &cursor).map_err(LoopError::Fatal)?;
    // Consecutive failed checks of one email: (UID, attempts).
    let mut failing: (u32, u32) = (0, 0);

    loop {
        let batch = tokio::select! {
            biased;
            _ = shutdown.cancelled() => return Ok(report),
            r = transport.wait_for_new(cursor.last_uid) => r,
        };
        let mut transient: Option<TransportError> = None;
        match batch {
            Ok(batch) => {
                let after = cursor.last_uid;
                for m in batch.into_iter().filter(|m| m.uid > after) {
                    let uid = m.uid;
                    match screen(transport, policy, m).await {
                        Ok(Some(turn)) => {
                            failing = (0, 0);
                            let outcome = runner.run(&turn).await.map_err(LoopError::Runner)?;
                            if outcome.is_none() && shutdown.is_cancelled() {
                                // Stopped mid-turn: don't answer and leave the
                                // cursor, so a restart handles it again. (A turn
                                // that finished — the daemon's always do — is
                                // answered below, then the loop stops.)
                                return Ok(report);
                            }
                            if let Some(text) = outcome {
                                match answer(transport, policy, &turn, &text).await {
                                    Ok(()) => report.answered += 1,
                                    Err(e) if e.is_fatal() => {
                                        cursor.last_uid = cursor.last_uid.max(uid);
                                        let _ = state::save(state_dir, &cursor);
                                        return Err(LoopError::Fatal(e.to_string()));
                                    }
                                    Err(e) => eprintln!(
                                        "aivyx-proton: couldn't send the reply ({e}); the email \
                                         stays unread in your inbox"
                                    ),
                                }
                            }
                            report.turns_run += 1;
                        }
                        Ok(None) => failing = (0, 0),
                        Err(e) if e.is_fatal() => return Err(LoopError::Fatal(e.to_string())),
                        Err(e) => {
                            let tries = if failing.0 == uid { failing.1 + 1 } else { 1 };
                            if tries < MAX_ATTEMPTS {
                                // Retried after the backoff, not skipped.
                                failing = (uid, tries);
                                transient = Some(e);
                                break;
                            }
                            // Don't let one email stall the channel: skip it.
                            eprintln!(
                                "aivyx-proton: giving up on message UID {uid} after {tries} tries: {e}"
                            );
                            failing = (0, 0);
                        }
                    }
                    cursor.last_uid = cursor.last_uid.max(uid);
                    state::save(state_dir, &cursor).map_err(LoopError::Fatal)?;
                    if shutdown.is_cancelled() {
                        return Ok(report);
                    }
                }
            }
            Err(e) if e.is_fatal() => return Err(LoopError::Fatal(e.to_string())),
            Err(e) => transient = Some(e),
        }
        if transient.is_none() {
            backoff = first_backoff;
        }
        if let Some(e) = transient {
            eprintln!("aivyx-proton: {e}; retrying in {}", show(backoff));
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => return Ok(report),
                _ = tokio::time::sleep(backoff) => {}
            }
            backoff = (backoff * 2).min(Duration::from_secs(60));
        }
    }
}

/// Same fields and meaning as `aivyx_matrix::MatrixSessionConfig`.
#[derive(Clone)]
pub struct ProtonSessionConfig {
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

fn build_agent(
    config: ProtonSessionConfig,
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

/// A thread's memory namespace: a short hash of its [`Turn::thread`] key
/// (sender and root `Message-ID`, which carry `@`, `<`, `>` and other
/// characters scope strings shouldn't).
pub fn partition(thread: &str) -> String {
    let digest = Sha256::digest(thread.as_bytes());
    format!("proton-{}", &hex::encode(digest.as_slice())[..16])
}

/// Threads remembered at once; past this the map starts over (a thread
/// then simply begins a fresh conversation).
const MAX_THREADS: usize = 256;

/// One conversation (`SessionId`) per email thread.
#[derive(Default)]
struct Threads(HashMap<String, SessionId>);

impl Threads {
    fn session_for(&mut self, root: &str) -> SessionId {
        if self.0.len() >= MAX_THREADS && !self.0.contains_key(root) {
            self.0.clear();
        }
        // `SessionId::default()` is a fresh random id.
        *self.0.entry(root.to_string()).or_default()
    }
}

struct AgentRunner<A> {
    agent: A,
    threads: Threads,
    shutdown: CancellationToken,
}

#[async_trait]
impl<A: Agent> TurnRunner for AgentRunner<A> {
    async fn run(&mut self, turn: &Turn) -> Result<Option<String>, String> {
        let root = turn.thread();
        let session = self.threads.session_for(&root);
        let channel = ProtonChannel::new(session, partition(&root));
        let fut = self.agent.turn(Message::text(session, &turn.text), &channel);
        tokio::pin!(fut);
        tokio::select! {
            biased;
            _ = &mut fut => Ok(channel.take_reply()),
            _ = self.shutdown.cancelled() => {
                // Stopping: let the turn wind down, send nothing.
                channel.cancellation_token().cancel();
                let _ = (&mut fut).await;
                Ok(None)
            }
        }
    }
}

/// The in-process session over `transport`. Production passes the
/// instance's `proton_dir()` as `state_dir` and the one
/// `BridgeTransport` built for the run.
#[allow(clippy::too_many_arguments)]
pub async fn run_proton_session_with_transport<T: MailTransport + ?Sized>(
    transport: &T,
    policy: &ProtonPolicy,
    state_dir: &Path,
    config: ProtonSessionConfig,
    provider: Arc<dyn LlmProvider>,
    audit: Arc<dyn AuditHook>,
    checkpointer: Option<Arc<aivyx_core::GitCheckpointer>>,
    shutdown: CancellationToken,
) -> Result<ProtonSessionReport, LoopError> {
    let mut runner = AgentRunner {
        agent: build_agent(config, provider, audit, checkpointer),
        threads: Threads::default(),
        shutdown: shutdown.clone(),
    };
    run_mail_loop(transport, policy, state_dir, &mut runner, shutdown).await
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn partitions_are_stable_short_and_scope_safe() {
        let p = partition("<root@proton.me>");
        assert_eq!(p, partition("<root@proton.me>"));
        assert_ne!(p, partition("<other@proton.me>"));
        assert_eq!(p.len(), "proton-".len() + 16);
        assert!(p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
    }

    #[test]
    fn a_thread_belongs_to_its_sender() {
        let turn = |from: &str| Turn {
            message: MailMessage {
                uid: 1,
                from: Some(from.into()),
                references: vec!["root@proton.me".into()],
                ..Default::default()
            },
            sender: Sender::Operator,
            text: String::new(),
            first_line: String::new(),
            notes: vec![],
        };
        let (mine, theirs) = (turn("me@proton.me"), turn("friend@example.com"));
        assert_eq!(mine.thread(), turn("Me@Proton.me").thread());
        assert!(mine.thread().contains("root@proton.me"));
        assert_ne!(mine.thread(), theirs.thread());
        assert_ne!(partition(&mine.thread()), partition(&theirs.thread()));
    }

    #[test]
    fn one_thread_one_session() {
        let mut t = Threads::default();
        let a = t.session_for("a");
        assert_eq!(t.session_for("a"), a);
        assert_ne!(t.session_for("b"), a);
    }

    #[test]
    fn notes_follow_the_answer() {
        assert_eq!(reply_body("hi", &[]), "hi");
        assert_eq!(
            reply_body("hi", &[CUT_NOTE.to_string(), attachments_note(&["a.pdf".to_string()])]),
            format!("hi\n\n{CUT_NOTE}\n(I can't open attachments, so I ignored: a.pdf.)")
        );
    }
}
