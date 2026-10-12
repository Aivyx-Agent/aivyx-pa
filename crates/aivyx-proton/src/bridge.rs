//! [`BridgeTransport`]: [`MailTransport`] over Proton Mail Bridge's local
//! IMAP (IDLE, 60 s poll fallback) and SMTP, both STARTTLS pinned to the
//! certificate captured at `connect proton` ([`BridgeDial`]).
//!
//! Connecting is behind [`Dial`] so the live test can drive the very same
//! transport over a plain local GreenMail (which has no STARTTLS) without
//! any plaintext path existing in this crate.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::TryStreamExt;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, MutexGuard};

use crate::message::{self, MailMessage};
use crate::outgoing::OutgoingMail;
use crate::smtp;
use crate::tls::{self, Pin};
use crate::transport::{InboxStatus, MailTransport, SearchQuery, TransportError};

/// Bridge's mailbox names.
pub const INBOX: &str = "INBOX";
pub const SENT: &str = "Sent";
pub const DRAFTS: &str = "Drafts";
pub const ALL_MAIL: &str = "All Mail";
/// Most messages one `wait_for_new` returns.
const BATCH: usize = 20;
/// How long one IDLE (or poll sleep) lasts before searching again.
const IDLE_FOR: Duration = Duration::from_secs(60);
/// Longest any one Bridge operation may take (`wait_for_new` gets `IDLE_FOR` on top).
const OP_TIMEOUT: Duration = Duration::from_secs(120);

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send + fmt::Debug {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + fmt::Debug> Io for T {}
pub type Stream = Box<dyn Io>;
pub type ImapClient = async_imap::Client<Stream>;
type ImapSession = async_imap::Session<Stream>;

/// Opens connections, ready to log in: IMAP after the greeting, SMTP
/// after EHLO.
#[async_trait]
pub trait Dial: Send + Sync {
    async fn imap(&self) -> Result<ImapClient, TransportError>;
    async fn smtp(&self) -> Result<smtp::Conn<Stream>, TransportError>;
}

/// STARTTLS to Bridge on loopback, each port pinned to its certificate.
pub struct BridgeDial {
    pub imap_addr: String,
    pub smtp_addr: String,
    pub imap_pin: Pin,
    pub smtp_pin: Pin,
}

fn platform(e: impl fmt::Display) -> TransportError {
    TransportError::Platform(e.to_string())
}

fn tls_error(e: std::io::Error) -> TransportError {
    if tls::is_pin_mismatch(&e) {
        TransportError::CertChanged
    } else {
        platform(e)
    }
}

/// Plain TCP → greeting → STARTTLS → the raw stream for the handshake.
async fn imap_starttls(addr: &str) -> Result<TcpStream, TransportError> {
    let tcp = TcpStream::connect(addr).await.map_err(platform)?;
    let mut client = async_imap::Client::new(tcp);
    client
        .read_response()
        .await
        .map_err(platform)?
        .ok_or_else(|| platform("Bridge closed the IMAP connection"))?;
    client
        .run_command_and_check_ok("STARTTLS", None)
        .await
        .map_err(platform)?;
    Ok(client.into_inner())
}

async fn smtp_starttls(addr: &str) -> Result<TcpStream, TransportError> {
    let tcp = TcpStream::connect(addr).await.map_err(platform)?;
    smtp::Conn::open(tcp)
        .await
        .map_err(platform)?
        .starttls()
        .await
        .map_err(platform)
}

#[async_trait]
impl Dial for BridgeDial {
    async fn imap(&self) -> Result<ImapClient, TransportError> {
        let tcp = imap_starttls(&self.imap_addr).await?;
        let name = tls::server_name(&self.imap_addr).map_err(platform)?;
        let stream = tls::pinned(self.imap_pin)
            .connect(name, tcp)
            .await
            .map_err(tls_error)?;
        Ok(async_imap::Client::new(Box::new(stream) as Stream))
    }

    async fn smtp(&self) -> Result<smtp::Conn<Stream>, TransportError> {
        let tcp = smtp_starttls(&self.smtp_addr).await?;
        let name = tls::server_name(&self.smtp_addr).map_err(platform)?;
        let stream = tls::pinned(self.smtp_pin)
            .connect(name, tcp)
            .await
            .map_err(tls_error)?;
        smtp::Conn::resume_after_tls(Box::new(stream) as Stream)
            .await
            .map_err(platform)
    }
}

/// For `connect proton`: STARTTLS to both ports accepting any certificate,
/// returning each one's pin for the operator to confirm.
pub async fn capture_pins(imap_addr: &str, smtp_addr: &str) -> Result<(Pin, Pin), TransportError> {
    async fn capture(addr: &str, tcp: TcpStream) -> Result<Pin, TransportError> {
        let (connector, seen) = tls::capturing();
        let name = tls::server_name(addr).map_err(platform)?;
        connector.connect(name, tcp).await.map_err(platform)?;
        let der = seen.lock().expect("seen mutex poisoned").clone();
        der.map(|d| tls::fingerprint(&d))
            .ok_or_else(|| platform("Bridge sent no certificate"))
    }
    let imap = capture(imap_addr, imap_starttls(imap_addr).await?).await?;
    let smtp = capture(smtp_addr, smtp_starttls(smtp_addr).await?).await?;
    Ok((imap, smtp))
}

pub struct Login {
    pub username: String,
    pub password: String,
}

impl fmt::Debug for Login {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Login")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

pub struct BridgeTransport {
    dial: Arc<dyn Dial>,
    login: Login,
    /// `Folders/<folder>`: Bridge shows Proton folders under `Folders/`.
    handled_mailbox: String,
    /// One IMAP session, reconnected after any error.
    imap: Mutex<Option<ImapSession>>,
}

impl BridgeTransport {
    pub fn new(dial: Arc<dyn Dial>, login: Login, folder: &str) -> Self {
        BridgeTransport {
            dial,
            login,
            handled_mailbox: format!("Folders/{folder}"),
            imap: Mutex::new(None),
        }
    }

    /// Logs in to IMAP and SMTP without changing anything — what
    /// `connect proton` runs before saving the credential.
    pub async fn check_login(&self) -> Result<(), TransportError> {
        self.bounded(OP_TIMEOUT, self.check_login_inner()).await
    }

    /// Runs `fut` for at most `limit`. On a timeout the cached IMAP session
    /// (possibly mid-command) is dropped and a transient error returned.
    async fn bounded<T>(
        &self,
        limit: Duration,
        fut: impl std::future::Future<Output = Result<T, TransportError>>,
    ) -> Result<T, TransportError> {
        match tokio::time::timeout(limit, fut).await {
            Ok(r) => r,
            Err(_) => {
                *self.imap.lock().await = None;
                Err(TransportError::Platform("Proton Mail Bridge didn't answer in time".into()))
            }
        }
    }

    async fn check_login_inner(&self) -> Result<(), TransportError> {
        self.inbox_status_inner().await?;
        let mut conn = self.dial.smtp().await?;
        let r = conn
            .auth_plain(&self.login.username, &self.login.password)
            .await
            .map_err(|e| match e {
                smtp::SmtpError::Auth(m) => TransportError::Unauthorized(m),
                other => platform(other),
            });
        conn.quit().await;
        r
    }

    async fn session(&self) -> Result<MutexGuard<'_, Option<ImapSession>>, TransportError> {
        let mut guard = self.imap.lock().await;
        if guard.is_none() {
            let client = self.dial.imap().await?;
            let session = client
                .login(&self.login.username, &self.login.password)
                .await
                .map_err(|(e, _)| match e {
                    async_imap::error::Error::No(m) => TransportError::Unauthorized(m),
                    other => platform(other),
                })?;
            *guard = Some(session);
        }
        Ok(guard)
    }

    /// Runs `op` on the session; any error drops it so the next call
    /// reconnects.
    async fn with_session<T>(
        &self,
        op: impl for<'s> FnOnce(
            &'s mut ImapSession,
        ) -> futures_util::future::BoxFuture<'s, Result<T, TransportError>>,
    ) -> Result<T, TransportError> {
        let mut guard = self.session().await?;
        let result = op(guard.as_mut().expect("session() connected")).await;
        if result.is_err() {
            *guard = None;
        }
        result
    }
}

