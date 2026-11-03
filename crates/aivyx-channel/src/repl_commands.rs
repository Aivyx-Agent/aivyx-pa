//! The terminal REPL's own slash commands: `/help`, `/quit`, `/exit`, and
//! the answer to a `/word` nothing handles. The routing commands
//! (`/models`, `/model`), `/allow-cloud` and `/image` are handled where
//! they always were; this only recognises them so they are not called
//! unknown.

use crate::studio_token::{not_running_message, StudioState};

/// What the REPL does with a line that starts with `/`.
#[derive(Debug, PartialEq, Eq)]
pub enum ReplCommand {
    Help,
    Quit,
    /// A `/word` no command answers to; carries the word, slash included.
    Unknown(String),
}

/// Commands handled elsewhere (routing, consent, images).
const HANDLED_ELSEWHERE: &[&str] = &["/allow-cloud", "/models", "/model", "/image"];

/// `None` for an ordinary message — including anything that merely starts
/// with a slash, like a path (`/etc/hosts`), and the commands handled
/// elsewhere.
pub fn parse(input: &str) -> Option<ReplCommand> {
    let word = input.split_whitespace().next()?;
    let name = word.strip_prefix('/')?;
    let looks_like_command = !name.is_empty()
        && name.starts_with(|c: char| c.is_ascii_alphabetic())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !looks_like_command {
        return None;
    }
    match word.to_ascii_lowercase().as_str() {
        "/help" => Some(ReplCommand::Help),
        "/quit" | "/exit" => Some(ReplCommand::Quit),
        w if HANDLED_ELSEWHERE.contains(&w) => None,
        _ => Some(ReplCommand::Unknown(word.to_string())),
    }
}

/// The reply to a `/word` that is not a command.
pub fn unknown_reply(word: &str) -> String {
    format!("Unknown command {word}. /help lists them.")
}

/// Where the Studio is, for `/help`. `None` state means there is no
/// daemon behind this session.
pub fn studio_help_line(state: Option<&StudioState>) -> String {
    match state {
        Some(StudioState::Bound(addr)) => format!(
            "Studio (web UI): {} - `aivyx-pa studio` prints the sign-in link.",
            crate::studio_token::studio_url(addr.ip(), addr.port())
        ),
        Some(StudioState::Failed(port)) => not_running_message(*port),
        Some(StudioState::Unknown) => "Studio (web UI): not running yet - `aivyx-pa studio` \
             prints its link once the daemon is up."
            .to_string(),
        None => "Studio (web UI): needs the daemon - `aivyx-pa daemon run`, then \
             `aivyx-pa studio` prints its link."
            .to_string(),
    }
}

/// The `/help` text.
pub fn help_text(studio_line: &str) -> String {
    format!(
        "Commands:\n  \
           /help           this list\n  \
           /quit, /exit    leave (Ctrl-D also works)\n  \
           /models         list the models routing can choose from\n  \
           /model <name>   pin this conversation to a model (/model auto to unpin)\n  \
           /allow-cloud    allow a cloud model for this conversation, then resend\n  \
           /image <path> [caption]   send a picture\n\
         Keys: Ctrl-C cancels the running turn (twice to leave).\n\
         When a tool needs your approval: [y] allow  [n] deny (default; Enter denies)  [a] show all\n\
         {studio_line}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_and_quit_in_any_case_with_trailing_words() {
        assert_eq!(parse("/help"), Some(ReplCommand::Help));
        assert_eq!(parse("  /HELP me\n"), Some(ReplCommand::Help));
        for q in ["/quit", "/exit", "/Quit now"] {
            assert_eq!(parse(q), Some(ReplCommand::Quit), "{q}");
        }
    }

    #[test]
    fn unknown_words_are_named() {
        assert_eq!(parse("/hepl"), Some(ReplCommand::Unknown("/hepl".into())));
        assert_eq!(parse("/foo bar"), Some(ReplCommand::Unknown("/foo".into())));
        assert_eq!(unknown_reply("/hepl"), "Unknown command /hepl. /help lists them.");
    }

    #[test]
    fn handled_elsewhere_paths_and_chat_pass_through() {
        for ok in [
            "/models", "/models why", "/model big", "/allow-cloud", "/image a.png hi",
            "hello", "", "/", "//", "/etc/hosts", "/tmp/x.txt is broken", "/1", "what is /help",
        ] {
            assert_eq!(parse(ok), None, "{ok:?}");
        }
    }

    #[test]
    fn help_names_the_exits_the_keys_and_the_studio() {
        let text = help_text(&studio_help_line(Some(&StudioState::Bound(
            "127.0.0.1:7843".parse().unwrap(),
        ))));
        for want in [
            "/help", "/quit, /exit", "Ctrl-D", "[y] allow", "[n] deny (default", "[a] show all",
            "http://127.0.0.1:7843",
        ] {
            assert!(text.contains(want), "{want}: {text}");
        }
        assert!(!text.contains("[A]"), "pa has no lasting allow: {text}");
        assert!(studio_help_line(Some(&StudioState::Failed(7843))).contains("7843"));
        assert!(studio_help_line(None).contains("daemon"));
    }
}
