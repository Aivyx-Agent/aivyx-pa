//! Daemon client — Phase 17 Task 4.
//!
//! Connects to a running daemon over the Unix domain socket,
//! manages a session, and supports multi-turn interaction. Includes
//! auto-spawn logic: if no daemon is listening, spawns one via
//! `aivyx-pa daemon run` and waits for the socket to appear.
//!
//! Phase 16 shipped the single-turn PoC (`run_poc_client`).
//! Phase 17 Task 2 added multi-turn on the server side.
//! Phase 17 Task 4 adds multi-turn on the client side plus
//! auto-spawn.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use std::sync::Arc;

use crate::daemon_ipc::{
    decode_frame, encode_frame, DaemonEnvelope, EffectivePersonaSummary, FrameError,
    FrontendMessage, FrontendType, IpcAttachment, MemoryEntrySummary,
    NotificationHistoryEntry, PersonaDeltaSummary, PersonaProposalResolution,
    PersonaProposalResolveSuccess, PersonaProposalSummary, QueryPayload,
    QueryResponsePayload, StreamEventPayload,
};
use crate::daemon_server::DaemonError;

/// Result of a single PoC daemon turn (Phase 16 shape, kept for
/// backward compatibility with the existing e2e test).
#[derive(Debug)]
pub struct DaemonTurnResult {
    pub session_id: String,
    pub events: Vec<StreamEventPayload>,
    pub outcome: String,
    pub daemon_version: Option<String>,
}

/// A connected, session-aware daemon client that supports multi-turn
/// interaction. Created by [`DaemonSession::connect`].
pub struct DaemonSession {
    reader: tokio::net::unix::OwnedReadHalf,
    writer: Arc<tokio::sync::Mutex<tokio::net::unix::OwnedWriteHalf>>,
    buf: Vec<u8>,
    pub session_id: String,
    pub daemon_version: Option<String>,
}

impl DaemonSession {
    /// Connect to a running daemon, read `DaemonReady`, send
    /// `StartSession`, and return a session handle ready for
    /// `submit_input` calls.
    pub async fn connect(
        socket_path: &Path,
        role: Option<String>,
        frontend_type: Option<FrontendType>,
    ) -> Result<Self, DaemonError> {
        let stream = UnixStream::connect(socket_path).await?;
        let (mut reader, writer) = stream.into_split();
        let mut buf = Vec::with_capacity(4096);

        // Read DaemonReady.
        read_more(&mut reader, &mut buf).await?;
        let daemon_version: Option<String> =
            match decode_frame::<DaemonEnvelope>(&buf) {
                Ok((DaemonEnvelope::DaemonReady { version }, consumed)) => {
                    buf.drain(..consumed);
                    Some(version)
                }
                Ok((other, _)) => {
                    return Err(DaemonError::Protocol(format!(
                        "expected DaemonReady, got {other:?}"
                    )));
                }
                Err(e) => return Err(e.into()),
            };

        // Send StartSession.
        let start = FrontendMessage::StartSession { role, frontend_type };
        let frame = encode_frame(&start)?;
        let writer = Arc::new(tokio::sync::Mutex::new(writer));
        {
            let mut w = writer.lock().await;
            w.write_all(&frame).await?;
        }

        // Read SessionStarted.
        let session_id: String = loop {
            match decode_frame::<DaemonEnvelope>(&buf) {
                Ok((
                    DaemonEnvelope::SessionStarted { session_id: sid },
                    consumed,
                )) => {
                    buf.drain(..consumed);
                    break sid;
                }
                // The daemon delivers a take-once `RecoveryNotice`
                // between `DaemonReady` and `SessionStarted` to the
                // first frontend that connects after it restarted with
                // stale state (a previous instance that crashed / was
                // killed rather than shut down cleanly). It is
                // informational — skip past it and keep waiting for
                // `SessionStarted`, surfacing it so the operator knows
                // a prior session/turn was lost. Without this arm the
                // first reconnect after an unclean shutdown fails for
                // every frontend (REPL, TUI, channels all share this).
                Ok((
                    DaemonEnvelope::RecoveryNotice {
                        lost_sessions,
                        lost_turns,
                        ..
                    },
                    consumed,
                )) => {
                    buf.drain(..consumed);
                    if !lost_sessions.is_empty() || !lost_turns.is_empty() {
                        eprintln!(
                            "aivyx-pa: daemon recovered from an unclean shutdown — \
                             {} session(s) and {} in-flight turn(s) were lost.",
                            lost_sessions.len(),
                            lost_turns.len(),
                        );
                    }
                }
                Ok((DaemonEnvelope::Error { code, message }, _)) => {
                    return Err(DaemonError::Protocol(format!(
                        "daemon error ({code}): {message}"
                    )));
                }
                Err(FrameError::IncompleteBuf) => {
                    read_more(&mut reader, &mut buf).await?;
                }
                Ok((other, _)) => {
                    return Err(DaemonError::Protocol(format!(
                        "expected SessionStarted, got {other:?}"
                    )));
                }
                Err(e) => return Err(e.into()),
            }
        };

        Ok(DaemonSession {
            reader,
            writer,
            buf,
            session_id,
            daemon_version,
        })
    }

    pub async fn submit_input_for_mission(
        &mut self,
        text: String,
        mission_id: String,
    ) -> Result<(Vec<StreamEventPayload>, String), DaemonError> {
        let submit = FrontendMessage::SubmitInput {
            session_id: self.session_id.clone(),
            text,
            mission_id: Some(mission_id),
            attachments: vec![],
            headless: false,
        };
        self.send_and_collect(submit).await
    }

    /// Submit a turn to the daemon and collect all streamed events
    /// until `TurnComplete`. Returns the events and the outcome string.
    pub async fn submit_input(
        &mut self,
        text: String,
    ) -> Result<(Vec<StreamEventPayload>, String), DaemonError> {
        let submit = FrontendMessage::SubmitInput {
            session_id: self.session_id.clone(),
            text,
            mission_id: None,
            attachments: vec![],
            headless: false,
        };
        self.send_and_collect(submit).await
    }

    /// Chapter H — submit an **unattended** turn: the daemon refuses (records
    /// the reason) at any approval gate rather than parking for an operator.
    /// Used by `aivyx-pa --headless "<task>"`.
    pub async fn submit_input_headless(
        &mut self,
        text: String,
    ) -> Result<(Vec<StreamEventPayload>, String), DaemonError> {
        let submit = FrontendMessage::SubmitInput {
            session_id: self.session_id.clone(),
            text,
            mission_id: None,
            attachments: vec![],
            headless: true,
        };
        self.send_and_collect(submit).await
    }

    /// Submit a turn with image attachments. Phase 45 multimodal path.
    pub async fn submit_input_with_attachments(
        &mut self,
        text: String,
        attachments: Vec<IpcAttachment>,
    ) -> Result<(Vec<StreamEventPayload>, String), DaemonError> {
        let submit = FrontendMessage::SubmitInput {
            session_id: self.session_id.clone(),
            text,
            mission_id: None,
            attachments,
            headless: false,
        };
        self.send_and_collect(submit).await
    }

