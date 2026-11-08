//! `aivyx-pa pack check <dir>` — the product checks for a config pack's
//! aivyx-pa part (format 2). Used on a publisher's staging folder and, by
//! `pack install`, on what it just unpacked, before anything is kept.
//!
//! The shared `aivyx-pack` crate checks the manifest's shape and the
//! signature; this module checks what only aivyx-pa can: the template loads
//! as a real config, autonomy stays at or below `supervised`, the team
//! config and skills load, and every integration named is a real one.

use std::path::Path;

use aivyx_config::{AivyxConfig, AutonomyLevel, LoadOptions};
use aivyx_pack::{ConfigPackManifest, Manifest};

/// Integrations a pack may name in `requires` / `optional`. The first four
/// are set up with `aivyx-pa connect`; the rest are tool processes.
pub const KNOWN_INTEGRATIONS: &[&str] = &[
    "gmail", "calendar", "drive", "contacts", "notion", "obsidian", "n8n", "toolkit", "vision",
];

/// The integrations `aivyx-pa connect` sets up.
pub const CONNECTABLE: &[&str] = &["gmail", "calendar", "drive", "contacts"];

/// What a good aivyx-pa part holds.
#[derive(Debug)]
pub struct PaPartSummary {
    pub manifest: ConfigPackManifest,
    /// Skill names found, sorted.
    pub skills: Vec<String>,
    /// Members of the team config; 0 when the pack has none.
    pub team_members: usize,
    /// `[[schedule]]` routines in the template.
    pub routines: usize,
    /// The template's global autonomy level.
    pub autonomy: AutonomyLevel,
    /// The template's per-area overrides, as `(area, level)`.
    pub autonomy_overrides: Vec<(String, AutonomyLevel)>,
    /// Every capability scope the pack's roles and team members ask for
    /// (audit P6), as `(who, scopes)`, in a stable order.
    pub grants: Vec<(String, Vec<String>)>,
}

/// The scope bases a config pack's roles and team members may hold: reads,
/// memory, the web, team messaging, and the narrow writes the shipped
/// Business Manager pack needs (drafting mail, calendar, tasks, contacts,
/// notes, budget, reminders). The vertical-packs spec says a config pack
/// "adds no capabilities; it can do only what the products already do", so
/// this is an ALLOW-list: every other base (shell, delete, send, post, file
/// and git writes, config, role and schedule changes, MCP calls, apps, ...) is
/// refused by name, and a base added to the registry later stays refused
/// until it is judged here. The operator adds anything else to their own
/// config after install.
const PACK_ALLOWED_SCOPES: &[&str] = &[
    "budget.read",
    "budget.write",
    "calc.eval",
    "calendar.read",
    "calendar.write",
    "contacts.read",
    "contacts.write",
    "convert.units",
    "date.compute",
    "drive.read",
    "email.read",
    "email.write",
    "fs.metadata",
    "fs.read",
    "health.read",
    "memory.read",
    "memory.write",
    "net.fetch",
    "notion.read",
    "notion.write",
    "remind.read",
    "remind.write",
    "skill_defaults.list",
    "skill_defaults.read",
    "task.read",
    "task.write",
    "team.delegate",
    "team.message",
    "web.search",
];

/// Why a scope is refused, or `None` when a pack may ask for it.
fn refused_scope(scope: &str) -> Option<&'static str> {
    let base = scope.split(':').next().unwrap_or(scope);
    if PACK_ALLOWED_SCOPES.contains(&base) {
        None
    } else {
        Some("not in the set a config pack may grant")
    }
}

/// Scopes a pack may hold that still change things (listed for the
/// operator, not refused).
fn changes_things(scope: &str) -> bool {
    let base = scope.split(':').next().unwrap_or(scope);
    base.ends_with(".write") || base.ends_with(".update") || base.ends_with(".create")
}

/// "who: scopes" lines for `pack check`, `pack inspect` and the install
/// output. Scopes that can change things are marked with `*`.
pub fn grants_lines(summary: &PaPartSummary) -> String {
    if summary.grants.is_empty() {
        return "  capability scopes asked for: none\n".to_string();
    }
    let mut out = String::from(
        "  capability scopes asked for (* = can change things):\n",
    );
    for (who, scopes) in &summary.grants {
        let shown: Vec<String> = scopes
            .iter()
            .map(|s| if changes_things(s) { format!("{s}*") } else { s.clone() })
            .collect();
        out.push_str(&format!(
            "    {}: {}\n",
            aivyx_capability::display::neutralise(who),
            if shown.is_empty() { "(none)".into() } else { aivyx_capability::display::neutralise(&shown.join(", ")) }
        ));
    }
    out
}

