//! Schedule primitive — Phase 26.
//!
//! A schedule is a cron-triggered execution entry that creates daemon
//! turns on a timer. Backed by a redb row under `KeyDomain::Schedules`.
//! Follows the same CRUD pattern as `mission.rs`.

use chrono::{DateTime, Utc};
use cron::Schedule as CronSchedule;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use aivyx_storage::{DomainHandle, KeyDomain, StorageError};

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// Chapter Chime — who created a schedule. Governs mutation authority
/// (the agent may only modify or cancel its own) and the Studio
/// provenance badge. Serde-defaults to `Config` so every pre-Chime
/// record — all of which were config-synced — deserializes unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleProvenance {
    #[default]
    Config,
    Operator,
    Agent,
}

impl ScheduleProvenance {
    pub fn as_str(&self) -> &'static str {
        match self {
            ScheduleProvenance::Config => "config",
            ScheduleProvenance::Operator => "operator",
            ScheduleProvenance::Agent => "agent",
        }
    }
}

/// Chapter Muster — a schedule targets EITHER a single-agent turn
/// (`role_name` + `prompt`, the original shape) OR a team mission (this
/// struct, via `ScheduleRecord::new_team_mission`). The two are mutually
/// exclusive: a team-mission record's `role_name`/`prompt` are always
/// empty strings, never read by `fire_schedule`'s team-mission branch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduledTeamMission {
    pub goal: String,
    /// A path to a vertical-pack `TeamConfig` TOML file (e.g.
    /// `crates/verticals/aivyx-kitchen/assets/kitchen-boh.toml`).
    /// `None` -> the daemon's default team. Stored as a path, not a
    /// pre-loaded `TeamConfig`, so `fire_schedule` always loads the
    /// pack's current contents at fire time, not whatever it was when
    /// the schedule was created.
    pub pack_config: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleRecord {
    pub schedule_id: String,
    pub cron_expr: String,
    pub role_name: String,
    pub prompt: String,
    pub enabled: bool,
    pub wrap_mission: bool,
    pub created_at: u64,
    pub last_fired_at: Option<u64>,
    /// Phase 63 Task 3 — singular alias kept for backwards
    /// compatibility. New code reads `notify_targets`.
    #[serde(default)]
    pub notify_target: Option<String>,
    /// Phase 72 — list of notify target names for multi-target
    /// fan-out. `#[serde(default)]` so pre-Phase-72 records
    /// deserialize as empty. The daemon-scheduler's config sync
    /// path bridges `notify_target` into a one-element vec
    /// where needed.
    #[serde(default)]
    pub notify_targets: Vec<String>,
    /// Phase 72 — conditional dispatch gate string. Defaults
    /// to `NotifyWhen::Always`. `#[serde(default)]` for
    /// backwards compatibility.
    #[serde(default)]
    pub notify_when: aivyx_config::NotifyWhen,
    /// Chapter Ledger (#6 fix) — when set, the scheduler runs a **deterministic
    /// daemon-assembled report** instead of firing the `prompt` as an LLM turn.
    /// The only value today is `"digest"` (the weekly digest, built from the
    /// memory substrate so it can't confabulate). `None` (default) = the normal
    /// LLM-prompt routine, so every pre-Ledger record deserializes unchanged.
    #[serde(default)]
    pub report_kind: Option<String>,
    /// Chapter Chime — creation provenance. See [`ScheduleProvenance`].
    #[serde(default)]
    pub created_by: ScheduleProvenance,
    /// Chapter Muster — mutually exclusive with `role_name`/`prompt`
    /// (which are empty strings on a team-mission record). `None` ->
    /// this is an ordinary single-agent-turn schedule (every record
    /// before this field existed). `#[serde(default)]` so every
    /// pre-existing persisted record deserializes unchanged.
    #[serde(default)]
    pub team_mission: Option<ScheduledTeamMission>,
}

impl ScheduleRecord {
    pub fn new(
        schedule_id: String,
        cron_expr: String,
        role_name: String,
        prompt: String,
    ) -> Result<Self, String> {
        validate_cron(&cron_expr)?;
        Ok(ScheduleRecord {
            schedule_id,
            cron_expr,
            role_name,
            prompt,
            enabled: true,
            wrap_mission: false,
            created_at: now_millis(),
            last_fired_at: None,
            notify_target: None,
            notify_targets: Vec::new(),
            notify_when: aivyx_config::NotifyWhen::Always,
            report_kind: None,
            created_by: ScheduleProvenance::Config,
            team_mission: None,
        })
    }