    async fn send_and_collect(
        &mut self,
        msg: FrontendMessage,
    ) -> Result<(Vec<StreamEventPayload>, String), DaemonError> {
        let frame = encode_frame(&msg)?;
        {
            let mut w = self.writer.lock().await;
            w.write_all(&frame).await?;
        }

        let mut events = Vec::new();
        loop {
            match decode_frame::<DaemonEnvelope>(&self.buf) {
                Ok((DaemonEnvelope::StreamEvent { event, .. }, consumed)) => {
                    self.buf.drain(..consumed);
                    events.push(event);
                }
                Ok((DaemonEnvelope::TurnComplete { outcome, .. }, consumed)) => {
                    self.buf.drain(..consumed);
                    return Ok((events, outcome));
                }
                Ok((DaemonEnvelope::Error { code, message }, _)) => {
                    return Err(DaemonError::Protocol(format!(
                        "daemon error ({code}): {message}"
                    )));
                }
                Ok((DaemonEnvelope::ShuttingDown { reason }, _)) => {
                    return Err(DaemonError::Protocol(format!(
                        "daemon shutting down: {reason}"
                    )));
                }
                Err(FrameError::IncompleteBuf) => {
                    read_more(&mut self.reader, &mut self.buf).await?;
                }
                Ok((other, consumed)) => {
                    self.buf.drain(..consumed);
                    return Err(DaemonError::Protocol(format!(
                        "unexpected message during turn: {other:?}"
                    )));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Send `CancelTurn` to request cancellation of the in-flight turn.
    pub async fn cancel_turn(&mut self) -> Result<(), DaemonError> {
        let cancel = FrontendMessage::CancelTurn {
            session_id: self.session_id.clone(),
        };
        let frame = encode_frame(&cancel)?;
        let mut w = self.writer.lock().await;
        w.write_all(&frame).await?;
        Ok(())
    }

    /// Send `ResolveGate` and wait for `GateResolved` (or `Error`).
    pub async fn resolve_gate(
        &mut self,
        mission_id: String,
        gate_id: String,
        approved: bool,
    ) -> Result<(), DaemonError> {
        let msg = FrontendMessage::ResolveGate {
            mission_id,
            gate_id,
            approved,
        };
        let frame = encode_frame(&msg)?;
        {
            let mut w = self.writer.lock().await;
            w.write_all(&frame).await?;
        }

        loop {
            match decode_frame::<DaemonEnvelope>(&self.buf) {
                Ok((DaemonEnvelope::GateResolved { .. }, consumed)) => {
                    self.buf.drain(..consumed);
                    return Ok(());
                }
                Ok((DaemonEnvelope::Error { code, message }, _)) => {
                    return Err(DaemonError::Protocol(format!(
                        "gate resolve error ({code}): {message}"
                    )));
                }
                Ok((DaemonEnvelope::ShuttingDown { reason }, _)) => {
                    return Err(DaemonError::Protocol(format!(
                        "daemon shutting down: {reason}"
                    )));
                }
                Err(FrameError::IncompleteBuf) => {
                    read_more(&mut self.reader, &mut self.buf).await?;
                }
                Ok((other, consumed)) => {
                    self.buf.drain(..consumed);
                    return Err(DaemonError::Protocol(format!(
                        "unexpected message during ResolveGate: {other:?}"
                    )));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Return a cloneable cancel handle for use from a signal handler.
    pub fn cancel_handle(&self) -> DaemonCancelHandle {
        DaemonCancelHandle {
            writer: Arc::clone(&self.writer),
            session_id: self.session_id.clone(),
        }
    }

    /// Send `Disconnect` and drop the connection cleanly.
    pub async fn disconnect(self) -> Result<(), DaemonError> {
        let frame = encode_frame(&FrontendMessage::Disconnect)?;
        let mut w = self.writer.lock().await;
        w.write_all(&frame).await?;
        Ok(())
    }
}

/// A cloneable handle for sending `CancelTurn` from a signal handler
/// without holding `&mut DaemonSession`. Created by
/// [`DaemonSession::cancel_handle`].
#[derive(Clone)]
pub struct DaemonCancelHandle {
    writer: Arc<tokio::sync::Mutex<tokio::net::unix::OwnedWriteHalf>>,
    session_id: String,
}

impl DaemonCancelHandle {
    /// Send `CancelTurn` to the daemon. Safe to call from any task.
    pub async fn cancel(&self) {
        let cancel = FrontendMessage::CancelTurn {
            session_id: self.session_id.clone(),
        };
        if let Ok(frame) = encode_frame(&cancel) {
            let mut w = self.writer.lock().await;
            let _ = w.write_all(&frame).await;
        }
    }
}

/// Check whether a daemon is listening at the given socket path.
/// Returns `true` if a connection succeeds, `false` otherwise.
pub async fn daemon_is_running(socket_path: &Path) -> bool {
    UnixStream::connect(socket_path).await.is_ok()
}

/// Result of a `daemon status` probe.
#[derive(Debug)]
pub struct DaemonStatusInfo {
    pub running: bool,
    pub version: Option<String>,
    pub pid: Option<u32>,
}

/// Read the PID from a daemon PID file, if it exists and contains a
/// valid u32. Returns `None` if the file is missing, empty, or
/// contains non-numeric data.
pub fn read_pid_file(pid_path: &Path) -> Option<u32> {
    std::fs::read_to_string(pid_path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

/// Probe a running daemon: connect, read `DaemonReady`, disconnect.
/// Returns status info without starting a session. The PID is read
/// from the sibling `.pid` file if it exists.
pub async fn daemon_status(socket_path: &Path) -> DaemonStatusInfo {
    let pid_path = socket_path.with_extension("pid");
    let pid = read_pid_file(&pid_path);

    let stream = match UnixStream::connect(socket_path).await {
        Ok(s) => s,
        Err(_) => return DaemonStatusInfo { running: false, version: None, pid },
    };
    let (mut reader, _writer) = stream.into_split();
    let mut buf = Vec::with_capacity(4096);
    if read_more(&mut reader, &mut buf).await.is_err() {
        return DaemonStatusInfo { running: true, version: None, pid };
    }
    match decode_frame::<DaemonEnvelope>(&buf) {
        Ok((DaemonEnvelope::DaemonReady { version }, _)) => {
            DaemonStatusInfo { running: true, version: Some(version), pid }
        }
        _ => DaemonStatusInfo { running: true, version: None, pid },
    }
}

/// Send `Shutdown` to a running daemon and wait for the `ShuttingDown`
/// lifecycle event. Returns the shutdown reason on success.
pub async fn daemon_stop(socket_path: &Path) -> Result<String, DaemonError> {
    let stream = UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();
    let mut buf = Vec::with_capacity(4096);

    // Read DaemonReady.
    read_more(&mut reader, &mut buf).await?;
    match decode_frame::<DaemonEnvelope>(&buf) {
        Ok((DaemonEnvelope::DaemonReady { .. }, consumed)) => {
            buf.drain(..consumed);
        }
        Ok((other, _)) => {
            return Err(DaemonError::Protocol(format!(
                "expected DaemonReady, got {other:?}"
            )));
        }
        Err(e) => return Err(e.into()),
    }

    // Send Shutdown.
    let frame = encode_frame(&FrontendMessage::Shutdown)?;
    writer.write_all(&frame).await?;

    // Wait for ShuttingDown.
    loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::ShuttingDown { reason }, _)) => {
                return Ok(reason);
            }
            Ok((other, consumed)) => {
                buf.drain(..consumed);
                return Err(DaemonError::Protocol(format!(
                    "expected ShuttingDown, got {other:?}"
                )));
            }
            Err(FrameError::IncompleteBuf) => {
                read_more(&mut reader, &mut buf).await?;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Phase 60 — fetch the daemon's current effective Persona snapshot
/// over IPC. Used by `aivyx-pa persona show` and by the Web UI's
/// Persona pane.
pub async fn get_effective_persona(
    socket_path: &Path,
) -> Result<EffectivePersonaSummary, DaemonError> {
    let payload = send_query(socket_path, "p-show", QueryPayload::GetEffectivePersona).await?;
    match payload {
        QueryResponsePayload::GetEffectivePersona { persona } => Ok(persona),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected GetEffectivePersona, got {other:?}"
        ))),
    }
}

/// Phase 60 — paginated read of the persona delta chain over IPC.
/// `from_seq` is zero-based; `limit` is capped server-side at 500.
pub async fn list_persona_deltas(
    socket_path: &Path,
    from_seq: u64,
    limit: u32,
) -> Result<(Vec<PersonaDeltaSummary>, u64), DaemonError> {
    let payload = send_query(
        socket_path,
        "p-list",
        QueryPayload::ListPersonaDeltas { from_seq, limit },
    )
    .await?;
    match payload {
        QueryResponsePayload::ListPersonaDeltas { entries, total_len } => Ok((entries, total_len)),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected ListPersonaDeltas, got {other:?}"
        ))),
    }
}

/// Phase 74 — list every distinct memory topic over IPC.
pub async fn list_memory_topics(
    socket_path: &Path,
) -> Result<Vec<String>, DaemonError> {
    let payload =
        send_query(socket_path, "m-topics", QueryPayload::ListMemoryTopics)
            .await?;
    match payload {
        QueryResponsePayload::ListMemoryTopics { topics } => Ok(topics),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected ListMemoryTopics, got {other:?}"
        ))),
    }
}

/// Chapter Codex — list the synthesized knowledge-wiki pages (compact rows).
pub async fn list_wiki_pages(
    socket_path: &Path,
) -> Result<Vec<aivyx_ipc::wiki::WikiPageSummary>, DaemonError> {
    let payload =
        send_query(socket_path, "m-wiki-list", QueryPayload::ListWikiPages).await?;
    match payload {
        QueryResponsePayload::ListWikiPages { pages } => Ok(pages),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected ListWikiPages, got {other:?}"
        ))),
    }
}

/// Chapter Codex — fetch one topic's full knowledge-wiki page (`None` if absent).
pub async fn get_wiki_page(
    socket_path: &Path,
    topic: String,
) -> Result<Option<aivyx_ipc::wiki::WikiPage>, DaemonError> {
    let payload =
        send_query(socket_path, "m-wiki-get", QueryPayload::GetWikiPage { topic })
            .await?;
    match payload {
        QueryResponsePayload::GetWikiPage { page } => Ok(page),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected GetWikiPage, got {other:?}"
        ))),
    }
}

/// Chapter Lattice — fetch the typed knowledge graph (entities + triples).
pub async fn get_knowledge_graph(
    socket_path: &Path,
    limit: u32,
) -> Result<
    (Vec<aivyx_ipc::graph::GraphEntity>, Vec<aivyx_ipc::graph::GraphTriple>),
    DaemonError,
> {
    let payload = send_query(
        socket_path,
        "m-kgraph",
        QueryPayload::GetKnowledgeGraph { limit },
    )
    .await?;
    match payload {
        QueryResponsePayload::GetKnowledgeGraph { entities, edges } => {
            Ok((entities, edges))
        }
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected GetKnowledgeGraph, got {other:?}"
        ))),
    }
}

/// Chapter Concord — run the on-demand contradiction detection pass and
/// return the conflicts (empty when none, or when the daemon has no LLM /
/// memory).
pub async fn get_memory_conflicts(
    socket_path: &Path,
) -> Result<Vec<aivyx_ipc::conflict::MemoryConflict>, DaemonError> {
    let payload =
        send_query(socket_path, "m-conflicts", QueryPayload::GetMemoryConflicts).await?;
    match payload {
        QueryResponsePayload::MemoryConflicts { conflicts } => Ok(conflicts),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected MemoryConflicts, got {other:?}"
        ))),
    }
}

