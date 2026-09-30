//! Phase 106 — `aivyx-pa mcp recipes` curated catalog.
//!
//! The substrate piece of the recipes catalog. The
//! canonical reference is `docs/MCP_RECIPES.md`; this module
//! holds the same data as a `&'static [Recipe]` slice so the
//! CLI can list and print recipes in-shell.
//!
//! Each recipe carries a paste-able `[[mcp_server]]` block,
//! an inline `[mcp_server.sandbox]` block per Q3a (the
//! Phase 55 sandbox-layer story is load-bearing; copy-paste
//! must produce a sandboxed config out of the gate),
//! required env-var notes, and capability-scope guidance for
//! the resulting `mcp.call:<server>:<tool>` qualifiers.
//!
//! ## Adding a recipe
//!
//! Append a new `Recipe { ... }` literal to `RECIPES` and add
//! a matching section to `docs/MCP_RECIPES.md`. The
//! `recipes_doc_and_module_stay_in_sync` test pins that
//! every entry in `RECIPES` has a matching `## ` heading in
//! the markdown doc — a forced-coupling that catches the
//! "module updated but doc didn't" failure mode.

// ---------------------------------------------------------------------------
// Recipe type
// ---------------------------------------------------------------------------

/// One curated MCP server recipe. All fields are `&'static
/// str` so the registry compiles to a flat byte slab — no
/// allocation at startup, no per-recipe `String` cost.
#[derive(Debug, Clone, Copy)]
pub struct Recipe {
    /// Short slug used as the `aivyx-pa mcp recipes <name>`
    /// lookup key. Stable; recipes must not be renamed once
    /// shipped without filing it as a renaming note in
    /// `docs/MCP_RECIPES.md`.
    pub name: &'static str,
    /// One-line description shown by `aivyx-pa mcp recipes`
    /// (no arg). Keep under ~70 chars so the listing fits
    /// in a terminal column without wrapping.
    pub description: &'static str,
    /// Paste-able TOML snippet — both `[[mcp_server]]` and
    /// `[mcp_server.sandbox]` blocks plus inline comments
    /// for env vars and capability scopes. Multi-line.
    pub toml_snippet: &'static str,
}

/// Lookup error for [`render_recipe`]. Carries the candidate
/// list so the dispatch printer can surface "did you mean…"
/// guidance without re-walking the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeError {
    pub message: String,
    pub candidates: Vec<&'static str>,
}