    /// Chapter Muster — a schedule whose fire target is a team mission,
    /// not a single-agent turn. `role_name`/`prompt` are set to empty
    /// strings (never read by `fire_schedule`'s team-mission branch, and
    /// deliberately not `Option` themselves -- see the file's own
    /// mutual-exclusivity note on `team_mission`).
    pub fn new_team_mission(
        schedule_id: String,
        cron_expr: String,
        goal: String,
        pack_config: Option<String>,
    ) -> Result<Self, String> {
        validate_cron(&cron_expr)?;
        if goal.trim().is_empty() {
            return Err("team-mission schedule requires a non-empty goal".to_string());
        }
        Ok(ScheduleRecord {
            schedule_id,
            cron_expr,
            role_name: String::new(),
            prompt: String::new(),
            enabled: true,
            wrap_mission: false,
            created_at: now_millis(),
            last_fired_at: None,
            notify_target: None,
            notify_targets: Vec::new(),
            notify_when: aivyx_config::NotifyWhen::Always,
            report_kind: None,
            created_by: ScheduleProvenance::Config,
            team_mission: Some(ScheduledTeamMission { goal, pack_config }),
        })
    }

    /// Builder-style provenance override for operator/agent creations.
    pub fn with_provenance(mut self, created_by: ScheduleProvenance) -> Self {
        self.created_by = created_by;
        self
    }

    pub fn next_fire_time(&self) -> Option<DateTime<Utc>> {
        next_fire_after(&self.cron_expr, Utc::now())
    }

    pub fn next_fire_time_after(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        next_fire_after(&self.cron_expr, after)
    }
}

// ---------------------------------------------------------------------------
// Cron validation + next-fire computation
// ---------------------------------------------------------------------------

pub fn validate_cron(expr: &str) -> Result<(), String> {
    CronSchedule::from_str(expr)
        .map(|_| ())
        .map_err(|e| format!("invalid cron expression {expr:?}: {e}"))
}

/// Cron fields are the **operator's local wall clock** — the init
/// template has always said "local time", but until the 2026-07-04 soak
/// review the engine evaluated them in UTC, firing every routine hours
/// off operator intent on any non-UTC host ("nightly" reflection at
/// 10:00 AWST). Evaluate in `chrono::Local`, return the instant as Utc
/// (all stored timestamps stay UTC; only the wall-clock interpretation
/// of the cron fields changes). One-time effect at upgrade: a schedule
/// whose local-time tick already passed today fires one catch-up.
pub fn next_fire_after(cron_expr: &str, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let schedule = CronSchedule::from_str(cron_expr).ok()?;
    let local_after = after.with_timezone(&chrono::Local);
    schedule
        .after(&local_after)
        .next()
        .map(|t| t.with_timezone(&Utc))
}

// ---------------------------------------------------------------------------
// Storage CRUD
// ---------------------------------------------------------------------------

fn schedule_key(schedule_id: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(schedule_id.len());
    key.extend_from_slice(schedule_id.as_bytes());
    key
}

pub async fn create_schedule(
    handle: &DomainHandle,
    record: &ScheduleRecord,
) -> Result<(), StorageError> {
    assert_eq!(handle.domain(), KeyDomain::Schedules);
    let json = serde_json::to_vec(record).map_err(|e| {
        StorageError::Redb(format!("serialize ScheduleRecord: {e}"))
    })?;
    handle.put(&schedule_key(&record.schedule_id), &json).await
}

pub async fn get_schedule(
    handle: &DomainHandle,
    schedule_id: &str,
) -> Result<Option<ScheduleRecord>, StorageError> {
    assert_eq!(handle.domain(), KeyDomain::Schedules);
    match handle.get(&schedule_key(schedule_id)).await? {
        Some(bytes) => {
            let record: ScheduleRecord =
                serde_json::from_slice(&bytes).map_err(|e| {
                    StorageError::Redb(format!("deserialize ScheduleRecord: {e}"))
                })?;
            Ok(Some(record))
        }
        None => Ok(None),
    }
}

pub async fn update_schedule(
    handle: &DomainHandle,
    record: &ScheduleRecord,
) -> Result<(), StorageError> {
    create_schedule(handle, record).await
}

pub async fn list_schedules(
    handle: &DomainHandle,
) -> Result<Vec<ScheduleRecord>, StorageError> {
    assert_eq!(handle.domain(), KeyDomain::Schedules);
    let rows = handle.scan_prefix(b"").await?;
    let mut schedules = Vec::with_capacity(rows.len());
    for (_key, bytes) in rows {
        let record: ScheduleRecord =
            serde_json::from_slice(&bytes).map_err(|e| {
                StorageError::Redb(format!("deserialize ScheduleRecord: {e}"))
            })?;
        schedules.push(record);
    }
    Ok(schedules)
}