/// Chapter Concord — resolve a conflict by deleting the losing entry
/// (`archive_seq` under `topic`). Returns whether an entry was removed
/// (`false` = it was already gone, an idempotent no-op).
/// Chapter Accord — run the on-demand Persona contradiction pass.
pub async fn get_soul_conflicts(
    socket_path: &Path,
) -> Result<Vec<aivyx_ipc::soul_conflict::SoulConflict>, DaemonError> {
    let payload =
        send_query(socket_path, "soul-conflicts", QueryPayload::GetSoulConflicts).await?;
    match payload {
        QueryResponsePayload::SoulConflicts { conflicts } => Ok(conflicts),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected SoulConflicts, got {other:?}"
        ))),
    }
}

/// Chapter Accord — resolve a Persona contradiction by removing the losing
/// facet `(category, value)`. Returns the new chain seq.
pub async fn resolve_soul_conflict(
    socket_path: &Path,
    category: &str,
    value: &str,
) -> Result<u64, DaemonError> {
    let stream = UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();
    let mut buf = Vec::with_capacity(1024);
    read_more(&mut reader, &mut buf).await?;
    match decode_frame::<DaemonEnvelope>(&buf) {
        Ok((DaemonEnvelope::DaemonReady { .. }, consumed)) => buf.drain(..consumed),
        Ok((other, _)) => {
            return Err(DaemonError::Protocol(format!(
                "expected DaemonReady, got {other:?}"
            )))
        }
        Err(e) => return Err(e.into()),
    };
    let req = FrontendMessage::ResolveSoulConflict {
        id: "soul-resolve".into(),
        category: category.to_string(),
        value: value.to_string(),
    };
    writer.write_all(&encode_frame(&req)?).await?;
    loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((
                DaemonEnvelope::SoulConflictResolved { ok, seq, error, .. },
                _,
            )) => {
                return if ok {
                    Ok(seq.unwrap_or(0))
                } else {
                    Err(DaemonError::Protocol(
                        error.unwrap_or_else(|| "resolve failed".into()),
                    ))
                };
            }
            Ok((DaemonEnvelope::RecoveryNotice { .. }, consumed)) => {
                buf.drain(..consumed);
            }
            Ok((other, consumed)) => {
                buf.drain(..consumed);
                return Err(DaemonError::Protocol(format!(
                    "expected SoulConflictResolved, got {other:?}"
                )));
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await?,
            Err(e) => return Err(e.into()),
        }
    }
}

pub async fn resolve_memory_conflict(
    socket_path: &Path,
    topic: &str,
    archive_seq: u64,
) -> Result<bool, DaemonError> {
    let stream = UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();
    let mut buf = Vec::with_capacity(1024);
    read_more(&mut reader, &mut buf).await?;
    match decode_frame::<DaemonEnvelope>(&buf) {
        Ok((DaemonEnvelope::DaemonReady { .. }, consumed)) => buf.drain(..consumed),
        Ok((other, _)) => {
            return Err(DaemonError::Protocol(format!(
                "expected DaemonReady, got {other:?}"
            )))
        }
        Err(e) => return Err(e.into()),
    };
    let req = FrontendMessage::ResolveMemoryConflict {
        id: "m-resolve".into(),
        topic: topic.to_string(),
        archive_seq,
    };
    writer.write_all(&encode_frame(&req)?).await?;
    loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((
                DaemonEnvelope::MemoryConflictResolved {
                    ok, removed, error, ..
                },
                _,
            )) => {
                return if ok {
                    Ok(removed)
                } else {
                    Err(DaemonError::Protocol(
                        error.unwrap_or_else(|| "resolve failed".into()),
                    ))
                };
            }
            Ok((DaemonEnvelope::RecoveryNotice { .. }, consumed)) => {
                buf.drain(..consumed);
            }
            Ok((other, consumed)) => {
                buf.drain(..consumed);
                return Err(DaemonError::Protocol(format!(
                    "expected MemoryConflictResolved, got {other:?}"
                )));
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await?,
            Err(e) => return Err(e.into()),
        }
    }
}

/// Chapter Concord — dismiss a conflict as a false positive ("keep
/// both"): the daemon records `conflict_id` so future detection passes
/// suppress that pair. Nothing is deleted.
/// Chapter Accord — dismiss a Soul contradiction as a false positive.
pub async fn dismiss_soul_conflict(
    socket_path: &Path,
    conflict_id: &str,
) -> Result<(), DaemonError> {
    let stream = UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();
    let mut buf = Vec::with_capacity(1024);
    read_more(&mut reader, &mut buf).await?;
    match decode_frame::<DaemonEnvelope>(&buf) {
        Ok((DaemonEnvelope::DaemonReady { .. }, consumed)) => buf.drain(..consumed),
        Ok((other, _)) => {
            return Err(DaemonError::Protocol(format!(
                "expected DaemonReady, got {other:?}"
            )))
        }
        Err(e) => return Err(e.into()),
    };
    let req = FrontendMessage::DismissSoulConflict {
        id: "soul-dismiss".into(),
        conflict_id: conflict_id.to_string(),
    };
    writer.write_all(&encode_frame(&req)?).await?;
    loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::SoulConflictDismissed { ok, error, .. }, _)) => {
                return if ok {
                    Ok(())
                } else {
                    Err(DaemonError::Protocol(
                        error.unwrap_or_else(|| "dismiss failed".into()),
                    ))
                };
            }
            Ok((DaemonEnvelope::RecoveryNotice { .. }, consumed)) => {
                buf.drain(..consumed);
            }
            Ok((other, consumed)) => {
                buf.drain(..consumed);
                return Err(DaemonError::Protocol(format!(
                    "expected SoulConflictDismissed, got {other:?}"
                )));
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await?,
            Err(e) => return Err(e.into()),
        }
    }
}

pub async fn dismiss_memory_conflict(
    socket_path: &Path,
    conflict_id: &str,
) -> Result<(), DaemonError> {
    let stream = UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();
    let mut buf = Vec::with_capacity(1024);
    read_more(&mut reader, &mut buf).await?;
    match decode_frame::<DaemonEnvelope>(&buf) {
        Ok((DaemonEnvelope::DaemonReady { .. }, consumed)) => buf.drain(..consumed),
        Ok((other, _)) => {
            return Err(DaemonError::Protocol(format!(
                "expected DaemonReady, got {other:?}"
            )))
        }
        Err(e) => return Err(e.into()),
    };
    let req = FrontendMessage::DismissMemoryConflict {
        id: "m-dismiss".into(),
        conflict_id: conflict_id.to_string(),
    };
    writer.write_all(&encode_frame(&req)?).await?;
    loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((
                DaemonEnvelope::MemoryConflictDismissed { ok, error, .. },
                _,
            )) => {
                return if ok {
                    Ok(())
                } else {
                    Err(DaemonError::Protocol(
                        error.unwrap_or_else(|| "dismiss failed".into()),
                    ))
                };
            }
            Ok((DaemonEnvelope::RecoveryNotice { .. }, consumed)) => {
                buf.drain(..consumed);
            }
            Ok((other, consumed)) => {
                buf.drain(..consumed);
                return Err(DaemonError::Protocol(format!(
                    "expected MemoryConflictDismissed, got {other:?}"
                )));
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await?,
            Err(e) => return Err(e.into()),
        }
    }
}

/// Phase 74 — fetch up to `limit` entries for one topic.
pub async fn get_memory_topic_entries(
    socket_path: &Path,
    topic: &str,
    limit: u32,
) -> Result<Vec<MemoryEntrySummary>, DaemonError> {
    let payload = send_query(
        socket_path,
        "m-entries",
        QueryPayload::GetMemoryTopicEntries {
            topic: topic.to_string(),
            limit,
        },
    )
    .await?;
    match payload {
        QueryResponsePayload::GetMemoryTopicEntries { entries } => Ok(entries),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected GetMemoryTopicEntries, got {other:?}"
        ))),
    }
}

/// Phase 74 — substring search across topics + bodies over IPC.
/// Phase 75 — `semantic` requests the embedding-ranked path;
/// the returned bool is `fell_back_to_keyword` (the daemon
/// transparently downgraded to keyword).
pub async fn search_memory(
    socket_path: &Path,
    query: &str,
    limit: u32,
    semantic: bool,
) -> Result<(Vec<MemoryEntrySummary>, bool), DaemonError> {
    let payload = send_query(
        socket_path,
        "m-search",
        QueryPayload::SearchMemory {
            query: query.to_string(),
            limit,
            semantic,
        },
    )
    .await?;
    match payload {
        QueryResponsePayload::SearchMemory {
            matches,
            fell_back_to_keyword,
        } => Ok((matches, fell_back_to_keyword)),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected SearchMemory, got {other:?}"
        ))),
    }
}

/// Phase 78 — read-only learning-observability query. `None`
/// window → the daemon's default lookback.
pub async fn get_learning_insights(
    socket_path: &Path,
    window_secs: Option<u64>,
) -> Result<
    (
        crate::recall_insights::LearningDigest,
        Vec<crate::recall_insights::ProposalProvenance>,
        Option<crate::persona_context::PersonaSelectionStat>,
        Option<crate::proactive_detect::ProactiveStat>,
        Option<crate::persona_lifecycle::PersonaLifecycleStat>,
        Option<crate::helpfulness_ledger::AccumulatedHelpfulness>,
        Option<crate::cooccurrence_ledger::CooccurrencePatterns>,
        Option<crate::memory_recall::RecallClusterStat>,
        Option<crate::persona_consolidation::PersonaConsolidationStat>,
        Option<crate::correction_ledger::AccumulatedCorrections>,
        Option<
            crate::correction_consolidation::CorrectionConsolidationStat,
        >,
        Option<crate::correction_judgment::CorrectionJudgmentStat>,
        Option<crate::recall_judgment::RecallJudgmentStat>,
        Vec<(
            String,
            crate::reflection_scheduler::RecentReflectionStat,
        )>,
    ),
    DaemonError,
