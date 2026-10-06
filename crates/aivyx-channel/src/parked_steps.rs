//! Supervised batching — tool calls that unattended runs park for the
//! operator's review.
//!
//! An unattended run (a routine, webhook, file watch, the autonomous loop, a
//! team mission) can't ask anyone, so a call that needs approval used to be
//! refused. In an area at the `supervised` autonomy level it is parked
//! instead: [`ParkingChannel`] answers the turn loop's approval request with
//! [`Approval::Parked`] after [`StepParker::park`] stores the exact call in
//! [`aivyx_storage::KeyDomain::ParkedSteps`]. The operator reviews parked
//! steps in the Studio or with `aivyx-pa review`; an approved step runs once,
//! exactly as parked, through [`aivyx_core::Agent::run_approved_call`].
//!
//! Key layout: the step's short id is the key; the value is the
//! [`ParkedStep`] as JSON (without its `preview`, which is computed when
//! listed).

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aivyx_audit::{AuditEvent, AuditWriter, PersistentAuditLog};
use aivyx_capability::TrustTier;
use aivyx_core::{
    Agent, Approval, ApprovalRequest, AreaFlags, CancellationToken, ChannelContext, ChannelError,
    ChannelPlatform, SessionId, StreamEvent, TurnOutcome,
};
pub use aivyx_ipc::parked::{ParkedState, ParkedStep};
use aivyx_ipc::protocol::{QueryPayload, QueryResponsePayload};
use aivyx_storage::DomainHandle;
use async_trait::async_trait;

use crate::daemon_ipc::FrontendType;
use crate::daemon_server::ChannelFactory;
use crate::notify_dispatcher::NotifyDispatcher;

/// How many resolved steps are kept for the record.
pub const KEEP_RESOLVED: usize = 200;
/// How much of a file a preview shows.
const PREVIEW_CHARS: usize = 1500;
const DAY_SECS: i64 = 24 * 3600;

