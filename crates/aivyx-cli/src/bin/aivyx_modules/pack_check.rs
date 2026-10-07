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
}

/// Check the aivyx-pa part of an unpacked config pack or a staging folder.
/// `Err` lists every problem found, not just the first.
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

    // The template loads as a real aivyx-pa config, within the ceiling.
    let template = dir.join(&pa.template);
    if !template.is_file() {
        problems.push(format!("the template {} is missing", pa.template));
    } else {
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
                routines = cfg.schedules.len();
            }
        }
    }

    // The team config loads.
    if let Some(rel) = &pa.team_config {
        match aivyx_team::TeamConfig::load(dir.join(rel)) {
            Ok(team) => team_members = team.members.len(),
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

    if problems.is_empty() {
        Ok(PaPartSummary { manifest, skills, team_members, routines, autonomy, autonomy_overrides })
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
         requires: {} · optional: {}\n",
        s.manifest.name,
        s.manifest.version,
        autonomy_line(s),
        s.team_members,
        s.skills.len(),
        s.routines,
        list(&pa.requires),
        list(&pa.optional),
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
cron = "30 7 * * 1-5"
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
}
