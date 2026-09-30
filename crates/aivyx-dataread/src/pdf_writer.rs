//! `data.pdf.write` — lay text out into a new, real PDF.
//!
//! Chapter Sheaf SH.6. The write-side symmetric to `data.pdf` (SH.3):
//! the reader extracts a PDF's text layer; this tool constructs a new
//! one. Reuses the **existing `fs.write` capability** via
//! [`crate::sandbox::ReaderSandbox::scope_for_write`] — no new
//! capability base, no new I/O reach.
//!
//! Deliberately modest, matching the reader's own "text layer only, no
//! OCR" honesty: single Helvetica font, approximate (not glyph-exact)
//! word-wrap, automatic pagination onto US-Letter pages. No images,
//! tables, or rich formatting — for anything beyond plain text, write
//! Markdown via `fs.write` instead.
//!
//! ## Tool surface
//!
//! - `data.pdf.write` — `{path: string (required), title?: string,
//!   text: string (required), overwrite?: bool (default false)}` →
//!   `{path, page_count}`.

use async_trait::async_trait;
use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};
use serde_json::{json, Value};

use aivyx_capability::Scope;
use aivyx_core::{AivyxError, Tool, ToolContext, ToolId, ToolOutcome, Verification};

use crate::sandbox::ReaderSandbox;

/// US Letter in points (matches the existing lopdf test fixture).
const PAGE_WIDTH: f32 = 612.0;
const PAGE_HEIGHT: f32 = 792.0;
const MARGIN: f32 = 72.0; // 1 inch
const BODY_FONT_SIZE: f32 = 11.0;
const TITLE_FONT_SIZE: f32 = 16.0;
const LINE_HEIGHT: f32 = 14.0;
/// Rough average glyph width for Helvetica at 11pt — not glyph-exact
/// (Helvetica isn't monospace), a conservative estimate so lines stay
/// inside the margins rather than an exact fit. Documented limitation,
/// same spirit as the reader's "no OCR."
const APPROX_CHARS_PER_LINE: usize = 80;
/// Hard cap on generated pages — bounds a pathological huge `text` input.
const MAX_PAGES: usize = 200;

pub struct DataPdfWriteTool {
    id: ToolId,
    schema: Value,
    sandbox: ReaderSandbox,
}

impl DataPdfWriteTool {
    pub fn new(sandbox: ReaderSandbox) -> Self {
        Self {
            id: ToolId::new(),
            schema: schema(),
            sandbox,
        }
    }
}

#[async_trait]
impl Tool for DataPdfWriteTool {
    fn id(&self) -> ToolId {
        self.id
    }
    fn name(&self) -> &str {
        "data.pdf.write"
    }
    /// Writes a real PDF file under fs_root via `fs.write` — checkpoint
    /// before every call, same as FsWriteTool/FsDeleteTool/ShellExecTool.
    fn mutates_fs_root(&self) -> bool {
        true
    }
    fn description(&self) -> &str {
        "Lay plain text out into a new PDF under the agent's sandbox \
         root. Input: `{path: string (required), title: string \
         (optional), text: string (required), overwrite: bool \
         (optional, default false — required to replace an existing \
         file)}`. Returns `{path, page_count}`. Single Helvetica font, \
         approximate word-wrap, automatic pagination — no images, \
         tables, or rich formatting; for anything beyond plain text \
         write Markdown via fs.write instead. Scope: `fs.write` (same \
         sandbox as fs.write)."
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn required_scope(&self, input: &Value) -> Scope {
        self.sandbox.scope_for_write(input)
    }
    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let title = input.get("title").and_then(Value::as_str).map(str::to_string);
        let text = match input.get("text").and_then(Value::as_str) {
            Some(s) if !s.trim().is_empty() => s,
            _ => {
                return fail(
                    self.id,
                    "data.pdf.write: input must have a non-empty string `text` field"
                        .to_string(),
                )
            }
        };

        let (tmp_path, final_path) = match self.sandbox.resolve_write_target(&input, self.id) {
            Ok(paths) => paths,
            Err(outcome) => return outcome,
        };

        let lines = wrap_text(text, APPROX_CHARS_PER_LINE);
        let pages = paginate(title.as_deref(), &lines);
        if pages.len() > MAX_PAGES {
            return fail(
                self.id,
                format!("data.pdf.write: text needs {} pages, exceeds the {MAX_PAGES} limit", pages.len()),
            );
        }

        let bytes = match build_pdf(&pages) {
            Ok(b) => b,
            Err(e) => return fail(self.id, format!("data.pdf.write: {e}")),
        };
        if let Err(e) = std::fs::write(&tmp_path, &bytes) {
            let _ = std::fs::remove_file(&tmp_path);
            return fail(self.id, format!("data.pdf.write: cannot write temp file: {e}"));
        }
        if let Err(outcome) = ReaderSandbox::commit_write(&tmp_path, &final_path, self.id) {
            return outcome;
        }

        ToolOutcome::Completed {
            output: json!({
                "path": final_path.display().to_string(),
                "page_count": pages.len(),
            }),
            verified: Verification::NotApplicable,
        }
    }
}

