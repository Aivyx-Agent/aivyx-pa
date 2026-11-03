//! The Tools screen's short description: the first sentence of a tool's
//! model-facing description, without its input spec, scope notes or code
//! marks. The full text stays available behind "Details".

/// Labels that start the model-facing part of a description.
const SPEC_MARKERS: &[&str] = &["Input:", "Inputs:", "Returns", "Args:", "Scope:"];

/// The longest summary shown before it is cut at a word.
const MAX_CHARS: usize = 180;

/// The first sentence of `description`, in plain text.
pub fn tool_summary(description: &str) -> String {
    let text = description.trim();
    let mut end = first_sentence_end(text);
    for marker in SPEC_MARKERS {
        if let Some(i) = text[..end].find(marker)
            && i > 0
        {
            end = end.min(i);
        }
    }
    let mut summary: String = text[..end].trim().replace('`', "");
    if summary.chars().count() > MAX_CHARS {
        let cut: String = summary.chars().take(MAX_CHARS).collect();
        let at = cut.rfind(' ').unwrap_or(cut.len());
        summary = format!("{}…", cut[..at].trim_end_matches([',', ';', ':', ' ']));
    }
    summary
}

/// Whether the full description says more than the summary.
pub fn has_details(description: &str) -> bool {
    let full = description.trim().replace('`', "");
    tool_summary(description) != full
}

/// Byte offset just past the first sentence: a `.`, `!` or `?` followed by
/// white space and then a capital, a backtick or a bracket (so `fs.read`
/// and "e.g. a file" don't end it). The whole text if there is no such end.
fn first_sentence_end(text: &str) -> usize {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for (k, &(i, c)) in chars.iter().enumerate() {
        if !matches!(c, '.' | '!' | '?') {
            continue;
        }
        let mut j = k + 1;
        let mut saw_space = false;
        while let Some(&(_, w)) = chars.get(j) {
            if w.is_whitespace() {
                saw_space = true;
                j += 1;
            } else {
                break;
            }
        }
        if !saw_space {
            continue;
        }
        if let Some(&(_, next)) = chars.get(j)
            && (next.is_uppercase() || next == '`' || next == '(')
        {
            return i + c.len_utf8();
        }
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_is_the_first_sentence_without_the_input_spec() {
        let d = "Read a CSV / delimited-text file from under the agent's sandbox root into structured rows. \
                 Input: `{path: string (required)}`. Returns `{path, headers}`. Scope: `fs.read`.";
        assert_eq!(
            tool_summary(d),
            "Read a CSV / delimited-text file from under the agent's sandbox root into structured rows."
        );
        assert!(has_details(d));
    }

    #[test]
    fn an_input_spec_inside_the_first_sentence_is_cut() {
        let d = "Lay plain text out into a new PDF Input: `{path: string}`.";
        assert_eq!(tool_summary(d), "Lay plain text out into a new PDF");
    }

    #[test]
    fn dotted_names_and_abbreviations_do_not_end_the_sentence() {
        let d = "Use fs.read for raw bytes, e.g. a binary file. More here.";
        assert_eq!(tool_summary(d), "Use fs.read for raw bytes, e.g. a binary file.");
    }

    #[test]
    fn code_marks_are_dropped_and_short_text_has_no_details() {
        let d = "Run `git status --porcelain` against a configured repo path.";
        assert_eq!(tool_summary(d), "Run git status --porcelain against a configured repo path.");
        assert!(!has_details(d));
    }

    #[test]
    fn a_very_long_sentence_is_cut_at_a_word() {
        let d = "word ".repeat(80);
        let s = tool_summary(&d);
        assert!(s.ends_with('…'));
        assert!(s.chars().count() <= MAX_CHARS + 1);
    }
}
