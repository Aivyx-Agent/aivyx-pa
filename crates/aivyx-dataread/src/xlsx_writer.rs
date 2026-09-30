//! `data.xlsx.write` — write structured rows into a new spreadsheet.
//!
//! Chapter Sheaf SH.6. The write-side symmetric to `data.xlsx`
//! (SH.2): the reader turns a spreadsheet the agent can already
//! `fs.read` into rows; this tool turns rows the agent already has
//! into a spreadsheet it could already `fs.write`. Reuses the
//! **existing `fs.write` capability** via
//! [`crate::sandbox::ReaderSandbox::scope_for_write`] — no new
//! capability base, no new I/O reach.
//!
//! ## Tool surface
//!
//! - `data.xlsx.write` — `{path: string (required), sheet_name?:
//!   string (default "Sheet1"), headers?: string[], rows: string[][]
//!   (required), overwrite?: bool (default false)}` → `{path,
//!   sheet_name, row_count, col_count}`. Every cell is written as a
//!   string (symmetric with the reader, which stringifies every cell
//!   on the way out) — no attempt to infer numeric/date types on the
//!   way in; a future revision can add typed cells if operators need
//!   real spreadsheet formulas/number formatting.

use async_trait::async_trait;
use rust_xlsxwriter::Workbook;
use serde_json::{json, Value};

use aivyx_capability::Scope;
use aivyx_core::{AivyxError, Tool, ToolContext, ToolId, ToolOutcome, Verification};

use crate::sandbox::ReaderSandbox;

/// Hard cap on rows written per call — mirrors the reader's
/// `HARD_MAX_ROWS`, bounding a pathological write the same way the
/// read side bounds a pathological parse.
const HARD_MAX_ROWS: usize = 100_000;
/// Hard cap on columns per row — a spreadsheet with more than this
/// many columns is almost certainly a malformed call, not a real
/// report.
const HARD_MAX_COLS: usize = 1_000;

pub struct DataXlsxWriteTool {
    id: ToolId,
    schema: Value,
    sandbox: ReaderSandbox,
}

impl DataXlsxWriteTool {
    pub fn new(sandbox: ReaderSandbox) -> Self {
        Self {
            id: ToolId::new(),
            schema: schema(),
            sandbox,
        }
    }
}