/// Check the aivyx-pa part of an unpacked config pack or a staging folder.
/// `Err` lists every problem found, not just the first.
/// The sections a pack's template may set: who the agent is and what it
/// does. Everything else — how far it reaches (`[access]`, `[fs]`,
/// `[sandbox]`), whether and where it's exposed (`[studio]`, `[daemon]`), what
/// programs it runs (`[[tool_process]]`, `[[mcp_server]]`), where its
/// traffic goes (`[openai]`, `[providers]`, …) and whom it trusts
/// (`[pack]`) — is the operator's, set after install. An allowlist, so a
/// section added later stays operator-only until it's judged safe here.
const PACK_SECTIONS: &[&str] = &[
    "agent",
    "profile",
    "persona_seed",
    "role",
    "autonomy",
    "schedule",
    "team",
    "memory",
    "skill_defaults",
];

/// `[agent]` keys a pack can't set: they switch off the agent's
/// prompt-injection scan.
const OPERATOR_AGENT_KEYS: &[&str] = &["injection_scan_enabled", "injection_scan_exempt"];

/// What the template at `path` sets that only the operator may.
fn operator_only_settings(path: &Path) -> Vec<String> {
    let Ok(doc) = std::fs::read_to_string(path)
        .map_err(|_| ())
        .and_then(|t| t.parse::<toml_edit::DocumentMut>().map_err(|_| ()))
    else {
        return Vec::new(); // loading it below reports what's wrong
    };
    let mut problems = Vec::new();
    for (key, item) in doc.iter() {
        if !PACK_SECTIONS.contains(&key) {
            let shown = if item.is_array_of_tables() { format!("[[{key}]]") } else { format!("[{key}]") };
            problems.push(format!(
                "the template sets {shown}; a pack can't — that's for the operator to set after install"
            ));
        }
    }
    if let Some(agent) = doc.get("agent").and_then(|a| a.as_table_like()) {
        for key in OPERATOR_AGENT_KEYS {
            if agent.contains_key(key) {
                problems.push(format!(
                    "the template sets [agent] {key}; a pack can't change the prompt-injection scan"
                ));
            }
        }
    }
    problems
}

