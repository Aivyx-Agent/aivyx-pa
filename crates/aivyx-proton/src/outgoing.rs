//! Mail the assistant writes: the channel's replies and the tools' drafts
//! and sends. Rendered to RFC 5322 bytes with `lettre`'s message builder
//! (CRLF, encoded headers); sent by our own SMTP client (`smtp`).

use lettre::message::header::ContentType;
use lettre::message::Mailbox;

use crate::message::MailMessage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingMail {
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    pub body: String,
    /// Without angle brackets.
    pub in_reply_to: Option<String>,
    /// Without angle brackets, oldest first.
    pub references: Vec<String>,
    /// Without angle brackets.
    pub message_id: String,
}

impl OutgoingMail {
    pub fn new(from: &str, to: Vec<String>, cc: Vec<String>, subject: &str, body: String) -> Self {
        OutgoingMail {
            from: from.to_string(),
            to,
            cc,
            subject: one_line(subject),
            body,
            in_reply_to: None,
            references: Vec::new(),
            message_id: new_message_id(from),
        }
    }

    /// A reply to `original`, threaded (`In-Reply-To` / `References`) so it
    /// lands in the same conversation in Proton Mail.
    pub fn reply(from: &str, to: Vec<String>, original: &MailMessage, body: String) -> Self {
        let mut mail = OutgoingMail::new(from, to, Vec::new(), &re_subject(&original.subject), body);
        mail.in_reply_to = original.message_id.clone();
        mail.references = original.references.clone();
        if let Some(id) = &original.message_id {
            mail.references.push(id.clone());
        }
        mail
    }

    /// Everyone the message goes to (SMTP `RCPT TO`).
    pub fn recipients(&self) -> Vec<String> {
        self.to.iter().chain(&self.cc).cloned().collect()
    }

    pub fn to_rfc5322(&self) -> Result<Vec<u8>, String> {
        let mailbox = |a: &str| a.parse::<Mailbox>().map_err(|e| format!("bad address {a:?}: {e}"));
        let mut b = lettre::Message::builder()
            .from(mailbox(&self.from)?)
            .subject(self.subject.clone())
            .message_id(Some(format!("<{}>", self.message_id)));
        for a in &self.to {
            b = b.to(mailbox(a)?);
        }
        for a in &self.cc {
            b = b.cc(mailbox(a)?);
        }
        if let Some(id) = &self.in_reply_to {
            b = b.in_reply_to(format!("<{id}>"));
        }
        if !self.references.is_empty() {
            let refs: Vec<String> = self.references.iter().map(|r| format!("<{r}>")).collect();
            b = b.references(refs.join(" "));
        }
        let message = b
            .header(ContentType::TEXT_PLAIN)
            .body(self.body.clone())
            .map_err(|e| format!("build message: {e}"))?;
        Ok(message.formatted())
    }
}

/// `Re: <subject>`, without stacking `Re: Re:`.
pub fn re_subject(subject: &str) -> String {
    let s = subject.trim();
    if s.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("re:")) {
        s.to_string()
    } else {
        format!("Re: {s}")
    }
}

fn one_line(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

fn new_message_id(from: &str) -> String {
    let domain = from.rsplit_once('@').map(|(_, d)| d).unwrap_or("localhost");
    format!("{}@{domain}", uuid::Uuid::new_v4().simple())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn original() -> MailMessage {
        MailMessage {
            uid: 3,
            subject: "Lunch?".into(),
            from: Some("me@proton.me".into()),
            message_id: Some("m2@proton.me".into()),
            references: vec!["m1@proton.me".into()],
            ..Default::default()
        }
    }

    #[test]
    fn a_reply_is_threaded_and_prefixed_once() {
        let r = OutgoingMail::reply("bot@proton.me", vec!["me@proton.me".into()], &original(), "Sure.".into());
        assert_eq!(r.subject, "Re: Lunch?");
        assert_eq!(r.in_reply_to.as_deref(), Some("m2@proton.me"));
        assert_eq!(r.references, vec!["m1@proton.me", "m2@proton.me"]);
        assert!(r.message_id.ends_with("@proton.me"));
        assert_eq!(re_subject("RE: x"), "RE: x");
        assert_eq!(re_subject("é"), "Re: é");
    }

    #[test]
    fn rendering_carries_the_threading_headers() {
        let r = OutgoingMail::reply("bot@proton.me", vec!["me@proton.me".into()], &original(), "Sure.".into());
        let raw = String::from_utf8(r.to_rfc5322().unwrap()).unwrap();
        assert!(raw.contains("In-Reply-To: <m2@proton.me>\r\n"), "{raw}");
        assert!(raw.contains("References: <m1@proton.me> <m2@proton.me>\r\n"), "{raw}");
        assert!(raw.contains(&format!("Message-ID: <{}>\r\n", r.message_id)), "{raw}");
        assert!(raw.contains("Subject: Re: Lunch?\r\n"), "{raw}");
        assert!(raw.ends_with("Sure."), "{raw}");
        // And it parses back.
        let back = crate::message::parse(1, raw.as_bytes()).unwrap();
        assert_eq!(back.in_reply_to.as_deref(), Some("m2@proton.me"));
        assert_eq!(back.from.as_deref(), Some("bot@proton.me"));
    }

    #[test]
    fn a_subject_cannot_inject_headers_and_bad_addresses_fail() {
        let m = OutgoingMail::new("bot@proton.me", vec!["a@x.com".into()], vec![], "hi\r\nBcc: evil@x.com", "b".into());
        assert_eq!(m.subject, "hi  Bcc: evil@x.com");
        let raw = String::from_utf8(m.to_rfc5322().unwrap()).unwrap();
        assert!(!raw.contains("\r\nBcc:"), "{raw}");
        let bad = OutgoingMail::new("bot@proton.me", vec!["not an address".into()], vec![], "s", "b".into());
        assert!(bad.to_rfc5322().is_err());
        assert_eq!(m.recipients(), vec!["a@x.com"]);
    }
}
