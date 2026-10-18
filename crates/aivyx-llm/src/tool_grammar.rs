//! Tool-call grammar generation — Chapter Stencil (ST.1).
//!
//! Grammar-constrained decoding forces a local model to emit a
//! **valid, real-named tool call by construction** instead of
//! hoping the prompt lands. [`tool_call_grammar`] turns the
//! turn's registered tools into a JSON Schema that admits *only*
//! a well-formed call — the constraint a provider then hands to
//! its decoder (for `mistralrs`, `Constraint::JsonSchema`).
//!
//! The function is **pure and provider-agnostic**: `&[LlmToolDescriptor]`
//! in, [`serde_json::Value`] out, no engine, no I/O. It lives at
//! the crate root (not under `mistral_rs/`) precisely so a future
//! `llama-server` `/completion` GBNF path can reuse it unchanged.
//!
//! ## Shape
//!
//! A `oneOf` discriminated union — one branch per tool, plus a
//! reserved [`RESPOND_SENTINEL`] branch:
//!
//! ```jsonc
//! { "oneOf": [
//!   { "type": "object",
//!     "properties": { "name": { "const": "fs.read" },
//!                     "arguments": { /* fs.read's input_schema verbatim */ } },
//!     "required": ["name", "arguments"], "additionalProperties": false },
//!   // …one branch per registered tool…
//!   { "type": "object",                                   // the sentinel
//!     "properties": { "name": { "const": "respond" },
//!                     "arguments": { "type": "object",
//!                                    "properties": { "text": { "type": "string" } },
//!                                    "required": ["text"] } },
//!     "required": ["name", "arguments"], "additionalProperties": false }
//! ] }
//! ```
//!
//! Each `name` is pinned to a `const` drawn from the registry, so
//! a **hallucinated name can never match**; that branch's
//! `arguments` is the matching tool's `input_schema` *verbatim*,
//! so **malformed arguments can never match**. Because every
//! branch's `name` const is distinct, an instance satisfies at
//! most one branch — `oneOf` (exactly-one) is the correct
//! combinator, and an unknown name or bad args satisfies *zero*
//! branches → invalid.
//!
//! ## The `respond` sentinel
//!
//! A constrained decoder is *forced* into the grammar, so without
//! an escape the model could never reply in plain text. The
//! reserved [`RESPOND_SENTINEL`] branch is that escape: the
//! provider (ST.3) unwraps a `{"name":"respond","arguments":{"text":…}}`
//! "call" back into an ordinary assistant text message rather than
//! dispatching it. The sentinel name has **no dot**, while every
//! real Aivyx tool is `namespace.action` — so it can never collide
//! with a registered tool.

use serde_json::{json, Value};

use crate::LlmToolDescriptor;

/// Reserved pseudo-tool name the grammar admits so a constrained
/// model can decline to call a real tool and reply in plain text.
/// Has no `.` separator, so it cannot collide with a real
/// `namespace.action` tool name. ST.3's provider wiring unwraps a
/// call to this name back into an assistant text message.
pub const RESPOND_SENTINEL: &str = "respond";

/// Build a JSON Schema that admits **only** a valid tool call for
/// one of `tools` (or the [`RESPOND_SENTINEL`] text escape).
///
/// Pure and engine-free — the result is handed to a decoder as a
/// generation constraint. With an empty `tools` slice the grammar
/// still admits the sentinel, so a constrained turn can always
/// produce *something* well-formed.
pub fn tool_call_grammar(tools: &[LlmToolDescriptor]) -> Value {
    let mut branches: Vec<Value> = tools
        .iter()
        .map(|t| tool_branch(&t.name, &t.input_schema))
        .collect();
    branches.push(respond_branch());
    json!({ "oneOf": branches })
}

/// One union branch: `name` pinned to `name`, `arguments` set to
/// the tool's input schema verbatim.
fn tool_branch(name: &str, input_schema: &Value) -> Value {
    // Tools' input schemas are objects at the top level (the
    // registry validates this at registration). If a schema ever
    // isn't an object it can't be a meaningful `arguments`
    // constraint, so fall back to a permissive object — still
    // pinning the *name*, which is the load-bearing guarantee.
    let arguments = if input_schema.is_object() {
        input_schema.clone()
    } else {
        json!({ "type": "object" })
    };
    json!({
        "type": "object",
        "properties": {
            "name": { "const": name },
            "arguments": arguments,
        },
        "required": ["name", "arguments"],
        "additionalProperties": false,
    })
}

/// The reserved text-reply escape branch.
fn respond_branch() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": { "const": RESPOND_SENTINEL },
            "arguments": {
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"],
                "additionalProperties": false,
            },
        },
        "required": ["name", "arguments"],
        "additionalProperties": false,
    })
}

// ---------------------------------------------------------------------------
// Constrained-decoding I/O helpers — Chapter Emboss (EB.1).
//
// Shared by every provider that grammar-constrains tool calls (the
// in-process mistral.rs engine and llama.cpp's server). Both constrain
// output to the [`tool_call_grammar`] shape, so the model emits the
// same `{"name", "arguments"}` JSON in the message `content` — and both
// parse it the same way. These lived in the mistral.rs provider through
// Stencil/Bridle; Emboss promotes them here so there is one
// implementation, no drift.
// ---------------------------------------------------------------------------