#[derive(Debug, thiserror::Error)]
pub enum ParkError {
    #[error("parked-step storage error: {0}")]
    Storage(String),
    #[error("no parked step with id `{0}`")]
    NotFound(String),
    #[error("parked step `{id}` is already {state}")]
    NotPending { id: String, state: &'static str },
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Parks needs-approval calls for supervised areas and resolves them.
pub struct StepParker {
    storage: DomainHandle,
    supervised: AreaFlags,
    expiry_secs: i64,
    audit_log: Option<Arc<PersistentAuditLog>>,
    /// The default notify target, told once per newly parked step.
    notify: Option<(Arc<NotifyDispatcher>, String)>,
    /// Where relative paths in a preview resolve (the agent's `fs_root`).
    preview_root: Option<PathBuf>,
    /// Held for a whole resolve (and while lapsing), so two decisions on
    /// the same step can't both run it.
    lock: tokio::sync::Mutex<()>,
}

impl StepParker {
    /// A parker over `storage` that parks calls in the `supervised` areas;
    /// pending steps lapse after `expiry_days`.
    pub fn new(storage: DomainHandle, supervised: AreaFlags, expiry_days: u32) -> Self {
        StepParker {
            storage,
            supervised,
            expiry_secs: i64::from(expiry_days) * DAY_SECS,
            audit_log: None,
            notify: None,
            preview_root: None,
            lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn with_audit_log(mut self, log: Arc<PersistentAuditLog>) -> Self {
        self.audit_log = Some(log);
        self
    }

    pub fn with_notify(mut self, dispatcher: Arc<NotifyDispatcher>, target: String) -> Self {
        self.notify = Some((dispatcher, target));
        self
    }

    pub fn with_preview_root(mut self, root: PathBuf) -> Self {
        self.preview_root = Some(root);
        self
    }

    /// Whether a call needing capability `scope_base` parks (its area is
    /// `supervised`).
    pub fn parks(&self, scope_base: &str) -> bool {
        self.supervised.for_base(scope_base)
    }

    fn audit(&self, event: AuditEvent) {
        if let Some(log) = &self.audit_log
            && let Err(e) = log.append(event)
        {
            eprintln!("aivyx-pa: failed to audit a parked step: {e}");
        }
    }

    async fn read_all(&self) -> Result<Vec<ParkedStep>, ParkError> {
        let rows = self
            .storage
            .scan_prefix(b"")
            .await
            .map_err(|e| ParkError::Storage(e.to_string()))?;
        let mut steps = Vec::with_capacity(rows.len());
        for (_, value) in rows {
            match serde_json::from_slice::<ParkedStep>(&value) {
                Ok(step) => steps.push(step),
                Err(e) => eprintln!("aivyx-pa: skipping an unreadable parked step: {e}"),
            }
        }
        Ok(steps)
    }

    async fn write(&self, step: &ParkedStep) -> Result<(), ParkError> {
        let mut stored = step.clone();
        stored.preview = None;
        let bytes = serde_json::to_vec(&stored).map_err(|e| ParkError::Storage(e.to_string()))?;
        self.storage
            .put(step.id.as_bytes(), &bytes)
            .await
            .map_err(|e| ParkError::Storage(e.to_string()))
    }

    /// Park `req` from the run named `origin`, or return the id of an
    /// identical step already pending. If the step can't be stored, the
    /// failure is audited and the call is refused as before
    /// ([`Approval::Unavailable`]).
    pub async fn park(&self, req: &ApprovalRequest, origin: &str) -> Approval {
        match self.try_park(req, origin).await {
            Ok((id, newly)) => {
                if newly && let Some((dispatcher, target)) = &self.notify {
                    let message = format!(
                        "The {origin} parked a step for your review: {} (aivyx-pa review).",
                        req.summary
                    );
                    if let Err(e) = dispatcher.dispatch(target, &message, None).await {
                        eprintln!("aivyx-pa: couldn't notify `{target}` about a parked step: {e}");
                    }
                }
                Approval::Parked { id }
            }
            Err(e) => {
                eprintln!("aivyx-pa: couldn't park {} from the {origin}: {e}", req.summary);
                self.audit(AuditEvent::StepParkFailed {
                    tool: req.tool.clone(),
                    origin: origin.to_string(),
                    reason: e.to_string(),
                });
                Approval::Unavailable
            }
        }
    }

    /// `(id, newly parked?)`.
    async fn try_park(&self, req: &ApprovalRequest, origin: &str) -> Result<(String, bool), ParkError> {
        let _guard = self.lock.lock().await;
        let existing = self.read_all().await?;
        if let Some(same) = existing.iter().find(|s| {
            s.state == ParkedState::Pending && s.tool == req.tool && s.input == req.input
        }) {
            return Ok((same.id.clone(), false));
        }
        let id = loop {
            let candidate = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
            if !existing.iter().any(|s| s.id == candidate) {
                break candidate;
            }
        };
        let area = req.scope_base.split('.').next().unwrap_or(&req.scope_base).to_string();
        let step = ParkedStep {
            id: id.clone(),
            tool: req.tool.clone(),
            input: req.input.clone(),
            summary: req.summary.clone(),
            reason: req.reason.clone(),
            area: area.clone(),
            origin: origin.to_string(),
            trust_tier: req.trust_tier,
            parked_at: now_unix(),
            state: ParkedState::Pending,
            resolved_at: None,
            result: None,
            preview: None,
        };
        self.write(&step).await?;
        self.audit(AuditEvent::StepParked {
            id: id.clone(),
            tool: step.tool.clone(),
            summary: step.summary.clone(),
            area,
            origin: origin.to_string(),
        });
        Ok((id, true))
    }

    /// Lapse pending steps older than the expiry and keep at most
    /// [`KEEP_RESOLVED`] resolved ones. Call with `lock` held. Returns the
    /// steps as they now are.
    async fn tidy(&self, now: i64) -> Result<Vec<ParkedStep>, ParkError> {
        let mut steps = self.read_all().await?;
        let days = self.expiry_secs / DAY_SECS;
        for step in steps.iter_mut() {
            if step.state == ParkedState::Pending && now - step.parked_at > self.expiry_secs {
                step.state = ParkedState::Lapsed;
                step.resolved_at = Some(now);
                step.result = Some(format!(
                    "Not reviewed within {days} {}.",
                    if days == 1 { "day" } else { "days" }
                ));
                self.write(step).await?;
                self.audit(AuditEvent::ParkedStepResolved {
                    id: step.id.clone(),
                    tool: step.tool.clone(),
                    outcome: "lapsed".into(),
                });
            }
        }
        let mut resolved: Vec<&ParkedStep> =
            steps.iter().filter(|s| s.state != ParkedState::Pending).collect();
        if resolved.len() > KEEP_RESOLVED {
            resolved.sort_by_key(|s| std::cmp::Reverse(s.resolved_at.unwrap_or(s.parked_at)));
            let drop: Vec<String> = resolved[KEEP_RESOLVED..].iter().map(|s| s.id.clone()).collect();
            for id in &drop {
                self.storage
                    .delete(id.as_bytes())
                    .await
                    .map_err(|e| ParkError::Storage(e.to_string()))?;
            }
            steps.retain(|s| !drop.contains(&s.id));
        }
        Ok(steps)
    }

    /// Lapse expired pending steps; returns how many lapsed.
    pub async fn lapse_expired(&self) -> Result<usize, ParkError> {
        let _guard = self.lock.lock().await;
        let before = self.read_all().await?;
        let pending = before.iter().filter(|s| s.state == ParkedState::Pending).count();
        let after = self.tidy(now_unix()).await?;
        let still = after.iter().filter(|s| s.state == ParkedState::Pending).count();
        Ok(pending - still)
    }

    /// Every step, newest first, after lapsing expired ones. Pending steps
    /// carry a fresh preview of what they touch.
    pub async fn list(&self) -> Result<Vec<ParkedStep>, ParkError> {
        let mut steps = {
            let _guard = self.lock.lock().await;
            self.tidy(now_unix()).await?
        };
        steps.sort_by_key(|s| std::cmp::Reverse(s.parked_at));
        for step in steps.iter_mut().filter(|s| s.state == ParkedState::Pending) {
            step.preview = preview_for(step, self.preview_root.as_deref());
        }
        Ok(steps)
    }

    /// Resolve one pending step. On approve, `run` executes it — once — and
    /// its `Ok` / `Err` becomes `approved` / `failed` with the text as the
    /// result. Resolving a step that isn't pending (or doesn't exist) is an
    /// error and runs nothing.
    pub async fn resolve<F, Fut>(&self, id: &str, approve: bool, run: F) -> Result<ParkedStep, ParkError>
    where
        F: FnOnce(ParkedStep) -> Fut,
        Fut: Future<Output = Result<String, String>>,
    {
        let _guard = self.lock.lock().await;
        let steps = self.tidy(now_unix()).await?;
        let mut step = steps
            .into_iter()
            .find(|s| s.id == id)
            .ok_or_else(|| ParkError::NotFound(id.to_string()))?;
        if step.state != ParkedState::Pending {
            return Err(ParkError::NotPending { id: id.to_string(), state: step.state.as_str() });
        }
        let (state, result) = if approve {
            match run(step.clone()).await {
                Ok(out) => (ParkedState::Approved, Some(out)),
                Err(why) => (ParkedState::Failed, Some(why)),
            }
        } else {
            (ParkedState::Denied, None)
        };
        step.state = state;
        step.resolved_at = Some(now_unix());
        step.result = result;
        self.write(&step).await?;
        self.audit(AuditEvent::ParkedStepResolved {
            id: step.id.clone(),
            tool: step.tool.clone(),
            outcome: state.as_str().into(),
        });
        self.tidy(now_unix()).await?;
        Ok(step)
    }
}

/// What a step touches, as it is now: for an `fs.*` call with a `path`, the
/// file's current state; otherwise the arguments.
pub fn preview_for(step: &ParkedStep, root: Option<&Path>) -> Option<String> {
    let path = step
        .input
        .get("path")
        .and_then(|p| p.as_str())
        .filter(|_| step.tool.starts_with("fs."));
    let Some(path) = path else {
        return serde_json::to_string_pretty(&step.input).ok();
    };
    let full = match root {
        Some(root) if Path::new(path).is_relative() => root.join(path),
        _ => PathBuf::from(path),
    };
    Some(match std::fs::metadata(&full) {
        Ok(m) if m.is_dir() => format!("Now: {path} is a folder."),
        Ok(m) => {
            let bytes = std::fs::read(&full).unwrap_or_default();
            let text = String::from_utf8_lossy(&bytes);
            let head: String = text.chars().take(PREVIEW_CHARS).collect();
            let more = if text.chars().count() > PREVIEW_CHARS { "\n…" } else { "" };
            format!("Now: {path} ({} bytes)\n{head}{more}", m.len())
        }
        Err(_) => format!("Now: {path} doesn't exist."),
    })
}

/// Wraps an unattended run's channel: a needs-approval call in a
/// `supervised` area is parked for review; anything else goes to the inner
/// channel (which, for an unattended run, can't ask — so it's refused as
/// before). Every other method delegates unchanged.
pub struct ParkingChannel {
    inner: Arc<dyn ChannelContext + Send + Sync>,
    parker: Arc<StepParker>,
    origin: String,
}

impl ParkingChannel {
    /// `origin` names the run in the review list (`routine digest`).
    pub fn new(
        inner: Arc<dyn ChannelContext + Send + Sync>,
        parker: Arc<StepParker>,
        origin: impl Into<String>,
    ) -> Self {
        ParkingChannel { inner, parker, origin: origin.into() }
    }
}

#[async_trait]
impl ChannelContext for ParkingChannel {
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

    async fn request_approval(&self, request: &ApprovalRequest) -> Approval {
        if self.parker.parks(&request.scope_base) {
            self.parker.park(request, &self.origin).await
        } else {
            self.inner.request_approval(request).await
        }
    }

    fn session_partition(&self) -> Option<String> {
        self.inner.session_partition()
    }

    fn reset_cancellation(&self) {
        self.inner.reset_cancellation();
    }

    fn cancel_inflight(&self) {
        self.inner.cancel_inflight();
    }
}

/// The daemon's answer to `GetParkedSteps` / `ResolveParkedStep`; `None` for
/// any other query. An approved step runs through `agent` on a local channel
/// at the trust tier it was parked at.
pub async fn handle_parked_query(
    payload: &QueryPayload,
    parker: Option<&StepParker>,
    agent: &Arc<dyn Agent>,
    channel_factory: &ChannelFactory,
) -> Option<QueryResponsePayload> {
    let unavailable = || QueryResponsePayload::QueryError {
        code: "parked_steps_unavailable".into(),
        message: "No area is supervised, so nothing is parked.".into(),
    };
    Some(match payload {
        QueryPayload::GetParkedSteps => match parker {
            None => unavailable(),
            Some(p) => match p.list().await {
                Ok(steps) => QueryResponsePayload::ParkedSteps { steps },
                Err(e) => QueryResponsePayload::QueryError {
                    code: "parked_steps_failed".into(),
                    message: e.to_string(),
                },
            },
        },
        QueryPayload::ResolveParkedStep { id, approve } => match parker {
            None => unavailable(),
            Some(p) => {
                let run = |step: ParkedStep| async move {
                    let channel = crate::local::TierOverride::new(
                        channel_factory(FrontendType::Local),
                        step.trust_tier,
                    );
                    agent.run_approved_call(&step.tool, step.input.clone(), &channel).await
                };
                match p.resolve(id, *approve, run).await {
                    Ok(step) => QueryResponsePayload::ParkedStepResolved { step },
                    Err(e) => QueryResponsePayload::QueryError {
                        code: "resolve_parked_step_failed".into(),
                        message: e.to_string(),
                    },
                }
            }
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_crypto::MasterKey;
    use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Scratch {
        dir: PathBuf,
    }
    impl Scratch {
        fn new() -> Self {
            let tmp = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
            let dir = PathBuf::from(tmp).join(format!("aivyx-parked-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Scratch { dir }
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    async fn open_store(scratch: &Scratch, key: u8) -> Arc<dyn Storage> {
        RedbStorage::open(
            StorageConfig::new(scratch.dir.join("s.redb")),
            MasterKey::from_raw([key; 32]),
        )
        .await
        .unwrap()
    }

    fn supervised_fs() -> AreaFlags {
        let mut f = AreaFlags::everywhere(false);
        f.areas.insert("fs".into(), true);
        f
    }

    async fn parker(scratch: &Scratch) -> (StepParker, Arc<dyn Storage>) {
        let store = open_store(scratch, 3).await;
        (StepParker::new(store.domain(KeyDomain::ParkedSteps), supervised_fs(), 7), store)
    }

    fn req(tool: &str, base: &str, path: &str) -> ApprovalRequest {
        ApprovalRequest {
            tool: tool.into(),
            summary: format!("{tool} {path}"),
            input: serde_json::json!({ "path": path }),
            reason: "deleting can't be undone".into(),
            scope_base: base.into(),
            trust_tier: TrustTier::Trusted,
        }
    }

    async fn park_id(p: &StepParker, r: &ApprovalRequest) -> String {
        match p.park(r, "routine tidy").await {
            Approval::Parked { id } => id,
            other => panic!("expected Parked, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn park_round_trips_and_lists_pending() {
        let s = Scratch::new();
        let (p, _store) = parker(&s).await;
        let id = park_id(&p, &req("fs.delete", "fs.delete", "a.txt")).await;
        assert_eq!(id.len(), 8);
        let steps = p.list().await.unwrap();
        assert_eq!(steps.len(), 1);
        let step = &steps[0];
        assert_eq!(step.id, id);
        assert_eq!(step.state, ParkedState::Pending);
        assert_eq!(step.area, "fs");
        assert_eq!(step.origin, "routine tidy");
        assert_eq!(step.input, serde_json::json!({"path": "a.txt"}));
        assert!(step.preview.is_some(), "pending steps carry a preview");
    }

    #[tokio::test]
    async fn an_identical_pending_step_is_not_parked_twice() {
        let s = Scratch::new();
        let (p, _store) = parker(&s).await;
        let a = park_id(&p, &req("fs.delete", "fs.delete", "a.txt")).await;
        let b = park_id(&p, &req("fs.delete", "fs.delete", "a.txt")).await;
        let c = park_id(&p, &req("fs.delete", "fs.delete", "b.txt")).await;
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(p.list().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn approve_runs_once_and_records_the_result() {
        let s = Scratch::new();
        let (p, _store) = parker(&s).await;
        let id = park_id(&p, &req("fs.delete", "fs.delete", "a.txt")).await;
        let runs = AtomicUsize::new(0);
        let run = |step: ParkedStep| {
            runs.fetch_add(1, Ordering::SeqCst);
            assert_eq!(step.input, serde_json::json!({"path": "a.txt"}));
            async { Ok("deleted".to_string()) }
        };
        let done = p.resolve(&id, true, run).await.unwrap();
        assert_eq!(done.state, ParkedState::Approved);
        assert_eq!(done.result.as_deref(), Some("deleted"));
        assert!(done.resolved_at.is_some());
        let again = p
            .resolve(&id, true, |_| async { Ok::<_, String>("again".to_string()) })
            .await
            .unwrap_err();
        assert!(matches!(again, ParkError::NotPending { state: "approved", .. }), "{again:?}");
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn deny_never_runs() {
        let s = Scratch::new();
        let (p, _store) = parker(&s).await;
        let id = park_id(&p, &req("fs.delete", "fs.delete", "a.txt")).await;
        let done = p
            .resolve(&id, false, |_| async { panic!("a denied step must not run") })
            .await
            .unwrap();
        assert_eq!(done.state, ParkedState::Denied);
        assert_eq!(done.result, None);
    }

    #[tokio::test]
    async fn a_failed_run_is_recorded_as_failed() {
        let s = Scratch::new();
        let (p, _store) = parker(&s).await;
        let id = park_id(&p, &req("fs.delete", "fs.delete", "a.txt")).await;
        let done = p
            .resolve(&id, true, |_| async { Err::<String, _>("the capability `fs.delete` is not granted any more".to_string()) })
            .await
            .unwrap();
        assert_eq!(done.state, ParkedState::Failed);
        assert!(done.result.unwrap().contains("not granted"));
    }

    #[tokio::test]
    async fn an_unknown_id_is_an_error() {
        let s = Scratch::new();
        let (p, _store) = parker(&s).await;
        let err = p
            .resolve("nope", true, |_| async { Ok::<_, String>(String::new()) })
            .await
            .unwrap_err();
        assert!(matches!(err, ParkError::NotFound(_)), "{err:?}");
    }

    async fn backdate(p: &StepParker, id: &str, secs: i64) {
        let mut step = p.read_all().await.unwrap().into_iter().find(|s| s.id == id).unwrap();
        step.parked_at -= secs;
        p.write(&step).await.unwrap();
    }

    #[tokio::test]
    async fn an_expired_step_lapses_and_cannot_be_approved() {
        let s = Scratch::new();
        let (p, _store) = parker(&s).await;
        let id = park_id(&p, &req("fs.delete", "fs.delete", "a.txt")).await;
        let fresh = park_id(&p, &req("fs.delete", "fs.delete", "b.txt")).await;
        backdate(&p, &id, 8 * DAY_SECS).await;
        assert_eq!(p.lapse_expired().await.unwrap(), 1);
        let steps = p.list().await.unwrap();
        let lapsed = steps.iter().find(|s| s.id == id).unwrap();
        assert_eq!(lapsed.state, ParkedState::Lapsed);
        assert_eq!(lapsed.result.as_deref(), Some("Not reviewed within 7 days."));
        assert_eq!(steps.iter().find(|s| s.id == fresh).unwrap().state, ParkedState::Pending);
        let err = p
            .resolve(&id, true, |_| async { panic!("a lapsed step must not run") })
            .await
            .unwrap_err();
        assert!(matches!(err, ParkError::NotPending { state: "lapsed", .. }), "{err:?}");
    }

    #[tokio::test]
    async fn retention_keeps_the_newest_resolved_steps() {
        let s = Scratch::new();
        let (p, _store) = parker(&s).await;
        let base = now_unix() - 10_000;
        for i in 0..(KEEP_RESOLVED + 5) {
            let step = ParkedStep {
                id: format!("old{i:05}"),
                tool: "fs.delete".into(),
                input: serde_json::json!({"path": format!("{i}.txt")}),
                summary: String::new(),
                reason: String::new(),
                area: "fs".into(),
                origin: "routine tidy".into(),
                trust_tier: TrustTier::Trusted,
                parked_at: base,
                state: ParkedState::Denied,
                resolved_at: Some(base + i as i64),
                result: None,
                preview: None,
            };
            p.write(&step).await.unwrap();
        }
        let id = park_id(&p, &req("fs.delete", "fs.delete", "new.txt")).await;
        p.resolve(&id, false, |_| async { Ok::<_, String>(String::new()) }).await.unwrap();
        let steps = p.list().await.unwrap();
        assert_eq!(steps.len(), KEEP_RESOLVED);
        assert!(steps.iter().any(|s| s.id == id), "the newest is kept");
        assert!(!steps.iter().any(|s| s.id == "old00000"), "the oldest went");
    }

    /// An inner channel that can't ask (as an unattended run's).
    struct CantAsk;
    #[async_trait]
    impl ChannelContext for CantAsk {
        fn channel_name(&self) -> &str {
            "cant-ask"
        }
        fn platform(&self) -> ChannelPlatform {
            ChannelPlatform::Local
        }
        fn trust_tier(&self) -> TrustTier {
            TrustTier::Untrusted
        }
        fn session_id(&self) -> SessionId {
            SessionId::new()
        }
        async fn stream_event(&self, _: StreamEvent<'_>) -> Result<(), ChannelError> {
            Ok(())
        }
        async fn finalize(&self, _: &TurnOutcome) -> Result<(), ChannelError> {
            Ok(())
        }
        fn cancellation_token(&self) -> CancellationToken {
            CancellationToken::new()
        }
    }

    #[tokio::test]
    async fn the_parking_channel_parks_only_supervised_areas() {
        let s = Scratch::new();
        let (p, _store) = parker(&s).await;
        let p = Arc::new(p);
        let ch = ParkingChannel::new(Arc::new(CantAsk), Arc::clone(&p), "webhook deploy");
        assert_eq!(ch.trust_tier(), TrustTier::Untrusted, "delegates the tier");
        let parked = ch.request_approval(&req("fs.delete", "fs.delete", "a.txt")).await;
        assert!(matches!(parked, Approval::Parked { .. }), "{parked:?}");
        let refused = ch.request_approval(&req("gmail.send", "email.send", "x")).await;
        assert_eq!(refused, Approval::Unavailable);
        let steps = p.list().await.unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].origin, "webhook deploy");
    }

    #[tokio::test]
    async fn a_storage_failure_falls_back_to_refusal() {
        let s = Scratch::new();
        {
            let (p, _store) = parker(&s).await;
            park_id(&p, &req("fs.delete", "fs.delete", "a.txt")).await;
        }
        // The same file under another key: every row fails to decrypt.
        let store = open_store(&s, 4).await;
        let p = StepParker::new(store.domain(KeyDomain::ParkedSteps), supervised_fs(), 7);
        let answer = p.park(&req("fs.delete", "fs.delete", "b.txt"), "routine tidy").await;
        assert_eq!(answer, Approval::Unavailable);
    }

    fn step_for(tool: &str, input: serde_json::Value) -> ParkedStep {
        ParkedStep {
            id: "ab12cd34".into(),
            tool: tool.into(),
            input,
            summary: String::new(),
            reason: String::new(),
            area: "fs".into(),
            origin: String::new(),
            trust_tier: TrustTier::Trusted,
            parked_at: 0,
            state: ParkedState::Pending,
            resolved_at: None,
            result: None,
            preview: None,
        }
    }

    #[test]
    fn the_preview_shows_the_file_as_it_is_now() {
        let s = Scratch::new();
        std::fs::write(s.dir.join("notes.txt"), "hello").unwrap();
        std::fs::create_dir(s.dir.join("sub")).unwrap();
        let root = Some(s.dir.as_path());
        let file = preview_for(&step_for("fs.delete", serde_json::json!({"path": "notes.txt"})), root);
        assert_eq!(file.as_deref(), Some("Now: notes.txt (5 bytes)\nhello"));
        let dir = preview_for(&step_for("fs.delete", serde_json::json!({"path": "sub"})), root);
        assert_eq!(dir.as_deref(), Some("Now: sub is a folder."));
        let gone = preview_for(&step_for("fs.write", serde_json::json!({"path": "nope.txt"})), root);
        assert_eq!(gone.as_deref(), Some("Now: nope.txt doesn't exist."));
        let other = preview_for(&step_for("gmail.send", serde_json::json!({"to": "a@b.c"})), root);
        assert!(other.unwrap().contains("\"to\": \"a@b.c\""));
    }
}
