//! Concrete tool implementations that ship with the Aivyx core crate.
//!
//! ## Why tools live in `aivyx-core` (for now)
//!
//! Phase 4 Q1 asked whether a filesystem tool should live in `aivyx-core`
//! (keeping D8's 9-crate workspace lock), in a new `aivyx-tool-fs` crate
//! (requiring a D8 amendment), or in an `aivyx-tools` umbrella. The
//! resolution at Phase 4 task 2 entry is **option 1**: a small `tools`
//! sub-module inside `aivyx-core`. The reasoning, to be re-evaluated at
//! Phase 4 exit and carried into Phase 6:
//!
//! - Every type the filesystem tool needs (`Tool`, `ToolContext`,
//!   `ToolOutcome`, `Scope`, `Verification`, `AivyxError`) already lives
//!   in `aivyx-core`. Spinning up a new crate just to get a `pub use`
//!   of these types back would be pure overhead.
//! - One concrete tool is not evidence of a "tools umbrella" pattern.
//!   The right time to split is when there are at least two concrete
//!   tools and the split pays for itself in compile-time isolation or
//!   test-surface isolation. Today there's one — move it later, once
//!   Phase 6's memory-as-tool work gives us a second data point.
//! - D8's 9-crate lock stands without amendment, which keeps the
//!   DESIGN.md empty-diff streak alive (3 phases, heading for 4).
//!
//! The trade is that `aivyx-core` starts to accrete a "standard library
//! of tools" surface. Phase 4 exit will note this and either ratify it
//! or propose an amendment with real evidence behind it.

pub mod fs;
pub mod git;
pub mod net_dns;
pub mod role_switch;
pub mod shell;
pub mod skill_defaults;
pub mod skills;
pub mod web_fetch;
pub mod workspace;

pub use fs::{
    FsDeleteTool, FsDeleteToolConfig, FsMetadataTool, FsMetadataToolConfig, FsReadTool,
    FsReadToolConfig, FsWriteTool, FsWriteToolConfig,
};
pub use git::{GitCommitTool, GitDiffTool, GitReadToolConfig, GitStatusTool, GitWriteToolConfig};
pub use net_dns::NetDnsTool;
pub use role_switch::{ChildAgentFactory, RoleSwitchTool};
pub use shell::{ShellExecTool, ShellExecToolConfig};
pub use skill_defaults::{
    SkillDefaultsListTool, SkillDefaultsReadTool, render_default_skills_section,
};
pub use skills::{SkillReader, SkillsInvokeTool, SkillsListTool};
pub use web_fetch::{
    WebExtractTool, WebExtractToolConfig, WebFetchTool, WebFetchToolConfig, WebPostTool,
    WebPostToolConfig,
};

/// Chapter Atlas (AT.3) — tool-metadata quality invariants.
///
/// The `name`, `description`, and `input_schema` a tool exposes are what the LLM
/// reads to decide whether and how to call it; broken metadata silently degrades
/// every turn. This checks the correctness floor and returns a list of human-
/// readable violations (empty = healthy). Each crate's tests run it over the
/// tools it owns (`tool_quality` sweeps), so a thin description or malformed
/// schema fails CI rather than shipping.
///
/// Invariants (correctness, not style):
/// - `name` is non-empty and **dotted** (`domain.verb`, the catalog convention);
/// - `description` is a real sentence (≥ 10 non-space chars), not a stub;
/// - `input_schema` is a JSON object with `"type": "object"` and, if present,
///   a `properties` object (the shape providers expect for tool params).
pub fn check_tool_quality(tool: &dyn crate::Tool) -> Vec<String> {
    let mut issues = Vec::new();
    let name = tool.name();
    if name.trim().is_empty() {
        issues.push("name is empty".to_string());
    } else if !name.contains('.') {
        issues.push(format!(
            "name {name:?} is not dotted (expected domain.verb)"
        ));
    }

    let desc = tool.description().trim();
    if desc.chars().filter(|c| !c.is_whitespace()).count() < 10 {
        issues.push(format!("{name}: description too short / stub ({desc:?})"));
    }

    let schema = tool.input_schema();
    if !schema.is_object() {
        issues.push(format!("{name}: input_schema is not a JSON object"));
    } else {
        if schema.get("type").and_then(serde_json::Value::as_str) != Some("object") {
            issues.push(format!("{name}: input_schema.type must be \"object\""));
        }
        if let Some(props) = schema.get("properties") {
            if !props.is_object() {
                issues.push(format!("{name}: input_schema.properties must be an object"));
            }
        }
    }
    issues
}