> {
    let payload = send_query(
        socket_path,
        "l-insights",
        QueryPayload::GetLearningInsights { window_secs },
    )
    .await?;
    match payload {
        QueryResponsePayload::LearningInsights {
            digest,
            proposals,
            persona_selection,
            proactive,
            persona_lifecycle,
            accumulated_helpfulness,
            cooccurrence,
            cluster_recall,
            persona_consolidation,
            accumulated_corrections,
            correction_consolidation,
            correction_judgment,
            recall_judgment,
            cadence,
        } => Ok((
            digest,
            proposals,
            persona_selection,
            proactive,
            persona_lifecycle,
            accumulated_helpfulness,
            cooccurrence,
            cluster_recall,
            persona_consolidation,
            accumulated_corrections,
            correction_consolidation,
            correction_judgment,
            recall_judgment,
            cadence,
        )),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected LearningInsights, got {other:?}"
        ))),
    }
}

/// Phase 102 — fetch per-tool observability stats from the daemon.
/// Backs `aivyx-pa tools [--window <secs>]`. `window_secs = None`
/// scopes the answer to the whole audit chain.
pub async fn get_tool_stats(
    socket_path: &Path,
    window_secs: Option<u64>,
) -> Result<Vec<crate::daemon_ipc::ToolStat>, DaemonError> {
    let payload = send_query(
        socket_path,
        "t-stats",
        QueryPayload::GetToolStats { window_secs },
    )
    .await?;
    match payload {
        QueryResponsePayload::ToolStats { tools } => Ok(tools),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected ToolStats, got {other:?}"
        ))),
    }
}

/// Phase 186 — fetch every pending reminder for the TUI Dashboard's
/// reminders panel. Mirrors `get_tool_stats`'s shape.
pub async fn get_reminders(
    socket_path: &Path,
) -> Result<Vec<crate::daemon_ipc::ReminderView>, DaemonError> {
    let payload = send_query(socket_path, "reminders", QueryPayload::GetReminders).await?;
    match payload {
        QueryResponsePayload::Reminders { reminders } => Ok(reminders),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected Reminders, got {other:?}"
        ))),
    }
}

/// Model routing Part 3b (A15) — allow cloud escalation for one
/// conversation on the running daemon. `Ok(true)` when recorded,
/// `Ok(false)` when the daemon has no cloud escalation configured.
pub async fn allow_cloud_escalation(
    socket_path: &Path,
    session_id: &str,
) -> Result<bool, DaemonError> {
    let payload = send_query(
        socket_path,
        "allow-cloud",
        QueryPayload::AllowCloudEscalation {
            session_id: session_id.to_string(),
        },
    )
    .await?;
    match payload {
        QueryResponsePayload::CloudEscalationAllowed { .. } => Ok(true),
        QueryResponsePayload::CloudEscalationNotEnabled => Ok(false),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected CloudEscalationAllowed, got {other:?}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Phase 173 — autonomous loop control
// ---------------------------------------------------------------------------

/// Add a story to the autonomous-loop backlog. Returns the new
/// story's id.
pub async fn loop_add(
    socket_path: &Path,
    title: String,
    body: String,
    priority: Option<u32>,
) -> Result<String, DaemonError> {
    let payload = send_query(
        socket_path,
        "loop-add",
        QueryPayload::LoopAdd {
            title,
            body,
            priority,
        },
    )
    .await?;
    match payload {
        QueryResponsePayload::LoopStoryAdded { story_id } => Ok(story_id),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected LoopStoryAdded, got {other:?}"
        ))),
    }
}

/// List every backlog story (all statuses).
pub async fn loop_list(
    socket_path: &Path,
) -> Result<Vec<crate::loop_backlog::Story>, DaemonError> {
    let payload =
        send_query(socket_path, "loop-list", QueryPayload::LoopList).await?;
    match payload {
        QueryResponsePayload::LoopBacklog { stories } => Ok(stories),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected LoopBacklog, got {other:?}"
        ))),
    }
}

/// Start an autonomous-loop run. Returns `(ok, message)`.
pub async fn loop_start(
    socket_path: &Path,
    max_iterations: Option<u32>,
) -> Result<(bool, String), DaemonError> {
    let payload = send_query(
        socket_path,
        "loop-start",
        QueryPayload::LoopStart { max_iterations },
    )
    .await?;
    match payload {
        QueryResponsePayload::LoopControl { ok, message } => Ok((ok, message)),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected LoopControl, got {other:?}"
        ))),
    }
}

/// Request the active loop run to stop. Returns `(ok, message)`.
pub async fn loop_stop(
    socket_path: &Path,
) -> Result<(bool, String), DaemonError> {
    let payload =
        send_query(socket_path, "loop-stop", QueryPayload::LoopStop).await?;
    match payload {
        QueryResponsePayload::LoopControl { ok, message } => Ok((ok, message)),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected LoopControl, got {other:?}"
        ))),
    }
}

/// Read the loop run state + remaining backlog. Returns
/// `(state, remaining, armed, gate_enabled, max_run_secs,
/// max_run_tokens, max_run_usd, max_idle_iterations)`.
#[allow(clippy::type_complexity)]
pub async fn loop_status(
    socket_path: &Path,
) -> Result<
    (
        crate::loop_driver::LoopRunState,
        usize,
        bool,
        bool,
        Option<u64>,
        Option<u64>,
        Option<f64>,
        u32,
    ),
    DaemonError,
> {
    let payload =
        send_query(socket_path, "loop-status", QueryPayload::LoopStatus)
            .await?;
    match payload {
        QueryResponsePayload::LoopStatus {
            state,
            remaining,
            armed,
            gate_enabled,
            max_run_secs,
            max_run_tokens,
            max_run_usd,
            max_idle_iterations,
        } => Ok((
            state,
            remaining,
            armed,
            gate_enabled,
            max_run_secs,
            max_run_tokens,
            max_run_usd,
            max_idle_iterations,
        )),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected LoopStatus, got {other:?}"
        ))),
    }
}

/// Read the recent loop progress-log notes (most-recent-first).
pub async fn loop_log(
    socket_path: &Path,
    limit: Option<u32>,
) -> Result<Vec<String>, DaemonError> {
    let payload = send_query(
        socket_path,
        "loop-log",
        QueryPayload::LoopLog { limit },
    )
    .await?;
    match payload {
        QueryResponsePayload::LoopProgressLog { notes } => Ok(notes),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected LoopProgressLog, got {other:?}"
        ))),
    }
}

/// Mark a pending backlog story `Skipped`. Returns
/// `(ok, message)`.
pub async fn loop_skip(
    socket_path: &Path,
    story_id: String,
) -> Result<(bool, String), DaemonError> {
    let payload = send_query(
        socket_path,
        "loop-skip",
        QueryPayload::LoopSkip { story_id },
    )
    .await?;
    match payload {
        QueryResponsePayload::LoopControl { ok, message } => Ok((ok, message)),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected LoopControl, got {other:?}"
        ))),
    }
}

/// Chapter L (L.5) — start a daemon-run team mission from an explicit plan.
/// Returns the new mission id (the drive runs in the background).
pub async fn team_run(
    socket_path: &Path,
    plan: aivyx_team::MissionPlan,
    config: Option<aivyx_team::TeamConfig>,
) -> Result<String, DaemonError> {
    let payload =
        send_query(socket_path, "team-run", QueryPayload::TeamRun { plan, config }).await?;
    match payload {
        QueryResponsePayload::TeamRunStarted { mission_id } => Ok(mission_id),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected TeamRunStarted, got {other:?}"
        ))),
    }
}

/// Chapter L — start a daemon-run team mission from a free-text goal; the
/// daemon decomposes it into a plan and runs it. Returns the new mission id.
pub async fn team_run_goal(
    socket_path: &Path,
    goal: String,
    config: Option<aivyx_team::TeamConfig>,
) -> Result<String, DaemonError> {
    let payload = send_query(
        socket_path,
        "team-run-goal",
        QueryPayload::TeamRunGoal { goal, config },
    )
    .await?;
    match payload {
        QueryResponsePayload::TeamRunStarted { mission_id } => Ok(mission_id),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected TeamRunStarted, got {other:?}"
        ))),
    }
}

/// Chapter L (L.5) — every team mission's snapshot (the poll feed).
pub async fn team_mission_list(
    socket_path: &Path,
) -> Result<Vec<crate::team_mission::TeamMissionRecord>, DaemonError> {
    let payload =
        send_query(socket_path, "team-mission-list", QueryPayload::TeamMissionList)
            .await?;
    match payload {
        QueryResponsePayload::TeamMissionList { missions } => Ok(missions),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected TeamMissionList, got {other:?}"
        ))),
    }
}

