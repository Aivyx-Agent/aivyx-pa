//! Schedule agent tools — Phase 26 Tasks 4–5.
//!
//! Four tools following the `OnceLock`-factory pattern from
//! `MissionCreateTool`:
//!
//! - `schedule.create` — create a new cron schedule
//! - `schedule.list` — list all schedules
//! - `schedule.delete` — delete a schedule by ID
//! - `schedule.update` — update fields on an existing schedule

use std::sync::OnceLock;

use async_trait::async_trait;
use serde_json::{json, Value};

use aivyx_capability::Scope;
use aivyx_core::{AivyxError, Tool, ToolContext, ToolId, ToolOutcome, Verification};
use aivyx_storage::{DomainHandle, KeyDomain};

use aivyx_config::GrowthAdoption;

use crate::schedule::{self, ScheduleProvenance, ScheduleRecord};

// ---------------------------------------------------------------------------
// Chapter Chime — agent self-scheduling rules
// ---------------------------------------------------------------------------
//
// The WRITE half (create/update/delete) was registered at Phase 26 but never
// granted, parked on an [autonomy] gating decision. Chapter Chime resolves it
// (operator sign-off 2026-07-06):
// - creations follow the Reins growth gradient: below `policy_auto` they land
//   DISABLED pending operator approval in the Studio; at `policy_auto`/
//   `broad_auto` they arm directly (audited);
// - the agent may only update/delete schedules it created (`agt-` ids);
//   an update below `policy_auto` re-disables the schedule (an edit
//   invalidates the operator's approval);
// - guardrails: no agent schedule may fire more often than every 15 minutes,
//   and at most 10 agent-created schedules may exist.

/// Minimum gap between consecutive fires of an agent-created schedule.
const MIN_FIRE_GAP_SECS: i64 = 15 * 60;
/// Cap on concurrently existing agent-created schedules.
const MAX_AGENT_SCHEDULES: usize = 10;

/// Two consecutive future fires closer than the floor ⇒ too frequent.
/// A cron with no second fire (one-shot with a year field) passes.
fn fires_too_frequently(cron: &str) -> bool {
    let now = chrono::Utc::now();
    let Some(first) = schedule::next_fire_after(cron, now) else {
        return false;
    };
    let Some(second) = schedule::next_fire_after(cron, first) else {
        return false;
    };
    (second - first).num_seconds() < MIN_FIRE_GAP_SECS
}

/// Whether the growth gradient lets agent creations arm without approval.
fn growth_arms_directly(growth: GrowthAdoption) -> bool {
    matches!(growth, GrowthAdoption::PolicyAuto | GrowthAdoption::BroadAuto)
}

// ---------------------------------------------------------------------------
// schedule.create
// ---------------------------------------------------------------------------

pub struct ScheduleCreateTool {
    id: ToolId,
    schema: Value,
    store: OnceLock<DomainHandle>,
    /// Chapter Chime — the resolved growth gradient; unset is treated
    /// as `ProposeOnly` (the safe default).
    growth: OnceLock<GrowthAdoption>,
}

impl std::fmt::Debug for ScheduleCreateTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScheduleCreateTool")
            .field("id", &self.id)
            .finish()
    }
}

impl Default for ScheduleCreateTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ScheduleCreateTool {
    pub fn new() -> Self {
        ScheduleCreateTool {
            id: ToolId::new(),
            schema: json!({
                "type": "object",
                "properties": {
                    "cron": {
                        "type": "string",
                        "description": "Cron expression (7-field: sec min hour dom month dow year), in the operator's LOCAL time — don't convert to UTC. Examples: \"0 0 9 * * * *\" daily at 09:00; \"0 30 8 * * Mon *\" Mondays at 08:30; \"0 0 18 * * Mon-Fri *\" weekdays at 18:00. Write days as names (Mon…Sun): numbered days count from Sunday = 1, so 1 is NOT Monday."
                    },
                    "role": {
                        "type": "string",
                        "description": "Role name to run the scheduled turn under."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "The prompt text submitted as a turn when the schedule fires. Mutually exclusive with `goal` -- set exactly one."
                    },
                    "goal": {
                        "type": "string",
                        "description": "Instead of `prompt`, delegate this goal to a durable team mission (the Nonagon, the daemon's default team) when the schedule fires, rather than a single-agent turn. Mutually exclusive with `prompt` -- set exactly one. Always runs on the daemon's default team -- picking a specific vertical pack is an operator-only setting (`aivyx-pa.toml`'s own `[schedule.team_mission] pack_config`), not available here, since a pack file can grant its own lead capability scopes and that authority decision belongs to the operator, not the model."
                    }
                },
                "required": ["cron"]
            }),
            store: OnceLock::new(),
            growth: OnceLock::new(),
        }
    }

