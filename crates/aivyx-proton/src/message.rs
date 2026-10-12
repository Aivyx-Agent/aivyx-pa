//! One received email, parsed once (`mail-parser`) into the fields the
//! trust policy, the channel and the tools read. No IMAP type crosses
//! this boundary.

use mail_parser::{Address, MessageParser, MimeHeaders};

/// The most of an email's text a turn reads (the reply says when it cut).
pub const TURN_TEXT_CAP: usize = 32 * 1024;
/// The most of an email's text `proton.read` returns.
pub const TOOL_TEXT_CAP: usize = 32 * 1024;
/// The most text kept from any one message in memory.
const KEPT_TEXT_CAP: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MailMessage {
    /// The UID in the mailbox it was fetched from.
    pub uid: u32,
    /// `X-Pm-Internal-Id`: Proton's own message ID. Bridge always sets it,
    /// overwriting any copy the sender wrote.
    pub internal_id: Option<String>,
    /// `Message-ID`, without angle brackets.
    pub message_id: Option<String>,
    /// `In-Reply-To`, without angle brackets.
    pub in_reply_to: Option<String>,
    /// `References`, oldest first, without angle brackets.
    pub references: Vec<String>,
    /// Control characters replaced by spaces.
    pub subject: String,
    /// The From address, lowercased.
    pub from: Option<String>,
    pub from_name: Option<String>,
    /// Lowercased addresses.
    pub to: Vec<String>,
    pub cc: Vec<String>,
    /// Every `Delivered-To` and `X-Original-To` value, lowercased — set by
    /// Proton's receiving server for the actual delivery.
    pub delivered_to: Vec<String>,
    /// `Authentication-Results` values in header order, folding collapsed.
    pub auth_results: Vec<String>,
    pub auto_submitted: Option<String>,
    pub precedence: Option<String>,
    /// RFC 3339.
    pub date: Option<String>,
    /// The `text/plain` part, else the HTML converted to text (mail-parser
    /// does both). At most 256 KiB.
    pub text: String,
    /// Attachment file names; content is never read.
    pub attachments: Vec<String>,
}

impl MailMessage {
    /// The conversation this email belongs to: the thread's first
    /// `References` entry, else `In-Reply-To`, else its own `Message-ID`.
    pub fn thread_root(&self) -> String {
        self.references
            .first()
            .cloned()
            .or_else(|| self.in_reply_to.clone())
            .or_else(|| self.message_id.clone())
            .unwrap_or_else(|| format!("uid:{}", self.uid))
    }
}