/// One line on one page, at a given font size (the title line renders
/// larger than body lines).
struct PageLine {
    text: String,
    font_size: f32,
}

/// Greedy word-wrap at `width` characters — mirrors the same shape used
/// for the TUI chat pane (Chapter Vitrine), breaking at the last space
/// when one exists; an overlong single word hard-breaks.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let candidate_len = if line.is_empty() {
                word.chars().count()
            } else {
                line.chars().count() + 1 + word.chars().count()
            };
            if candidate_len > width && !line.is_empty() {
                out.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
            // A single word longer than the width hard-breaks.
            while line.chars().count() > width {
                let cut: String = line.chars().take(width).collect();
                out.push(cut.clone());
                line = line.chars().skip(width).collect();
            }
        }
        out.push(line);
    }
    out
}

/// Lines-per-page available for body text (a title, when present, uses
/// two body-line-heights' worth of vertical space on page one only).
fn body_lines_per_page(has_title_on_this_page: bool) -> usize {
    let usable_height = PAGE_HEIGHT - 2.0 * MARGIN;
    let title_rows = if has_title_on_this_page { 2 } else { 0 };
    ((usable_height / LINE_HEIGHT) as usize).saturating_sub(title_rows)
}

/// Chunk wrapped lines into pages, each a `Vec<PageLine>` ready to
/// render. The title (if any) only appears on page one.
fn paginate(title: Option<&str>, lines: &[String]) -> Vec<Vec<PageLine>> {
    let mut pages = Vec::new();
    let mut idx = 0;
    let mut first_page = true;
    if lines.is_empty() {
        // A title with no body still produces one page.
        first_page = false;
        pages.push(build_page(title, &[]));
    }
    while idx < lines.len() {
        let per_page = body_lines_per_page(first_page);
        let end = (idx + per_page).min(lines.len());
        let chunk = &lines[idx..end];
        pages.push(build_page(if first_page { title } else { None }, chunk));
        idx = end;
        first_page = false;
    }
    if pages.is_empty() {
        pages.push(build_page(title, &[]));
    }
    pages
}

fn build_page(title: Option<&str>, body: &[String]) -> Vec<PageLine> {
    let mut page = Vec::with_capacity(body.len() + 2);
    if let Some(t) = title {
        page.push(PageLine { text: t.to_string(), font_size: TITLE_FONT_SIZE });
        page.push(PageLine { text: String::new(), font_size: BODY_FONT_SIZE });
    }
    for line in body {
        page.push(PageLine { text: line.clone(), font_size: BODY_FONT_SIZE });
    }
    page
}

