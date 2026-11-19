//! A21 — standing agents: validation and the due rule.

use std::collections::HashMap;

use aivyx_ipc::board::{Agent, Owner, Task, TaskState};

use super::model::BoardError;

pub fn validate_name(name: &str) -> Result<(), BoardError> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(BoardError::NotAllowed(
            "an agent name is 1-64 letters, digits, - or _",
        ))
    }
}

pub fn validate_heartbeat(cron: &str) -> Result<(), BoardError> {
    crate::schedule::validate_cron(cron).map_err(|_| {
        BoardError::NotAllowed(
            "the heartbeat isn't a valid cron pattern (6 or 7 fields, seconds first)",
        )
    })
}

/// Refuse a `reports_to` that names an unknown agent, the agent itself,
/// or closes a loop.
pub fn check_reports_to(
    agent_id: &str,
    reports_to: Option<&str>,
    agents: &HashMap<String, Agent>,
) -> Result<(), BoardError> {
    let mut next = reports_to.map(str::to_string);
    let mut hops = 0;
    while let Some(id) = next {
        if id == agent_id {
            return Err(BoardError::NotAllowed("reports-to would make a loop"));
        }
        let Some(a) = agents.get(&id) else {
            return Err(BoardError::NotFound("agent"));
        };
        hops += 1;
        if hops > agents.len() {
            return Err(BoardError::NotAllowed("reports-to would make a loop"));
        }
        next = a.reports_to.clone();
    }
    Ok(())
}

/// The open (To do, Running, Review) tasks owned by `agent_id`.
pub fn open_tasks_owned_by<'a>(agent_id: &str, tasks: &'a [Task]) -> Vec<&'a Task> {
    tasks
        .iter()
        .filter(|t| matches!(&t.owner, Some(Owner::Agent { id }) if id == agent_id))
        .filter(|t| {
            matches!(
                t.state,
                TaskState::Todo | TaskState::Running | TaskState::Review
            )
        })
        .collect()
}

