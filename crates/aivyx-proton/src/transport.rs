//! The seam between the channel/tools and the mailbox. Production is
//! `bridge::BridgeTransport` (IMAP + SMTP to Proton Mail Bridge); tests
//! use [`ScriptedTransport`]. No IMAP or SMTP type crosses this boundary.

use std::collections::VecDeque;
use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;
use tokio::sync::Mutex;

use crate::message::MailMessage;
use crate::outgoing::OutgoingMail;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("Proton Mail Bridge: {0}")]
    Platform(String),
    /// Bridge rejected the stored login. Retrying can't help.
    #[error("Proton Mail Bridge rejected the login ({0}) — run `aivyx-pa connect proton` again")]
    Unauthorized(String),
    /// Bridge presented a certificate other than the pinned one.
    #[error(
        "Proton Mail Bridge's certificate changed — if you reinstalled or reset Bridge, \
         run `aivyx-pa connect proton` again"
    )]
    CertChanged,
}

impl TransportError {
    /// Ends the channel: no retry or backoff can recover from it.
    pub fn is_fatal(&self) -> bool {
        matches!(self, TransportError::Unauthorized(_) | TransportError::CertChanged)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InboxStatus {
    pub uid_validity: u32,
    /// The UID the next message will get.
    pub uid_next: u32,
}

/// `proton.search` criteria; all given ones must match.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchQuery {
    pub from: Option<String>,
    pub to: Option<String>,
    pub subject: Option<String>,
    pub text: Option<String>,
    /// `YYYY-MM-DD`, inclusive.
    pub since: Option<String>,
    /// `YYYY-MM-DD`, exclusive.
    pub before: Option<String>,
    pub max_results: usize,
}

impl SearchQuery {
    /// IMAP `SEARCH` criteria. Values are quoted; CR, LF and NUL refused.
    pub fn to_imap(&self) -> Result<String, String> {
        let mut parts = Vec::new();
        for (key, value) in [
            ("FROM", &self.from),
            ("TO", &self.to),
            ("SUBJECT", &self.subject),
            ("TEXT", &self.text),
        ] {
            if let Some(v) = value {
                parts.push(format!("{key} {}", quote(v)?));
            }
        }
        if let Some(d) = &self.since {
            parts.push(format!("SINCE {}", imap_date(d)?));
        }
        if let Some(d) = &self.before {
            parts.push(format!("BEFORE {}", imap_date(d)?));
        }
        Ok(if parts.is_empty() { "ALL".into() } else { parts.join(" ") })
    }
}

/// An IMAP quoted string.
pub fn quote(s: &str) -> Result<String, String> {
    if s.contains(['\r', '\n', '\0']) {
        return Err("search text can't contain line breaks".into());
    }
    Ok(format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")))
}

/// `2026-10-01` → `1-Oct-2026`.
pub fn imap_date(ymd: &str) -> Result<String, String> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let bad = || format!("dates are YYYY-MM-DD, got {ymd:?}");
    let mut it = ymd.trim().splitn(3, '-');
    let (y, m, d) = (it.next(), it.next(), it.next());
    let y: u32 = y.and_then(|v| v.parse().ok()).filter(|v| (1970..=9999).contains(v)).ok_or_else(bad)?;
    let m: usize = m.and_then(|v| v.parse().ok()).filter(|v| (1..=12).contains(v)).ok_or_else(bad)?;
    let d: u32 = d.and_then(|v| v.parse().ok()).filter(|v| (1..=31).contains(v)).ok_or_else(bad)?;
    Ok(format!("{d}-{}-{y}", MONTHS[m - 1]))
}

#[async_trait]
pub trait MailTransport: Send + Sync {
    /// INBOX's UIDVALIDITY and next UID.
    async fn inbox_status(&self) -> Result<InboxStatus, TransportError>;
    /// INBOX messages with a UID above `after_uid`, oldest first (at most
    /// 20). Waits (IDLE, or a 60 s poll) when there are none; may return
    /// an empty batch.
    async fn wait_for_new(&self, after_uid: u32) -> Result<Vec<MailMessage>, TransportError>;
    /// Whether the Proton message with this `X-Pm-Internal-Id` is also in
    /// the Sent folder — i.e. this account sent it.
    async fn in_sent(&self, internal_id: &str) -> Result<bool, TransportError>;
    async fn send(&self, mail: &OutgoingMail) -> Result<(), TransportError>;
    /// Marks INBOX message `uid` read and moves it, with a copy of
    /// `reply`, to the handled-mail folder.
    async fn file_handled(&self, uid: u32, reply: &OutgoingMail) -> Result<(), TransportError>;
    /// All Mail, newest first, at most `q.max_results`.
    async fn search(&self, q: &SearchQuery) -> Result<Vec<MailMessage>, TransportError>;
    /// One All Mail message by UID.
    async fn fetch(&self, uid: u32) -> Result<Option<MailMessage>, TransportError>;
    async fn append_draft(&self, mail: &OutgoingMail) -> Result<(), TransportError>;
}

/// Test double: pops queued INBOX batches, records everything written.
pub struct ScriptedTransport {
    status: InboxStatus,
    batches: Mutex<VecDeque<Vec<MailMessage>>>,
    sent_ids: Vec<String>,
    all_mail: Vec<MailMessage>,
    fail_send: bool,
    fail_in_sent_once: std::sync::atomic::AtomicBool,
    fail_when_empty: Option<fn() -> TransportError>,
    sent: Mutex<Vec<OutgoingMail>>,
    filed: Mutex<Vec<(u32, OutgoingMail)>>,
    drafts: Mutex<Vec<OutgoingMail>>,
    searches: Mutex<Vec<SearchQuery>>,
}

impl ScriptedTransport {
    pub fn new(status: InboxStatus, batches: Vec<Vec<MailMessage>>) -> Self {
        ScriptedTransport {
            status,
            batches: Mutex::new(batches.into()),
            sent_ids: Vec::new(),
            all_mail: Vec::new(),
            fail_send: false,
            fail_in_sent_once: std::sync::atomic::AtomicBool::new(false),
            fail_when_empty: None,
            sent: Mutex::new(Vec::new()),
            filed: Mutex::new(Vec::new()),
            drafts: Mutex::new(Vec::new()),
            searches: Mutex::new(Vec::new()),
        }
    }

