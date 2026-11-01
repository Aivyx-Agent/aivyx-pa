//! How an approval request is shown to the operator (audit P4).
//!
//! Shared by the agent's one-line summary, the terminal prompt and the Studio
//! card, so all three agree: every argument the approval covers is listed,
//! recipients come first, text that has to be shortened says how much is
//! hidden, and characters that could disguise what is on screen (control,
//! bidirectional-override, zero-width) are shown as visible escapes.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A secret string (a password, bot token or API key). Serializes as the plain string, so the wire
/// format is unchanged, but `Debug` never prints it: a stray `{:?}` of a
/// message can't put a credential in a log (audit P14).
#[derive(Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    /// The secret itself. Call only where it is used, never to format.
    pub fn expose(&self) -> &str {
        &self.0
    }
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl std::ops::Deref for SecretString {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl From<String> for SecretString {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl std::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Keys whose values name who a call reaches. Strings or arrays of strings.
pub const RECIPIENT_KEYS: [&str; 6] = ["to", "cc", "bcc", "recipient", "recipients", "attendees"];

/// Characters that can hide, reorder or fake text on screen: controls, every
/// Unicode format (Cf) character (zero-width, bidi, invisible operators,
/// tag characters, BOM, soft hyphen, Arabic letter mark, ...), the invisible
/// fillers that render as blank, variation selectors, and the line/paragraph
/// separators.
fn is_disguise(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'                 // soft hyphen
                | '\u{034F}'           // combining grapheme joiner
                | '\u{061C}'           // Arabic letter mark
                | '\u{115F}' | '\u{1160}' // Hangul fillers
                | '\u{17B4}' | '\u{17B5}' // Khmer inherent vowels
                | '\u{180B}'..='\u{180F}' // Mongolian selectors, vowel separator
                | '\u{200B}'..='\u{200F}' // zero-width, LRM, RLM
                | '\u{2028}' | '\u{2029}' // line / paragraph separators
                | '\u{202A}'..='\u{202E}' // bidi embeddings and overrides
                | '\u{2060}'..='\u{206F}' // word joiner, invisible operators, isolates
                | '\u{3164}'           // Hangul filler
                | '\u{FE00}'..='\u{FE0F}' // variation selectors
                | '\u{FEFF}'           // BOM / zero-width no-break space
                | '\u{FFA0}'           // halfwidth Hangul filler
                | '\u{FFF9}'..='\u{FFFB}' // interlinear annotation
                | '\u{E0000}'..='\u{E007F}' // tag characters
                | '\u{E0100}'..='\u{E01EF}' // variation selectors supplement
        )
}

/// `text` with newlines, tabs and every disguising character replaced by a
/// visible escape (`\n`, `\t`, `\u{202e}`), so the line shown is the line sent.
/// A literal backslash is doubled, so a typed `\u{202e}` cannot pass for an
/// escape this function made.
pub fn neutralise(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if is_disguise(c) => out.push_str(&format!("\\u{{{:04x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// The first `max` characters of `text` (neutralised) and how many were left
/// out.
pub fn clip(text: &str, max: usize) -> (String, usize) {
    let total = text.chars().count();
    if total <= max {
        return (neutralise(text), 0);
    }
    let head: String = text.chars().take(max).collect();
    (neutralise(&head), total - max)
}

/// "… 1,240 more characters hidden", with a thousands separator.
pub fn hidden_note(hidden: usize) -> String {
    let digits = hidden.to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    format!("… {grouped} more characters hidden")
}

/// Every recipient string in `input`, in key order of [`RECIPIENT_KEYS`],
/// arrays flattened.
pub fn recipients(input: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for key in RECIPIENT_KEYS {
        match input.get(key) {
            Some(Value::String(s)) if !s.trim().is_empty() => out.push(s.clone()),
            Some(Value::Array(items)) => {
                for item in items {
                    match item {
                        Value::String(s) if !s.trim().is_empty() => out.push(s.clone()),
                        // {"email": "..", "name": ".."} style entries
                        Value::Object(o) => {
                            if let Some(Value::String(s)) = o.get("email").or_else(|| o.get("address")) {
                                out.push(s.clone());
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The arguments as lines, `key: value`. Short values first (so recipients
/// and subjects never sit behind a long body), every argument present. A
/// string value longer than `per_value` characters is cut with a note of how
/// much is hidden; `None` shows everything.
pub fn describe_args(input: &Value, per_value: Option<usize>) -> Described {
    let render = |v: &Value| -> (String, usize) {
        let flat = match v {
            Value::String(s) => s.clone(),
            Value::Array(items) if items.iter().all(|i| i.is_string()) => items
                .iter()
                .filter_map(|i| i.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            other => other.to_string(),
        };
        match per_value {
            Some(max) => clip(&flat, max),
            None => (neutralise(&flat), 0),
        }
    };
    let Some(obj) = input.as_object() else {
        let (text, hidden) = render(input);
        return Described { text, hidden };
    };
    let mut rows: Vec<(usize, &String, String, usize)> = obj
        .iter()
        .map(|(k, v)| {
            let (text, hidden) = render(v);
            (text.chars().count() + hidden, k, text, hidden)
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(b.1)));
    let mut text = String::new();
    let mut hidden_total = 0;
    for (_, k, value, hidden) in rows {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&format!("  {}: {}", neutralise(k), value));
        if hidden > 0 {
            text.push(' ');
            text.push_str(&hidden_note(hidden));
            hidden_total += hidden;
        }
    }
    Described { text, hidden: hidden_total }
}

/// Output of [`describe_args`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Described {
    pub text: String,
    /// Characters left out across all values (0 = nothing hidden).
    pub hidden: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_invisible_class_is_escaped_and_a_typed_escape_is_doubled() {
        for c in [
            '\u{E0041}', '\u{E007F}', '\u{FE0F}', '\u{E0100}', '\u{3164}', '\u{115F}',
            '\u{1160}', '\u{180E}', '\u{034F}', '\u{2028}', '\u{2029}', '\u{200B}',
            '\u{202E}', '\u{2066}', '\u{FEFF}', '\u{00AD}', '\u{061C}', '\u{FFA0}',
        ] {
            let shown = neutralise(&format!("a{c}b"));
            assert!(!shown.contains(c), "{c:?} survived: {shown:?}");
            assert!(shown.starts_with("a\\u{") && shown.ends_with("}b"), "{shown:?}");
        }
        // a typed escape can't pass for a real one: the backslash is doubled
        assert_eq!(neutralise("x\\u{202e}y"), "x\\\\u{202e}y");
        assert_ne!(neutralise("x\\u{202e}y"), neutralise("x\u{202e}y"));
        assert_eq!(neutralise("C:\\dir"), "C:\\\\dir");
    }

    #[test]
    fn disguising_characters_become_visible_escapes() {
        assert_eq!(neutralise("a\u{202e}b\u{200b}c\nd"), "a\\u{202e}b\\u{200b}c\\nd");
        assert_eq!(neutralise("plain ünï"), "plain ünï");
    }

    #[test]
    fn recipients_flatten_arrays_and_objects() {
        let i = json!({"to": ["a@x", "b@x"], "cc": "c@x", "bcc": [{"email": "d@x"}]});
        assert_eq!(recipients(&i), ["a@x", "b@x", "c@x", "d@x"]);
    }

    #[test]
    fn short_values_come_first_and_cuts_are_counted() {
        let i = json!({"body": "x".repeat(1500), "to": ["a@x"], "subject": "a longer subject"});
        let d = describe_args(&i, Some(1000));
        let lines: Vec<&str> = d.text.lines().collect();
        assert!(lines[0].contains("to: a@x"), "{}", d.text);
        assert!(lines[1].contains("subject: a longer subject"));
        assert!(lines[2].contains("500 more characters hidden"), "{}", d.text);
        assert_eq!(d.hidden, 500);
        assert_eq!(describe_args(&i, None).hidden, 0);
    }

    #[test]
    fn the_count_has_a_thousands_separator() {
        assert_eq!(hidden_note(1240), "… 1,240 more characters hidden");
        assert_eq!(hidden_note(12), "… 12 more characters hidden");
    }
}
