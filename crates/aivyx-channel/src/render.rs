//! `StreamEvent` → terminal bytes.
//!
//! This module holds the pure rendering logic for the CLI channel:
//! given a `StreamEvent` (or a `TurnOutcome` at finalize time) and a
//! mutable `Write` sink, produce the human-readable bytes a user
//! should see.
//!
//! ## Why it's its own module
//!
//! Task 1 inlined the rendering into `LocalChannel::stream_event` /
//! `finalize`. That worked but mixed three concerns: locking the
//! writer, deciding *what bytes* each variant should produce, and
//! flushing so tokens appear as they arrive. Task 2 factors the middle
//! concern out so that:
//!
//! - **Rendering can be tested without async or mutex machinery.** A
//!   test feeds a `&mut Vec<u8>` in and asserts the bytes that come
//!   out. No `#[tokio::test]`, no `Arc<Mutex<>>`, no writer handle
//!   gymnastics.
//! - **A future machine-oriented channel can reuse the same renderer**
//!   with a different output mode. For now there's only a human mode
//!   (what the CLI prints); a JSON-lines mode for programmatic
//!   consumers is a natural extension of the same module, not a new
//!   channel.
//! - **Display-format tweaks (colors, compact mode, tool-name
//!   lookup) land in one place** — they don't leak into the
//!   `ChannelContext` impl, so the channel layer stays about
//!   lifecycle and the render layer stays about bytes.
//!
//! ## What this module does *not* do
//!
//! - It does not flush. The caller decides when partial output should
//!   be pushed to the terminal; the renderer only writes.
//! - It does not lock. If the sink needs synchronization, the caller
//!   holds the mutex.
//! - It does not resolve `ToolId` → tool name. The current
//!   `StreamEvent::ToolCallStarted` variant (per `DESIGN.md` D3)
//!   carries only `tool: ToolId`, not a human name. The renderer
//!   therefore displays a short UUID prefix (`tool[a1b2c3d4]`) —
//!   ugly but unambiguous and dependency-free. Adding a name would
//!   require a contract amendment and belongs in a future phase if
//!   the CLI ergonomics prove painful.

use std::io::{self, Write};

use aivyx_core::{StreamEvent, TurnOutcome, TurnOutcomeSummary};

/// How the renderer should format output. For now there's only one
/// mode (`Human`). A `JsonLines` or `Compact` mode would be a new
/// enum variant plus a new match arm in each render function, not a
/// new module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    /// Human-readable: text chunks verbatim, tool calls as single-
    /// line arrow markers, outcomes as bracketed status lines.
    Human,
}

/// Render a single `StreamEvent` into the given sink.
///
/// Does not flush. The caller is responsible for calling `flush()`
/// after each call if partial output should be visible immediately —
/// which is exactly what `LocalChannel::stream_event` does.
pub fn render_stream_event(
    mode: RenderMode,
    w: &mut dyn Write,
    event: &StreamEvent<'_>,
) -> io::Result<()> {
    match mode {
        RenderMode::Human => render_stream_event_human(w, event),
    }
}

/// Render the finalization marker for a completed turn.
pub fn render_finalize(
    mode: RenderMode,
    w: &mut dyn Write,
    outcome: &TurnOutcome,
) -> io::Result<()> {
    match mode {
        RenderMode::Human => render_finalize_human(w, outcome),
    }
}

// ---------------------------------------------------------------------------
// Human renderer
// ---------------------------------------------------------------------------

fn render_stream_event_human(w: &mut dyn Write, event: &StreamEvent<'_>) -> io::Result<()> {
    match *event {
        StreamEvent::Text(chunk) => w.write_all(chunk.as_bytes()),
        StreamEvent::Status(msg) => writeln!(w, "\n  · {msg}"),
        // Phase 10 task 3: render the human `tool_name` that the
        // loop now threads through the event, not the short-UUID
        // fallback we used before Task 3. `tool` (the ToolId) is
        // still on the event for audit bridges, but isn't shown to
        // the human.
        StreamEvent::ToolCallStarted {
            tool_name, input, ..
        } => {
            writeln!(w, "\n  → {tool_name} {input}")
        }
        StreamEvent::ToolCallFinished {
            tool_name,
            outcome_summary,
            ..
        } => writeln!(w, "  ← {tool_name} {outcome_summary}"),
        StreamEvent::Attachment {
            kind,
            data,
            filename,
        } => {
            let name = filename.unwrap_or("<unnamed>");
            writeln!(
                w,
                "\n  ⎘ attachment[{kind:?}] {name} ({} bytes)",
                data.len()
            )
        }
        // Phase 12 task 1: tool output streams through inline as
        // verbatim text between the start/finish markers already
        // emitted above. No per-chunk marker — the user sees a
        // continuous body, same visual shape as streamed LLM text.
        // `tool_name` and `tool` id are on the event for audit
        // bridges, not displayed.
        StreamEvent::ToolOutput { chunk, .. } => w.write_all(chunk.as_bytes()),
    }
}

