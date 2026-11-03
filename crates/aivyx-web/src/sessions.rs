//! The Sessions screen's text: which client a session belongs to, how much
//! the daemon trusts it, and which sessions have gone quiet.
//!
//! Pure functions over the wire types so they can be unit-tested natively.

use aivyx_ipc::protocol::{FrontendType, SessionSummary, WireChannelPlatform};

/// A session with no message for this long is listed under "Idle".
pub const IDLE_AFTER_MS: u64 = 30 * 60 * 1000;

/// The client that opened a session, in plain words. Falls back to the
/// channel for a daemon that doesn't report the client.
pub fn client_label(frontend: Option<FrontendType>, channel: WireChannelPlatform) -> &'static str {
    match frontend {
        Some(FrontendType::Web) => "Studio",
        Some(FrontendType::Local) => "Terminal",
        Some(FrontendType::Telegram) => "Telegram",
        Some(FrontendType::Discord) => "Discord",
        Some(FrontendType::Slack) => "Slack",
        Some(FrontendType::Matrix) => "Matrix",
        Some(FrontendType::Proton) => "Proton Mail",
        Some(FrontendType::Signal) => "Signal",
        None => match channel {
            WireChannelPlatform::Local => "This computer",
            WireChannelPlatform::Telegram => "Telegram",
            WireChannelPlatform::Discord => "Discord",
            WireChannelPlatform::Slack => "Slack",
            WireChannelPlatform::Matrix => "Matrix",
            WireChannelPlatform::Email => "Email",
            WireChannelPlatform::Signal => "Signal",
            WireChannelPlatform::Rest => "API",
            WireChannelPlatform::Voice => "Voice",
        },
    }
}

/// The row's title: the client, and "this tab" for the Studio tab showing
/// the list.
pub fn session_title(s: &SessionSummary, own_session: Option<&str>) -> String {
    let client = client_label(s.frontend, s.channel);
    if own_session == Some(s.session_id.as_str()) {
        format!("{client} · this tab")
    } else {
        client.to_string()
    }
}

/// The trust tier as a short lower-case tag ("trusted", "semi-trusted").
pub fn trust_label(tier_debug: &str) -> String {
    match tier_debug {
        "SemiTrusted" => "semi-trusted".to_string(),
        other => other.to_lowercase(),
    }
}

/// The first block of a session id: enough to tell two rows apart.
pub fn short_id(session_id: &str) -> &str {
    session_id.split('-').next().unwrap_or(session_id)
}

/// Splits sessions into (recent, idle), each newest-active first. The tab
/// showing the list is never idle.
pub fn split_idle(
    sessions: &[SessionSummary],
    now_ms: u64,
    own_session: Option<&str>,
) -> (Vec<SessionSummary>, Vec<SessionSummary>) {
    let mut rows = sessions.to_vec();
    rows.sort_by_key(|s| std::cmp::Reverse(s.last_active_at_ms));
    rows.into_iter().partition(|s| {
        own_session == Some(s.session_id.as_str())
            || now_ms.saturating_sub(s.last_active_at_ms) < IDLE_AFTER_MS
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, frontend: Option<FrontendType>, last_active_at_ms: u64) -> SessionSummary {
        serde_json::from_value(serde_json::json!({
            "session_id": id,
            "channel": "Local",
            "trust_tier": "Trusted",
            "created_at_ms": 0,
            "last_active_at_ms": last_active_at_ms,
            "frontend": frontend,
        }))
        .unwrap()
    }

    #[test]
    fn a_session_is_named_by_its_client() {
        assert_eq!(client_label(Some(FrontendType::Web), WireChannelPlatform::Local), "Studio");
        assert_eq!(client_label(Some(FrontendType::Local), WireChannelPlatform::Local), "Terminal");
        assert_eq!(client_label(None, WireChannelPlatform::Telegram), "Telegram");
        assert_eq!(client_label(None, WireChannelPlatform::Local), "This computer");
    }

    #[test]
    fn the_viewing_tab_is_marked() {
        let s = session("abc-1", Some(FrontendType::Web), 0);
        assert_eq!(session_title(&s, Some("abc-1")), "Studio · this tab");
        assert_eq!(session_title(&s, Some("other")), "Studio");
    }

    #[test]
    fn ids_and_tiers_read_short() {
        assert_eq!(short_id("fd294a83-744f-4e35-81bf-ac407fdd460e"), "fd294a83");
        assert_eq!(trust_label("SemiTrusted"), "semi-trusted");
        assert_eq!(trust_label("Trusted"), "trusted");
    }

    #[test]
    fn quiet_sessions_go_under_idle_but_never_this_tab() {
        let now = 10 * IDLE_AFTER_MS;
        let rows = vec![
            session("old", Some(FrontendType::Web), now - IDLE_AFTER_MS - 1),
            session("new", Some(FrontendType::Local), now - 1_000),
            session("mine", Some(FrontendType::Web), 0),
        ];
        let (recent, idle) = split_idle(&rows, now, Some("mine"));
        let ids = |v: &[SessionSummary]| v.iter().map(|s| s.session_id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&recent), ["new", "mine"]);
        assert_eq!(ids(&idle), ["old"]);
    }
}