    /// `X-Pm-Internal-Id`s `in_sent` reports as in Sent.
    pub fn in_sent_folder(mut self, ids: Vec<&str>) -> Self {
        self.sent_ids = ids.into_iter().map(str::to_string).collect();
        self
    }

    /// What `search` and `fetch` see.
    pub fn with_all_mail(mut self, mail: Vec<MailMessage>) -> Self {
        self.all_mail = mail;
        self
    }

    pub fn failing_send(mut self) -> Self {
        self.fail_send = true;
        self
    }

    /// The first `in_sent` call fails with a transient error; later calls
    /// behave normally.
    pub fn fail_in_sent_once(self) -> Self {
        self.fail_in_sent_once
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self
    }

    /// Once the batches run out, `wait_for_new` returns `Err(err())`.
    pub fn then_fail(mut self, err: fn() -> TransportError) -> Self {
        self.fail_when_empty = Some(err);
        self
    }

    pub async fn sent(&self) -> Vec<OutgoingMail> {
        self.sent.lock().await.clone()
    }
    pub async fn filed(&self) -> Vec<(u32, OutgoingMail)> {
        self.filed.lock().await.clone()
    }
    pub async fn drafts(&self) -> Vec<OutgoingMail> {
        self.drafts.lock().await.clone()
    }
    pub async fn searches(&self) -> Vec<SearchQuery> {
        self.searches.lock().await.clone()
    }
}

#[async_trait]
impl MailTransport for ScriptedTransport {
    async fn inbox_status(&self) -> Result<InboxStatus, TransportError> {
        Ok(self.status)
    }