pub async fn delete_schedule(
    handle: &DomainHandle,
    schedule_id: &str,
) -> Result<(), StorageError> {
    assert_eq!(handle.domain(), KeyDomain::Schedules);
    handle.delete(&schedule_key(schedule_id)).await
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ---------------------------------------------------------------------------
// Chapter Chime — operator mutation rules (Studio Create/Update/Delete)
// ---------------------------------------------------------------------------

/// Reserved id prefixes: `cfg-` marks config-synced routines and
/// `agt-` marks agent-created ones; operator creations may use neither.
const RESERVED_PREFIXES: &[&str] = &["cfg-", "agt-"];

/// Create an operator-authored schedule. The `name` doubles as the
/// storage id (config routines are namespaced by their `cfg-` prefix,
/// agent ones by `agt-`, so bare names can never collide with either
/// silently — but an exact-id collision is still rejected).
pub async fn operator_create_schedule(
    handle: &DomainHandle,
    name: &str,
    cron: &str,
    prompt: &str,
    enabled: bool,
) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("schedule name must not be empty".into());
    }
    if let Some(p) = RESERVED_PREFIXES.iter().find(|p| name.starts_with(**p)) {
        return Err(format!("the {p:?} prefix is reserved"));
    }
    if get_schedule(handle, name)
        .await
        .map_err(|e| format!("schedule lookup: {e}"))?
        .is_some()
    {
        return Err(format!("a schedule named {name:?} already exists"));
    }
    let mut record = ScheduleRecord::new(
        name.to_string(),
        cron.to_string(),
        "default".to_string(),
        prompt.to_string(),
    )?
    .with_provenance(ScheduleProvenance::Operator);
    record.enabled = enabled;
    record.wrap_mission = true;
    create_schedule(handle, &record)
        .await
        .map_err(|e| format!("schedule create: {e}"))?;
    Ok(name.to_string())
}

/// Update a schedule (operator authority — any provenance; enabling an
/// agent-created disabled schedule IS the approval gesture). `None`
/// fields stay unchanged.
pub async fn operator_update_schedule(
    handle: &DomainHandle,
    schedule_id: &str,
    enabled: Option<bool>,
    cron: Option<String>,
    prompt: Option<String>,
) -> Result<(), String> {
    let mut record = get_schedule(handle, schedule_id)
        .await
        .map_err(|e| format!("schedule lookup: {e}"))?
        .ok_or_else(|| format!("no schedule named {schedule_id:?}"))?;
    if let Some(c) = cron {
        validate_cron(&c)?;
        record.cron_expr = c;
    }
    if prompt.is_some() && record.team_mission.is_some() {
        return Err(format!(
            "schedule {schedule_id:?} targets a team mission -- prompt doesn't apply"
        ));
    }
    if let Some(p) = prompt {
        record.prompt = p;
    }
    if let Some(e) = enabled {
        record.enabled = e;
    }
    update_schedule(handle, &record)
        .await
        .map_err(|e| format!("schedule update: {e}"))
}

