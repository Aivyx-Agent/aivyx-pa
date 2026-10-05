//! Phase 66 — starter profile template registry.
//!
//! Loads named templates from two sources per **Q1(c)** at
//! sign-off:
//!
//! 1. **Bundled**: source-of-truth files live in
//!    `examples/templates/*.toml`. They're embedded into the
//!    binary at compile time via `include_str!`. Every fresh
//!    install ships with the bundled set; no filesystem setup
//!    needed for `aivyx-pa init --template <name>` to work.
//! 2. **User override** at `~/.local/share/aivyx-pa/templates/`.
//!    If a `<name>.toml` exists there it shadows the bundled
//!    template of the same name. Operators can author their
//!    own templates without rebuilding the binary.
//!
//! The user-dir directory is optional — the loader silently
//! ignores it if it doesn't exist.
//!
//! ## Description metadata
//!
//! Every template begins with a TOML comment of the form
//! `# description: <one-line summary>`. The registry parses
//! this line to build the `--list-templates` output. User-
//! authored templates that omit the line get a default
//! `"(no description)"` label.

use std::path::PathBuf;

/// One starter template. Bundled templates carry `'static`
/// strings; user-dir templates own their content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    /// Operator-facing identifier (e.g. `"coder"`, `"researcher"`).
    pub name: String,
    /// One-line summary parsed from the leading
    /// `# description: …` comment. `"(no description)"` if the
    /// template's first line doesn't match that shape.
    pub description: String,
    /// Where this template came from. Surfaced in the
    /// `--list-templates` output so operators can tell which
    /// templates shadow which.
    pub source: TemplateSource,
    /// Raw TOML content of the template.
    pub toml_content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateSource {
    /// Embedded at compile time via `include_str!`.
    Bundled,
    /// Read from the user-dir override at
    /// `~/.local/share/aivyx-pa/templates/<name>.toml`.
    User,
}

impl TemplateSource {
    pub fn label(self) -> &'static str {
        match self {
            TemplateSource::Bundled => "bundled",
            TemplateSource::User => "user",
        }
    }
}

// ---------------------------------------------------------------------------
// Bundled templates (compile-time embedded)
// ---------------------------------------------------------------------------

const BUNDLED_CODER: &str = include_str!("../../../../../examples/templates/aivyx-coder.toml");
const BUNDLED_RESEARCHER: &str =
    include_str!("../../../../../examples/templates/aivyx-researcher.toml");
const BUNDLED_PERSONAL: &str =
    include_str!("../../../../../examples/templates/aivyx-personal.toml");
// The first Aivyx vertical pack — Kitchen / Back-of-House
// (docs/VERTICAL_PACKS.md). Richer than the personal-assistant
// archetypes: it wires the `aivyx-kitchen` tool process + a BOH role
// + the opt-in nightly reorder schedule.
const BUNDLED_KITCHEN: &str = include_str!("../../../../../examples/templates/aivyx-kitchen.toml");
// Chapter N — the full personal-assistant posture: home-directory access
// with the confirm-first seatbelt on destructive ops.
const BUNDLED_FULL_ACCESS: &str =
    include_str!("../../../../../examples/templates/aivyx-full-access.toml");

/// Every bundled template, keyed by name. Order is the
/// canonical listing order.
fn bundled_specs() -> &'static [(&'static str, &'static str)] {
    &[
        ("coder", BUNDLED_CODER),
        ("researcher", BUNDLED_RESEARCHER),
        ("personal", BUNDLED_PERSONAL),
        ("kitchen", BUNDLED_KITCHEN),
        ("full-access", BUNDLED_FULL_ACCESS),
    ]
}