/// Build the actual PDF bytes: one Type1 Helvetica font resource
/// shared by every page's content stream (only the `Tf` size argument
/// varies for the title), one Page object per pagination chunk.
fn build_pdf(pages: &[Vec<PageLine>]) -> Result<Vec<u8>, String> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
    });
    let resources_id = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font_id } });

    let mut page_ids = Vec::with_capacity(pages.len());
    for page_lines in pages {
        let mut operations = vec![Operation::new("BT", vec![])];
        let mut y = PAGE_HEIGHT - MARGIN;
        let mut first_line = true;
        for line in page_lines {
            operations.push(Operation::new("Tf", vec!["F1".into(), line.font_size.into()]));
            if first_line {
                operations.push(Operation::new("Td", vec![MARGIN.into(), y.into()]));
                first_line = false;
            } else {
                operations.push(Operation::new("Td", vec![0.into(), (-LINE_HEIGHT).into()]));
            }
            if !line.text.is_empty() {
                operations.push(Operation::new(
                    "Tj",
                    vec![Object::string_literal(line.text.as_str())],
                ));
            }
            y -= LINE_HEIGHT;
        }
        operations.push(Operation::new("ET", vec![]));
        let content = Content { operations };
        let content_bytes = content.encode().map_err(|e| format!("content stream encode: {e}"))?;
        let content_id = doc.add_object(Stream::new(dictionary! {}, content_bytes));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content_id,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), PAGE_WIDTH.into(), PAGE_HEIGHT.into()],
        });
        page_ids.push(page_id);
    }

    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => page_ids.iter().map(|id| (*id).into()).collect::<Vec<_>>(),
            "Count" => page_ids.len() as i64,
        }),
    );
    let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog_id);

    let mut buf = Vec::new();
    doc.save_to(&mut buf).map_err(|e| format!("cannot serialize PDF: {e}"))?;
    Ok(buf)
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "minLength": 1, "description": "Where to write the PDF, under the sandbox root." },
            "title": { "type": "string", "description": "Optional title, rendered larger at the top of page one." },
            "text": { "type": "string", "minLength": 1, "description": "Body text. Newlines start new paragraphs; long lines word-wrap automatically." },
            "overwrite": { "type": "boolean", "description": "Must be true to replace an existing file (default false)." },
            "confirmed": { "type": "boolean", "description": "Set only by Aivyx PA after the operator approves replacing an existing file." }
        },
        "required": ["path", "text"],
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

    #[test]
    fn wrap_text_breaks_at_spaces_and_preserves_content() {
        let lines = wrap_text("the quick brown fox jumps over the lazy dog", 12);
        assert!(lines.len() > 1);
        assert!(lines.iter().all(|l| l.chars().count() <= 12));
        assert_eq!(lines.join(" "), "the quick brown fox jumps over the lazy dog");
    }

    #[test]
    fn wrap_text_hard_breaks_an_overlong_word() {
        let lines = wrap_text("abcdefghijklmnop", 5);
        assert!(lines.iter().all(|l| l.chars().count() <= 5));
        assert_eq!(lines.concat(), "abcdefghijklmnop");
    }

    #[test]
    fn wrap_text_preserves_blank_paragraph_lines() {
        let lines = wrap_text("first\n\nsecond", 80);
        assert_eq!(lines, vec!["first", "", "second"]);
    }

    #[test]
    fn paginate_splits_long_content_across_multiple_pages() {
        let lines: Vec<String> = (0..200).map(|i| format!("line {i}")).collect();
        let pages = paginate(Some("Report"), &lines);
        assert!(pages.len() > 1, "200 lines must not fit on one page");
        // Every body line appears exactly once across all pages.
        let total_body_lines: usize = pages
            .iter()
            .map(|p| p.iter().filter(|l| l.font_size == BODY_FONT_SIZE).count())
            .sum();
        // Page one has a title + blank spacer line at BODY_FONT_SIZE too.
        assert_eq!(total_body_lines, 200 + 1);
    }

    #[test]
    fn build_pdf_produces_bytes_lopdf_can_reopen() {
        let pages = paginate(Some("Title"), &["hello".to_string()]);
        let bytes = build_pdf(&pages).expect("build");
        assert!(lopdf::Document::load_mem(&bytes).is_ok());
    }

    fn scratch_root() -> std::path::PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "aivyx-pdfwrite-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
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
    async fn writes_a_real_pdf_the_reader_can_extract() {
        let root = scratch_root();
        let sandbox = ReaderSandbox::new(&root).unwrap();
        let tool = DataPdfWriteTool::new(sandbox);
        let channel = NoopChannel;
        let audit = NoopAudit;
        let cancellation = aivyx_core::CancellationToken::new();
        let ctx = ToolContext {
            agent_id: aivyx_core::AgentId::new(),
            session_id: aivyx_core::SessionId::new(),
            turn_id: aivyx_core::TurnId::new(),
            channel: &channel,
            audit: &audit,
            cancellation: &cancellation,
            message_origin: aivyx_core::MessageOrigin::Operator,
        };

        let out = tool
            .execute(
                json!({"path": "report.pdf", "title": "Weekly Report", "text": "Hello Sheaf PDF writer."}),
                &ctx,
            )
            .await;
        let ToolOutcome::Completed { output, .. } = out else {
            panic!("expected Completed, got {out:?}");
        };
        assert_eq!(output["page_count"], 1);

        let bytes = std::fs::read(root.join("report.pdf")).unwrap();
        let extracted = pdf_extract::extract_text_from_mem(&bytes).unwrap();
        assert!(extracted.contains("Hello Sheaf PDF writer"));
    }

    /// With confirm-first on, replacing an existing file needs `overwrite`
    /// AND the operator's `confirmed` (set by the agent once approved):
    /// without it the writer asks (`RequiresEscalation`).
    #[tokio::test]
    async fn replacing_a_file_under_confirm_first_asks_then_runs_when_confirmed() {
        let root = scratch_root();
        std::fs::write(root.join("report.pdf"), b"the operator's file").unwrap();
        let tool =
            DataPdfWriteTool::new(ReaderSandbox::new(&root).unwrap().with_confirm_destructive(true));
        let (channel, audit) = (NoopChannel, NoopAudit);
        let cancellation = aivyx_core::CancellationToken::new();
        let ctx = ToolContext {
            agent_id: aivyx_core::AgentId::new(),
            session_id: aivyx_core::SessionId::new(),
            turn_id: aivyx_core::TurnId::new(),
            channel: &channel,
            audit: &audit,
            cancellation: &cancellation,
            message_origin: aivyx_core::MessageOrigin::Operator,
        };
        let asked = tool
            .execute(json!({"path": "report.pdf", "text": "new", "overwrite": true}), &ctx)
            .await;
        assert!(matches!(asked, ToolOutcome::RequiresEscalation { .. }), "{asked:?}");
        assert_eq!(std::fs::read(root.join("report.pdf")).unwrap(), b"the operator's file");
        let ran = tool
            .execute(
                json!({"path": "report.pdf", "text": "new", "overwrite": true, "confirmed": true}),
                &ctx,
            )
            .await;
        assert!(matches!(ran, ToolOutcome::Completed { .. }), "{ran:?}");
    }

    #[test]
    fn tool_metadata_is_sound() {
        let dir = std::env::temp_dir();
        let sb = ReaderSandbox::new(&dir).unwrap();
        let tool = DataPdfWriteTool::new(sb);
        assert_eq!(tool.name(), "data.pdf.write");
        assert!(!tool.description().is_empty());
        assert_eq!(tool.input_schema()["type"], "object");
        assert_eq!(
            tool.required_scope(&json!({"path": "x.pdf"})).base(),
            "fs.write"
        );
    }

    #[test]
    fn data_pdf_write_mutates_fs_root() {
        let dir = std::env::temp_dir();
        let sb = ReaderSandbox::new(&dir).unwrap();
        let tool = DataPdfWriteTool::new(sb);
        assert!(tool.mutates_fs_root());
    }
}