#[async_trait]
impl Tool for DataXlsxWriteTool {
    fn id(&self) -> ToolId {
        self.id
    }
    fn name(&self) -> &str {
        "data.xlsx.write"
    }
    /// Writes a real .xlsx file under fs_root via `fs.write` — checkpoint
    /// before every call, same as FsWriteTool/FsDeleteTool/ShellExecTool.
    fn mutates_fs_root(&self) -> bool {
        true
    }
    fn description(&self) -> &str {
        "Write structured rows into a new .xlsx spreadsheet under the \
         agent's sandbox root. Input: `{path: string (required), \
         sheet_name: string (optional, default \"Sheet1\"), headers: \
         string[] (optional), rows: string[][] (required), overwrite: \
         bool (optional, default false — required to replace an \
         existing file)}`. Returns `{path, sheet_name, row_count, \
         col_count}`. Every cell is written as text. Scope: `fs.write` \
         (same sandbox as fs.write)."
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn required_scope(&self, input: &Value) -> Scope {
        self.sandbox.scope_for_write(input)
    }
    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let sheet_name = input
            .get("sheet_name")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("Sheet1")
            .to_string();

        let headers: Option<Vec<String>> = match input.get("headers") {
            None | Some(Value::Null) => None,
            Some(v) => match parse_string_array(v) {
                Ok(h) => Some(h),
                Err(e) => return fail(self.id, format!("data.xlsx.write: `headers` {e}")),
            },
        };

        let rows = match input.get("rows") {
            Some(Value::Array(rows)) => rows,
            _ => {
                return fail(
                    self.id,
                    "data.xlsx.write: input must have a `rows` array of arrays".to_string(),
                )
            }
        };
        if rows.len() > HARD_MAX_ROWS {
            return fail(
                self.id,
                format!(
                    "data.xlsx.write: {} rows exceeds the {HARD_MAX_ROWS} limit",
                    rows.len()
                ),
            );
        }
        let mut parsed_rows: Vec<Vec<String>> = Vec::with_capacity(rows.len());
        for (i, row) in rows.iter().enumerate() {
            match parse_string_array(row) {
                Ok(r) => {
                    if r.len() > HARD_MAX_COLS {
                        return fail(
                            self.id,
                            format!(
                                "data.xlsx.write: row {i} has {} columns, exceeds the \
                                 {HARD_MAX_COLS} limit",
                                r.len()
                            ),
                        );
                    }
                    parsed_rows.push(r);
                }
                Err(e) => return fail(self.id, format!("data.xlsx.write: `rows[{i}]` {e}")),
            }
        }

        let (tmp_path, final_path) = match self.sandbox.resolve_write_target(&input, self.id) {
            Ok(paths) => paths,
            Err(outcome) => return outcome,
        };

        let mut workbook = Workbook::new();
        let worksheet = workbook.add_worksheet();
        if let Err(e) = worksheet.set_name(&sheet_name) {
            return fail(self.id, format!("data.xlsx.write: invalid sheet name: {e}"));
        }

        let mut row_cursor: u32 = 0;
        let mut col_count = 0usize;
        if let Some(h) = &headers {
            col_count = col_count.max(h.len());
            for (col, cell) in h.iter().enumerate() {
                if let Err(e) = worksheet.write_string(row_cursor, col as u16, cell) {
                    return fail(self.id, format!("data.xlsx.write: {e}"));
                }
            }
            row_cursor += 1;
        }
        for row in &parsed_rows {
            col_count = col_count.max(row.len());
            for (col, cell) in row.iter().enumerate() {
                if let Err(e) = worksheet.write_string(row_cursor, col as u16, cell) {
                    return fail(self.id, format!("data.xlsx.write: {e}"));
                }
            }
            row_cursor += 1;
        }

        if let Err(e) = workbook.save(&tmp_path) {
            let _ = std::fs::remove_file(&tmp_path);
            return fail(self.id, format!("data.xlsx.write: cannot serialize workbook: {e}"));
        }
        if let Err(outcome) = ReaderSandbox::commit_write(&tmp_path, &final_path, self.id) {
            return outcome;
        }

        ToolOutcome::Completed {
            output: json!({
                "path": final_path.display().to_string(),
                "sheet_name": sheet_name,
                "row_count": parsed_rows.len(),
                "col_count": col_count,
            }),
            verified: Verification::NotApplicable,
        }
    }
}

fn parse_string_array(v: &Value) -> Result<Vec<String>, String> {
    let Value::Array(items) = v else {
        return Err("must be an array".to_string());
    };
    items
        .iter()
        .map(|c| match c {
            Value::String(s) => Ok(s.clone()),
            Value::Null => Ok(String::new()),
            other => Err(format!("cell {other:?} must be a string")),
        })
        .collect()
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "minLength": 1, "description": "Where to write the .xlsx file, under the sandbox root." },
            "sheet_name": { "type": "string", "description": "Worksheet name (default \"Sheet1\")." },
            "headers": { "type": "array", "items": { "type": "string" }, "description": "Optional header row." },
            "rows": {
                "type": "array",
                "items": { "type": "array", "items": { "type": "string" } },
                "description": "Data rows, each an array of cell strings."
            },
            "overwrite": { "type": "boolean", "description": "Must be true to replace an existing file (default false)." },
            "confirmed": { "type": "boolean", "description": "Set only by Aivyx PA after the operator approves replacing an existing file." }
        },
        "required": ["path", "rows"],
        "additionalProperties": false
    })
}