    pub fn set_schedule_store(&self, handle: DomainHandle) -> Result<(), DomainHandle> {
        assert_eq!(handle.domain(), KeyDomain::Schedules);
        self.store.set(handle)
    }

    /// Chapter Chime — wire the resolved growth gradient at daemon startup.
    pub fn set_growth(&self, growth: GrowthAdoption) -> Result<(), GrowthAdoption> {
        self.growth.set(growth)
    }
}

#[async_trait]
impl Tool for ScheduleCreateTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "schedule.create"
    }

    fn description(&self) -> &str {
        "Create a new cron-triggered schedule. When the schedule fires, \
         the daemon submits the prompt as a turn under the specified role. \
         Alternatively, set `goal` instead of `prompt` to delegate to a \
         durable team mission (the Nonagon, always the daemon's default \
         team) when the schedule fires, instead of a single-agent turn -- \
         the two are mutually exclusive. The cron expression uses 7 \
         fields (sec min hour dom month dow year) in the operator's local \
         time, with days written as names (Mon…Sun). Returns the \
         schedule_id for future reference."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("schedule.create").expect("known base")
    }

    async fn execute(&self, input: Value, ctx: &ToolContext<'_>) -> ToolOutcome {
        let Some(store) = self.store.get() else {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "schedule.create: no schedule store configured".to_string(),
            });
        };

        let cron = input
            .get("cron")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if cron.is_empty() {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "schedule.create requires a non-empty `cron` field".to_string(),
            });
        }

        if ctx.message_origin == aivyx_core::MessageOrigin::System {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "schedule.create: cannot be called from within a triggered or \
                         scheduled run — creating new schedules is an operator/interactive-\
                         only action, to prevent unattended runs from recursively \
                         propagating more automation"
                    .to_string(),
            });
        }

        let prompt = input
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let goal = input
            .get("goal")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let role = input
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("default")
            .to_string();

        // Chapter Muster — `prompt` (single-agent turn) and `goal`
        // (team mission) are mutually exclusive, same rule as the
        // ScheduleRecord/ScheduleConfig layers below this tool.
        if !prompt.is_empty() && !goal.is_empty() {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "schedule.create: set exactly one of `prompt` or `goal`, not both"
                    .to_string(),
            });
        }
        if prompt.is_empty() && goal.is_empty() {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "schedule.create requires exactly one of `prompt` or `goal`".to_string(),
            });
        }

        // Chapter Chime guardrails — gradient, frequency floor, count cap.
        let growth = self
            .growth
            .get()
            .copied()
            .unwrap_or(GrowthAdoption::ProposeOnly);
        if growth == GrowthAdoption::None {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "schedule.create: the autonomy level does not permit \
                         self-scheduling"
                    .to_string(),
            });
        }
        if fires_too_frequently(&cron) {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!(
                    "schedule.create: agent schedules may not fire more often \
                     than every {} minutes",
                    MIN_FIRE_GAP_SECS / 60
                ),
            });
        }
        match schedule::list_schedules(store).await {
            Ok(all) => {
                let agent_count = all
                    .iter()
                    .filter(|r| r.created_by == ScheduleProvenance::Agent)
                    .count();
                if agent_count >= MAX_AGENT_SCHEDULES {
                    return ToolOutcome::Failed(AivyxError::Tool {
                        tool: self.id,
                        detail: format!(
                            "schedule.create: the agent-created schedule cap \
                             ({MAX_AGENT_SCHEDULES}) is reached — cancel one \
                             first (schedule.list, then schedule.delete)"
                        ),
                    });
                }
            }
            Err(e) => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!("failed to list schedules: {e}"),
                });
            }
        }

        let schedule_id = format!("agt-{}", uuid::Uuid::new_v4().as_simple());
        let mut record = if !goal.is_empty() {
            match ScheduleRecord::new_team_mission(schedule_id.clone(), cron, goal, None) {
                Ok(r) => r.with_provenance(ScheduleProvenance::Agent),
                Err(e) => {
                    return ToolOutcome::Failed(AivyxError::Tool {
                        tool: self.id,
                        detail: format!("schedule.create: {e}"),
                    });
                }
            }
        } else {
            match ScheduleRecord::new(schedule_id.clone(), cron, role, prompt) {
                Ok(r) => r.with_provenance(ScheduleProvenance::Agent),
                Err(e) => {
                    return ToolOutcome::Failed(AivyxError::Tool {
                        tool: self.id,
                        detail: format!("schedule.create: {e}"),
                    });
                }
            }
        };
        let armed = growth_arms_directly(growth);
        record.enabled = armed;
        record.wrap_mission = true;

        if let Err(e) = schedule::create_schedule(store, &record).await {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!("failed to persist schedule: {e}"),
            });
        }

        let output = if armed {
            json!({
                "schedule_id": schedule_id,
                "status": "armed",
                "next_fire": record.next_fire_time().map(|dt| dt.to_rfc3339()),
            })
        } else {
            json!({
                "schedule_id": schedule_id,
                "status": "pending_approval",
                "note": "created disabled — the operator enables it in the \
                         Studio Schedules screen",
            })
        };
        ToolOutcome::Completed {
            output,
            verified: Verification::NotApplicable,
        }
    }
}