pub fn check_pa_part(dir: &Path) -> Result<PaPartSummary, Vec<String>> {
    let text = std::fs::read_to_string(dir.join("manifest.toml"))
        .map_err(|e| vec![format!("can't read manifest.toml: {e}")])?;
    let manifest = match Manifest::parse(&text).map_err(|e| vec![e.to_string()])? {
        Manifest::Config(m) => m,
        Manifest::Binary(_) => {
            return Err(vec![
                "this is a tool pack — `pack check` checks config packs (format 2)".into(),
            ]);
        }
    };
    let Some(pa) = manifest.pa.clone() else {
        return Err(vec!["this pack has no aivyx-pa part — it's for aivyx-coder".into()]);
    };

    let mut problems = Vec::new();
    let mut skills = Vec::new();
    let mut team_members = 0;
    let mut routines = 0;
    let mut autonomy = AutonomyLevel::default();
    let mut autonomy_overrides = Vec::new();
    let mut grants: Vec<(String, Vec<String>)> = Vec::new();

    // The template loads as a real aivyx-pa config, within the ceiling.
    let template = dir.join(&pa.template);
    if !template.is_file() {
        problems.push(format!("the template {} is missing", pa.template));
    } else {
        problems.extend(operator_only_settings(&template));
        let opts = LoadOptions {
            toml_path: Some(template),
            require_api_key: false,
            require_telegram_token: false,
            require_discord_token: false,
            require_slack_tokens: false,
            role_override: None,
        };
        match AivyxConfig::load_from_env_and_toml(&opts) {
            Err(e) => problems.push(format!("the template doesn't load: {e}")),
            Ok(cfg) => {
                autonomy = cfg.autonomy_level.value;
                if autonomy > AutonomyLevel::Supervised {
                    problems.push(format!(
                        "the template sets autonomy `{autonomy}`; a pack can't go above `supervised`"
                    ));
                }
                for o in &cfg.autonomy_overrides {
                    if o.level > AutonomyLevel::Supervised {
                        problems.push(format!(
                            "the template sets `{}` for `{}`; a pack can't go above `supervised`",
                            o.level, o.domain
                        ));
                    }
                    autonomy_overrides.push((o.domain.clone(), o.level));
                }
                for (name, role) in &cfg.roles {
                    grants.push((format!("role `{name}`"), role.capability_scopes.value.iter().map(|s| s.to_string()).collect()));
                }
                routines = cfg.schedules.len();
                // The config loader accepts any cron text; a bad pattern
                // would only fail once the scheduler runs, so check here.
                for s in &cfg.schedules {
                    if let Err(e) = aivyx_channel::schedule::validate_cron(&s.cron) {
                        problems.push(format!(
                            "routine `{}`: {e} — use 6 or 7 fields, seconds first \
                             (e.g. `0 30 7 * * Mon-Fri`)",
                            s.name
                        ));
                    }
                }
            }
        }
    }

    // The team config loads.
    if let Some(rel) = &pa.team_config {
        match aivyx_team::TeamConfig::load(dir.join(rel)) {
            Ok(team) => {
                team_members = team.members.len();
                for m in &team.members {
                    grants.push((
                        format!("team member `{}` (ceiling {:?})", m.name, m.trust_ceiling),
                        m.capability_scopes.clone(),
                    ));
                }
            }
            Err(e) => problems.push(format!("the team config doesn't load: {e}")),
        }
    }

    // Every skill folder holds a skill that parses under its own name.
    if let Some(rel) = &pa.skills {
        let skills_dir = dir.join(rel);
        let loader = aivyx_skills::SkillLoader::new().with_project_dir(skills_dir.clone());
        let mut names: Vec<String> = std::fs::read_dir(&skills_dir)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.path().join("SKILL.md").is_file())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        if names.is_empty() {
            problems.push(format!("the skills folder {rel} has no skills"));
        }
        for name in names {
            // A same-named bundled skill would also answer `get`, so make
            // sure the one found is the pack's own.
            let ours = loader.get(&name).is_some_and(|s| {
                s.source == aivyx_skills::SkillSource::Project && !s.body.trim().is_empty()
            });
            if ours {
                skills.push(name);
            } else {
                problems.push(format!(
                    "skill `{name}` doesn't parse (it needs `name` and `description` \
                     frontmatter, and `name` must match its folder)"
                ));
            }
        }
    }

    // Only integrations aivyx-pa knows.
    for name in pa.requires.iter().chain(&pa.optional) {
        if !KNOWN_INTEGRATIONS.contains(&name.as_str()) {
            problems.push(format!(
                "unknown integration `{name}` (known: {})",
                KNOWN_INTEGRATIONS.join(", ")
            ));
        }
    }

    // Audit P6: a pack's roles and team members carry capability scopes; the
    // dangerous ones are refused, the rest are shown to the operator.
    grants.retain(|(_, scopes)| !scopes.is_empty());
    for (who, scopes) in &grants {
        for scope in scopes {
            if let Some(why) = refused_scope(scope) {
                let (who, scope) = (
                    aivyx_capability::display::neutralise(who),
                    aivyx_capability::display::neutralise(scope),
                );
                problems.push(format!(
                    "{who} asks for `{scope}` ({why}); a pack may only grant reads, memory, web, \
                     team and the narrow writes the Business Manager pack uses — the operator \
                     adds anything else to their own config"
                ));
            }
        }
    }

    if problems.is_empty() {
        Ok(PaPartSummary {
            manifest,
            skills,
            team_members,
            routines,
            autonomy,
            autonomy_overrides,
            grants,
        })
    } else {
        Err(problems)
    }
}

/// `assisted (fs: supervised, email: manual)`.
pub fn autonomy_line(summary: &PaPartSummary) -> String {
    if summary.autonomy_overrides.is_empty() {
        return summary.autonomy.to_string();
    }
    let overrides: Vec<String> = summary
        .autonomy_overrides
        .iter()
        .map(|(area, level)| format!("{area}: {level}"))
        .collect();
    format!("{} ({})", summary.autonomy, overrides.join(", "))
}

/// What `pack check` prints for a good part.
pub fn render_ok(s: &PaPartSummary) -> String {
    let pa = s.manifest.pa.as_ref().expect("checked: has a pa part");
    let list = |v: &[String]| if v.is_empty() { "(none)".to_string() } else { v.join(", ") };
    format!(
        "✓ {} v{} — aivyx-pa part looks good\n  \
         template loads; autonomy {} (within supervised)\n  \
         team: {} members · skills: {} · routines: {}\n  \
         requires: {} · optional: {}\n{}",
        s.manifest.name,
        s.manifest.version,
        autonomy_line(s),
        s.team_members,
        s.skills.len(),
        s.routines,
        list(&pa.requires),
        list(&pa.optional),
        grants_lines(s),
    )
}