/// `/classic` retirement (Task E) — paginated read of the audit chain
/// for the TUI's Audit view. Mirrors the Studio's own ListAuditEntries
/// query; the daemon caps `limit` at 500 server-side regardless of
/// what's requested here.
pub async fn list_audit_entries(
    socket_path: &Path,
    from_seq: u64,
    limit: u32,
) -> Result<(Vec<aivyx_ipc::protocol::AuditEntrySummary>, u64), DaemonError> {
    let payload = send_query(
        socket_path,
        "tui-audit",
        QueryPayload::ListAuditEntries { from_seq, limit },
    )
    .await?;
    match payload {
        QueryResponsePayload::ListAuditEntries { entries, total_len } => {
            Ok((entries, total_len))
        }
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected ListAuditEntries, got {other:?}"
        ))),
    }
}

/// Chapter L (L.5) — one team mission's snapshot, or `None` if unknown.
pub async fn team_mission_status(
    socket_path: &Path,
    mission_id: String,
) -> Result<Option<crate::team_mission::TeamMissionRecord>, DaemonError> {
    let payload = send_query(
        socket_path,
        "team-mission-status",
        QueryPayload::TeamMissionStatus { mission_id },
    )
    .await?;
    match payload {
        QueryResponsePayload::TeamMissionStatus { mission } => Ok(mission),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected TeamMissionStatus, got {other:?}"
        ))),
    }
}

/// Chapter L (L.5) — approve/reject a mission paused at a human gate. Returns
/// the phase the decision moved it to.
pub async fn resolve_team_gate(
    socket_path: &Path,
    mission_id: String,
    step: String,
    approve: bool,
) -> Result<crate::team_mission::TeamMissionPhase, DaemonError> {
    let payload = send_query(
        socket_path,
        "resolve-team-gate",
        QueryPayload::ResolveTeamGate { mission_id, step, approve },
    )
    .await?;
    match payload {
        QueryResponsePayload::TeamGateResolved { phase, .. } => Ok(phase),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected TeamGateResolved, got {other:?}"
        ))),
    }
}

/// Chapter Belay — request that a running team mission halt. Returns the
/// daemon's status message.
pub async fn abort_team_mission(
    socket_path: &Path,
    mission_id: String,
) -> Result<String, DaemonError> {
    let payload = send_query(
        socket_path,
        "abort-team-mission",
        QueryPayload::AbortTeamMission { mission_id },
    )
    .await?;
    match payload {
        QueryResponsePayload::TeamMissionAborted { message, .. } => Ok(message),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected TeamMissionAborted, got {other:?}"
        ))),
    }
}

/// Chapter Mission Control — request that a running team mission pause
/// (resumable, unlike abort). Returns the daemon's status message.
pub async fn pause_team_mission(
    socket_path: &Path,
    mission_id: String,
) -> Result<String, DaemonError> {
    let payload = send_query(
        socket_path,
        "pause-team-mission",
        QueryPayload::PauseTeamMission { mission_id },
    )
    .await?;
    match payload {
        QueryResponsePayload::TeamMissionPaused { message, .. } => Ok(message),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected TeamMissionPaused, got {other:?}"
        ))),
    }
}

/// Chapter Mission Control — resume a paused team mission. Returns the
/// phase the resume moved it to (always `Executing`).
pub async fn resume_team_mission(
    socket_path: &Path,
    mission_id: String,
) -> Result<crate::team_mission::TeamMissionPhase, DaemonError> {
    let payload = send_query(
        socket_path,
        "resume-team-mission",
        QueryPayload::ResumeTeamMission { mission_id },
    )
    .await?;
    match payload {
        QueryResponsePayload::TeamMissionResumed { phase, .. } => Ok(phase),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected TeamMissionResumed, got {other:?}"
        ))),
    }
}

/// Piece C (2026-08-23) — start a new team mission from a channel's
/// `/team run <goal>` command. Unlike the other one-shot
/// `team_mission_*` functions above (which use the anonymous `Query`
/// path via `send_query`), this function does its own `StartSession`
/// handshake first, declaring `frontend_type` — the daemon's
/// authorization check needs to know which real channel is asking,
/// which the anonymous `Query` path cannot provide (see the Piece C
/// plan's Global Constraints for the full rationale).
pub async fn run_team_mission_channel(
    socket_path: &Path,
    frontend_type: FrontendType,
    goal: String,
) -> Result<String, DaemonError> {
    let stream = UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();
    let mut buf = Vec::with_capacity(4096);

    read_more(&mut reader, &mut buf).await?;
    match decode_frame::<DaemonEnvelope>(&buf) {
        Ok((DaemonEnvelope::DaemonReady { .. }, consumed)) => buf.drain(..consumed),
        Ok((other, _)) => {
            return Err(DaemonError::Protocol(format!(
                "expected DaemonReady, got {other:?}"
            )))
        }
        Err(e) => return Err(e.into()),
    };

    let start = FrontendMessage::StartSession {
        role: None,
        frontend_type: Some(frontend_type),
    };
    let frame = encode_frame(&start)?;
    writer.write_all(&frame).await?;

    loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::SessionStarted { .. }, consumed)) => {
                buf.drain(..consumed);
                break;
            }
            Ok((DaemonEnvelope::RecoveryNotice { .. }, consumed)) => {
                buf.drain(..consumed);
            }
            Ok((other, _)) => {
                return Err(DaemonError::Protocol(format!(
                    "expected SessionStarted, got {other:?}"
                )))
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await?,
            Err(e) => return Err(e.into()),
        }
    }

    let req = FrontendMessage::RunTeamMissionChannel { goal };
    let frame = encode_frame(&req)?;
    writer.write_all(&frame).await?;

    loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::TeamMissionChannelStarted { mission_id }, _)) => {
                return Ok(mission_id)
            }
            Ok((DaemonEnvelope::Error { code, message }, _)) => {
                return Err(DaemonError::Protocol(format!("{code}: {message}")))
            }
            Ok((other, consumed)) => {
                buf.drain(..consumed);
                return Err(DaemonError::Protocol(format!(
                    "expected TeamMissionChannelStarted, got {other:?}"
                )));
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await?,
            Err(e) => return Err(e.into()),
        }
    }
}