impl std::fmt::Display for RecipeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)?;
        if !self.candidates.is_empty() {
            write!(f, "\nAvailable recipes: {}", self.candidates.join(", "))?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// The curated MCP recipe set. Ordered by expected operator-
/// touch frequency: filesystem and version-control servers
/// come first; auxiliary / experimental servers come last.
pub const RECIPES: &[Recipe] = &[
    Recipe {
        name: "filesystem",
        description: "Read/write files inside a configured directory. Official npm package.",
        toml_snippet: r#"# Filesystem MCP server — read/write inside a single directory.
# The path argument scopes the server; the sandbox config is
# what *enforces* that scope at the OS level.
#
# Required env: none.
# Capability scopes the agent gets: mcp.call:fs-local:*

[[mcp_server]]
name = "fs-local"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/me/projects"]

[mcp_server.sandbox]
# bubblewrap binds the same path the server is told about so a
# typo in the args can't escape into $HOME.
wrapper = "bwrap"
args = [
    "--ro-bind", "/usr", "/usr",
    "--ro-bind", "/etc", "/etc",
    "--bind", "/home/me/projects", "/home/me/projects",
    "--dev", "/dev", "--proc", "/proc",
    "--unshare-net",   # filesystem server does not need network
    "--",
]
"#,
    },
    Recipe {
        name: "github",
        description: "Read/write GitHub repos, issues, PRs via the GitHub REST API.",
        toml_snippet: r#"# GitHub MCP server — issue / PR / repo operations.
#
# Required env: GITHUB_PERSONAL_ACCESS_TOKEN with the scopes
# the agent needs (typically `repo` for private + `read:org`).
# Capability scopes the agent gets: mcp.call:github:*

[[mcp_server]]
name = "github"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]

[mcp_server.sandbox]
# Network-only sandbox: no filesystem access, network kept so
# the server can reach api.github.com.
wrapper = "bwrap"
args = [
    "--ro-bind", "/usr", "/usr",
    "--ro-bind", "/etc", "/etc",
    "--ro-bind", "/etc/ssl", "/etc/ssl",
    "--dev", "/dev", "--proc", "/proc",
    "--setenv", "GITHUB_PERSONAL_ACCESS_TOKEN", "${GITHUB_PERSONAL_ACCESS_TOKEN}",
    "--",
]
"#,
    },
    Recipe {
        name: "gitlab",
        description: "GitLab projects / issues / MRs via the GitLab REST API.",
        toml_snippet: r#"# GitLab MCP server — symmetric to github above.
#
# Required env: GITLAB_PERSONAL_ACCESS_TOKEN, optional
# GITLAB_API_URL (defaults to https://gitlab.com).
# Capability scopes the agent gets: mcp.call:gitlab:*

[[mcp_server]]
name = "gitlab"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-gitlab"]

[mcp_server.sandbox]
wrapper = "bwrap"
args = [
    "--ro-bind", "/usr", "/usr",
    "--ro-bind", "/etc", "/etc",
    "--ro-bind", "/etc/ssl", "/etc/ssl",
    "--dev", "/dev", "--proc", "/proc",
    "--setenv", "GITLAB_PERSONAL_ACCESS_TOKEN", "${GITLAB_PERSONAL_ACCESS_TOKEN}",
    "--",
]
"#,
    },
    Recipe {
        name: "sqlite",
        description: "Query / mutate a local SQLite database with safety prompts.",
        toml_snippet: r#"# SQLite MCP server — point at a single .db file.
#
# Required env: none.
# Capability scopes the agent gets: mcp.call:sqlite:*

[[mcp_server]]
name = "sqlite"
command = "npx"
args = [
    "-y",
    "@modelcontextprotocol/server-sqlite",
    "/home/me/data/notes.db",
]

[mcp_server.sandbox]
# Bind only the directory holding the db file; no network.
wrapper = "bwrap"
args = [
    "--ro-bind", "/usr", "/usr",
    "--ro-bind", "/etc", "/etc",
    "--bind", "/home/me/data", "/home/me/data",
    "--dev", "/dev", "--proc", "/proc",
    "--unshare-net",
    "--",
]
"#,
    },
    Recipe {
        name: "postgres",
        description: "Read-only Postgres queries against a configured connection.",
        toml_snippet: r#"# Postgres MCP server — read-only query surface.
#
# Required env: POSTGRES_CONNECTION_STRING (e.g.
# "postgresql://user:pass@host:5432/dbname").
# Capability scopes the agent gets: mcp.call:postgres:*
#
# The official server is read-only by design; even an
# operator who grants write access at the database level
# cannot get write tools through this server.

[[mcp_server]]
name = "postgres"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-postgres"]

[mcp_server.sandbox]
wrapper = "bwrap"
args = [
    "--ro-bind", "/usr", "/usr",
    "--ro-bind", "/etc", "/etc",
    "--ro-bind", "/etc/ssl", "/etc/ssl",
    "--dev", "/dev", "--proc", "/proc",
    "--setenv", "POSTGRES_CONNECTION_STRING", "${POSTGRES_CONNECTION_STRING}",
    "--",
]
"#,
    },
    Recipe {
        name: "time",
        description: "Timezone-aware date / time math (current time, conversion).",
        toml_snippet: r#"# Time MCP server — small, no deps, no network.
#
# Required env: none.
# Capability scopes the agent gets: mcp.call:time:*

[[mcp_server]]
name = "time"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-time"]

[mcp_server.sandbox]
# Pure-compute server: no filesystem, no network.
wrapper = "bwrap"
args = [
    "--ro-bind", "/usr", "/usr",
    "--ro-bind", "/etc", "/etc",
    "--dev", "/dev", "--proc", "/proc",
    "--unshare-net",
    "--",
]
"#,
    },
    Recipe {
        name: "fetch",
        description: "HTTP GET into clean markdown — robots.txt-respecting.",
        toml_snippet: r#"# Fetch MCP server — HTTP GET with markdown conversion.
# Mostly overlaps with Aivyx's first-party `web.fetch` tool;
# use this when an agent wants the markdown-conversion path
# rather than raw response bytes.
#
# Required env: none.
# Capability scopes the agent gets: mcp.call:fetch:*

[[mcp_server]]
name = "fetch"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-fetch"]

[mcp_server.sandbox]
wrapper = "bwrap"
args = [
    "--ro-bind", "/usr", "/usr",
    "--ro-bind", "/etc", "/etc",
    "--ro-bind", "/etc/ssl", "/etc/ssl",
    "--dev", "/dev", "--proc", "/proc",
    "--",
]
"#,
    },
    Recipe {
        name: "brave-search",
        description: "Web + local search via the Brave Search API.",
        toml_snippet: r#"# Brave Search MCP server.
#
# Required env: BRAVE_API_KEY (sign up at api.search.brave.com).
# Capability scopes the agent gets: mcp.call:brave-search:*
#
# Aivyx already ships a bundled `web-search` MCP server with
# its own Brave fallback (Phase 46); use this entry only if
# you want the official Brave server's exact tool surface
# (web_search + local_search) rather than the bundled
# Aivyx surface.

[[mcp_server]]
name = "brave-search"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-brave-search"]

[mcp_server.sandbox]
wrapper = "bwrap"
args = [
    "--ro-bind", "/usr", "/usr",
    "--ro-bind", "/etc", "/etc",
    "--ro-bind", "/etc/ssl", "/etc/ssl",
    "--dev", "/dev", "--proc", "/proc",
    "--setenv", "BRAVE_API_KEY", "${BRAVE_API_KEY}",
    "--",
]
"#,
    },
    Recipe {
        name: "slack",
        description: "Read / post Slack messages via a bot token.",
        toml_snippet: r#"# Slack MCP server — read channels, post messages.
#
# Required env: SLACK_BOT_TOKEN (xoxb-…) and SLACK_TEAM_ID.
# Capability scopes the agent gets: mcp.call:slack:*
#
# Phase 108 (planned) lands a first-party Aivyx-Slack
# channel adapter. The MCP server is the "agent calls into
# Slack as a tool" surface; the Phase 108 adapter is the
# "operator talks to Aivyx from Slack" surface — they
# compose.

[[mcp_server]]
name = "slack"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-slack"]

[mcp_server.sandbox]
wrapper = "bwrap"
args = [
    "--ro-bind", "/usr", "/usr",
    "--ro-bind", "/etc", "/etc",
    "--ro-bind", "/etc/ssl", "/etc/ssl",
    "--dev", "/dev", "--proc", "/proc",
    "--setenv", "SLACK_BOT_TOKEN", "${SLACK_BOT_TOKEN}",
    "--setenv", "SLACK_TEAM_ID", "${SLACK_TEAM_ID}",
    "--",
]
"#,
    },
    Recipe {
        name: "memory",
        description: "Knowledge-graph memory the agent maintains across turns.",
        toml_snippet: r#"# Memory MCP server — knowledge-graph persistence.
# Distinct from Aivyx's first-party `memory.*` tools (which
# write to encrypted KeyDomain::Memory in the redb store);
# this MCP server keeps a JSON knowledge graph the agent can
# walk by entity / relation.
#
# Required env: none. The server writes its graph to a JSON
# file inside the sandbox path.
# Capability scopes the agent gets: mcp.call:memory:*

[[mcp_server]]
name = "kg-memory"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-memory"]

[mcp_server.sandbox]
wrapper = "bwrap"
args = [
    "--ro-bind", "/usr", "/usr",
    "--ro-bind", "/etc", "/etc",
    "--bind", "/home/me/.aivyx-pa/kg-memory", "/home/me/.aivyx-pa/kg-memory",
    "--dev", "/dev", "--proc", "/proc",
    "--unshare-net",
    "--",
]
"#,
    },
    Recipe {
        name: "puppeteer",
        description: "Headless-browser automation (navigate, screenshot, eval).",
        toml_snippet: r#"# Puppeteer MCP server — browser automation. Higher
# blast-radius than read-only servers — make sure the
# sandbox really constrains what pages the headless browser
# can be told to load.
#
# Required env: none.
# Capability scopes the agent gets: mcp.call:puppeteer:*

[[mcp_server]]
name = "puppeteer"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-puppeteer"]

[mcp_server.sandbox]
# Docker is a better fit than bwrap here — Chromium needs
# more pieces than a bwrap one-liner cleanly carries.
wrapper = "docker"
args = [
    "run", "--rm", "-i",
    "--cap-drop=ALL",
    "--security-opt=no-new-privileges",
    "--network=host",   # puppeteer needs to reach the internet
    "node:20-slim",
    "--",
]
"#,
    },
    Recipe {
        name: "everything",
        description: "Official reference / test server — useful to smoke-test setup.",
        toml_snippet: r#"# Everything MCP server — the official reference / test
# server that exposes one of each tool kind so an operator
# can verify Aivyx's MCP wiring without paying for a
# real-API setup. Useful first thing after `aivyx-pa init`.
#
# Required env: none.
# Capability scopes the agent gets: mcp.call:everything:*

[[mcp_server]]
name = "everything"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-everything"]

[mcp_server.sandbox]
wrapper = "bwrap"
args = [
    "--ro-bind", "/usr", "/usr",
    "--ro-bind", "/etc", "/etc",
    "--dev", "/dev", "--proc", "/proc",
    "--unshare-net",
    "--",
]
"#,
    },
    Recipe {
        name: "aivyx-coder",
        description: "Delegate bounded coding tasks to a local aivyx-coder process over MCP.",
        toml_snippet: r#"# aivyx-coder MCP server -- delegates bounded coding tasks to a
# local `aivyx-coder --mcp-server` process. Unlike every other
# recipe in this catalog, aivyx-coder is not third-party code: it
# ships its own Landlock+seccomp confinement and its own tiered
# access ceiling, so the sandbox block below is optional
# defense-in-depth here, not the thing actually keeping the
# operator safe -- that's the prerequisite below.
#
# Prerequisite: aivyx-coder's own config.toml must set
# `[mcp_server].max_access_level` ("plan" | "edit" | "execute")
# before this server can start -- there is no default, and it
# refuses to start unconfigured. This ceiling caps every session's
# access regardless of what a specialist's model requests; set it
# no higher than the specialists calling it actually need.
#
# Security note: an MCP-server session has no human to show a
# permission prompt to -- every tool call within its granted tier
# auto-resolves. "execute" means run_shell is auto-approved with no
# human in the loop; set max_access_level no higher than this team
# actually needs (see aivyx-coder's own README "MCP server
# integration" section for the full caveat).
#
# Required env: none.
# Capability scopes the agent gets: mcp.call:aivyx-coder:*

[[mcp_server]]
name = "aivyx-coder"
command = "aivyx-coder"
args = ["--mcp-server"]

[mcp_server.sandbox]
# aivyx-coder inherits the daemon's own working directory when
# spawned over stdio (there's no separate `cwd` field to point it
# elsewhere) -- bind that same directory, read-write, so its
# fs/shell tools can actually reach your project; substitute the
# real path, matching wherever your aivyx-pa daemon runs. No
# --unshare-net here (unlike filesystem/time/everything above):
# aivyx-coder needs network to reach its own configured local LLM
# backend (Ollama/vLLM/llama-server).
wrapper = "bwrap"
args = [
    "--ro-bind", "/usr", "/usr",
    "--ro-bind", "/etc", "/etc",
    "--ro-bind", "/home/me/.config/aivyx-coder", "/home/me/.config/aivyx-coder",
    "--bind", "/home/me/projects", "/home/me/projects",
    "--dev", "/dev", "--proc", "/proc",
    "--",
]
"#,
    },
];

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------
//
// `RECIPES` itself is `pub const` so callers needing to walk
// the set can do so directly; no `list_recipes()` accessor
// until a non-trivial caller surfaces (per the project's
// "fields added only when a concrete caller needs them"
// convention).