async fn new_uids(s: &mut ImapSession, after: u32) -> Result<Vec<u32>, TransportError> {
    s.select(INBOX).await.map_err(platform)?;
    // `N:*` also matches the last message when none is newer: filter.
    let mut uids: Vec<u32> = s
        .uid_search(format!("UID {}:*", after.saturating_add(1)))
        .await
        .map_err(platform)?
        .into_iter()
        .filter(|u| *u > after)
        .collect();
    uids.sort_unstable();
    uids.truncate(BATCH);
    Ok(uids)
}

async fn fetch_uids(s: &mut ImapSession, uids: &[u32]) -> Result<Vec<MailMessage>, TransportError> {
    if uids.is_empty() {
        return Ok(Vec::new());
    }
    let set: Vec<String> = uids.iter().map(u32::to_string).collect();
    let fetches: Vec<_> = s
        .uid_fetch(set.join(","), "(UID BODY.PEEK[])")
        .await
        .map_err(platform)?
        .try_collect()
        .await
        .map_err(platform)?;
    let mut out: Vec<MailMessage> = fetches
        .iter()
        .filter_map(|f| message::parse(f.uid?, f.body()?))
        .collect();
    out.sort_by_key(|m| m.uid);
    Ok(out)
}

/// Proton message IDs are base64url (plus `=`); anything else can't match.
fn plausible_internal_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'='))
}