// ---------------------------------------------------------------------------
// schedule.list
// ---------------------------------------------------------------------------

pub struct ScheduleListTool {
    id: ToolId,
    schema: Value,
    store: OnceLock<DomainHandle>,
}

impl std::fmt::Debug for ScheduleListTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScheduleListTool")
            .field("id", &self.id)
            .finish()
    }
}

impl Default for ScheduleListTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ScheduleListTool {
    pub fn new() -> Self {
        ScheduleListTool {
            id: ToolId::new(),
            schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            store: OnceLock::new(),
        }
    }

    pub fn set_schedule_store(&self, handle: DomainHandle) -> Result<(), DomainHandle> {
        assert_eq!(handle.domain(), KeyDomain::Schedules);
        self.store.set(handle)
    }
}

#[async_trait]
impl Tool for ScheduleListTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "schedule.list"
    }

    fn description(&self) -> &str {
        "List all cron schedules. Returns an array of schedule objects \
         with id, cron expression, role, prompt, enabled status, and \
         next fire time."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("schedule.list").expect("known base")
    }

    async fn execute(&self, _input: Value, ctx: &ToolContext<'_>) -> ToolOutcome {
        let Some(store) = self.store.get() else {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "schedule.list: no schedule store configured".to_string(),
            });
        };

        // schedule.list is read-only and idempotent, no origin guard needed.
        let _ = ctx;

        match schedule::list_schedules(store).await {
            Ok(schedules) => {
                let entries: Vec<Value> = schedules
                    .iter()
                    .map(|s| {
                        let next = s.next_fire_time().map(|dt| dt.to_rfc3339());
                        json!({
                            "schedule_id": s.schedule_id,
                            "cron": s.cron_expr,
                            "role": s.role_name,
                            "prompt": s.prompt,
                            "enabled": s.enabled,
                            "next_fire": next,
                        })
                    })
                    .collect();
                ToolOutcome::Completed {
                    output: json!({ "schedules": entries }),
                    verified: Verification::NotApplicable,
                }
            }
            Err(e) => ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!("failed to list schedules: {e}"),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// schedule.delete
// ---------------------------------------------------------------------------

pub struct ScheduleDeleteTool {
    id: ToolId,
    schema: Value,
    store: OnceLock<DomainHandle>,
}

impl std::fmt::Debug for ScheduleDeleteTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScheduleDeleteTool")
            .field("id", &self.id)
            .finish()
    }
}

impl Default for ScheduleDeleteTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ScheduleDeleteTool {
    pub fn new() -> Self {
        ScheduleDeleteTool {
            id: ToolId::new(),
            schema: json!({
                "type": "object",
                "properties": {
                    "schedule_id": {
                        "type": "string",
                        "description": "The ID of the schedule to delete."
                    }
                },
                "required": ["schedule_id"]
            }),
            store: OnceLock::new(),
        }
    }

    pub fn set_schedule_store(&self, handle: DomainHandle) -> Result<(), DomainHandle> {
        assert_eq!(handle.domain(), KeyDomain::Schedules);
        self.store.set(handle)
    }
}