/// Parses a full RFC 5322 message. `None` when it isn't one.
pub fn parse(uid: u32, raw: &[u8]) -> Option<MailMessage> {
    let m = MessageParser::default().parse(raw)?;
    let values = |name: &str| -> Vec<String> {
        m.header_values(name)
            .filter_map(|v| v.as_text())
            .map(collapse)
            .collect()
    };
    let first = |name: &str| values(name).into_iter().next();
    let addresses = |a: Option<&Address>| -> Vec<String> {
        a.map(|a| {
            a.iter()
                .filter_map(|x| x.address())
                .map(str::to_ascii_lowercase)
                .collect()
        })
        .unwrap_or_default()
    };
    let from = m.from().and_then(|a| a.first());
    let mut delivered_to: Vec<String> = values("Delivered-To");
    delivered_to.extend(values("X-Original-To"));
    let delivered_to = delivered_to
        .iter()
        .map(|a| a.trim_matches(|c| c == '<' || c == '>').to_ascii_lowercase())
        .collect();
    let (text, _) = cap(
        &m.body_text(0).map(|t| t.into_owned()).unwrap_or_default(),
        KEPT_TEXT_CAP,
    );
    Some(MailMessage {
        uid,
        internal_id: first("X-Pm-Internal-Id"),
        message_id: m.message_id().map(str::to_string),
        in_reply_to: m
            .in_reply_to()
            .as_text_list()
            .and_then(|l| l.first().map(|s| s.to_string())),
        references: m
            .references()
            .as_text_list()
            .map(|l| l.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default(),
        subject: m.subject().map(no_control).unwrap_or_default(),
        from: from.and_then(|a| a.address()).map(str::to_ascii_lowercase),
        from_name: from.and_then(|a| a.name()).map(no_control),
        to: addresses(m.to()),
        cc: addresses(m.cc()),
        delivered_to,
        auth_results: values("Authentication-Results"),
        auto_submitted: first("Auto-Submitted"),
        precedence: first("Precedence"),
        date: m.date().map(|d| d.to_rfc3339()),
        text,
        attachments: m
            .attachments()
            .map(|a| a.attachment_name().unwrap_or("(unnamed)").to_string())
            .collect(),
    })
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn no_control(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// The new part of a reply: everything before the first quote intro
/// ("On … wrote:", Proton's "------- Original Message -------"), with
/// `>`-quoted lines dropped.
pub fn without_quoted_history(text: &str) -> String {
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if (t.starts_with("On ") && t.ends_with("wrote:"))
            || t.starts_with("------- Original Message -------")
            || t.starts_with("-----Original Message-----")
        {
            break;
        }
        if t.starts_with('>') {
            continue;
        }
        out.push(line);
    }
    out.join("\n").trim().to_string()
}

/// At most `max` bytes of `text`, cut on a character boundary; `true` when
/// something was cut.
pub fn cap(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_string(), false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/proton-external-dmarc-pass.headers");

    fn with_fixture(extra: &str) -> Vec<u8> {
        let mut raw = FIXTURE.to_vec();
        raw.extend_from_slice(extra.as_bytes());
        raw
    }

    #[test]
    fn proton_headers_are_read_in_order() {
        let raw = with_fixture(
            "From: Ann <Ann@NewsLetter.example>\r\nSubject: hi\r\n\r\nhello\r\n",
        );
        let m = parse(7, &raw).unwrap();
        assert_eq!(m.uid, 7);
        assert_eq!(m.from.as_deref(), Some("ann@newsletter.example"));
        assert_eq!(m.delivered_to, vec!["you@proton.me", "you@proton.me"]);
        assert_eq!(m.auth_results.len(), 4);
        assert_eq!(
            m.auth_results[0],
            "mail.protonmail.ch; dmarc=pass (p=reject dis=none) header.from=newsletter.example"
        );
        assert!(m.auth_results[1].contains("spf=pass"));
        assert_eq!(m.text.trim(), "hello");
    }

    #[test]
    fn threading_attachments_and_automatic_headers() {
        let raw = b"From: Me <me@proton.me>\r\nTo: a@x.com, B <B@y.com>\r\nCc: c@z.com\r\n\
            Message-ID: <abc@proton.me>\r\nIn-Reply-To: <root@proton.me>\r\n\
            References: <root@proton.me> <mid@proton.me>\r\nX-Pm-Internal-Id: AbC-_=\r\n\
            Auto-Submitted: auto-replied\r\nPrecedence: bulk\r\nSubject: hel\x01lo\r\n\
            Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\n\
            body here\r\n--b\r\nContent-Type: application/pdf\r\n\
            Content-Disposition: attachment; filename=x.pdf\r\n\r\nAAAA\r\n--b--\r\n";
        let m = parse(1, raw).unwrap();
        assert_eq!(m.to, vec!["a@x.com", "b@y.com"]);
        assert_eq!(m.cc, vec!["c@z.com"]);
        assert_eq!(m.message_id.as_deref(), Some("abc@proton.me"));
        assert_eq!(m.in_reply_to.as_deref(), Some("root@proton.me"));
        assert_eq!(m.references, vec!["root@proton.me", "mid@proton.me"]);
        assert_eq!(m.internal_id.as_deref(), Some("AbC-_="));
        assert_eq!(m.auto_submitted.as_deref(), Some("auto-replied"));
        assert_eq!(m.precedence.as_deref(), Some("bulk"));
        assert_eq!(m.subject, "hel lo");
        assert_eq!(m.text, "body here");
        assert_eq!(m.attachments, vec!["x.pdf"]);
        assert_eq!(m.thread_root(), "root@proton.me");
    }

    #[test]
    fn html_only_mail_becomes_text() {
        let raw = b"From: a@x.com\r\nContent-Type: text/html\r\n\r\n<p>Hello <b>you</b></p>";
        assert_eq!(parse(1, raw).unwrap().text.trim(), "Hello you");
    }

    #[test]
    fn thread_root_falls_back_to_own_id_then_uid() {
        let mut m = MailMessage { uid: 9, ..Default::default() };
        assert_eq!(m.thread_root(), "uid:9");
        m.message_id = Some("own@x".into());
        assert_eq!(m.thread_root(), "own@x");
        m.in_reply_to = Some("parent@x".into());
        assert_eq!(m.thread_root(), "parent@x");
    }

    #[test]
    fn quoted_history_is_dropped() {
        let t = "Thanks, and one more thing.\n\nOn Thu, Oct 1, 2026 at 12:30, Bot <b@x> wrote:\n> old\n> stuff";
        assert_eq!(without_quoted_history(t), "Thanks, and one more thing.");
        let t = "new\n> quoted inline\nmore\n------- Original Message -------\nold";
        assert_eq!(without_quoted_history(t), "new\nmore");
    }

    #[test]
    fn cap_cuts_on_a_char_boundary() {
        assert_eq!(cap("short", 10), ("short".to_string(), false));
        let (s, cut) = cap("ééé", 3);
        assert!(cut);
        assert_eq!(s, "é");
    }
}
