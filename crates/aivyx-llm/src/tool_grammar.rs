//! Tool-call grammar — Chapters Stencil (ST.1) and Emboss (EB.1).
//!
//! The implementation lives in the shared `aivyx-stencil` crate
//! (`Aivyx-Agent/aivyx-toolbench`), extracted from this file on
//! 2026-10-18 so aivyx-toolbench (and later aivyx-coder) use the same
//! grammar. This module keeps pa's API: it converts pa's
//! [`LlmToolDescriptor`] and re-exports the rest unchanged.

use serde_json::Value;
// The unchanged tests module reaches `json!` through `use super::*`.
#[cfg(test)]
use serde_json::json;

use crate::LlmToolDescriptor;

pub use aivyx_stencil::{
    parse_constrained_output, system_message_for, ConstrainedOutput, RESPOND_PREAMBLE,
    RESPOND_SENTINEL,
};

/// See [`aivyx_stencil::tool_call_grammar`].
pub fn tool_call_grammar(tools: &[LlmToolDescriptor]) -> Value {
    let specs: Vec<aivyx_stencil::ToolSpec> = tools
        .iter()
        .map(|t| aivyx_stencil::ToolSpec {
            name: t.name.clone(),
            description: t.description.clone(),
            input_schema: t.input_schema.clone(),
        })
        .collect();
    aivyx_stencil::tool_call_grammar(&specs)
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
            "parameters": { "path": "/etc/hosts" },
        })));
        // The second tool too — both branches are reachable.
        assert!(v.is_valid(&json!({
            "name": "web.fetch",
            "parameters": { "url": "https://example.com" },
        })));
    }

    #[test]
    fn rejects_an_unknown_tool_name() {
        let v = validator_for(&sample_tools());
        // The exact failure prompting could never fix: a name that
        // isn't in the registry. No branch's `const` matches.
        assert!(!v.is_valid(&json!({
            "name": "fs.write_file", // gemma4's classic hallucination
            "parameters": { "path": "x", "content": "y" },
        })));
        assert!(!v.is_valid(&json!({
            "name": "browser_navigate",
            "parameters": {},
        })));
    }

    #[test]
    fn rejects_malformed_arguments() {
        let v = validator_for(&sample_tools());
        // Real name, missing the required `path`.
        assert!(!v.is_valid(&json!({
            "name": "fs.read",
            "parameters": {},
        })));
        // Real name, wrong type for `path`.
        assert!(!v.is_valid(&json!({
            "name": "fs.read",
            "parameters": { "path": 123 },
        })));
        // Real name, an argument the schema doesn't allow.
        assert!(!v.is_valid(&json!({
            "name": "fs.read",
            "parameters": { "path": "/x", "bogus": true },
        })));
    }

    #[test]
    fn admits_the_respond_sentinel() {
        let v = validator_for(&sample_tools());
        assert!(v.is_valid(&json!({
            "name": RESPOND_SENTINEL,
            "parameters": { "text": "I've finished the task." },
        })));
        // …even with no tools registered — the escape is always
        // available so a constrained turn can produce plain text.
        let v_empty = validator_for(&[]);
        assert!(v_empty.is_valid(&json!({
            "name": "respond",
            "parameters": { "text": "hello" },
        })));
        // But the sentinel still has to be well-formed.
        assert!(!v_empty.is_valid(&json!({
            "name": "respond",
            "parameters": {},
        })));
    }

    #[test]
    fn rejects_missing_top_level_fields_and_cross_tool_args() {
        let v = validator_for(&sample_tools());
        // No `parameters` at all.
        assert!(!v.is_valid(&json!({ "name": "fs.read" })));
        // No `name` at all.
        assert!(!v.is_valid(&json!({ "parameters": { "path": "/x" } })));
        // fs.read's name with web.fetch's arguments — the name
        // const selects the fs.read branch, whose schema rejects
        // `url`/forbids the missing `path`.
        assert!(!v.is_valid(&json!({
            "name": "fs.read",
            "parameters": { "url": "https://example.com" },
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
    fn parse_constrained_name_first_parameters_shape() {
        let out = parse_constrained_output(
            r#"{"name": "fs.read", "parameters": {"path": "/etc/hosts"}}"#,
        )
        .expect("name-first constrained call parses");
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

    /// Frozen output of the shared grammar; changes only deliberately - last
    /// changed when aivyx-stencil switched to a `parameters` key so the tool
    /// name comes first.
    #[test]
    fn grammar_output_is_frozen() {
        let got = serde_json::to_string(&tool_call_grammar(&sample_tools())).unwrap();
        let want = r#"{"oneOf":[{"additionalProperties":false,"properties":{"name":{"const":"fs.read"},"parameters":{"additionalProperties":false,"properties":{"path":{"type":"string"}},"required":["path"],"type":"object"}},"required":["name","parameters"],"type":"object"},{"additionalProperties":false,"properties":{"name":{"const":"web.fetch"},"parameters":{"additionalProperties":false,"properties":{"url":{"type":"string"}},"required":["url"],"type":"object"}},"required":["name","parameters"],"type":"object"},{"additionalProperties":false,"properties":{"name":{"const":"respond"},"parameters":{"additionalProperties":false,"properties":{"text":{"type":"string"}},"required":["text"],"type":"object"}},"required":["name","parameters"],"type":"object"}]}"#;
        assert_eq!(got, want);
    }
}