#[async_trait]
impl Tool for ScheduleDeleteTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "schedule.delete"
    }

    fn description(&self) -> &str {
        "Delete a cron schedule by ID. The schedule will no longer fire. \
         Returns whether the schedule was found and deleted."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("schedule.delete").expect("known base")
    }

    async fn execute(&self, input: Value, ctx: &ToolContext<'_>) -> ToolOutcome {
        let Some(store) = self.store.get() else {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "schedule.delete: no schedule store configured".to_string(),
            });
        };

        let schedule_id = input
            .get("schedule_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if schedule_id.is_empty() {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "schedule.delete requires a non-empty `schedule_id` field".to_string(),
            });
        }

        if ctx.message_origin == aivyx_core::MessageOrigin::System {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "schedule.delete: cannot be called from within a triggered or \
                         scheduled run — deleting schedules is an operator/interactive-\
                         only action, to prevent unattended runs from recursively \
                         propagating more automation"
                    .to_string(),
            });
        }

        let record = match schedule::get_schedule(store, &schedule_id).await {
            Ok(Some(r)) => Some(r),
            Ok(None) => None,
            Err(e) => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!("failed to check schedule: {e}"),
                });
            }
        };

        let Some(record) = record else {
            return ToolOutcome::Completed {
                output: json!({
                    "deleted": false,
                    "reason": format!("schedule {schedule_id} not found")
                }),
                verified: Verification::NotApplicable,
            };
        };
        // Chapter Chime — own-schedules-only authority: the agent may
        // delete what it created, never config routines or the
        // operator's.
        if record.created_by != ScheduleProvenance::Agent {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!(
                    "schedule.delete: {schedule_id} was created by the \
                     {} — only agent-created schedules (agt-…) may be \
                     deleted; ask the operator",
                    record.created_by.as_str()
                ),
            });
        }

        if let Err(e) = schedule::delete_schedule(store, &schedule_id).await {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!("failed to delete schedule: {e}"),
            });
        }

        ToolOutcome::Completed {
            output: json!({ "deleted": true, "schedule_id": schedule_id }),
            verified: Verification::NotApplicable,
        }
    }
}

// ---------------------------------------------------------------------------
// schedule.update
// ---------------------------------------------------------------------------

pub struct ScheduleUpdateTool {
    id: ToolId,
    schema: Value,
    store: OnceLock<DomainHandle>,
    /// Chapter Chime — see [`ScheduleCreateTool::set_growth`].
    growth: OnceLock<GrowthAdoption>,
}

impl std::fmt::Debug for ScheduleUpdateTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScheduleUpdateTool")
            .field("id", &self.id)
            .finish()
    }
}

impl Default for ScheduleUpdateTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ScheduleUpdateTool {
    pub fn new() -> Self {
        ScheduleUpdateTool {
            id: ToolId::new(),
            schema: json!({
                "type": "object",
                "properties": {
                    "schedule_id": {
                        "type": "string",
                        "description": "The ID of the schedule to update."
                    },
                    "enabled": {
                        "type": "boolean",
                        "description": "Set to true to enable or false to disable the schedule."
                    },
                    "cron": {
                        "type": "string",
                        "description": "New cron expression (7-field: sec min hour dom month dow year), in the operator's LOCAL time; write days as names (Mon…Sun) — numbered days count from Sunday = 1."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "New prompt text for the scheduled turn."
                    },
                    "role": {
                        "type": "string",
                        "description": "New role name for the scheduled turn."
                    }
                },
                "required": ["schedule_id"]
            }),
            store: OnceLock::new(),
            growth: OnceLock::new(),
        }
    }

    pub fn set_schedule_store(&self, handle: DomainHandle) -> Result<(), DomainHandle> {
        assert_eq!(handle.domain(), KeyDomain::Schedules);
        self.store.set(handle)
    }

    /// Chapter Chime — wire the resolved growth gradient at daemon startup.
    pub fn set_growth(&self, growth: GrowthAdoption) -> Result<(), GrowthAdoption> {
        self.growth.set(growth)
    }
}