/// The instruction appended to the system message under
/// grammar-constrained decoding (Chapter Bridle, BR.3). Stencil's
/// grammar *admits* the `respond` sentinel as the plain-text escape,
/// but a small model never *chooses* it unless told — ST.4 watched a 4B
/// loop on one tool call because it had no way to "finish." This is that
/// missing instruction.
pub const RESPOND_PREAMBLE: &str = "\
Tool-calling mode: every reply must be a single JSON object. To use a tool, emit \
{\"name\":\"<tool>\",\"arguments\":{…}}. To answer the user in plain text — or when \
you are done and need no tool — emit {\"name\":\"respond\",\"arguments\":{\"text\":\"…\"}}; \
this ends your turn. Do not repeat the same tool call: if a call did not help, either \
try a different one or `respond`.";

/// Build the system message for a turn, appending [`RESPOND_PREAMBLE`]
/// when decoding is constrained. Returns `None` when there is nothing
/// to send (no base prompt and not constrained) so the unconstrained
/// path stays byte-identical. Pure — unit-testable without an engine.
pub fn system_message_for(base: Option<&str>, constrain: bool) -> Option<String> {
    match (base, constrain) {
        (Some(s), false) => Some(s.to_string()),
        (None, false) => None,
        (Some(s), true) => Some(format!("{s}\n\n{RESPOND_PREAMBLE}")),
        (None, true) => Some(RESPOND_PREAMBLE.to_string()),
    }
}

/// Outcome of parsing a grammar-constrained turn's output (Chapter
/// Stencil ST.3). [`tool_call_grammar`] admits a single `{"name",
/// "arguments"}` object; the `respond` sentinel maps to plain text, any
/// other name to a real tool call.
#[derive(Debug, PartialEq)]
pub enum ConstrainedOutput {
    ToolCall {
        tool_name: String,
        input: Value,
    },
    Text(String),
}