/// Phase 74 — operator-initiated memory topic eviction over IPC.
/// Returns the number of entries deleted on success.
pub async fn evict_memory_topic(
    socket_path: &Path,
    topic: &str,
) -> Result<u64, DaemonError> {
    let stream = UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();
    let mut buf = Vec::with_capacity(4096);
    read_more(&mut reader, &mut buf).await?;
    match decode_frame::<DaemonEnvelope>(&buf) {
        Ok((DaemonEnvelope::DaemonReady { .. }, consumed)) => {
            buf.drain(..consumed);
        }
        Ok((other, _)) => {
            return Err(DaemonError::Protocol(format!(
                "expected DaemonReady, got {other:?}"
            )))
        }
        Err(e) => return Err(e.into()),
    }
    let req = FrontendMessage::EvictMemoryTopic {
        id: "ev-cli".into(),
        topic: topic.to_string(),
    };
    let frame = encode_frame(&req)?;
    writer.write_all(&frame).await?;
    loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((
                DaemonEnvelope::MemoryEvictResolved {
                    ok, deleted, error, ..
                },
                _,
            )) => {
                if ok {
                    return deleted.ok_or_else(|| {
                        DaemonError::Protocol(
                            "MemoryEvictResolved ok=true but deleted is None"
                                .into(),
                        )
                    });
                }
                return Err(DaemonError::Protocol(
                    error.unwrap_or_else(|| "memory evict failed".into()),
                ));
            }
            Ok((other, consumed)) => {
                buf.drain(..consumed);
                return Err(DaemonError::Protocol(format!(
                    "expected MemoryEvictResolved, got {other:?}"
                )));
            }
            Err(FrameError::IncompleteBuf) => {
                read_more(&mut reader, &mut buf).await?;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Phase 119 — operator-CLI ApplyProfileHint over IPC. Sends the
/// apply-record request after the CLI has already mutated
/// `aivyx-pa.toml` via the Task 3 atomic primitive; the daemon's job is
/// to record the `AuditEvent::ProfileHintApplied` entry. Returns the
/// error message on a daemon-side failure (audit log unconfigured,
/// chain append error, etc.); the caller decides whether to surface
/// the audit failure as a soft warning (the file mutation already
/// landed) or as a hard error.
pub async fn apply_profile_hint(
    socket_path: &Path,
    proposal_id: &str,
    field: &str,
    applied_value: &str,
) -> Result<(), DaemonError> {
    let stream = UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();
    let mut buf = Vec::with_capacity(4096);
    read_more(&mut reader, &mut buf).await?;
    match decode_frame::<DaemonEnvelope>(&buf) {
        Ok((DaemonEnvelope::DaemonReady { .. }, consumed)) => {
            buf.drain(..consumed);
        }
        Ok((other, _)) => {
            return Err(DaemonError::Protocol(format!(
                "expected DaemonReady, got {other:?}"
            )))
        }
        Err(e) => return Err(e.into()),
    }
    let req = FrontendMessage::ApplyProfileHint {
        id: "ah-cli".into(),
        proposal_id: proposal_id.to_string(),
        field: field.to_string(),
        applied_value: applied_value.to_string(),
    };
    let frame = encode_frame(&req)?;
    writer.write_all(&frame).await?;
    loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((
                DaemonEnvelope::ProfileHintApplyAcked { ok, error, .. },
                _,
            )) => {
                if ok {
                    return Ok(());
                }
                return Err(DaemonError::Protocol(
                    error.unwrap_or_else(|| "apply-profile-hint failed".into()),
                ));
            }
            Ok((other, consumed)) => {
                buf.drain(..consumed);
                return Err(DaemonError::Protocol(format!(
                    "expected ProfileHintApplyAcked, got {other:?}"
                )));
            }
            Err(FrameError::IncompleteBuf) => {
                read_more(&mut reader, &mut buf).await?;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Phase 119 Task 6 — operator-CLI tool-relevance ledger dump over
/// IPC. Returns the per-row dump table (one row per
/// `(keyword_key, surface_kind, identifier)` triple) optionally
/// filtered to a single keyword key.
pub async fn dump_tool_relevance(
    socket_path: &Path,
    keyword_key_filter: Option<&str>,
) -> Result<Vec<crate::daemon_ipc::ToolRelevanceDumpRow>, DaemonError> {
    let payload = send_query(
        socket_path,
        "tr-dump",
        QueryPayload::DumpToolRelevance {
            keyword_key_filter: keyword_key_filter.map(str::to_string),
        },
    )
    .await?;
    match payload {
        QueryResponsePayload::ToolRelevanceDump { rows } => Ok(rows),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected ToolRelevanceDump, got {other:?}"
        ))),
    }
}

/// Phase 119 — operator-CLI ImportRoleDraft over IPC. Mirrors
/// `apply_profile_hint` for the second Phase 118 category.
pub async fn import_role_draft(
    socket_path: &Path,
    proposal_id: &str,
    role_name: &str,
    parent: Option<&str>,
) -> Result<(), DaemonError> {
    let stream = UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();
    let mut buf = Vec::with_capacity(4096);
    read_more(&mut reader, &mut buf).await?;
    match decode_frame::<DaemonEnvelope>(&buf) {
        Ok((DaemonEnvelope::DaemonReady { .. }, consumed)) => {
            buf.drain(..consumed);
        }
        Ok((other, _)) => {
            return Err(DaemonError::Protocol(format!(
                "expected DaemonReady, got {other:?}"
            )))
        }
        Err(e) => return Err(e.into()),
    }
    let req = FrontendMessage::ImportRoleDraft {
        id: "ir-cli".into(),
        proposal_id: proposal_id.to_string(),
        role_name: role_name.to_string(),
        parent: parent.map(str::to_string),
    };
    let frame = encode_frame(&req)?;
    writer.write_all(&frame).await?;
    loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((
                DaemonEnvelope::RoleDraftImportAcked { ok, error, .. },
                _,
            )) => {
                if ok {
                    return Ok(());
                }
                return Err(DaemonError::Protocol(
                    error.unwrap_or_else(|| "import-role-draft failed".into()),
                ));
            }
            Ok((other, consumed)) => {
                buf.drain(..consumed);
                return Err(DaemonError::Protocol(format!(
                    "expected RoleDraftImportAcked, got {other:?}"
                )));
            }
            Err(FrameError::IncompleteBuf) => {
                read_more(&mut reader, &mut buf).await?;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Phase 73 — paginated notification history walk over IPC.
/// Returns the `(entries, total_len)` pair from
/// `QueryPayload::ListNotificationHistory`.
pub async fn list_notification_history(
    socket_path: &Path,
    from_seq: u64,
    limit: u32,
    target_filter: Option<&str>,
) -> Result<(Vec<NotificationHistoryEntry>, u64), DaemonError> {
    let payload = send_query(
        socket_path,
        "n-hist",
        QueryPayload::ListNotificationHistory {
            from_seq,
            limit,
            target_filter: target_filter.map(str::to_string),
        },
    )
    .await?;
    match payload {
        QueryResponsePayload::ListNotificationHistory { entries, total_len } => {
            Ok((entries, total_len))
        }
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected ListNotificationHistory, got {other:?}"
        ))),
    }
}

/// Phase 70 — list pending and resolved Persona proposals over
/// IPC. `status_filter` is the same string the wire envelope
/// expects: `"all" | "pending" | "approved" | "rejected" |
/// "superseded"`; unknown values fall through to `"pending"`
/// server-side.
pub async fn list_persona_proposals(
    socket_path: &Path,
    status_filter: &str,
    limit: u32,
) -> Result<(Vec<PersonaProposalSummary>, u64), DaemonError> {
    let payload = send_query(
        socket_path,
        "pp-list",
        QueryPayload::ListPersonaProposals {
            status_filter: status_filter.to_string(),
            limit,
        },
    )
    .await?;
    match payload {
        QueryResponsePayload::ListPersonaProposals {
            proposals,
            total_len,
        } => Ok((proposals, total_len)),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected ListPersonaProposals, got {other:?}"
        ))),
    }
}

/// Phase 70 — fetch a single Persona proposal by id.
pub async fn get_persona_proposal(
    socket_path: &Path,
    proposal_id: &str,
) -> Result<Option<PersonaProposalSummary>, DaemonError> {
    let payload = send_query(
        socket_path,
        "pp-get",
        QueryPayload::GetPersonaProposal {
            proposal_id: proposal_id.to_string(),
        },
    )
    .await?;
    match payload {
        QueryResponsePayload::GetPersonaProposal { proposal } => Ok(proposal),
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected GetPersonaProposal, got {other:?}"
        ))),
    }
}