#[cfg(test)]
mod quality_tests {
    use super::*;
    use crate::Tool;
    use std::path::PathBuf;
    use std::sync::Arc;

    /// Chapter Atlas (AT.3) — every cheaply-constructible aivyx-core substrate
    /// tool must satisfy the metadata quality floor. (Skills tools need a live
    /// SkillReader and are covered by their own module tests.)
    #[test]
    fn substrate_tools_meet_quality_floor() {
        let dir = std::env::temp_dir();
        let (git_status, git_diff) = GitReadToolConfig::new(Vec::<PathBuf>::new())
            .build()
            .expect("git tools build");
        // git.commit with an empty allow-set builds without a real repo
        // on disk (no entries to canonicalize) — enough to sweep metadata.
        let git_commit = GitWriteToolConfig::new(Vec::<PathBuf>::new())
            .build()
            .expect("git.commit builds");
        let mut tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(WebFetchToolConfig::new().build().expect("web.fetch")),
            Arc::new(WebPostToolConfig::new().build().expect("web.post")),
            Arc::new(WebExtractToolConfig::new().build().expect("web.extract")),
            Arc::new(NetDnsTool::default()),
            Arc::new(git_status),
            Arc::new(git_diff),
            Arc::new(git_commit),
            Arc::new(
                ShellExecToolConfig::new(dir.clone())
                    .build()
                    .expect("shell"),
            ),
            Arc::new(FsReadToolConfig::new(dir.clone()).build().expect("fs.read")),
            Arc::new(
                FsWriteToolConfig::new(dir.clone())
                    .build()
                    .expect("fs.write"),
            ),
            Arc::new(
                FsMetadataToolConfig::new(dir.clone())
                    .build()
                    .expect("fs.metadata"),
            ),
            Arc::new(
                FsDeleteToolConfig::new(dir.clone())
                    .build()
                    .expect("fs.delete"),
            ),
            Arc::new(RoleSwitchTool::default()),
        ];
        if let Ok((ws_tools, _)) = workspace::build_workspace_tools(&dir, None) {
            tools.extend(ws_tools);
        }

        let mut all = Vec::new();
        for t in &tools {
            for issue in check_tool_quality(t.as_ref()) {
                all.push(issue);
            }
        }
        assert!(
            all.is_empty(),
            "substrate tool quality issues:\n  {}",
            all.join("\n  ")
        );
    }

    #[test]
    fn quality_helper_flags_bad_metadata() {
        // Guard the guard: a stub-described, undotted, wrong-schema tool is caught.
        struct Bad;
        #[async_trait::async_trait]
        impl Tool for Bad {
            fn id(&self) -> crate::ToolId {
                crate::ToolId::new()
            }
            fn name(&self) -> &str {
                "bad"
            }
            fn description(&self) -> &str {
                "x"
            }
            fn input_schema(&self) -> &serde_json::Value {
                static S: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
                S.get_or_init(|| serde_json::json!("not an object"))
            }
            fn required_scope(&self, _: &serde_json::Value) -> aivyx_capability::Scope {
                aivyx_capability::Scope::parse("audit.read").unwrap()
            }
            async fn execute(
                &self,
                _: serde_json::Value,
                _: &crate::ToolContext<'_>,
            ) -> crate::ToolOutcome {
                crate::ToolOutcome::Completed {
                    output: serde_json::Value::Null,
                    verified: crate::Verification::NotApplicable,
                }
            }
        }
        let issues = check_tool_quality(&Bad);
        assert!(
            issues.iter().any(|i| i.contains("not dotted")),
            "{issues:?}"
        );
        assert!(
            issues.iter().any(|i| i.contains("description too short")),
            "{issues:?}"
        );
        assert!(
            issues.iter().any(|i| i.contains("not a JSON object")),
            "{issues:?}"
        );
    }
}
