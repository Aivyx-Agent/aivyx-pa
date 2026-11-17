//! The tools a config pack's roles and team members may name, and the
//! scope base each needs. Only tools whose scope a pack may grant
//! (`PACK_ALLOWED_SCOPES` in `pack_check.rs`) are here, plus the team tools.
//! A test keeps every name in step with the generated tools reference.

/// `(tool, scope base it needs)`.
pub const PACK_TOOLS: &[(&str, &str)] = &[
    ("budget.categories", "budget.read"),
    ("budget.summary", "budget.read"),
    ("budget.trend", "budget.read"),
    ("budget.record", "budget.write"),
    ("budget.update", "budget.write"),
    ("budget.delete", "budget.write"),
    ("calc.eval", "calc.eval"),
    ("calendar.upcoming", "calendar.read"),
    ("calendar.list_calendars", "calendar.read"),
    ("calendar.list_events", "calendar.read"),
    ("calendar.get_event", "calendar.read"),
    ("calendar.create_event", "calendar.write"),
    ("calendar.update_event", "calendar.write"),
    ("calendar.delete_event", "calendar.write"),
    ("contacts.search", "contacts.read"),
    ("contacts.list", "contacts.read"),
    ("contacts.get", "contacts.read"),
    ("contacts.create", "contacts.write"),
    ("contacts.update", "contacts.write"),
    ("contacts.delete", "contacts.write"),
    ("convert.units", "convert.units"),
    ("convert.time", "convert.units"),
    ("data.csv", "fs.read"),
    ("data.pdf", "fs.read"),
    ("data.xlsx", "fs.read"),
    ("date.add", "date.compute"),
    ("date.diff", "date.compute"),
    ("drive.search", "drive.read"),
    ("drive.get_metadata", "drive.read"),
    ("drive.list_drives", "drive.read"),
    ("drive.list_folder", "drive.read"),
    ("drive.download_file", "drive.read"),
    ("drive.recent_files", "drive.read"),
    ("drive.recent_activity", "drive.read"),
    ("drive.recent_changes", "drive.read"),
    ("fs.metadata", "fs.metadata"),
    ("fs.read", "fs.read"),
    ("gmail.search", "email.read"),
    ("gmail.read", "email.read"),
    ("gmail.draft", "email.write"),
    ("proton.search", "email.read"),
    ("proton.read", "email.read"),
    ("proton.draft", "email.write"),
    ("health.check.list", "health.read"),
    ("health.check.recent_changes", "health.read"),
    ("memory.read", "memory.read"),
    ("memory.search", "memory.read"),
    ("memory.write", "memory.write"),
    ("notion.search", "notion.read"),
    ("notion.get_page", "notion.read"),
    ("notion.list_database", "notion.read"),
    ("notion.create_page", "notion.write"),
    ("notion.update_page_properties", "notion.write"),
    ("notion.append_blocks", "notion.write"),
    ("notion.archive_page", "notion.write"),
    ("remind.list", "remind.read"),
    ("remind.set", "remind.write"),
    ("remind.cancel", "remind.write"),
    ("skill_defaults.list", "skill_defaults.list"),
    ("skill_defaults.read", "skill_defaults.read"),
    ("task.list", "task.read"),
    ("task.create", "task.write"),
    ("task.complete", "task.write"),
    ("task.delete", "task.write"),
    ("web.search", "web.search"),
    ("web.fetch", "net.fetch"),
    ("web.extract", "net.fetch"),
    ("decompose_task", "team.delegate"),
    ("delegate_task", "team.delegate"),
    ("query_agent", "team.delegate"),
    ("verify_output", "team.delegate"),
    ("synthesize_results", "team.delegate"),
    ("send_message", "team.message"),
    ("read_message", "team.message"),
];

/// The team tools (not in the generated tools reference).
#[cfg(test)]
const TEAM_TOOLS: &[&str] = &[
    "decompose_task",
    "delegate_task",
    "query_agent",
    "verify_output",
    "synthesize_results",
    "send_message",
    "read_message",
];

pub fn tool_scope(tool: &str) -> Option<&'static str> {
    PACK_TOOLS.iter().find(|(t, _)| *t == tool).map(|(_, s)| *s)
}

/// Problems with one role's or member's tools against its scopes: a tool
/// a pack can't use, a tool whose scope isn't granted, and a granted scope
/// no tool needs. `team.message` is allowed for roles and members alike,
/// without a tool that needs it: the team runtime uses it.
pub fn tool_lints(who: &str, tools: &[String], scopes: &[String]) -> Vec<String> {
    let base = |s: &str| s.split(':').next().unwrap_or(s).to_string();
    let granted: Vec<String> = scopes.iter().map(|s| base(s)).collect();
    let mut problems = Vec::new();
    let mut needed: Vec<&str> = vec!["team.message"];
    for tool in tools {
        match tool_scope(tool) {
            None => problems.push(format!(
                "{who} names tool `{tool}`, which aivyx-pa doesn't have or a pack can't use"
            )),
            Some(scope) => {
                needed.push(scope);
                if !granted.iter().any(|g| g == scope) {
                    problems.push(format!(
                        "{who} holds `{tool}` but not the `{scope}` scope it needs"
                    ));
                }
            }
        }
    }
    for g in &granted {
        if !needed.contains(&g.as_str()) {
            problems.push(format!(
                "{who} has the `{g}` scope, but none of its tools needs it"
            ));
        }
    }
    // Each message once, in first-seen order (`dedup` only drops adjacent repeats).
    let mut seen = std::collections::HashSet::new();
    problems.retain(|p| seen.insert(p.clone()));
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every tool here exists: in the generated reference, or a team tool.
    #[test]
    fn every_pack_tool_is_a_real_tool() {
        let reference = include_str!("../../../../../docs/manual/reference/03-tools.md");
        for (tool, _) in PACK_TOOLS {
            assert!(
                TEAM_TOOLS.contains(tool) || reference.contains(&format!("| `{tool}` |")),
                "{tool} isn't in docs/manual/reference/03-tools.md"
            );
        }
    }

    #[test]
    fn lints_catch_unknown_tools_and_missing_or_spare_scopes() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(tool_lints("role `r`", &s(&["gmail.draft"]), &s(&["email.write"])).is_empty());
        let p = tool_lints("role `r`", &s(&["gmail.drafts"]), &s(&[]));
        assert!(p[0].contains("doesn't have or a pack can't use"), "{p:?}");
        let p = tool_lints("role `r`", &s(&["gmail.draft"]), &s(&[]));
        assert!(p[0].contains("not the `email.write` scope"), "{p:?}");
        let p = tool_lints(
            "role `r`",
            &s(&["memory.read"]),
            &s(&["memory.read:topic:x*", "budget.read"]),
        );
        assert_eq!(
            p,
            vec![
                "role `r` has the `budget.read` scope, but none of its tools needs it".to_string()
            ]
        );
        assert!(tool_lints("m", &s(&["send_message"]), &s(&["team.message"])).is_empty());
    }

    #[test]
    fn a_repeated_problem_is_reported_once() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let p = tool_lints("m", &s(&["gmail.draft", "x.y", "gmail.draft"]), &s(&[]));
        assert_eq!(p.len(), 2, "{p:?}");
        let p = tool_lints(
            "m",
            &s(&["memory.read"]),
            &s(&["memory.read", "a.read:x", "b.read", "a.read:y"]),
        );
        assert_eq!(p.len(), 2, "{p:?}");
    }
}