/// Fills in a blank message for every UID that was found but didn't parse,
/// so the receive loop advances past it instead of finding it again.
fn with_stand_ins(found: &[u32], parsed: Vec<MailMessage>) -> Vec<MailMessage> {
    let mut out = parsed;
    for &uid in found {
        if !out.iter().any(|m| m.uid == uid) {
            eprintln!("aivyx-proton: skipping message UID {uid}: couldn't read it");
            out.push(MailMessage { uid, ..Default::default() });
        }
    }
    out.sort_by_key(|m| m.uid);
    out
}

async fn fetch_new(s: &mut ImapSession, uids: &[u32]) -> Result<Vec<MailMessage>, TransportError> {
    let parsed = fetch_uids(s, uids).await?;
    Ok(with_stand_ins(uids, parsed))
}

#[async_trait]
impl MailTransport for BridgeTransport {
    async fn inbox_status(&self) -> Result<InboxStatus, TransportError> {
        self.bounded(OP_TIMEOUT, self.inbox_status_inner()).await
    }
    async fn wait_for_new(&self, after_uid: u32) -> Result<Vec<MailMessage>, TransportError> {
        self.bounded(IDLE_FOR + OP_TIMEOUT, self.wait_for_new_inner(after_uid)).await
    }
    async fn in_sent(&self, internal_id: &str) -> Result<bool, TransportError> {
        self.bounded(OP_TIMEOUT, self.in_sent_inner(internal_id)).await
    }
    async fn send(&self, mail: &OutgoingMail) -> Result<(), TransportError> {
        self.bounded(OP_TIMEOUT, self.send_inner(mail)).await
    }
    async fn file_handled(&self, uid: u32, reply: &OutgoingMail) -> Result<(), TransportError> {
        self.bounded(OP_TIMEOUT, self.file_handled_inner(uid, reply)).await
    }
    async fn search(&self, q: &SearchQuery) -> Result<Vec<MailMessage>, TransportError> {
        self.bounded(OP_TIMEOUT, self.search_inner(q)).await
    }
    async fn fetch(&self, uid: u32) -> Result<Option<MailMessage>, TransportError> {
        self.bounded(OP_TIMEOUT, self.fetch_inner(uid)).await
    }
    async fn append_draft(&self, mail: &OutgoingMail) -> Result<(), TransportError> {
        self.bounded(OP_TIMEOUT, self.append_draft_inner(mail)).await
    }
}