    async fn wait_for_new(&self, after_uid: u32) -> Result<Vec<MailMessage>, TransportError> {
        if let Some(batch) = self.batches.lock().await.pop_front() {
            return Ok(batch.into_iter().filter(|m| m.uid > after_uid).collect());
        }
        if let Some(err) = self.fail_when_empty {
            return Err(err());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
        Ok(Vec::new())
    }

    async fn in_sent(&self, internal_id: &str) -> Result<bool, TransportError> {
        if self
            .fail_in_sent_once
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(TransportError::Platform("bridge busy".into()));
        }
        Ok(self.sent_ids.iter().any(|id| id == internal_id))
    }

    async fn send(&self, mail: &OutgoingMail) -> Result<(), TransportError> {
        if self.fail_send {
            return Err(TransportError::Platform("smtp down".into()));
        }
        self.sent.lock().await.push(mail.clone());
        Ok(())
    }

    async fn file_handled(&self, uid: u32, reply: &OutgoingMail) -> Result<(), TransportError> {
        self.filed.lock().await.push((uid, reply.clone()));
        Ok(())
    }

    async fn search(&self, q: &SearchQuery) -> Result<Vec<MailMessage>, TransportError> {
        self.searches.lock().await.push(q.clone());
        let mut hits = self.all_mail.clone();
        hits.sort_by_key(|m| std::cmp::Reverse(m.uid));
        hits.truncate(q.max_results);
        Ok(hits)
    }

    async fn fetch(&self, uid: u32) -> Result<Option<MailMessage>, TransportError> {
        Ok(self.all_mail.iter().find(|m| m.uid == uid).cloned())
    }

    async fn append_draft(&self, mail: &OutgoingMail) -> Result<(), TransportError> {
        self.drafts.lock().await.push(mail.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(uid: u32) -> MailMessage {
        MailMessage { uid, ..Default::default() }
    }

    #[tokio::test]
    async fn batches_are_filtered_by_uid_and_writes_recorded() {
        let t = ScriptedTransport::new(InboxStatus::default(), vec![vec![m(3), m(5)]]);
        assert_eq!(t.wait_for_new(3).await.unwrap(), vec![m(5)]);
        assert!(t.wait_for_new(0).await.unwrap().is_empty());
        let mail = OutgoingMail::new("a@x.com", vec!["b@y.com".into()], vec![], "s", "b".into());
        t.send(&mail).await.unwrap();
        t.file_handled(5, &mail).await.unwrap();
        assert_eq!(t.sent().await.len(), 1);
        assert_eq!(t.filed().await[0].0, 5);
    }

    #[test]
    fn only_a_bad_login_and_a_changed_certificate_are_fatal() {
        assert!(TransportError::Unauthorized("x".into()).is_fatal());
        assert!(TransportError::CertChanged.is_fatal());
        assert!(!TransportError::Platform("x".into()).is_fatal());
        assert!(TransportError::CertChanged.to_string().contains("connect proton"));
        assert!(TransportError::Unauthorized("x".into()).to_string().contains("connect proton"));
    }

    #[test]
    fn search_criteria_are_quoted_and_dated() {
        let q = SearchQuery {
            from: Some("ann@x.com".into()),
            subject: Some("say \"hi\" \\o/".into()),
            since: Some("2026-10-01".into()),
            before: Some("2026-12-31".into()),
            ..Default::default()
        };
        assert_eq!(
            q.to_imap().unwrap(),
            "FROM \"ann@x.com\" SUBJECT \"say \\\"hi\\\" \\\\o/\" SINCE 1-Oct-2026 BEFORE 31-Dec-2026"
        );
        assert_eq!(SearchQuery::default().to_imap().unwrap(), "ALL");
        let inject = SearchQuery { text: Some("x\r\nA DELETE INBOX".into()), ..Default::default() };
        assert!(inject.to_imap().is_err());
        assert!(imap_date("2026-13-01").is_err());
        assert!(imap_date("01/10/2026").is_err());
    }
}
