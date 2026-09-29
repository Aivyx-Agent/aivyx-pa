//! The daemon ↔ client IPC protocol — the wire envelope + frame codec.
//!
//! Moved wholesale to the wasm-clean [`aivyx_ipc`] crate (Chapter M.2f) so the
//! browser Mission-Control app (a `wasm32` Dioxus client) and the daemon
//! serialize from one source of wire truth. Re-exported here so every
//! `crate::daemon_ipc::…` reference across the daemon — and external
//! `aivyx_channel::daemon_ipc::…` users (the CLI/TUI clients) — are unchanged.
//!
//! `FrontendMessage` / `DaemonMessage` / `QueryPayload` /
//! `QueryResponsePayload`, the `*Summary` wire structs, the lifecycle + stream
//! events, and `encode_frame` / `decode_frame` all live in
//! [`aivyx_ipc::protocol`]; the daemon-side handlers that *produce* them stay
//! in `daemon_server` / `daemon_client`.
pub use aivyx_ipc::protocol::*;

/// A chat app's reply for one turn: its own rendering of the streamed
/// events (`rendered`, which is `"(no reply)"` when nothing displayable
/// streamed), plus the turn's authoritative outcome when it differs. A turn
/// that streamed no text but has an outcome message — a routing or
/// `/allow-cloud` command reply, a cloud-consent stop, a reply floor —
/// shows that message alone, never behind a "(no reply)" prefix. Shared by
/// the Telegram, Discord and Slack daemon front ends.
pub fn chat_reply(rendered: String, events: &[StreamEventPayload], outcome: &str) -> String {
    let displayed = concat_text_events(events);
    let Some(note) = turn_outcome_correction(&displayed, outcome) else {
        return rendered;
    };
    if rendered.trim() == "(no reply)" {
        return note;
    }
    let mut buf = rendered;
    if !buf.is_empty() && !buf.ends_with('\n') {
        buf.push('\n');
    }
    buf.push_str(&note);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_reply_is_the_note_alone() {
        // `/models`, `/model x`, `/allow-cloud`: the daemon answers with no
        // events at all — the chat apps must not prefix "(no reply)".
        let out = chat_reply(
            "(no reply)".to_string(),
            &[],
            "completed: Routing candidates (* = this conversation's model):",
        );
        assert_eq!(out, "Routing candidates (* = this conversation's model):");
    }

    #[test]
    fn a_text_less_turn_with_an_outcome_shows_just_the_outcome() {
        let out = chat_reply(
            "(no reply)".to_string(),
            &[],
            "completed: I wasn't able to produce a usable reply this turn — please try again.",
        );
        assert_eq!(
            out,
            "I wasn't able to produce a usable reply this turn — please try again."
        );
    }

    #[test]
    fn streamed_text_keeps_a_differing_correction_after_it() {
        let events = vec![StreamEventPayload::Text { text: "partial".into() }];
        let out = chat_reply("partial".to_string(), &events, "completed: the real answer");
        assert!(out.starts_with("partial\n"), "{out}");
        assert!(out.ends_with("the real answer"), "{out}");
    }

    #[test]
    fn a_turn_with_nothing_at_all_still_says_no_reply() {
        let out = chat_reply("(no reply)".to_string(), &[], "completed: ");
        assert_eq!(out.trim(), "(no reply)");
    }
}