#[async_trait]
impl Tool for ScheduleUpdateTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "schedule.update"
    }

    fn description(&self) -> &str {
        "Update an existing schedule. Provide the schedule_id and any \
         fields to change: enabled (true/false), cron expression, prompt, \
         or role. Unspecified fields are left unchanged. Returns the \
         updated schedule."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("schedule.update").expect("known base")
    }

    async fn execute(&self, input: Value, ctx: &ToolContext<'_>) -> ToolOutcome {
        let Some(store) = self.store.get() else {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "schedule.update: no schedule store configured".to_string(),
            });
        };

        let schedule_id = input
            .get("schedule_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if schedule_id.is_empty() {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "schedule.update requires a non-empty `schedule_id` field".to_string(),
            });
        }

        if ctx.message_origin == aivyx_core::MessageOrigin::System {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "schedule.update: cannot be called from within a triggered or \
                         scheduled run — updating schedules is an operator/interactive-\
                         only action, to prevent unattended runs from recursively \
                         propagating more automation"
                    .to_string(),
            });
        }

        let mut record = match schedule::get_schedule(store, &schedule_id).await {
            Ok(Some(r)) => r,
            Ok(None) => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!("schedule {schedule_id} not found"),
                });
            }
            Err(e) => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!("failed to read schedule: {e}"),
                });
            }
        };

        // Chapter Chime — own-schedules-only authority.
        if record.created_by != ScheduleProvenance::Agent {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!(
                    "schedule.update: {schedule_id} was created by the {} — \
                     only agent-created schedules (agt-…) may be updated; \
                     ask the operator",
                    record.created_by.as_str()
                ),
            });
        }

        if let Some(enabled) = input.get("enabled").and_then(|v| v.as_bool()) {
            record.enabled = enabled;
        }

        if let Some(cron) = input.get("cron").and_then(|v| v.as_str()) {
            if let Err(e) = schedule::validate_cron(cron) {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!("schedule.update: {e}"),
                });
            }
            record.cron_expr = cron.to_string();
        }

        if (input.get("prompt").is_some() || input.get("role").is_some())
            && record.team_mission.is_some()
        {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!(
                    "schedule.update: {schedule_id} targets a team mission -- \
                     `prompt`/`role` don't apply and editing them would silently \
                     do nothing (the schedule always dispatches its own `goal`); \
                     delete and recreate it with schedule.create to change the goal"
                ),
            });
        }

        if let Some(prompt) = input.get("prompt").and_then(|v| v.as_str()) {
            if prompt.is_empty() {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: "schedule.update: prompt cannot be empty".to_string(),
                });
            }
            record.prompt = prompt.to_string();
        }

        if let Some(role) = input.get("role").and_then(|v| v.as_str()) {
            record.role_name = role.to_string();
        }

        // Chime guardrails apply to edits too: the frequency floor, and
        // below `policy_auto` any edit re-disables the schedule — an
        // agent edit invalidates the operator's approval (and blocks the
        // self-approval bypass of setting `enabled: true` directly).
        if fires_too_frequently(&record.cron_expr) {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!(
                    "schedule.update: agent schedules may not fire more often \
                     than every {} minutes",
                    MIN_FIRE_GAP_SECS / 60
                ),
            });
        }
        let growth = self
            .growth
            .get()
            .copied()
            .unwrap_or(GrowthAdoption::ProposeOnly);
        let reapproval_needed = !growth_arms_directly(growth) && record.enabled;
        if reapproval_needed {
            record.enabled = false;
        }

        if let Err(e) = schedule::update_schedule(store, &record).await {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!("failed to persist schedule update: {e}"),
            });
        }

        let next = record.next_fire_time().map(|dt| dt.to_rfc3339());
        let mut output = json!({
            "schedule_id": record.schedule_id,
            "cron": record.cron_expr,
            "role": record.role_name,
            "prompt": record.prompt,
            "enabled": record.enabled,
            "next_fire": next,
        });
        if reapproval_needed {
            output["note"] = json!(
                "edit saved but disabled — the operator re-approves it in \
                 the Studio Schedules screen"
            );
        }
        ToolOutcome::Completed {
            output,
            verified: Verification::NotApplicable,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_core::{
        AgentId, CancellationToken, ChannelContext, ChannelError,
        ChannelPlatform, SessionId, StreamEvent, TurnId, TurnOutcome,
    };
    use aivyx_crypto::MasterKey;
    use aivyx_storage::{RedbStorage, Storage, StorageConfig};
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    // NoopChannel/NoopAudit/ctx_parts/make_ctx -- copied from
    // reminder_tool.rs's own test module (the closest real, working
    // ToolContext fixture in this crate; this file's own tests never
    // called `.execute()` before this task).
    struct NoopChannel {
        session: SessionId,
        token: CancellationToken,
    }
    #[async_trait]
    impl ChannelContext for NoopChannel {
        fn session_id(&self) -> SessionId {
            self.session
        }
        fn platform(&self) -> ChannelPlatform {
            ChannelPlatform::Local
        }
        fn channel_name(&self) -> &str {
            "test"
        }
        fn trust_tier(&self) -> aivyx_capability::TrustTier {
            aivyx_capability::TrustTier::Trusted
        }
        async fn stream_event(
            &self,
            _event: StreamEvent<'_>,
        ) -> Result<(), ChannelError> {
            Ok(())
        }
        async fn finalize(
            &self,
            _outcome: &TurnOutcome,
        ) -> Result<(), ChannelError> {
            Ok(())
        }
        fn cancellation_token(&self) -> CancellationToken {
            self.token.clone()
        }
    }
    struct NoopAudit;
    impl aivyx_core::AuditHook for NoopAudit {
        fn on_event(&self, _tag: aivyx_core::AuditTag) {}
    }
    fn ctx_parts() -> (NoopChannel, NoopAudit) {
        (
            NoopChannel {
                session: SessionId::new(),
                token: CancellationToken::new(),
            },
            NoopAudit,
        )
    }
    fn make_ctx<'a>(
        ch: &'a NoopChannel,
        audit: &'a dyn aivyx_core::AuditHook,
        message_origin: aivyx_core::MessageOrigin,
    ) -> ToolContext<'a> {
        ToolContext {
            agent_id: AgentId::new(),
            session_id: ch.session,
            turn_id: TurnId::new(),
            channel: ch,
            audit,
            cancellation: &ch.token,
            message_origin,
        }
    }

    /// A `KeyDomain::Schedules` handle, mirroring
    /// `team_mission_driver.rs`'s own `team_domain()` shape.
    async fn schedule_domain() -> DomainHandle {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir()
            .join(format!("aivyx-schedule-tool-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let storage: Arc<dyn Storage> = RedbStorage::open(
            StorageConfig::new(dir.join("store.redb")),
            MasterKey::from_raw([7u8; 32]),
        )
        .await
        .expect("open storage");
        storage.domain(KeyDomain::Schedules)
    }

    #[tokio::test]
    async fn schedule_create_builds_a_team_mission_record() {
        let tool = ScheduleCreateTool::new();
        let store = schedule_domain().await;
        tool.set_schedule_store(store.clone()).unwrap();
        let (ch, audit) = ctx_parts();
        let ctx = make_ctx(&ch, &audit, aivyx_core::MessageOrigin::Operator);
        let outcome = tool
            .execute(
                json!({
                    "cron": "0 0 2 * * * *",
                    "goal": "run the overnight close"
                }),
                &ctx,
            )
            .await;
        let ToolOutcome::Completed { output, .. } = outcome else {
            panic!("expected success, got {outcome:?}");
        };
        let schedule_id = output["schedule_id"].as_str().unwrap().to_string();
        let record = crate::schedule::get_schedule(&store, &schedule_id)
            .await
            .unwrap()
            .unwrap();
        assert!(record.team_mission.is_some());
        assert_eq!(record.team_mission.as_ref().unwrap().goal, "run the overnight close");
    }

    #[tokio::test]
    async fn schedule_create_rejects_both_prompt_and_goal() {
        let tool = ScheduleCreateTool::new();
        let store = schedule_domain().await;
        tool.set_schedule_store(store).unwrap();
        let (ch, audit) = ctx_parts();
        let ctx = make_ctx(&ch, &audit, aivyx_core::MessageOrigin::Operator);
        let outcome = tool
            .execute(
                json!({
                    "cron": "0 0 2 * * * *",
                    "prompt": "check system health",
                    "goal": "run the overnight close"
                }),
                &ctx,
            )
            .await;
        assert!(matches!(outcome, ToolOutcome::Failed(_)));
    }

    #[tokio::test]
    async fn schedule_create_rejects_neither_prompt_nor_goal() {
        let tool = ScheduleCreateTool::new();
        let store = schedule_domain().await;
        tool.set_schedule_store(store).unwrap();
        let (ch, audit) = ctx_parts();
        let ctx = make_ctx(&ch, &audit, aivyx_core::MessageOrigin::Operator);
        let outcome = tool.execute(json!({"cron": "0 0 2 * * * *"}), &ctx).await;
        assert!(matches!(outcome, ToolOutcome::Failed(_)));
    }

    #[test]
    fn schedule_create_schema_has_no_pack_config() {
        let tool = ScheduleCreateTool::new();
        let schema = tool.input_schema();
        assert!(
            schema["properties"].get("pack_config").is_none(),
            "pack_config must not be agent-reachable -- see the final review's Critical finding"
        );
    }

    #[test]
    fn schedule_create_scope() {
        let tool = ScheduleCreateTool::new();
        assert_eq!(
            tool.required_scope(&json!({})).as_str(),
            "schedule.create"
        );
    }

    #[test]
    fn schedule_create_name_and_schema() {
        let tool = ScheduleCreateTool::new();
        assert_eq!(tool.name(), "schedule.create");
        let schema = tool.input_schema();
        assert!(schema["required"].as_array().unwrap().contains(&json!("cron")));
        assert!(!schema["required"].as_array().unwrap().contains(&json!("prompt")));
    }

    #[test]
    fn schedule_list_scope() {
        let tool = ScheduleListTool::new();
        assert_eq!(
            tool.required_scope(&json!({})).as_str(),
            "schedule.list"
        );
    }

    #[test]
    fn schedule_list_name() {
        let tool = ScheduleListTool::new();
        assert_eq!(tool.name(), "schedule.list");
    }

    #[test]
    fn schedule_delete_scope() {
        let tool = ScheduleDeleteTool::new();
        assert_eq!(
            tool.required_scope(&json!({})).as_str(),
            "schedule.delete"
        );
    }

    #[test]
    fn schedule_delete_name_and_schema() {
        let tool = ScheduleDeleteTool::new();
        assert_eq!(tool.name(), "schedule.delete");
        let schema = tool.input_schema();
        assert!(schema["required"].as_array().unwrap().contains(&json!("schedule_id")));
    }

    #[test]
    fn schedule_update_scope() {
        let tool = ScheduleUpdateTool::new();
        assert_eq!(
            tool.required_scope(&json!({})).as_str(),
            "schedule.update"
        );
    }

    #[test]
    fn schedule_update_name_and_schema() {
        let tool = ScheduleUpdateTool::new();
        assert_eq!(tool.name(), "schedule.update");
        let schema = tool.input_schema();
        assert!(schema["required"].as_array().unwrap().contains(&json!("schedule_id")));
        assert!(schema["properties"]["enabled"].is_object());
        assert!(schema["properties"]["cron"].is_object());
        assert!(schema["properties"]["prompt"].is_object());
        assert!(schema["properties"]["role"].is_object());
    }

    #[tokio::test]
    async fn schedule_update_rejects_a_prompt_edit_on_a_team_mission_schedule() {
        let create_tool = ScheduleCreateTool::new();
        let store = schedule_domain().await;
        create_tool.set_schedule_store(store.clone()).unwrap();
        let (ch, audit) = ctx_parts();
        let ctx = make_ctx(&ch, &audit, aivyx_core::MessageOrigin::Operator);
        let created = create_tool
            .execute(
                json!({"cron": "0 0 2 * * * *", "goal": "run the overnight close"}),
                &ctx,
            )
            .await;
        let ToolOutcome::Completed { output, .. } = created else {
            panic!("setup failed")
        };
        let schedule_id = output["schedule_id"].as_str().unwrap().to_string();

        let update_tool = ScheduleUpdateTool::new();
        update_tool.set_schedule_store(store.clone()).unwrap();
        let outcome = update_tool
            .execute(
                json!({"schedule_id": schedule_id, "prompt": "do something else"}),
                &ctx,
            )
            .await;
        assert!(matches!(outcome, ToolOutcome::Failed(_)));
    }

    #[tokio::test]
    async fn schedule_update_still_allows_enabled_and_cron_edits_on_a_team_mission_schedule() {
        // Regression guard for the I2 fix above: the new prompt/role guard
        // must key ONLY on the prompt/role input keys, not on
        // team_mission being set at all -- Studio's own Pause/Resume
        // button (aivyx-web's UpdateSchedule sender) only ever sends
        // `enabled`, never `prompt`, and must keep working on a
        // team-mission schedule.
        let create_tool = ScheduleCreateTool::new();
        let store = schedule_domain().await;
        create_tool.set_schedule_store(store.clone()).unwrap();
        let (ch, audit) = ctx_parts();
        let ctx = make_ctx(&ch, &audit, aivyx_core::MessageOrigin::Operator);
        let created = create_tool
            .execute(
                json!({"cron": "0 0 2 * * * *", "goal": "run the overnight close"}),
                &ctx,
            )
            .await;
        let ToolOutcome::Completed { output, .. } = created else {
            panic!("setup failed")
        };
        let schedule_id = output["schedule_id"].as_str().unwrap().to_string();

        let update_tool = ScheduleUpdateTool::new();
        update_tool.set_schedule_store(store.clone()).unwrap();
        let outcome = update_tool
            .execute(
                json!({"schedule_id": schedule_id, "enabled": false, "cron": "0 0 3 * * * *"}),
                &ctx,
            )
            .await;
        let ToolOutcome::Completed { output, .. } = outcome else {
            panic!("expected success, got {outcome:?}");
        };
        assert_eq!(output["enabled"], false);
        assert_eq!(output["cron"], "0 0 3 * * * *");
    }

    #[tokio::test]
    async fn schedule_create_refuses_when_message_origin_is_system() {
        let tool = ScheduleCreateTool::new();
        let store = schedule_domain().await;
        tool.set_schedule_store(store).unwrap();
        tool.set_growth(GrowthAdoption::BroadAuto).unwrap();
        let (ch, audit) = ctx_parts();
        let ctx = make_ctx(&ch, &audit, aivyx_core::MessageOrigin::System);
        let outcome = tool
            .execute(
                json!({"cron": "0 0 9 * * * *", "prompt": "check something"}),
                &ctx,
            )
            .await;
        let ToolOutcome::Failed(AivyxError::Tool { detail, .. }) = outcome else {
            panic!("expected a refusal, got {outcome:?}");
        };
        assert!(
            detail.contains("triggered") || detail.contains("scheduled"),
            "error should explain the refusal reason: {detail}"
        );
    }

    #[tokio::test]
    async fn schedule_create_still_succeeds_under_operator_origin() {
        // Companion to the refusal test above -- proves the guard doesn't
        // over-block ordinary interactive self-scheduling.
        let tool = ScheduleCreateTool::new();
        let store = schedule_domain().await;
        tool.set_schedule_store(store).unwrap();
        tool.set_growth(GrowthAdoption::BroadAuto).unwrap();
        let (ch, audit) = ctx_parts();
        let ctx = make_ctx(&ch, &audit, aivyx_core::MessageOrigin::Operator);
        let outcome = tool
            .execute(
                json!({"cron": "0 0 9 * * * *", "prompt": "check something"}),
                &ctx,
            )
            .await;
        assert!(matches!(outcome, ToolOutcome::Completed { .. }));
    }

    #[tokio::test]
    async fn schedule_update_refuses_when_message_origin_is_system() {
        let create_tool = ScheduleCreateTool::new();
        let store = schedule_domain().await;
        create_tool.set_schedule_store(store.clone()).unwrap();
        create_tool.set_growth(GrowthAdoption::BroadAuto).unwrap();
        let (ch, audit) = ctx_parts();
        let create_ctx = make_ctx(&ch, &audit, aivyx_core::MessageOrigin::Operator);
        let created = create_tool
            .execute(
                json!({"cron": "0 0 2 * * * *", "prompt": "do a thing"}),
                &create_ctx,
            )
            .await;
        let ToolOutcome::Completed { output, .. } = created else {
            panic!("setup failed")
        };
        let schedule_id = output["schedule_id"].as_str().unwrap().to_string();

        let update_tool = ScheduleUpdateTool::new();
        update_tool.set_schedule_store(store).unwrap();
        let sys_ctx = make_ctx(&ch, &audit, aivyx_core::MessageOrigin::System);
        let outcome = update_tool
            .execute(json!({"schedule_id": schedule_id, "enabled": true}), &sys_ctx)
            .await;
        let ToolOutcome::Failed(AivyxError::Tool { detail, .. }) = outcome else {
            panic!("expected a refusal, got {outcome:?}");
        };
        assert!(
            detail.contains("cannot be called from within a triggered or scheduled run"),
            "error should explain the refusal reason: {detail}"
        );
    }

    #[tokio::test]
    async fn schedule_delete_refuses_when_message_origin_is_system() {
        let create_tool = ScheduleCreateTool::new();
        let store = schedule_domain().await;
        create_tool.set_schedule_store(store.clone()).unwrap();
        create_tool.set_growth(GrowthAdoption::BroadAuto).unwrap();
        let (ch, audit) = ctx_parts();
        let create_ctx = make_ctx(&ch, &audit, aivyx_core::MessageOrigin::Operator);
        let created = create_tool
            .execute(
                json!({"cron": "0 0 2 * * * *", "prompt": "do a thing"}),
                &create_ctx,
            )
            .await;
        let ToolOutcome::Completed { output, .. } = created else {
            panic!("setup failed")
        };
        let schedule_id = output["schedule_id"].as_str().unwrap().to_string();

        let delete_tool = ScheduleDeleteTool::new();
        delete_tool.set_schedule_store(store).unwrap();
        let sys_ctx = make_ctx(&ch, &audit, aivyx_core::MessageOrigin::System);
        let outcome = delete_tool
            .execute(json!({"schedule_id": schedule_id}), &sys_ctx)
            .await;
        let ToolOutcome::Failed(AivyxError::Tool { detail, .. }) = outcome else {
            panic!("expected a refusal, got {outcome:?}");
        };
        assert!(
            detail.contains("cannot be called from within a triggered or scheduled run"),
            "error should explain the refusal reason: {detail}"
        );
    }
}