impl BridgeTransport {
    async fn inbox_status_inner(&self) -> Result<InboxStatus, TransportError> {
        self.with_session(|s| {
            Box::pin(async move {
                let mb = s.select(INBOX).await.map_err(platform)?;
                Ok(InboxStatus {
                    uid_validity: mb.uid_validity.unwrap_or(0),
                    uid_next: mb.uid_next.unwrap_or(1),
                })
            })
        })
        .await
    }

    async fn wait_for_new_inner(&self, after_uid: u32) -> Result<Vec<MailMessage>, TransportError> {
        let found = self
            .with_session(|s| Box::pin(async move { new_uids(s, after_uid).await }))
            .await?;
        if !found.is_empty() {
            return self
                .with_session(|s| Box::pin(async move { fetch_new(s, &found).await }))
                .await;
        }
        // Nothing new: wait for the server to say something (or 60 s).
        let mut guard = self.session().await?;
        let mut session = guard.take().expect("session() connected");
        let mut session = match session.capabilities().await {
            Ok(caps) if caps.has_str("IDLE") => {
                let mut idle = session.idle();
                idle.init().await.map_err(platform)?;
                {
                    let (wait, _stop) = idle.wait_with_timeout(IDLE_FOR);
                    wait.await.map_err(platform)?;
                }
                idle.done().await.map_err(platform)?
            }
            Ok(_) => {
                tokio::time::sleep(IDLE_FOR).await;
                session
            }
            Err(e) => return Err(platform(e)),
        };
        let found = new_uids(&mut session, after_uid).await;
        let result = match found {
            Ok(uids) => fetch_new(&mut session, &uids).await,
            Err(e) => Err(e),
        };
        if result.is_ok() {
            *guard = Some(session);
        }
        result
    }

    async fn in_sent_inner(&self, internal_id: &str) -> Result<bool, TransportError> {
        if !plausible_internal_id(internal_id) {
            return Ok(false);
        }
        let query = format!("HEADER X-Pm-Internal-Id {}", crate::transport::quote(internal_id).map_err(platform)?);
        self.with_session(|s| {
            Box::pin(async move {
                s.examine(SENT).await.map_err(platform)?;
                Ok(!s.uid_search(query).await.map_err(platform)?.is_empty())
            })
        })
        .await
    }

    async fn send_inner(&self, mail: &OutgoingMail) -> Result<(), TransportError> {
        let data = mail.to_rfc5322().map_err(platform)?;
        let mut conn = self.dial.smtp().await?;
        conn.auth_plain(&self.login.username, &self.login.password)
            .await
            .map_err(|e| match e {
                smtp::SmtpError::Auth(m) => TransportError::Unauthorized(m),
                other => platform(other),
            })?;
        conn.send(&mail.from, &mail.recipients(), &data)
            .await
            .map_err(platform)?;
        conn.quit().await;
        Ok(())
    }

    async fn file_handled_inner(&self, uid: u32, reply: &OutgoingMail) -> Result<(), TransportError> {
        let copy = reply.to_rfc5322().map_err(platform)?;
        let mailbox = self.handled_mailbox.clone();
        self.with_session(|s| {
            Box::pin(async move {
                s.select(INBOX).await.map_err(platform)?;
                let _: Vec<_> = s
                    .uid_store(uid.to_string(), "+FLAGS (\\Seen)")
                    .await
                    .map_err(platform)?
                    .try_collect()
                    .await
                    .map_err(platform)?;
                let exists = {
                    let names: Vec<_> = s
                        .list(Some(""), Some(&mailbox))
                        .await
                        .map_err(platform)?
                        .try_collect()
                        .await
                        .map_err(platform)?;
                    !names.is_empty()
                };
                if !exists {
                    s.create(&mailbox).await.map_err(platform)?;
                }
                s.uid_mv(uid.to_string(), &mailbox).await.map_err(platform)?;
                s.append(&mailbox, Some("(\\Seen)"), None, copy)
                    .await
                    .map_err(platform)?;
                Ok(())
            })
        })
        .await
    }