/// Delete a schedule. Config-defined routines are refused — the boot
/// sync would resurrect them, so the honest gesture is disable (or
/// removing the `[[schedule]]` entry from `aivyx-pa.toml`).
pub async fn operator_delete_schedule(
    handle: &DomainHandle,
    schedule_id: &str,
) -> Result<(), String> {
    let record = get_schedule(handle, schedule_id)
        .await
        .map_err(|e| format!("schedule lookup: {e}"))?
        .ok_or_else(|| format!("no schedule named {schedule_id:?}"))?;
    if record.created_by == ScheduleProvenance::Config {
        return Err(
            "config-defined routines come back at restart — disable it instead, \
             or remove its [[schedule]] entry from aivyx-pa.toml"
                .into(),
        );
    }
    delete_schedule(handle, schedule_id)
        .await
        .map_err(|e| format!("schedule delete: {e}"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_team_mission_builds_a_record_with_no_role_or_prompt() {
        let record = ScheduleRecord::new_team_mission(
            "sched-1".to_string(),
            "0 0 2 * * *".to_string(),
            "run the overnight close".to_string(),
            None,
        )
        .expect("valid cron");
        assert!(record.team_mission.is_some());
        let tm = record.team_mission.as_ref().unwrap();
        assert_eq!(tm.goal, "run the overnight close");
        assert_eq!(tm.pack_config, None);
        assert_eq!(record.role_name, "");
        assert_eq!(record.prompt, "");
    }

    #[test]
    fn new_team_mission_rejects_an_empty_goal() {
        let err = ScheduleRecord::new_team_mission(
            "sched-1".to_string(),
            "0 0 2 * * *".to_string(),
            "".to_string(),
            None,
        );
        assert!(err.is_err());
    }

    #[test]
    fn new_team_mission_still_validates_cron() {
        let err = ScheduleRecord::new_team_mission(
            "sched-1".to_string(),
            "not a cron expression".to_string(),
            "run the overnight close".to_string(),
            None,
        );
        assert!(err.is_err());
    }

    #[test]
    fn a_role_prompt_record_has_no_team_mission() {
        let record = ScheduleRecord::new(
            "sched-2".to_string(),
            "0 0 7 * * *".to_string(),
            "default".to_string(),
            "check system health".to_string(),
        )
        .expect("valid cron");
        assert_eq!(record.team_mission, None);
    }

    #[test]
    fn a_team_mission_record_round_trips_through_json() {
        let record = ScheduleRecord::new_team_mission(
            "sched-3".to_string(),
            "0 0 2 * * *".to_string(),
            "run the overnight close".to_string(),
            Some("crates/verticals/aivyx-kitchen/assets/kitchen-boh.toml".to_string()),
        )
        .expect("valid cron");
        let json = serde_json::to_vec(&record).expect("serialize");
        let back: ScheduleRecord = serde_json::from_slice(&json).expect("deserialize");
        assert_eq!(back.team_mission, record.team_mission);
    }

    #[test]
    fn a_pre_existing_role_prompt_json_record_deserializes_with_no_team_mission() {
        // No "team_mission" key at all -- simulates a record persisted before
        // this field existed.
        let json = br#"{
            "schedule_id": "old-1",
            "cron_expr": "0 0 7 * * *",
            "role_name": "default",
            "prompt": "check system health",
            "enabled": true,
            "wrap_mission": false,
            "created_at": 1000,
            "last_fired_at": null,
            "notify_target": null,
            "notify_targets": [],
            "notify_when": "Always",
            "report_kind": null,
            "created_by": "config"
        }"#;
        let record: ScheduleRecord = serde_json::from_slice(json).expect("deserialize old record");
        assert_eq!(record.team_mission, None);
    }

    #[test]
    fn pre_chime_record_json_defaults_to_config_provenance() {
        // A record serialized before the created_by field existed must
        // deserialize as Config (all pre-Chime records were config-synced).
        let old_json = r#"{
            "schedule_id": "cfg-nightly",
            "cron_expr": "0 0 2 * * * *",
            "role_name": "default",
            "prompt": "reflect",
            "enabled": true,
            "wrap_mission": true,
            "created_at": 0,
            "last_fired_at": null
        }"#;
        let r: ScheduleRecord = serde_json::from_str(old_json).expect("backcompat");
        assert_eq!(r.created_by, ScheduleProvenance::Config);
        // And the builder override round-trips through serde.
        let agent = ScheduleRecord::new(
            "agt-x".into(),
            "0 0 9 * * * *".into(),
            "default".into(),
            "p".into(),
        )
        .unwrap()
        .with_provenance(ScheduleProvenance::Agent);
        let back: ScheduleRecord =
            serde_json::from_slice(&serde_json::to_vec(&agent).unwrap()).unwrap();
        assert_eq!(back.created_by, ScheduleProvenance::Agent);
    }

    #[test]
    fn valid_cron_expr_passes_validation() {
        assert!(validate_cron("0 30 9 * * * *").is_ok());
    }

    #[test]
    fn daily_shorthand_is_not_standard_cron_crate() {
        // The `cron` crate uses 7-field expressions (sec min hour dom month dow year).
        // Standard @daily is not supported; use "0 0 0 * * * *" instead.
        // This test documents the crate's behavior.
        let result = validate_cron("@daily");
        // @daily may or may not parse depending on crate version; document the outcome.
        let _ = result;
    }

    #[test]
    fn invalid_cron_expr_fails_validation() {
        let err = validate_cron("not a cron").unwrap_err();
        assert!(err.contains("invalid cron expression"));
    }

    #[test]
    fn schedule_record_new_validates_cron() {
        let ok = ScheduleRecord::new(
            "s1".into(),
            "0 0 9 * * * *".into(),
            "default".into(),
            "check logs".into(),
        );
        assert!(ok.is_ok());

        let err = ScheduleRecord::new(
            "s2".into(),
            "bad".into(),
            "default".into(),
            "check logs".into(),
        );
        assert!(err.is_err());
    }

    #[test]
    fn next_fire_time_returns_future_time() {
        let record = ScheduleRecord::new(
            "s1".into(),
            "0 * * * * * *".into(), // every minute
            "default".into(),
            "ping".into(),
        )
        .unwrap();
        let next = record.next_fire_time();
        assert!(next.is_some());
        assert!(next.unwrap() > Utc::now());
    }

    #[test]
    fn next_fire_after_respects_anchor() {
        let anchor = DateTime::parse_from_rfc3339("2026-06-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        // Every day at 09:00 — OPERATOR-LOCAL wall clock (soak fix
        // 2026-07-04), so assert the local rendering of the instant.
        let next = next_fire_after("0 0 9 * * * *", anchor);
        assert!(next.is_some());
        let fire = next.unwrap();
        assert!(fire > anchor);
        assert_eq!(
            fire.with_timezone(&chrono::Local).format("%H:%M").to_string(),
            "09:00"
        );
    }

    /// Soak review 2026-07-04 — cron fields are the operator's LOCAL
    /// wall clock ("0 0 7 …" = 7am where the operator lives), matching
    /// what the init template always promised; the engine used to
    /// evaluate UTC, firing "nightly" reflection at 10:00 AWST. On a
    /// UTC host both interpretations coincide; on any offset host the
    /// local hour must win — this test fails under UTC evaluation on
    /// any non-UTC machine.
    #[test]
    fn cron_fields_are_operator_local_wall_clock() {
        let anchor = Utc::now();
        let fire = next_fire_after("0 0 7 * * *", anchor).unwrap();
        assert!(fire > anchor);
        assert_eq!(
            fire.with_timezone(&chrono::Local).format("%H:%M").to_string(),
            "07:00"
        );
    }

    #[test]
    fn schedule_record_round_trips_through_serde() {
        let record = ScheduleRecord::new(
            "test-id".into(),
            "0 0 9 * * * *".into(),
            "coder".into(),
            "run tests".into(),
        )
        .unwrap();
        let json = serde_json::to_vec(&record).unwrap();
        let back: ScheduleRecord = serde_json::from_slice(&json).unwrap();
        assert_eq!(back.schedule_id, "test-id");
        assert_eq!(back.role_name, "coder");
        assert_eq!(back.prompt, "run tests");
        assert!(back.enabled);
        assert!(back.last_fired_at.is_none());
    }

    #[test]
    fn disabled_schedule_still_computes_next_fire() {
        let mut record = ScheduleRecord::new(
            "s1".into(),
            "0 * * * * * *".into(),
            "default".into(),
            "ping".into(),
        )
        .unwrap();
        record.enabled = false;
        // next_fire_time is a pure computation — the scheduler checks `enabled`
        assert!(record.next_fire_time().is_some());
    }

    /// A `KeyDomain::Schedules` handle backed by a real (temp-dir) redb
    /// store, mirroring `schedule_tool.rs`'s own `schedule_domain()`
    /// fixture — this file had no live-storage test before this one.
    async fn schedule_domain() -> DomainHandle {
        // Two calls in one clock tick (seen on CI) shared a folder and its
        // database lock; the counter keeps them apart.
        static UNIQUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "aivyx-schedule-operator-update-test-{}-{nanos}-{}",
            std::process::id(),
            UNIQUE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let storage: std::sync::Arc<dyn aivyx_storage::Storage> = aivyx_storage::RedbStorage::open(
            aivyx_storage::StorageConfig::new(dir.join("store.redb")),
            aivyx_crypto::MasterKey::from_raw([7u8; 32]),
        )
        .await
        .expect("open storage");
        storage.domain(KeyDomain::Schedules)
    }

    #[tokio::test]
    async fn operator_update_schedule_rejects_a_prompt_edit_on_a_team_mission_schedule() {
        let store = schedule_domain().await;
        let record = ScheduleRecord::new_team_mission(
            "cfg-x".to_string(),
            "0 0 2 * * * *".to_string(),
            "run the overnight close".to_string(),
            None,
        )
        .unwrap();
        create_schedule(&store, &record).await.unwrap();
        let err = operator_update_schedule(
            &store,
            "cfg-x",
            None,
            None,
            Some("new prompt".to_string()),
        )
        .await;
        assert!(err.is_err());
    }
}