/// Snapshot of bundled templates with descriptions parsed.
pub fn bundled_templates() -> Vec<Template> {
    bundled_specs()
        .iter()
        .map(|(name, content)| Template {
            name: (*name).to_string(),
            description: parse_description(content),
            source: TemplateSource::Bundled,
            toml_content: (*content).to_string(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// User-dir override lookup
// ---------------------------------------------------------------------------

/// Resolve the user template directory for the current instance.
/// For default instance: `$XDG_DATA_HOME/aivyx-pa/templates/` if set,
/// otherwise `~/.local/share/aivyx-pa/templates/`.
/// For named instances: `~/.local/share/aivyx-pa/instances/<n>/templates/`.
/// Returns `None` if the instance cannot be resolved.
pub fn user_template_dir() -> Option<PathBuf> {
    let instance = aivyx_instance::InstancePaths::current().ok()?;
    instance.data_dir().map(|d| d.join("templates"))
}

/// Enumerate user-dir templates. Returns an empty Vec if the
/// directory doesn't exist or isn't readable — user-dir
/// templates are an opt-in, so absence is silent.
pub fn user_templates() -> Vec<Template> {
    let Some(dir) = user_template_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("toml") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        out.push(Template {
            name: stem.to_string(),
            description: parse_description(&content),
            source: TemplateSource::User,
            toml_content: content,
        });
    }
    // Stable alphabetical order for listing.
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

// ---------------------------------------------------------------------------
// Listing + lookup
// ---------------------------------------------------------------------------

/// Every available template — bundled and user, with user-dir
/// entries shadowing bundled entries of the same name. Order:
/// bundled-canonical first, then any user-only templates
/// (alphabetical).
pub fn list_templates() -> Vec<Template> {
    let user = user_templates();
    let user_names: std::collections::HashSet<String> =
        user.iter().map(|t| t.name.clone()).collect();
    let mut out: Vec<Template> = Vec::new();
    for t in bundled_templates() {
        if let Some(override_t) = user.iter().find(|u| u.name == t.name) {
            out.push(override_t.clone());
        } else {
            out.push(t);
        }
    }
    // Append user-only templates (those without a bundled counterpart).
    let bundled_names: std::collections::HashSet<&str> =
        bundled_specs().iter().map(|(n, _)| *n).collect();
    for t in user
        .into_iter()
        .filter(|t| !bundled_names.contains(t.name.as_str()))
    {
        if user_names.contains(&t.name) && !bundled_names.contains(t.name.as_str()) {
            out.push(t);
        }
    }
    out
}

/// Look up a single template by name. User-dir wins over
/// bundled. Returns `Err` if no template with that name exists
/// in either source.
pub fn load_template(name: &str) -> Result<Template, String> {
    for t in user_templates() {
        if t.name == name {
            return Ok(t);
        }
    }
    for t in bundled_templates() {
        if t.name == name {
            return Ok(t);
        }
    }
    Err(format!(
        "unknown template `{name}` — run `aivyx-pa init --list-templates` \
         to see what's available"
    ))
}

// ---------------------------------------------------------------------------
// Description parser
// ---------------------------------------------------------------------------

/// Extract the description line from the leading TOML comment.
/// Expected shape: `# description: …` on (or near) the first
/// non-empty line of the file. Returns `"(no description)"` if
/// the marker isn't found in the first 5 lines.
pub fn parse_description(content: &str) -> String {
    for (idx, line) in content.lines().enumerate() {
        if idx >= 5 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("# description:") {
            return rest.trim().to_string();
        }
        if let Some(rest) = trimmed.strip_prefix("# Description:") {
            return rest.trim().to_string();
        }
    }
    "(no description)".to_string()
}

/// Render the listing output an operator sees with
/// `--list-templates`. Public for the CLI to call directly.
pub fn render_template_list(templates: &[Template]) -> String {
    if templates.is_empty() {
        return "No templates available.\n".to_string();
    }
    let mut out = String::new();
    out.push_str("Available templates:\n\n");
    let max_name = templates.iter().map(|t| t.name.len()).max().unwrap_or(0);
    for t in templates {
        out.push_str(&format!(
            "  {name:<width$}  ({source})  {desc}\n",
            name = t.name,
            width = max_name,
            source = t.source.label(),
            desc = t.description,
        ));
    }
    out.push_str(
        "\nUse `aivyx-pa init --template <name>` to start the wizard \
         pre-filled from a template.\n",
    );
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_templates_returns_the_starter_archetypes_and_kitchen_pack() {
        let bundled = bundled_templates();
        let names: Vec<&str> = bundled.iter().map(|t| t.name.as_str()).collect();
        // Three personal-assistant archetypes + the Kitchen vertical pack
        // + the Chapter N full-access posture.
        assert_eq!(
            names,
            vec!["coder", "researcher", "personal", "kitchen", "full-access"]
        );
        for t in &bundled {
            assert_eq!(t.source, TemplateSource::Bundled);
            assert!(!t.toml_content.is_empty(), "{} has content", t.name);
            assert_ne!(
                t.description, "(no description)",
                "{} must have a description: line",
                t.name
            );
        }
    }

    #[test]
    fn parse_description_finds_marker_on_first_line() {
        let content = "# description: foo bar baz\n[agent]\nprovider = \"ollama\"\n";
        assert_eq!(parse_description(content), "foo bar baz");
    }

    #[test]
    fn parse_description_finds_marker_after_blank_lines() {
        let content = "\n\n# description: lorem ipsum\n[agent]\n";
        assert_eq!(parse_description(content), "lorem ipsum");
    }

    #[test]
    fn parse_description_accepts_capitalized_marker() {
        let content = "# Description: caps are fine\n";
        assert_eq!(parse_description(content), "caps are fine");
    }

    #[test]
    fn parse_description_returns_placeholder_when_missing() {
        let content = "# this comment doesn't match\n# nor does this\n[agent]\n";
        assert_eq!(parse_description(content), "(no description)");
    }

    #[test]
    fn parse_description_only_scans_first_five_lines() {
        let mut content = String::new();
        for _ in 0..10 {
            content.push_str("# noise\n");
        }
        content.push_str("# description: too late\n");
        assert_eq!(parse_description(&content), "(no description)");
    }

    #[test]
    fn load_template_returns_bundled_by_name() {
        let coder = load_template("coder").expect("coder must load");
        assert_eq!(coder.name, "coder");
        assert_eq!(coder.source, TemplateSource::Bundled);
    }

    #[test]
    fn load_template_unknown_name_errors() {
        let err = load_template("nonexistent").expect_err("must error");
        assert!(err.contains("unknown template"), "error: {err}");
        assert!(err.contains("nonexistent"), "error: {err}");
        assert!(err.contains("--list-templates"), "error: {err}");
    }

    #[test]
    fn list_templates_includes_all_bundled() {
        let list = list_templates();
        assert!(list.len() >= 3, "at least the three bundled archetypes");
        let names: Vec<&str> = list.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"coder"));
        assert!(names.contains(&"researcher"));
        assert!(names.contains(&"personal"));
    }

    #[test]
    fn bundled_templates_are_well_formed_toml() {
        // Every bundled template must parse as TOML — catches
        // malformed content at test time, not at user time.
        // Uses `toml_edit` (already a workspace dep) rather
        // than the `toml` crate to avoid adding a new dependency.
        for t in bundled_templates() {
            let parsed: Result<toml_edit::DocumentMut, _> = t.toml_content.parse();
            assert!(
                parsed.is_ok(),
                "{}.toml failed to parse: {:?}",
                t.name,
                parsed.err()
            );
        }
    }

    #[test]
    fn render_template_list_formats_a_visible_block() {
        let templates = bundled_templates();
        let rendered = render_template_list(&templates);
        assert!(rendered.contains("Available templates:"));
        assert!(rendered.contains("coder"));
        assert!(rendered.contains("(bundled)"));
        assert!(rendered.contains("--template"));
    }

    #[test]
    fn render_template_list_handles_empty() {
        let rendered = render_template_list(&[]);
        assert!(rendered.contains("No templates available"));
    }

    #[test]
    fn template_source_labels_are_stable() {
        assert_eq!(TemplateSource::Bundled.label(), "bundled");
        assert_eq!(TemplateSource::User.label(), "user");
    }

    #[test]
    fn user_template_dir_uses_xdg_when_set() {
        // SAFETY: serialized through env_lock in other tests'
        // patterns; this test's mutation is contained.
        let prior = std::env::var("XDG_DATA_HOME").ok();
        unsafe {
            std::env::set_var("XDG_DATA_HOME", "/custom/xdg");
        }
        let dir = user_template_dir().expect("dir must be Some");
        assert_eq!(dir, PathBuf::from("/custom/xdg/aivyx-pa/templates"));
        // Restore.
        unsafe {
            match prior {
                Some(v) => std::env::set_var("XDG_DATA_HOME", v),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }
    }
}