/// Is `agent` due at `now_ms`? Paused or busy agents never are. Due when a
/// pending assignment wake has come, or when its heartbeat has fired since
/// `last_wake` (or since it was created, if it never woke). Only the most
/// recent firing counts: no catch-up.
pub fn is_due(agent: &Agent, busy: bool, pending_wake: Option<u64>, now_ms: u64) -> bool {
    if agent.paused || busy {
        return false;
    }
    if pending_wake.is_some_and(|t| t <= now_ms) {
        return true;
    }
    let Some(cron) = &agent.heartbeat else {
        return false;
    };
    let since = agent.last_wake.unwrap_or(agent.created_at);
    let Some(since) = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(since as i64) else {
        return false;
    };
    crate::schedule::next_fire_after(cron, since)
        .is_some_and(|next| next.timestamp_millis() as u64 <= now_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_ipc::board::WorksAs;

    fn agent(id: &str, reports_to: Option<&str>) -> Agent {
        Agent {
            id: id.into(),
            name: id.into(),
            title: String::new(),
            reports_to: reports_to.map(str::to_string),
            works_as: WorksAs::Team,
            heartbeat: None,
            wake_on_assign: false,
            paused: false,
            last_wake: None,
            created_at: 1,
            updated_at: 1,
        }
    }

    fn map(list: Vec<Agent>) -> HashMap<String, Agent> {
        list.into_iter().map(|a| (a.id.clone(), a)).collect()
    }

    fn task(id: &str, owner: Option<Owner>, state: TaskState) -> Task {
        Task {
            id: id.into(),
            project_id: "p-1".into(),
            parent_task: None,
            title: "T".into(),
            description: String::new(),
            owner,
            review_required: true,
            state,
            thread: vec![],
            active_run: None,
            created_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn names() {
        assert!(validate_name("scout_1-b").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name(&"a".repeat(64)).is_ok());
        assert!(validate_name(&"a".repeat(65)).is_err());
        assert!(validate_name("a b").is_err());
    }

    #[test]
    fn heartbeats() {
        assert!(validate_heartbeat("0 0 9 * * *").is_ok());
        assert!(validate_heartbeat("every day").is_err());
    }

    #[test]
    fn reports_to_rules() {
        let m = map(vec![
            agent("a", None),
            agent("b", Some("a")),
            agent("c", Some("b")),
        ]);
        assert!(check_reports_to("d", None, &m).is_ok());
        assert!(check_reports_to("d", Some("c"), &m).is_ok());
        assert!(check_reports_to("a", Some("a"), &m).is_err());
        // a -> c would close a -> c -> b -> a.
        assert!(check_reports_to("a", Some("c"), &m).is_err());
        assert!(matches!(
            check_reports_to("d", Some("zz"), &m),
            Err(BoardError::NotFound("agent"))
        ));
        // A pre-existing loop elsewhere in the chain terminates.
        let looped = map(vec![agent("x", Some("y")), agent("y", Some("x"))]);
        assert!(check_reports_to("d", Some("x"), &looped).is_err());
    }

    #[test]
    fn open_tasks_filter() {
        let mine = Some(Owner::Agent { id: "a-1".into() });
        let tasks = vec![
            task("1", mine.clone(), TaskState::Todo),
            task("2", mine.clone(), TaskState::Running),
            task("3", mine.clone(), TaskState::Review),
            task("4", mine.clone(), TaskState::Done),
            task("5", mine.clone(), TaskState::Cancelled),
            task(
                "6",
                Some(Owner::Agent { id: "a-2".into() }),
                TaskState::Todo,
            ),
            task("7", Some(Owner::Team), TaskState::Todo),
            task("8", None, TaskState::Todo),
        ];
        let ids: Vec<_> = open_tasks_owned_by("a-1", &tasks)
            .iter()
            .map(|t| t.id.as_str())
            .collect();
        assert_eq!(ids, ["1", "2", "3"]);
    }

    // ---- is_due, with a fake clock ---------------------------------------

    /// 2025-11-19 12:00:30 UTC: half a minute past a minute boundary.
    const NOW: u64 = 1_763_553_630_000;

    fn beating(last_wake_ago_ms: Option<u64>) -> Agent {
        let mut a = agent("a-1", None);
        a.heartbeat = Some("0 * * * * *".into());
        a.created_at = NOW - 3_600_000;
        a.last_wake = last_wake_ago_ms.map(|ago| NOW - ago);
        a
    }

    #[test]
    fn paused_or_busy_is_never_due() {
        let mut a = beating(Some(120_000));
        assert!(is_due(&a, false, None, NOW));
        assert!(!is_due(&a, true, Some(NOW), NOW));
        a.paused = true;
        assert!(!is_due(&a, false, Some(NOW), NOW));
    }

    #[test]
    fn a_pending_wake_is_due_once_it_has_come() {
        let a = agent("a-1", None);
        assert!(!is_due(&a, false, Some(NOW + 1), NOW));
        assert!(is_due(&a, false, Some(NOW), NOW));
        assert!(is_due(&a, false, Some(NOW - 5_000), NOW));
    }

    #[test]
    fn a_heartbeat_is_due_once_it_has_fired_since_the_last_wake() {
        // Last woke at 12:00:00: the next firing is 12:01:00, not yet.
        assert!(!is_due(&beating(Some(30_000)), false, None, NOW));
        // Last woke at 11:59:29: 12:00:00 has fired.
        assert!(is_due(&beating(Some(61_000)), false, None, NOW));
    }

    #[test]
    fn a_heartbeat_never_woken_counts_from_creation() {
        let mut a = beating(None);
        assert!(is_due(&a, false, None, NOW));
        a.created_at = NOW - 10_000;
        assert!(!is_due(&a, false, None, NOW));
    }

    #[test]
    fn no_catch_up_after_a_long_gap() {
        // Three days of missed minutes are one wake, not thousands.
        let mut a = beating(Some(3 * 24 * 3_600_000));
        assert!(is_due(&a, false, None, NOW));
        // The wake sets last_wake = now; the next check is not due.
        a.last_wake = Some(NOW);
        assert!(!is_due(&a, false, None, NOW));
        assert!(!is_due(&a, false, None, NOW + 29_000));
        // Until the next firing (12:01:00).
        assert!(is_due(&a, false, None, NOW + 30_000));
    }

    #[test]
    fn nothing_to_wake_for_is_not_due() {
        let a = agent("a-1", None);
        assert!(!is_due(&a, false, None, NOW));
        assert!(!is_due(&a, false, None, u64::MAX / 2));
    }
}