    async fn search_inner(&self, q: &SearchQuery) -> Result<Vec<MailMessage>, TransportError> {
        let criteria = q.to_imap().map_err(platform)?;
        let max = q.max_results;
        self.with_session(|s| {
            Box::pin(async move {
                s.examine(ALL_MAIL).await.map_err(platform)?;
                let mut uids: Vec<u32> = s
                    .uid_search(criteria)
                    .await
                    .map_err(platform)?
                    .into_iter()
                    .collect();
                uids.sort_unstable_by_key(|u| std::cmp::Reverse(*u));
                uids.truncate(max);
                let mut found = fetch_uids(s, &uids).await?;
                found.sort_by_key(|m| std::cmp::Reverse(m.uid));
                Ok(found)
            })
        })
        .await
    }

    async fn fetch_inner(&self, uid: u32) -> Result<Option<MailMessage>, TransportError> {
        self.with_session(|s| {
            Box::pin(async move {
                s.examine(ALL_MAIL).await.map_err(platform)?;
                Ok(fetch_uids(s, &[uid]).await?.into_iter().next())
            })
        })
        .await
    }

    async fn append_draft_inner(&self, mail: &OutgoingMail) -> Result<(), TransportError> {
        let data = mail.to_rfc5322().map_err(platform)?;
        self.with_session(|s| {
            Box::pin(async move {
                s.append(DRAFTS, Some("(\\Draft \\Seen)"), None, data)
                    .await
                    .map_err(platform)
            })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_ids_are_checked_before_they_reach_a_search() {
        assert!(plausible_internal_id("AbC-_=09"));
        assert!(!plausible_internal_id(""));
        assert!(!plausible_internal_id("a\" OR ALL"));
        assert!(!plausible_internal_id("a b"));
    }

    #[test]
    fn the_password_never_prints() {
        let l = Login { username: "u".into(), password: "hunter2".into() };
        assert!(!format!("{l:?}").contains("hunter2"));
    }

    #[test]
    fn handled_mail_goes_under_folders() {
        struct Never;
        #[async_trait]
        impl Dial for Never {
            async fn imap(&self) -> Result<ImapClient, TransportError> {
                Err(platform("no"))
            }
            async fn smtp(&self) -> Result<smtp::Conn<Stream>, TransportError> {
                Err(platform("no"))
            }
        }
        let t = BridgeTransport::new(Arc::new(Never), Login { username: "u".into(), password: "p".into() }, "Aivyx");
        assert_eq!(t.handled_mailbox, "Folders/Aivyx");
    }

    #[test]
    fn unreadable_uids_come_back_as_blank_stand_ins() {
        let parsed = vec![MailMessage { uid: 5, subject: "hi".into(), ..Default::default() }];
        let out = with_stand_ins(&[3, 5, 7], parsed);
        assert_eq!(out.iter().map(|m| m.uid).collect::<Vec<_>>(), vec![3, 5, 7]);
        assert!(out[0].from.is_none() && out[2].from.is_none());
        assert_eq!(out[1].subject, "hi");
    }

    #[tokio::test]
    async fn a_hung_bridge_times_out_and_drops_the_session() {
        struct Hung;
        #[async_trait]
        impl Dial for Hung {
            async fn imap(&self) -> Result<ImapClient, TransportError> {
                std::future::pending().await
            }
            async fn smtp(&self) -> Result<smtp::Conn<Stream>, TransportError> {
                std::future::pending().await
            }
        }
        let t = BridgeTransport::new(Arc::new(Hung), Login { username: "u".into(), password: "p".into() }, "Aivyx");
        let started = std::time::Instant::now();
        let r = t.bounded(Duration::from_millis(50), t.inbox_status_inner()).await;
        match r {
            Err(TransportError::Platform(m)) => assert!(m.contains("didn't answer in time"), "{m}"),
            other => panic!("{other:?}"),
        }
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(t.imap.lock().await.is_none());
    }
}