fn fail(tool: ToolId, detail: String) -> ToolOutcome {
    ToolOutcome::Failed(AivyxError::Tool { tool, detail })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn scratch_root() -> std::path::PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "aivyx-xlsxwrite-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ctx_parts() -> (aivyx_core::AgentId, aivyx_core::SessionId, aivyx_core::TurnId) {
        (
            aivyx_core::AgentId::new(),
            aivyx_core::SessionId::new(),
            aivyx_core::TurnId::new(),
        )
    }

    struct NoopChannel;
    #[async_trait]
    impl aivyx_core::ChannelContext for NoopChannel {
        fn channel_name(&self) -> &str {
            "test"
        }
        fn platform(&self) -> aivyx_core::ChannelPlatform {
            aivyx_core::ChannelPlatform::Local
        }
        fn trust_tier(&self) -> aivyx_capability::TrustTier {
            aivyx_capability::TrustTier::Trusted
        }
        fn session_id(&self) -> aivyx_core::SessionId {
            aivyx_core::SessionId::new()
        }
        async fn stream_event(
            &self,
            _: aivyx_core::StreamEvent<'_>,
        ) -> Result<(), aivyx_core::ChannelError> {
            Ok(())
        }
        async fn finalize(
            &self,
            _: &aivyx_core::TurnOutcome,
        ) -> Result<(), aivyx_core::ChannelError> {
            Ok(())
        }
        fn cancellation_token(&self) -> aivyx_core::CancellationToken {
            aivyx_core::CancellationToken::new()
        }
    }
    struct NoopAudit;
    impl aivyx_core::AuditHook for NoopAudit {
        fn on_event(&self, _: aivyx_core::AuditTag) {}
    }

    #[tokio::test]
    async fn writes_a_readable_workbook_with_headers() {
        let root = scratch_root();
        let sandbox = ReaderSandbox::new(&root).unwrap();
        let tool = DataXlsxWriteTool::new(sandbox.clone());
        let (agent_id, session_id, turn_id) = ctx_parts();
        let channel = NoopChannel;
        let audit = NoopAudit;
        let cancellation = aivyx_core::CancellationToken::new();
        let ctx = ToolContext {
            agent_id,
            session_id,
            turn_id,
            channel: &channel,
            audit: &audit,
            cancellation: &cancellation,
            message_origin: aivyx_core::MessageOrigin::Operator,
        };

        let out = tool
            .execute(
                json!({
                    "path": "report.xlsx",
                    "headers": ["name", "age"],
                    "rows": [["Ada", "36"], ["Grace", "45"]],
                }),
                &ctx,
            )
            .await;
        let ToolOutcome::Completed { output, .. } = out else {
            panic!("expected Completed, got {out:?}");
        };
        assert_eq!(output["row_count"], 2);
        assert_eq!(output["col_count"], 2);

        // Round-trip through the reader to prove it's a real workbook.
        let file = std::fs::read(root.join("report.xlsx")).unwrap();
        let parsed = crate::xlsx_reader::parse_xlsx(&file, None, true, 100).unwrap();
        assert_eq!(parsed.headers.unwrap(), vec!["name", "age"]);
        assert_eq!(parsed.rows[0], vec!["Ada", "36"]);
    }

    #[tokio::test]
    async fn refuses_to_overwrite_without_the_flag() {
        let root = scratch_root();
        std::fs::write(root.join("existing.xlsx"), b"not a real workbook").unwrap();
        let sandbox = ReaderSandbox::new(&root).unwrap();
        let tool = DataXlsxWriteTool::new(sandbox);
        let (agent_id, session_id, turn_id) = ctx_parts();
        let channel = NoopChannel;
        let audit = NoopAudit;
        let cancellation = aivyx_core::CancellationToken::new();
        let ctx = ToolContext {
            agent_id,
            session_id,
            turn_id,
            channel: &channel,
            audit: &audit,
            cancellation: &cancellation,
            message_origin: aivyx_core::MessageOrigin::Operator,
        };
        let out = tool
            .execute(json!({"path": "existing.xlsx", "rows": [["x"]]}), &ctx)
            .await;
        assert!(matches!(out, ToolOutcome::Failed(_)));
    }

    #[test]
    fn tool_metadata_is_sound() {
        let dir = std::env::temp_dir();
        let sb = ReaderSandbox::new(&dir).unwrap();
        let tool = DataXlsxWriteTool::new(sb);
        assert_eq!(tool.name(), "data.xlsx.write");
        assert!(!tool.description().is_empty());
        assert_eq!(tool.input_schema()["type"], "object");
        assert_eq!(
            tool.required_scope(&json!({"path": "x.xlsx"})).base(),
            "fs.write"
        );
    }

    #[test]
    fn data_xlsx_write_mutates_fs_root() {
        let dir = std::env::temp_dir();
        let sb = ReaderSandbox::new(&dir).unwrap();
        let tool = DataXlsxWriteTool::new(sb);
        assert!(tool.mutates_fs_root());
    }
}
