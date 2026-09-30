//! The terminal's approval prompt — shared by the daemon-mode and
//! in-process chats.

use std::io::{BufRead, Write};

/// The block shown before asking.
pub fn render(summary: &str, reason: &str, input: &serde_json::Value) -> String {
    let args = input.to_string();
    let args = if args.chars().count() > 200 {
        format!("{}…", args.chars().take(200).collect::<String>())
    } else {
        args
    };
    format!(
        "\n⚑ Approval needed — {summary}\n  why: {reason}\n  args: {args}\n  Approve? [y/N] (10 min) "
    )
}

/// `y` / `yes` (any case) approves; anything else — Enter included — denies.
pub fn is_yes(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Show `prompt_text` on the controlling terminal and read one line from it
/// (the chat loop owns stdin). `None` when there's no terminal to ask.
pub fn ask_tty(prompt_text: &str) -> Option<bool> {
    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .ok()?;
    tty.write_all(prompt_text.as_bytes()).ok()?;
    tty.flush().ok()?;
    let mut line = String::new();
    std::io::BufReader::new(tty).read_line(&mut line).ok()?;
    Some(is_yes(&line))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_yes_approves() {
        for a in ["y", "Y", "yes", " YES \n"] {
            assert!(is_yes(a), "{a:?}");
        }
        for a in ["", "\n", "n", "no", "yep", "sure"] {
            assert!(!is_yes(a), "{a:?}");
        }
    }

    #[test]
    fn the_prompt_names_the_action_and_trims_long_args() {
        let long = serde_json::json!({"content": "x".repeat(500)});
        let text = render("fs.write notes.txt", "overwriting can't be undone", &long);
        assert!(text.contains("⚑ Approval needed — fs.write notes.txt"));
        assert!(text.contains("why: overwriting can't be undone"));
        assert!(text.contains("…"));
        assert!(text.trim_end().ends_with("Approve? [y/N] (10 min)"));
    }
}