/// Render a single recipe's worked snippet, or an error
/// with the candidate list when `name` is not in the set.
///
/// The returned `String` is the exact bytes that
/// `aivyx-pa mcp recipes <name>` writes to stdout — no header,
/// no trailing summary, just the snippet plus a final
/// newline if the snippet didn't already end with one.
pub fn render_recipe(name: &str) -> Result<String, RecipeError> {
    match RECIPES.iter().find(|r| r.name == name) {
        Some(r) => {
            let mut out = r.toml_snippet.to_string();
            if !out.ends_with('\n') {
                out.push('\n');
            }
            Ok(out)
        }
        None => Err(RecipeError {
            message: format!("unknown recipe: `{name}`"),
            candidates: RECIPES.iter().map(|r| r.name).collect(),
        }),
    }
}

/// Render the bare `aivyx-pa mcp recipes` listing — one line
/// per recipe with the name padded to the longest name in
/// the set so the descriptions line up.
pub fn render_listing() -> String {
    let max_name_len = RECIPES.iter().map(|r| r.name.len()).max().unwrap_or(0);
    let mut out = String::new();
    for r in RECIPES {
        out.push_str(&format!(
            "  {:<width$}  {}\n",
            r.name,
            r.description,
            width = max_name_len,
        ));
    }
    out.push_str(
        "\nRun `aivyx-pa mcp recipes <name>` to print a recipe's full \
         TOML snippet.\nSee https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/MCP_RECIPES.md for the canonical \
         catalog.\n",
    );
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // -- Registry shape --------------------------------------------------

    #[test]
    fn registry_is_non_empty() {
        assert!(!RECIPES.is_empty(), "RECIPES must ship at least one entry");
    }

    #[test]
    fn every_recipe_has_a_unique_name() {
        let mut seen: HashSet<&str> = HashSet::new();
        for r in RECIPES {
            assert!(
                seen.insert(r.name),
                "duplicate recipe name `{}` in RECIPES",
                r.name,
            );
        }
    }

    #[test]
    fn every_recipe_has_a_non_empty_name() {
        for r in RECIPES {
            assert!(
                !r.name.is_empty(),
                "RECIPES contains an entry with an empty name",
            );
        }
    }

    #[test]
    fn every_recipe_has_a_non_empty_description() {
        for r in RECIPES {
            assert!(
                !r.description.is_empty(),
                "recipe `{}` has an empty description",
                r.name,
            );
        }
    }

    #[test]
    fn every_recipe_has_a_non_empty_toml_snippet() {
        for r in RECIPES {
            assert!(
                !r.toml_snippet.is_empty(),
                "recipe `{}` has an empty snippet",
                r.name,
            );
        }
    }

    #[test]
    fn every_recipe_snippet_includes_an_mcp_server_block() {
        // Q3a (sandbox by default) means every recipe's
        // snippet must carry a `[[mcp_server]]` block; this
        // test pins the property so a future recipe that
        // accidentally ships a sandbox-only snippet trips
        // here.
        for r in RECIPES {
            assert!(
                r.toml_snippet.contains("[[mcp_server]]"),
                "recipe `{}` snippet missing [[mcp_server]] block",
                r.name,
            );
        }
    }

    #[test]
    fn every_recipe_snippet_includes_a_sandbox_block() {
        // The Q3a contract: every recipe ships a sandbox
        // block so a copy-paste produces a sandboxed config
        // out of the gate.
        for r in RECIPES {
            assert!(
                r.toml_snippet.contains("[mcp_server.sandbox]"),
                "recipe `{}` snippet missing [mcp_server.sandbox] block",
                r.name,
            );
        }
    }

    // -- render_recipe ---------------------------------------------------

    #[test]
    fn render_recipe_known_name_returns_snippet() {
        let out = render_recipe("filesystem").expect("filesystem recipe must exist");
        assert!(out.contains("[[mcp_server]]"));
        assert!(out.contains("server-filesystem"));
        assert!(out.ends_with('\n'), "snippet must end with newline");
    }

    #[test]
    fn render_recipe_aivyx_coder_returns_snippet() {
        let out = render_recipe("aivyx-coder").expect("aivyx-coder recipe must exist");
        assert!(out.contains("[[mcp_server]]"));
        assert!(out.contains("aivyx-coder"));
        assert!(out.contains("--mcp-server"));
        assert!(out.contains("mcp.call:aivyx-coder:*"));
        assert!(out.ends_with('\n'), "snippet must end with newline");
    }

    #[test]
    fn render_recipe_unknown_name_errors_with_candidate_list() {
        let err = render_recipe("does-not-exist").expect_err("unknown recipe must error");
        assert!(err.message.contains("does-not-exist"));
        assert!(
            err.candidates.contains(&"filesystem"),
            "candidate list should include known recipes; got {:?}",
            err.candidates,
        );
    }

    #[test]
    fn render_recipe_error_display_carries_candidates() {
        let err = render_recipe("nope").unwrap_err();
        let s = format!("{err}");
        assert!(s.contains("nope"));
        assert!(s.contains("Available recipes:"));
        assert!(s.contains("filesystem"));
    }

    // -- render_listing --------------------------------------------------

    #[test]
    fn render_listing_includes_every_recipe_name() {
        let listing = render_listing();
        for r in RECIPES {
            assert!(
                listing.contains(r.name),
                "listing missing recipe name `{}`",
                r.name,
            );
        }
    }

    #[test]
    fn render_listing_points_at_canonical_doc() {
        let listing = render_listing();
        assert!(
            listing
                .contains("https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/MCP_RECIPES.md"),
            "listing should point at the canonical doc",
        );
    }

    #[test]
    fn render_listing_pads_names_for_column_alignment() {
        // The padding is what makes the descriptions line
        // up in a terminal column. Verify by checking that
        // the longest name's row has the smallest padding
        // (or equal — depends on tie-breaking).
        let listing = render_listing();
        let max_name_len = RECIPES.iter().map(|r| r.name.len()).max().unwrap();
        // Every line that names a recipe should be wider
        // than the longest name + the two-space leader.
        for r in RECIPES {
            let needle = format!("  {:<width$}", r.name, width = max_name_len);
            assert!(
                listing.contains(&needle),
                "expected padded recipe line for `{}`, got listing: {listing}",
                r.name,
            );
        }
    }
}