fn render_finalize_human(w: &mut dyn Write, outcome: &TurnOutcome) -> io::Result<()> {
    let summary = TurnOutcomeSummary::from(outcome);
    let marker = match summary {
        TurnOutcomeSummary::Completed => "completed",
        TurnOutcomeSummary::Escalated => "escalated",
        TurnOutcomeSummary::TimedOut => "timed out",
        TurnOutcomeSummary::Cancelled => "cancelled",
        TurnOutcomeSummary::MaxStepsExceeded => "max steps exceeded",
        // Chapter Bridle — distinct from "max steps exceeded" so the
        // operator sees *why* a turn stopped: a runaway repeated call.
        TurnOutcomeSummary::Looping => "stopped: repeated tool call",
        TurnOutcomeSummary::Failed => "failed",
    };
    // Why it escalated or failed: nothing else in the terminal says so.
    match outcome {
        TurnOutcome::Escalated { reason, .. } => writeln!(w, "\n[turn {marker}: {reason}]"),
        TurnOutcome::Failed(error) => writeln!(w, "\n[turn {marker}: {error}]"),
        _ => writeln!(w, "\n[turn {marker}]"),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_core::{AivyxError, AttachmentKind, ToolId};
    use serde_json::json;
    use std::time::Duration;

    /// Drive a single `StreamEvent` through the human renderer and
    /// return what was written. No async, no locks.
    fn render_one(event: StreamEvent<'_>) -> String {
        let mut buf = Vec::<u8>::new();
        render_stream_event(RenderMode::Human, &mut buf, &event).unwrap();
        String::from_utf8(buf).unwrap()
    }

    fn render_final(outcome: &TurnOutcome) -> String {
        let mut buf = Vec::<u8>::new();
        render_finalize(RenderMode::Human, &mut buf, outcome).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn text_is_written_verbatim_with_no_decoration() {
        assert_eq!(render_one(StreamEvent::Text("hello")), "hello");
        assert_eq!(render_one(StreamEvent::Text("")), "");
        assert_eq!(
            render_one(StreamEvent::Text("multi\nline\n")),
            "multi\nline\n",
            "newlines inside a text chunk should be preserved verbatim"
        );
    }

    #[test]
    fn status_renders_with_dot_prefix_on_its_own_line() {
        let out = render_one(StreamEvent::Status("thinking"));
        assert!(
            out.starts_with('\n'),
            "status should start with a leading newline so it breaks from any in-flight text: {out:?}"
        );
        assert!(out.contains("· thinking"), "got {out:?}");
        assert!(out.ends_with('\n'), "status should end with a newline");
    }

    #[test]
    fn tool_call_started_renders_tool_name_and_arrow() {
        // Phase 10 task 3: renderer shows the human name
        // (`memory.read`), not a UUID short id. The ToolId is still
        // on the event for audit bridges but must NOT leak into the
        // rendered human output — a UUID in a terminal is noise
        // against a plain tool name.
        let tool = ToolId::new();
        let input = json!({"topic": "todos"});
        let out = render_one(StreamEvent::ToolCallStarted {
            tool,
            tool_name: "memory.read",
            input: &input,
        });

        assert!(
            out.contains("→ memory.read"),
            "should contain name-prefixed arrow, got {out:?}"
        );
        assert!(
            out.contains("\"topic\":\"todos\""),
            "JSON input should be rendered: {out:?}"
        );
        assert!(
            !out.contains(&tool.to_string()),
            "ToolId UUID must not appear in human render: {out:?}"
        );
    }

    #[test]
    fn tool_call_finished_renders_tool_name_and_summary() {
        let tool = ToolId::new();
        let out = render_one(StreamEvent::ToolCallFinished {
            tool,
            tool_name: "memory.write",
            outcome_summary: "completed (verified)",
        });
        assert!(out.contains("← memory.write"), "got {out:?}");
        assert!(out.contains("completed (verified)"), "got {out:?}");
        assert!(
            !out.contains(&tool.to_string()),
            "ToolId UUID must not appear in human render: {out:?}"
        );
    }

    #[test]
    fn attachment_renders_byte_count_and_filename() {
        let out = render_one(StreamEvent::Attachment {
            kind: AttachmentKind::Image { mime: "image/png" },
            data: &[0u8; 1234],
            filename: Some("screenshot.png"),
        });
        assert!(out.contains("screenshot.png"), "got {out:?}");
        assert!(out.contains("1234 bytes"), "got {out:?}");
    }

    #[test]
    fn attachment_without_filename_shows_placeholder() {
        let out = render_one(StreamEvent::Attachment {
            kind: AttachmentKind::File { mime: "text/plain" },
            data: &[0u8; 10],
            filename: None,
        });
        assert!(out.contains("<unnamed>"), "got {out:?}");
    }

    // Phase 12 task 1: `StreamEvent::ToolOutput` renders as verbatim
    // text in the human renderer, matching the `Text` variant's shape
    // so streamed tool output reads like streamed LLM text. No
    // per-chunk marker, no UUID leak, concatenation-friendly.

    #[test]
    fn tool_output_chunk_renders_verbatim_like_text() {
        let tool = ToolId::new();
        let out = render_one(StreamEvent::ToolOutput {
            tool,
            tool_name: "web.fetch",
            chunk: "hello world",
        });
        assert_eq!(out, "hello world");
    }

    #[test]
    fn tool_output_empty_chunk_renders_empty() {
        let tool = ToolId::new();
        let out = render_one(StreamEvent::ToolOutput {
            tool,
            tool_name: "web.fetch",
            chunk: "",
        });
        assert_eq!(out, "");
    }

    #[test]
    fn tool_output_does_not_leak_tool_id_or_name_into_human_output() {
        let tool = ToolId::new();
        let out = render_one(StreamEvent::ToolOutput {
            tool,
            tool_name: "web.fetch",
            chunk: "body content",
        });
        // Body text is the only thing in the output — same rule as
        // `Text`. The `tool_name` is there for audit bridges that
        // observe `StreamEvent` but deliberately is not rendered to
        // the human, because a streamed body should read like LLM
        // text rather than like a decorated event log.
        assert!(
            !out.contains("web.fetch"),
            "tool_name must not appear in rendered body: {out:?}"
        );
        assert!(
            !out.contains(&tool.to_string()),
            "ToolId UUID must not appear in rendered body: {out:?}"
        );
        assert_eq!(out, "body content");
    }

    #[test]
    fn tool_output_consecutive_chunks_concatenate_with_no_separators() {
        let tool = ToolId::new();
        let mut buf = Vec::<u8>::new();
        for chunk in ["part-one ", "part-two ", "part-three"] {
            render_stream_event(
                RenderMode::Human,
                &mut buf,
                &StreamEvent::ToolOutput {
                    tool,
                    tool_name: "web.fetch",
                    chunk,
                },
            )
            .unwrap();
        }
        assert_eq!(String::from_utf8(buf).unwrap(), "part-one part-two part-three");
    }

    #[test]
    fn finalize_renders_completed_marker() {
        let out = render_final(&TurnOutcome::Completed {
            final_message: "done".into(),
            tool_calls_made: 0,
            duration: Duration::from_millis(1),
        });
        assert_eq!(out, "\n[turn completed]\n");
    }

    #[test]
    fn finalize_renders_all_outcome_kinds_distinctly() {
        let completed = render_final(&TurnOutcome::Completed {
            final_message: String::new(),
            tool_calls_made: 0,
            duration: Duration::ZERO,
        });
        let escalated = render_final(&TurnOutcome::Escalated {
            reason: "needs kernel".into(),
            pending_tool: ToolId::new(),
            scope: None,
            tool_calls_made: 0,
        });
        let timed_out = render_final(&TurnOutcome::TimedOut {
            tool_calls_made: 0,
            elapsed: Duration::ZERO,
        });
        let cancelled = render_final(&TurnOutcome::Cancelled {
            tool_calls_made: 0,
        });
        let failed = render_final(&TurnOutcome::Failed(AivyxError::Internal("boom".into())));
        // Chapter Bridle — the repeated-call outcome must render with
        // its own distinct marker, not collapse into "max steps".
        let looping = render_final(&TurnOutcome::Looping {
            final_message: "stopped".into(),
            tool_calls_made: 3,
            duration: Duration::ZERO,
            repeat_limit: 3,
        });
        let max_steps = render_final(&TurnOutcome::MaxStepsExceeded {
            tool_calls_made: 32,
            duration: Duration::ZERO,
            max_steps: 32,
        });

        assert!(completed.contains("[turn completed]"));
        // Escalations and failures say why — the terminal shows nothing
        // else about them.
        assert!(escalated.contains("[turn escalated: needs kernel]"), "{escalated}");
        assert!(failed.contains("[turn failed: "), "{failed}");
        assert!(failed.contains("boom"), "{failed}");
        assert!(timed_out.contains("[turn timed out]"));
        assert!(cancelled.contains("[turn cancelled]"));
        assert!(looping.contains("[turn stopped: repeated tool call]"));
        assert!(max_steps.contains("[turn max steps exceeded]"));
        // Bridle's loop marker must read differently from max-steps.
        assert_ne!(looping, max_steps);

        // All markers must be distinct — otherwise the user
        // can't tell why a turn ended.
        let all = [completed, escalated, timed_out, cancelled, failed, looping, max_steps];
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                assert_ne!(all[i], all[j], "markers {i} and {j} must differ");
            }
        }
    }

}