/// Phase 70 — operator-initiated proposal resolution over IPC.
/// Sends a `ResolvePersonaProposal` frame and blocks for the
/// matching `PersonaProposalResolved` reply.
pub async fn resolve_persona_proposal(
    socket_path: &Path,
    proposal_id: &str,
    resolution: PersonaProposalResolution,
) -> Result<PersonaProposalResolveSuccess, DaemonError> {
    let stream = UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();
    let mut buf = Vec::with_capacity(4096);
    read_more(&mut reader, &mut buf).await?;
    match decode_frame::<DaemonEnvelope>(&buf) {
        Ok((DaemonEnvelope::DaemonReady { .. }, consumed)) => {
            buf.drain(..consumed);
        }
        Ok((other, _)) => {
            return Err(DaemonError::Protocol(format!(
                "expected DaemonReady, got {other:?}"
            )))
        }
        Err(e) => return Err(e.into()),
    }
    let req = FrontendMessage::ResolvePersonaProposal {
        id: "rsp-cli".into(),
        proposal_id: proposal_id.to_string(),
        resolution,
    };
    let frame = encode_frame(&req)?;
    writer.write_all(&frame).await?;
    loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((
                DaemonEnvelope::PersonaProposalResolved {
                    ok, success, error, ..
                },
                _,
            )) => {
                if ok {
                    return success.ok_or_else(|| {
                        DaemonError::Protocol(
                            "PersonaProposalResolved ok=true but success is None"
                                .into(),
                        )
                    });
                }
                return Err(DaemonError::Protocol(
                    error.unwrap_or_else(|| "proposal resolution failed".into()),
                ));
            }
            Ok((other, consumed)) => {
                buf.drain(..consumed);
                return Err(DaemonError::Protocol(format!(
                    "expected PersonaProposalResolved, got {other:?}"
                )));
            }
            Err(FrameError::IncompleteBuf) => {
                read_more(&mut reader, &mut buf).await?;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Phase 64 Task 3 — full-fidelity Persona chain dump over IPC.
/// Single-shot response (no pagination). Used by
/// `aivyx-pa identity export` to read the chain into memory before
/// writing the export bundle to disk. Returns the chain in order
/// and the effective state at fetch time.
pub async fn export_persona_chain(
    socket_path: &Path,
) -> Result<
    (
        Vec<crate::identity_export::DeltaExport>,
        crate::persona::EffectivePersona,
    ),
    DaemonError,
> {
    let payload = send_query(socket_path, "p-export", QueryPayload::ExportPersonaChain).await?;
    match payload {
        QueryResponsePayload::ExportPersonaChain { deltas, effective } => {
            Ok((deltas, effective))
        }
        QueryResponsePayload::QueryError { code, message } => {
            Err(DaemonError::Protocol(format!("{code}: {message}")))
        }
        other => Err(DaemonError::Protocol(format!(
            "expected ExportPersonaChain, got {other:?}"
        ))),
    }
}

/// Phase 60 — operator-initiated Persona delta revert over IPC.
/// Returns the chain sequence number of the appended Revert delta
/// on success.
pub async fn revert_persona_delta(
    socket_path: &Path,
    target_delta_id: &str,
) -> Result<u64, DaemonError> {
    let stream = UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();
    let mut buf = Vec::with_capacity(4096);
    read_more(&mut reader, &mut buf).await?;
    match decode_frame::<DaemonEnvelope>(&buf) {
        Ok((DaemonEnvelope::DaemonReady { .. }, consumed)) => {
            buf.drain(..consumed);
        }
        Ok((other, _)) => {
            return Err(DaemonError::Protocol(format!(
                "expected DaemonReady, got {other:?}"
            )))
        }
        Err(e) => return Err(e.into()),
    }
    let req = FrontendMessage::RevertPersonaDelta {
        id: "rv-cli".into(),
        target_delta_id: target_delta_id.to_string(),
    };
    let frame = encode_frame(&req)?;
    writer.write_all(&frame).await?;
    loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((
                DaemonEnvelope::PersonaRevertResolved {
                    ok, seq, error, ..
                },
                _,
            )) => {
                if ok {
                    return seq.ok_or_else(|| {
                        DaemonError::Protocol(
                            "PersonaRevertResolved ok=true but seq is None".into(),
                        )
                    });
                }
                return Err(DaemonError::Protocol(
                    error.unwrap_or_else(|| "persona revert failed".into()),
                ));
            }
            Ok((other, consumed)) => {
                buf.drain(..consumed);
                return Err(DaemonError::Protocol(format!(
                    "expected PersonaRevertResolved, got {other:?}"
                )));
            }
            Err(FrameError::IncompleteBuf) => {
                read_more(&mut reader, &mut buf).await?;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Chapter Tutor — send an operator-authored skill op (`teach` / `update` /
/// `forget`) to the daemon over the local socket. The daemon writes it to the
/// signed persona chain via operator authority (no agent scope). Returns the
/// chain seq of the appended delta on success. Mirrors [`revert_persona_delta`].
pub async fn author_skill(
    socket_path: &Path,
    op: aivyx_ipc::protocol::SkillAuthorOp,
    name: &str,
    trigger: Option<&str>,
    procedure: Option<&str>,
) -> Result<u64, DaemonError> {
    let stream = UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();
    let mut buf = Vec::with_capacity(4096);
    read_more(&mut reader, &mut buf).await?;
    match decode_frame::<DaemonEnvelope>(&buf) {
        Ok((DaemonEnvelope::DaemonReady { .. }, consumed)) => {
            buf.drain(..consumed);
        }
        Ok((other, _)) => {
            return Err(DaemonError::Protocol(format!(
                "expected DaemonReady, got {other:?}"
            )))
        }
        Err(e) => return Err(e.into()),
    }
    let req = FrontendMessage::AuthorSkill {
        id: "skill-cli".into(),
        op,
        name: name.to_string(),
        trigger: trigger.map(|s| s.to_string()),
        procedure: procedure.map(|s| s.to_string()),
    };
    let frame = encode_frame(&req)?;
    writer.write_all(&frame).await?;
    loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::SkillAuthored { ok, seq, error, .. }, _)) => {
                if ok {
                    return seq.ok_or_else(|| {
                        DaemonError::Protocol(
                            "SkillAuthored ok=true but seq is None".into(),
                        )
                    });
                }
                return Err(DaemonError::Protocol(
                    error.unwrap_or_else(|| "skill authoring failed".into()),
                ));
            }
            Ok((other, consumed)) => {
                buf.drain(..consumed);
                return Err(DaemonError::Protocol(format!(
                    "expected SkillAuthored, got {other:?}"
                )));
            }
            Err(FrameError::IncompleteBuf) => {
                read_more(&mut reader, &mut buf).await?;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Phase 65 — operator-driven Persona chain import over IPC.
/// Closes the Phase 60 identity-deferral end to end. Sends the
/// parsed export bundle to the daemon for replay against the
/// local store. The daemon refuses on a non-empty chain unless
/// `force` is set, then wipes and replays. On success returns
/// the daemon's `PersonaImportSuccess` payload with
/// `deltas_imported` + `final_chain_seq` per Q4(a).
pub async fn import_persona_chain(
    socket_path: &Path,
    deltas: Vec<crate::identity_export::DeltaExport>,
    effective_at_export: crate::persona::EffectivePersona,
    force: bool,
) -> Result<crate::daemon_ipc::PersonaImportSuccess, DaemonError> {
    let stream = UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();
    let mut buf = Vec::with_capacity(4096);
    read_more(&mut reader, &mut buf).await?;
    match decode_frame::<DaemonEnvelope>(&buf) {
        Ok((DaemonEnvelope::DaemonReady { .. }, consumed)) => {
            buf.drain(..consumed);
        }
        Ok((other, _)) => {
            return Err(DaemonError::Protocol(format!(
                "expected DaemonReady, got {other:?}"
            )))
        }
        Err(e) => return Err(e.into()),
    }
    let req = FrontendMessage::ImportPersonaChain {
        id: "im-cli".into(),
        deltas,
        // Phase 118 — boxed at the IPC boundary; see
        // FrontendMessage::ImportPersonaChain field doc.
        effective_at_export: Box::new(effective_at_export),
        force,
    };
    let frame = encode_frame(&req)?;
    writer.write_all(&frame).await?;
    loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((
                DaemonEnvelope::PersonaImportResolved {
                    ok, success, error, ..
                },
                _,
            )) => {
                if ok {
                    return success.ok_or_else(|| {
                        DaemonError::Protocol(
                            "PersonaImportResolved ok=true but success is None".into(),
                        )
                    });
                }
                return Err(DaemonError::Protocol(
                    error.unwrap_or_else(|| "persona import failed".into()),
                ));
            }
            Ok((other, consumed)) => {
                buf.drain(..consumed);
                return Err(DaemonError::Protocol(format!(
                    "expected PersonaImportResolved, got {other:?}"
                )));
            }
            Err(FrameError::IncompleteBuf) => {
                read_more(&mut reader, &mut buf).await?;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Shared helper: connect, send a `Query`, return the response
/// payload. Used by the Persona inspection helpers above.
async fn send_query(
    socket_path: &Path,
    id: &str,
    query: QueryPayload,
) -> Result<QueryResponsePayload, DaemonError> {
    let stream = UnixStream::connect(socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();
    let mut buf = Vec::with_capacity(4096);
    read_more(&mut reader, &mut buf).await?;
    match decode_frame::<DaemonEnvelope>(&buf) {
        Ok((DaemonEnvelope::DaemonReady { .. }, consumed)) => buf.drain(..consumed),
        Ok((other, _)) => {
            return Err(DaemonError::Protocol(format!(
                "expected DaemonReady, got {other:?}"
            )))
        }
        Err(e) => return Err(e.into()),
    };
    let req = FrontendMessage::Query {
        id: id.to_string(),
        payload: query,
    };
    let frame = encode_frame(&req)?;
    writer.write_all(&frame).await?;
    loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::QueryResponse { payload, .. }, _)) => return Ok(payload),
            // The daemon delivers a take-once `RecoveryNotice` to the first
            // frontend that connects after an unclean restart — it arrives
            // between `DaemonReady` and our `QueryResponse` (this one-shot
            // query connection skips the session handshake, so unlike the
            // session path it meets the notice here). It is informational:
            // skip past it and keep reading. Without this arm the *first*
            // query after any daemon restart fails (observed live as the
            // "memory list/wiki transient flakiness").
            Ok((
                DaemonEnvelope::RecoveryNotice {
                    lost_sessions,
                    lost_turns,
                    ..
                },
                consumed,
            )) => {
                buf.drain(..consumed);
                if !lost_sessions.is_empty() || !lost_turns.is_empty() {
                    eprintln!(
                        "aivyx-pa: daemon recovered from an unclean shutdown — \
                         {} session(s) and {} in-flight turn(s) were lost.",
                        lost_sessions.len(),
                        lost_turns.len(),
                    );
                }
            }
            Ok((other, consumed)) => {
                buf.drain(..consumed);
                return Err(DaemonError::Protocol(format!(
                    "expected QueryResponse, got {other:?}"
                )));
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await?,
            Err(e) => return Err(e.into()),
        }
    }
}

/// The auto-spawned daemon's log file: a sibling `daemon.log` next to
/// the socket (and the `daemon.pid`), so the three live together in
/// the runtime dir.
fn daemon_log_path(socket_path: &Path) -> PathBuf {
    socket_path.with_extension("log")
}

/// Open the sibling `daemon.log` for the auto-spawned daemon's
/// stdout + stderr (created if missing, appended otherwise). Falls
/// back to discarding output if the log cannot be opened, so a
/// logging problem never blocks the daemon from starting.
fn daemon_log_stdio(socket_path: &Path) -> (std::process::Stdio, std::process::Stdio) {
    let log_path = daemon_log_path(socket_path);
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        // Two independent handles so stdout and stderr can be written
        // concurrently without sharing one offset cursor.
        Ok(file) => match file.try_clone() {
            Ok(file2) => (file.into(), file2.into()),
            Err(_) => (file.into(), std::process::Stdio::null()),
        },
        Err(_) => (std::process::Stdio::null(), std::process::Stdio::null()),
    }
}

/// Spawn a daemon process in the background and wait for its socket
/// to appear. Returns the socket path on success.
///
/// Uses `tokio::process::Command` to launch `aivyx-pa daemon run` as a
/// detached child. The daemon's stdout/stderr are redirected to a
/// sibling `daemon.log` rather than inherited: a background daemon
/// must not print onto the launching terminal — in the TUI the
/// startup banner bleeds under the alternate screen, and in the REPL
/// it interleaves with the prompt. The banner + ongoing logs stay
/// recoverable in the log file. (A direct `aivyx-pa daemon run` is
/// unaffected — it does not go through this path and keeps writing to
/// the operator's terminal.)
pub async fn spawn_daemon_and_wait(
    socket_path: &Path,
    timeout: Duration,
) -> Result<PathBuf, DaemonError> {
    let exe = std::env::current_exe()?;

    let (stdout, stderr) = daemon_log_stdio(socket_path);
    let _child = tokio::process::Command::new(&exe)
        .args(["daemon", "run"])
        .stdin(std::process::Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .map_err(|e| DaemonError::Internal(format!("failed to spawn daemon: {e}")))?;

    // Poll for the socket to appear with exponential backoff.
    let start = tokio::time::Instant::now();
    let mut delay = Duration::from_millis(20);
    loop {
        if daemon_is_running(socket_path).await {
            return Ok(socket_path.to_path_buf());
        }
        if start.elapsed() > timeout {
            return Err(DaemonError::Internal(format!(
                "daemon did not start within {}ms — socket not found at {}",
                timeout.as_millis(),
                socket_path.display(),
            )));
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_millis(500));
    }
}

// -----------------------------------------------------------------------
// Phase 16 backward-compatible single-turn client
// -----------------------------------------------------------------------

/// Connect to the daemon, start a session, submit one input, and
/// collect all streamed events until `TurnComplete`.
pub async fn run_poc_client(
    socket_path: &Path,
    role: Option<String>,
    input_text: String,
) -> Result<DaemonTurnResult, DaemonError> {
    let mut session = DaemonSession::connect(socket_path, role, None).await?;
    let daemon_version = session.daemon_version.clone();
    let session_id = session.session_id.clone();

    let (events, outcome) = session.submit_input(input_text).await?;
    let _ = session.disconnect().await;

    Ok(DaemonTurnResult {
        session_id,
        events,
        outcome,
        daemon_version,
    })
}

async fn read_more(
    reader: &mut tokio::net::unix::OwnedReadHalf,
    buf: &mut Vec<u8>,
) -> Result<(), DaemonError> {
    let mut tmp = [0u8; 4096];
    let n = reader.read(&mut tmp).await?;
    if n == 0 {
        return Err(DaemonError::Protocol(
            "connection closed unexpectedly".into(),
        ));
    }
    buf.extend_from_slice(&tmp[..n]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon_ipc::{encode_frame, DaemonEnvelope};
    use tokio::net::UnixListener;

    /// A fake daemon that performs the lifecycle handshake while
    /// injecting a `RecoveryNotice` between `DaemonReady` and
    /// `SessionStarted` — exactly what a real daemon sends to the
    /// first frontend that connects after an unclean restart. `connect`
    /// must skip the notice and still succeed.
    #[tokio::test]
    async fn connect_skips_recovery_notice_in_the_handshake() {
        let sock = std::env::temp_dir()
            .join(format!("aivyx-recov-{}.sock", uuid::Uuid::new_v4()));
        let listener = UnixListener::bind(&sock).expect("bind fake daemon");

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            for env in [
                DaemonEnvelope::DaemonReady {
                    version: "0.1".into(),
                },
                DaemonEnvelope::RecoveryNotice {
                    lost_sessions: vec!["old-session".into()],
                    lost_turns: vec!["old-turn".into()],
                    stale_since: 42,
                },
                DaemonEnvelope::SessionStarted {
                    session_id: "sess-recovered".into(),
                },
            ] {
                let frame = encode_frame(&env).expect("encode");
                stream.write_all(&frame).await.expect("write frame");
            }
            // Drain the client's StartSession so the socket stays open
            // until the client has finished the handshake.
            let mut tmp = [0u8; 1024];
            let _ = stream.read(&mut tmp).await;
        });

        let session = DaemonSession::connect(&sock, None, None)
            .await
            .expect("connect must succeed despite the RecoveryNotice");
        assert_eq!(session.session_id, "sess-recovered");
        assert_eq!(session.daemon_version.as_deref(), Some("0.1"));

        let _ = session.disconnect().await;
        let _ = server.await;
        let _ = std::fs::remove_file(&sock);
    }

    /// A fake daemon that emits the take-once `RecoveryNotice` between
    /// `DaemonReady` and the `QueryResponse` — what a real daemon sends
    /// to the first frontend after an unclean restart. `send_query` must
    /// skip it and still return the response (the one-shot query path
    /// skips the session handshake, so it meets the notice here — the
    /// observed `aivyx-pa memory wiki`/`list` post-restart failure).
    #[tokio::test]
    async fn send_query_skips_recovery_notice_before_the_response() {
        use aivyx_ipc::protocol::QueryResponsePayload;

        let sock = std::env::temp_dir()
            .join(format!("aivyx-qrecov-{}.sock", uuid::Uuid::new_v4()));
        let listener = UnixListener::bind(&sock).expect("bind fake daemon");

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let ready = encode_frame(&DaemonEnvelope::DaemonReady {
                version: "0.1".into(),
            })
            .expect("encode ready");
            let notice = encode_frame(&DaemonEnvelope::RecoveryNotice {
                lost_sessions: vec![],
                lost_turns: vec![],
                stale_since: 7,
            })
            .expect("encode notice");
            stream.write_all(&ready).await.expect("write ready");
            stream.write_all(&notice).await.expect("write notice");
            // Read the client's Query frame, then answer it.
            let mut tmp = [0u8; 2048];
            let _ = stream.read(&mut tmp).await;
            let resp = encode_frame(&DaemonEnvelope::QueryResponse {
                id: "q1".into(),
                payload: QueryResponsePayload::ListWikiPages { pages: vec![] },
            })
            .expect("encode resp");
            stream.write_all(&resp).await.expect("write resp");
            let _ = stream.read(&mut tmp).await;
        });

        let payload = send_query(&sock, "q1", QueryPayload::ListWikiPages)
            .await
            .expect("send_query must skip the RecoveryNotice and return");
        assert!(matches!(
            payload,
            QueryResponsePayload::ListWikiPages { .. }
        ));

        let _ = server.await;
        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn daemon_log_path_is_sibling_of_socket_and_pid() {
        let socket = Path::new("/run/user/1000/aivyx/daemon.sock");
        let log = daemon_log_path(socket);
        assert_eq!(log, Path::new("/run/user/1000/aivyx/daemon.log"));
        // Lives alongside the pid file (same `with_extension` rule the
        // server + status probe use), so the runtime dir holds the
        // socket, pid, and log together.
        assert_eq!(log.parent(), socket.with_extension("pid").parent());
        assert_eq!(log.file_name().unwrap(), "daemon.log");
    }

    #[test]
    fn daemon_log_stdio_opens_a_log_under_a_temp_runtime_dir() {
        // The auto-spawn redirect must create the log (and its parent
        // dir if missing) so the daemon never bleeds onto the
        // launching terminal. Use a throwaway dir under the temp root.
        let base = std::env::temp_dir().join(format!(
            "aivyx-daemon-log-test-{}",
            uuid::Uuid::new_v4()
        ));
        let socket = base.join("nested").join("daemon.sock");
        assert!(!base.exists());

        // Should not panic and should materialize the log + parents.
        let _stdio = daemon_log_stdio(&socket);
        let log = daemon_log_path(&socket);
        assert!(log.exists(), "daemon.log was created: {}", log.display());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn run_team_mission_channel_does_the_start_session_handshake_then_sends_the_request() {
        let sock = std::env::temp_dir()
            .join(format!("aivyx-runteam-{}.sock", uuid::Uuid::new_v4()));
        let listener = UnixListener::bind(&sock).expect("bind fake daemon");

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let ready = encode_frame(&DaemonEnvelope::DaemonReady {
                version: "0.1".into(),
            })
            .expect("encode ready");
            stream.write_all(&ready).await.expect("write ready");

            // Read the client's StartSession, then reply SessionStarted.
            let mut tmp = [0u8; 2048];
            let _ = stream.read(&mut tmp).await;
            let started = encode_frame(&DaemonEnvelope::SessionStarted {
                session_id: "sess-1".into(),
            })
            .expect("encode started");
            stream.write_all(&started).await.expect("write started");

            // Read the client's RunTeamMissionChannel, then reply success.
            let _ = stream.read(&mut tmp).await;
            let resp = encode_frame(&DaemonEnvelope::TeamMissionChannelStarted {
                mission_id: "m-1".into(),
            })
            .expect("encode resp");
            stream.write_all(&resp).await.expect("write resp");
            let _ = stream.read(&mut tmp).await;
        });

        let mission_id = run_team_mission_channel(
            &sock,
            FrontendType::Telegram,
            "close the books".to_string(),
        )
        .await
        .expect("run_team_mission_channel must succeed");
        assert_eq!(mission_id, "m-1");

        let _ = server.await;
        let _ = std::fs::remove_file(&sock);
    }

    #[tokio::test]
    async fn run_team_mission_channel_surfaces_a_capability_denial_error() {
        let sock = std::env::temp_dir()
            .join(format!("aivyx-runteam-denied-{}.sock", uuid::Uuid::new_v4()));
        let listener = UnixListener::bind(&sock).expect("bind fake daemon");

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let ready = encode_frame(&DaemonEnvelope::DaemonReady {
                version: "0.1".into(),
            })
            .expect("encode ready");
            stream.write_all(&ready).await.expect("write ready");
            let mut tmp = [0u8; 2048];
            let _ = stream.read(&mut tmp).await;
            let started = encode_frame(&DaemonEnvelope::SessionStarted {
                session_id: "sess-1".into(),
            })
            .expect("encode started");
            stream.write_all(&started).await.expect("write started");
            let _ = stream.read(&mut tmp).await;
            let err = encode_frame(&DaemonEnvelope::Error {
                code: "team_run_channel_denied".into(),
                message: "this channel is not authorized".into(),
            })
            .expect("encode err");
            stream.write_all(&err).await.expect("write err");
            let _ = stream.read(&mut tmp).await;
        });

        let result = run_team_mission_channel(
            &sock,
            FrontendType::Discord,
            "close the books".to_string(),
        )
        .await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("team_run_channel_denied"));

        let _ = server.await;
        let _ = std::fs::remove_file(&sock);
    }
}