/// Parse the JSON a grammar-constrained turn produced. Returns `None`
/// when `content` isn't the expected shape — the grammar guarantees it
/// is, so `None` is purely defensive (the caller falls back to the
/// provider's native extraction).
pub fn parse_constrained_output(content: &str) -> Option<ConstrainedOutput> {
    let value: Value = serde_json::from_str(content.trim()).ok()?;
    let name = value.get("name")?.as_str()?;
    let arguments = value
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if name == RESPOND_SENTINEL {
        let text = arguments
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string();
        Some(ConstrainedOutput::Text(text))
    } else {
        Some(ConstrainedOutput::ToolCall {
            tool_name: name.to_string(),
            input: arguments,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two sample tools with realistic object input schemas.
    fn sample_tools() -> Vec<LlmToolDescriptor> {
        vec![
            LlmToolDescriptor {
                name: "fs.read".to_string(),
                description: "Read a file".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"],
                    "additionalProperties": false,
                }),
            },
            LlmToolDescriptor {
                name: "web.fetch".to_string(),
                description: "Fetch a URL".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": { "url": { "type": "string" } },
                    "required": ["url"],
                    "additionalProperties": false,
                }),
            },
        ]
    }

    /// Compile the generated grammar into a validator. Asserts the
    /// grammar is itself a well-formed schema (the precondition for
    /// every other test).
    fn validator_for(tools: &[LlmToolDescriptor]) -> jsonschema::Validator {
        let grammar = tool_call_grammar(tools);
        jsonschema::validator_for(&grammar)
            .expect("tool_call_grammar must produce a valid JSON Schema")
    }

    #[test]
    fn admits_a_valid_tool_call() {
        let v = validator_for(&sample_tools());
        assert!(v.is_valid(&json!({
            "name": "fs.read",
            "arguments": { "path": "/etc/hosts" },
        })));
        // The second tool too — both branches are reachable.
        assert!(v.is_valid(&json!({
            "name": "web.fetch",
            "arguments": { "url": "https://example.com" },
        })));
    }

    #[test]
    fn rejects_an_unknown_tool_name() {
        let v = validator_for(&sample_tools());
        // The exact failure prompting could never fix: a name that
        // isn't in the registry. No branch's `const` matches.
        assert!(!v.is_valid(&json!({
            "name": "fs.write_file", // gemma4's classic hallucination
            "arguments": { "path": "x", "content": "y" },
        })));
        assert!(!v.is_valid(&json!({
            "name": "browser_navigate",
            "arguments": {},
        })));
    }

    #[test]
    fn rejects_malformed_arguments() {
        let v = validator_for(&sample_tools());
        // Real name, missing the required `path`.
        assert!(!v.is_valid(&json!({
            "name": "fs.read",
            "arguments": {},
        })));
        // Real name, wrong type for `path`.
        assert!(!v.is_valid(&json!({
            "name": "fs.read",
            "arguments": { "path": 123 },
        })));
        // Real name, an argument the schema doesn't allow.
        assert!(!v.is_valid(&json!({
            "name": "fs.read",
            "arguments": { "path": "/x", "bogus": true },
        })));
    }

    #[test]
    fn admits_the_respond_sentinel() {
        let v = validator_for(&sample_tools());
        assert!(v.is_valid(&json!({
            "name": RESPOND_SENTINEL,
            "arguments": { "text": "I've finished the task." },
        })));
        // …even with no tools registered — the escape is always
        // available so a constrained turn can produce plain text.
        let v_empty = validator_for(&[]);
        assert!(v_empty.is_valid(&json!({
            "name": "respond",
            "arguments": { "text": "hello" },
        })));
        // But the sentinel still has to be well-formed.
        assert!(!v_empty.is_valid(&json!({
            "name": "respond",
            "arguments": {},
        })));
    }

    #[test]
    fn rejects_missing_top_level_fields_and_cross_tool_args() {
        let v = validator_for(&sample_tools());
        // No `arguments` at all.
        assert!(!v.is_valid(&json!({ "name": "fs.read" })));
        // No `name` at all.
        assert!(!v.is_valid(&json!({ "arguments": { "path": "/x" } })));
        // fs.read's name with web.fetch's arguments — the name
        // const selects the fs.read branch, whose schema rejects
        // `url`/forbids the missing `path`.
        assert!(!v.is_valid(&json!({
            "name": "fs.read",
            "arguments": { "url": "https://example.com" },
        })));
    }

    // ---- Constrained-output parsing (moved here in Chapter Emboss EB.1) ----

    #[test]
    fn parse_constrained_real_tool_call() {
        let out = parse_constrained_output(
            r#"{"name": "fs.read", "arguments": {"path": "/etc/hosts"}}"#,
        )
        .expect("well-formed constrained call parses");
        assert_eq!(
            out,
            ConstrainedOutput::ToolCall {
                tool_name: "fs.read".to_string(),
                input: json!({"path": "/etc/hosts"}),
            }
        );
    }

    #[test]
    fn parse_constrained_respond_sentinel_unwraps_to_text() {
        let out = parse_constrained_output(
            r#"{"name": "respond", "arguments": {"text": "All done."}}"#,
        )
        .expect("sentinel parses");
        assert_eq!(out, ConstrainedOutput::Text("All done.".to_string()));
    }

    #[test]
    fn parse_constrained_tolerates_surrounding_whitespace() {
        let out = parse_constrained_output(
            "\n  {\"name\": \"respond\", \"arguments\": {\"text\": \"hi\"}}\n",
        )
        .expect("trimmed JSON parses");
        assert_eq!(out, ConstrainedOutput::Text("hi".to_string()));
    }

    #[test]
    fn parse_constrained_rejects_non_json() {
        // Defensive: non-JSON / non-object content yields None so the
        // caller falls back to the provider's native extraction.
        assert!(parse_constrained_output("not json at all").is_none());
        assert!(parse_constrained_output(r#"{"missing": "name"}"#).is_none());
    }

    #[test]
    fn preamble_appended_only_when_constrained() {
        // Unconstrained → the operator prompt passes through untouched
        // (byte-identical), and an absent prompt stays absent.
        assert_eq!(
            system_message_for(Some("You are Aivyx."), false).as_deref(),
            Some("You are Aivyx.")
        );
        assert_eq!(system_message_for(None, false), None);

        // Constrained → the preamble is appended after the operator
        // prompt (appended, never replacing it).
        let got = system_message_for(Some("You are Aivyx."), true).unwrap();
        assert!(got.starts_with("You are Aivyx."), "operator prompt preserved");
        assert!(got.contains(RESPOND_PREAMBLE), "preamble present");
        assert!(got.contains(RESPOND_SENTINEL), "names the respond sentinel");

        // Constrained with no base prompt → the preamble alone.
        assert_eq!(
            system_message_for(None, true).as_deref(),
            Some(RESPOND_PREAMBLE)
        );
    }

    /// The exact grammar for the sample tools, frozen before the move to
    /// aivyx-stencil: the move must not change a byte of it.
    #[test]
    fn grammar_is_byte_identical_to_the_pre_extraction_output() {
        let got = serde_json::to_string(&tool_call_grammar(&sample_tools())).unwrap();
        let want = r#"{"oneOf":[{"additionalProperties":false,"properties":{"arguments":{"additionalProperties":false,"properties":{"path":{"type":"string"}},"required":["path"],"type":"object"},"name":{"const":"fs.read"}},"required":["name","arguments"],"type":"object"},{"additionalProperties":false,"properties":{"arguments":{"additionalProperties":false,"properties":{"url":{"type":"string"}},"required":["url"],"type":"object"},"name":{"const":"web.fetch"}},"required":["name","arguments"],"type":"object"},{"additionalProperties":false,"properties":{"arguments":{"additionalProperties":false,"properties":{"text":{"type":"string"}},"required":["text"],"type":"object"},"name":{"const":"respond"}},"required":["name","arguments"],"type":"object"}]}"#;
        assert_eq!(got, want);
    }
}