/// What `pack check` prints for a bad part.
pub fn render_problems(problems: &[String]) -> String {
    let mut out = format!(
        "✗ {} problem{} in the aivyx-pa part:\n",
        problems.len(),
        if problems.len() == 1 { "" } else { "s" }
    );
    for p in problems {
        out.push_str(&format!("  - {p}\n"));
    }
    out
}

/// `aivyx-pa pack check <dir>`.
pub fn run_check(dir: &Path) -> Result<(), String> {
    match check_pa_part(dir) {
        Ok(summary) => {
            print!("{}", render_ok(&summary));
            Ok(())
        }
        Err(problems) => {
            print!("{}", render_problems(&problems));
            Err("the pack has problems (see above)".into())
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::PathBuf;

    pub(crate) const MANIFEST: &str = r#"format = 2
name = "business-manager"
version = "0.1.0"
publisher = "Aivyx"
products = ["pa"]

[pa]
min_version = "0.17.0"
template = "pa/aivyx-pa.toml"
team_config = "pa/team.toml"
skills = "pa/skills"
requires = ["gmail"]
optional = ["notion"]
"#;

    pub(crate) const TEMPLATE: &str = r#"[profile]
assistant_name = "Manager"

[autonomy]
level = "assisted"

[[autonomy.override]]
domain = "fs"
level = "supervised"

[[schedule]]
name = "morning-briefing"
cron = "0 30 7 * * Mon-Fri"
prompt = "Prepare the morning briefing."
"#;

    pub(crate) const TEAM: &str = r#"[team]
name = "business"
description = "A small business crew."
lead = "manager"

[[team.member]]
name = "manager"
role = "Manager"
soul = "You coordinate."
tool_allowlist = ["delegate_task"]
capability_scopes = ["team.delegate"]
trust_ceiling = "Trusted"

[[team.member]]
name = "books"
role = "Books"
soul = "You keep the books."
tool_allowlist = ["budget.summary"]
capability_scopes = ["team.message"]
trust_ceiling = "Trusted"
"#;

    pub(crate) const SKILL: &str =
        "---\nname: example\ndescription: An example skill.\n---\n\nDo the example thing.\n";

    pub(crate) fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aivyx-pa-packcheck-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A staging folder for a good config pack with an aivyx-pa part.
    pub(crate) fn good_pack(tag: &str) -> PathBuf {
        let dir = tmp(tag);
        for (rel, body) in [
            ("manifest.toml", MANIFEST),
            ("pa/aivyx-pa.toml", TEMPLATE),
            ("pa/team.toml", TEAM),
            ("pa/skills/example/SKILL.md", SKILL),
        ] {
            let path = dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        dir
    }

    pub(crate) fn rewrite(dir: &Path, rel: &str, f: impl Fn(String) -> String) {
        let path = dir.join(rel);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(path, f(text)).unwrap();
    }

    #[test]
    fn a_good_pa_part_passes() {
        let d = good_pack("good");
        let s = check_pa_part(&d).unwrap();
        assert_eq!(s.skills, vec!["example"]);
        assert_eq!(s.routines, 1);
        assert_eq!(s.team_members, 2);
        assert_eq!(autonomy_line(&s), "assisted (fs: supervised)");
        let out = render_ok(&s);
        assert!(out.contains("team: 2 members · skills: 1 · routines: 1"), "{out}");
        assert!(out.contains("requires: gmail · optional: notion"), "{out}");
    }

    #[test]
    fn autonomy_above_supervised_is_refused() {
        let d = good_pack("unleashed");
        rewrite(&d, "pa/aivyx-pa.toml", |t| {
            t.replace(r#"level = "assisted""#, r#"level = "unleashed""#)
        });
        let problems = check_pa_part(&d).unwrap_err();
        assert!(problems.iter().any(|p| p.contains("can't go above `supervised`")), "{problems:?}");
    }

    #[test]
    fn an_override_above_supervised_is_refused() {
        let d = good_pack("override");
        rewrite(&d, "pa/aivyx-pa.toml", |t| {
            t.replace(r#"level = "supervised""#, r#"level = "autonomous""#)
        });
        let problems = check_pa_part(&d).unwrap_err();
        assert!(problems.iter().any(|p| p.contains("for `fs`")), "{problems:?}");
    }

    #[test]
    fn every_problem_is_reported_at_once() {
        let d = good_pack("many");
        std::fs::remove_dir_all(d.join("pa/skills/example")).unwrap();
        rewrite(&d, "manifest.toml", |t| {
            t.replace(r#"requires = ["gmail"]"#, r#"requires = ["gmial"]"#)
        });
        rewrite(&d, "pa/team.toml", |_| "not = [valid".into());
        let problems = check_pa_part(&d).unwrap_err();
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(render_problems(&problems).starts_with("✗ 3 problems in the aivyx-pa part:\n"));
    }

    #[test]
    fn a_bad_routine_schedule_is_refused() {
        let d = good_pack("cron");
        rewrite(&d, "pa/aivyx-pa.toml", |t| t.replace("0 30 7 * * Mon-Fri", "30 7 * * 1-5"));
        let problems = check_pa_part(&d).unwrap_err();
        assert!(problems.iter().any(|p| p.starts_with("routine `morning-briefing`")), "{problems:?}");
    }

    #[test]
    fn a_skill_with_a_mismatched_name_is_refused() {
        let d = good_pack("skillname");
        rewrite(&d, "pa/skills/example/SKILL.md", |t| t.replace("name: example", "name: other"));
        let problems = check_pa_part(&d).unwrap_err();
        assert!(problems.iter().any(|p| p.contains("skill `example`")), "{problems:?}");
    }

    #[test]
    fn a_template_that_doesnt_load_is_refused() {
        let d = good_pack("badtemplate");
        rewrite(&d, "pa/aivyx-pa.toml", |t| t.replace(r#"domain = "fs""#, r#"domain = "nope""#));
        let problems = check_pa_part(&d).unwrap_err();
        assert!(problems.iter().any(|p| p.starts_with("the template doesn't load")), "{problems:?}");
    }

    #[test]
    fn a_tool_pack_or_coder_only_pack_is_explained() {
        let d = good_pack("coderonly");
        rewrite(&d, "manifest.toml", |_| {
            "format = 2\nname = \"c\"\nversion = \"0.1.0\"\npublisher = \"A\"\nproducts = [\"coder\"]\n\n\
             [coder]\nmin_version = \"0.5.0\"\nagents_file = \"coder/AGENTS.md\"\n"
                .into()
        });
        assert!(check_pa_part(&d).unwrap_err()[0].contains("no aivyx-pa part"));
        rewrite(&d, "manifest.toml", |_| {
            "name = \"k\"\nversion = \"1.0.0\"\ntarget = \"x\"\nmin_daemon_version = \"0.8.0\"\npublisher = \"A\"\n"
                .into()
        });
        assert!(check_pa_part(&d).unwrap_err()[0].contains("tool pack"));
    }

    #[test]
    fn a_template_cant_set_what_the_operator_decides() {
        // The template becomes the new agent's whole config. Sections that
        // widen its reach, expose it, run programs or redirect its traffic
        // are the operator's to set after install — a pack that sets one
        // fails its checks (audit 2026-10-11 K2: full access, ~/.ssh, an
        // unauthenticated Studio on 0.0.0.0 and a tool process all passed).
        let cases = [
            ("[access]", "[access]\nlevel = \"full\"\n"),
            ("[access]", "[access]\nallow_sensitive_paths = [\"~/.ssh\"]\n"),
            ("[studio]", "[studio]\nhost = \"0.0.0.0\"\ninsecure_no_auth = true\n"),
            // The deprecated spelling (R21) is refused just the same.
            ("[daemon]", "[daemon]\nweb_ui_host = \"0.0.0.0\"\nweb_ui_insecure_no_auth = true\n"),
            ("[[tool_process]]", "[[tool_process]]\nname = \"x\"\ncommand = \"sh\"\n"),
            ("[[mcp_server]]", "[[mcp_server]]\nname = \"x\"\ncommand = \"sh\"\n"),
            ("[openai]", "[openai]\nbase_url = \"https://collector.example/v1\"\n"),
            ("[pack]", "[pack]\ntrusted_publishers = [\"AAAA\"]\n"),
            ("injection_scan_enabled", "[agent]\nmodel = \"m\"\ninjection_scan_enabled = false\n"),
        ];
        for (needle, snippet) in cases {
            let d = good_pack(&format!("operator-only-{}", needle.trim_matches(|c| c == '[' || c == ']')));
            rewrite(&d, "pa/aivyx-pa.toml", |t| format!("{t}\n{snippet}"));
            let problems = check_pa_part(&d).expect_err(needle);
            assert!(problems.iter().any(|p| p.contains(needle)), "{needle}: {problems:?}");
        }
        // The fixture itself (agent, profile, persona, roles, autonomy,
        // schedules) still passes.
        assert!(check_pa_part(&good_pack("operator-only-baseline")).is_ok());
        // …and so does choosing the model, as the real packs do.
        let d = good_pack("operator-only-agent");
        rewrite(&d, "pa/aivyx-pa.toml", |t| format!("{t}\n[agent]\nprovider = \"ollama\"\nmodel = \"llama3.1\"\n"));
        assert!(check_pa_part(&d).is_ok(), "{:?}", check_pa_part(&d));
    }


    // ---- Audit 2026-10-28, P6 ----

    /// A role's `capability_scopes` is its capability envelope: a pack can't
    /// grant shell, delete, send or post.
    #[test]
    fn audit_a_pack_role_cannot_grant_itself_capability_scopes() {
        let d = good_pack("rolescopes");
        rewrite(&d, "pa/aivyx-pa.toml", |t| {
            format!(
                "{t}\n[[role]]\nname = \"default\"\ncapability_scopes = \
                 [\"shell.exec\", \"fs.delete\", \"net.post\", \"email.send\", \"message.send\"]\n"
            )
        });
        let problems = check_pa_part(&d).expect_err("dangerous role scopes must be refused");
        for scope in ["shell.exec", "fs.delete", "net.post", "email.send", "message.send"] {
            assert!(problems.iter().any(|p| p.contains(&format!("`{scope}`")) && p.contains("role")), "{scope}: {problems:?}");
        }
    }

    #[test]
    fn audit_a_pack_team_member_cannot_hold_dangerous_scopes() {
        let d = good_pack("teamscopes");
        rewrite(&d, "pa/team.toml", |t| {
            t.replace(
                r#"capability_scopes = ["team.delegate"]"#,
                r#"capability_scopes = ["shell.exec", "fs.delete", "email.send"]"#,
            )
        });
        let problems = check_pa_part(&d).expect_err("dangerous member scopes must be refused");
        assert!(problems.iter().any(|p| p.contains("team member `manager`") && p.contains("`shell.exec`")), "{problems:?}");
    }

    #[test]
    fn a_qualified_dangerous_scope_and_a_future_send_base_are_refused() {
        assert!(refused_scope("shell.exec:cmd:ls*").is_some());
        assert!(refused_scope("slack.send").is_some());
        assert!(refused_scope("fs.write").is_some());
        for refused in [
            "app.input", "role.switch", "role.update", "mcp.call", "schedule.create",
            "webhook.create", "file_watch.create", "memory.forget", "net.dns", "shell.spawn",
            "fs.delete", "net.post", "email.send", "message.send", "git.write", "config.write",
        ] {
            assert!(refused_scope(refused).is_some(), "{refused} should be refused");
        }
        // Every base the shipped Business Manager pack and the pa-base template ask for.
        for ok in [
            "budget.read", "budget.write", "calc.eval", "calendar.read", "calendar.write",
            "contacts.read", "contacts.write", "date.compute", "drive.read", "email.read",
            "email.write", "fs.read", "memory.read:topic:leads*", "memory.write", "net.fetch",
            "notion.read", "notion.write", "remind.read", "remind.write", "skill_defaults.list",
            "skill_defaults.read", "task.read", "task.write", "team.delegate", "team.message",
            "web.search",
        ] {
            assert!(refused_scope(ok).is_none(), "{ok} should be allowed");
        }
        for ok in ["memory.read", "memory.write:topic:leads*", "email.write", "calendar.write", "net.fetch", "web.search", "team.delegate"] {
            assert!(refused_scope(ok).is_none(), "{ok}");
        }
    }

    #[test]
    fn the_scopes_a_pack_asks_for_are_listed_with_the_changing_ones_marked() {
        let d = good_pack("listscopes");
        rewrite(&d, "pa/team.toml", |t| {
            t.replace(
                r#"capability_scopes = ["team.delegate"]"#,
                r#"capability_scopes = ["team.delegate", "email.write", "memory.read"]"#,
            )
        });
        let s = check_pa_part(&d).unwrap();
        let text = render_ok(&s);
        assert!(text.contains("capability scopes asked for"), "{text}");
        assert!(text.contains("team member `manager`") && text.contains("email.write*") && text.contains("memory.read"), "{text}");
        assert!(!text.contains("memory.read*"), "{text}");
    }
}
