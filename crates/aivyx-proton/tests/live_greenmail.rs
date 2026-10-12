//! Live test against a local GreenMail (IMAP + SMTP) in Docker. Run by hand:
//!
//!   docker run -d --rm --name aivyx-greenmail \
//!     -p 127.0.0.1:3025:3025 -p 127.0.0.1:3143:3143 greenmail/standalone:2.1.3
//!   cargo test -p aivyx-proton --test live_greenmail -- --ignored --nocapture
//!   docker stop aivyx-greenmail
//!
//! GreenMail has no STARTTLS, so the test dials it in plain TCP through its
//! own `Dial` — the crate itself has no plaintext path. Everything above
//! the dial (IMAP search/fetch/IDLE/move/append, SMTP, the mail loop) is
//! the production code. The pinned TLS is covered by `tls.rs`'s tests.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use aivyx_core::CancellationToken;
use aivyx_proton::bridge::{BridgeTransport, Dial, ImapClient, Login, Stream};
use aivyx_proton::outgoing::OutgoingMail;
use aivyx_proton::policy::ProtonPolicy;
use aivyx_proton::session::Turn;
use aivyx_proton::smtp;
use aivyx_proton::transport::{MailTransport, SearchQuery, TransportError};
use aivyx_proton::{run_mail_loop, TurnRunner};

const IMAP: &str = "127.0.0.1:3143";
const SMTP: &str = "127.0.0.1:3025";

struct PlainDial;

fn platform(e: impl std::fmt::Display) -> TransportError {
    TransportError::Platform(e.to_string())
}

#[async_trait]
impl Dial for PlainDial {
    async fn imap(&self) -> Result<ImapClient, TransportError> {
        let tcp = tokio::net::TcpStream::connect(IMAP).await.map_err(platform)?;
        let mut c = async_imap::Client::new(Box::new(tcp) as Stream);
        c.read_response().await.map_err(platform)?;
        Ok(c)
    }
    async fn smtp(&self) -> Result<smtp::Conn<Stream>, TransportError> {
        let tcp = tokio::net::TcpStream::connect(SMTP).await.map_err(platform)?;
        smtp::Conn::open(Box::new(tcp) as Stream).await.map_err(platform)
    }
}

fn transport(user: &str) -> BridgeTransport {
    BridgeTransport::new(
        Arc::new(PlainDial),
        Login { username: user.into(), password: "pw".into() },
        "Aivyx",
    )
}

/// A raw IMAP session for setting up and inspecting the test mailbox.
async fn raw(user: &str) -> async_imap::Session<Stream> {
    PlainDial.imap().await.unwrap().login(user, "pw").await.map_err(|(e, _)| e).unwrap()
}

async fn wait_for_mail(t: &BridgeTransport, after: u32) -> Vec<aivyx_proton::message::MailMessage> {
    tokio::time::timeout(Duration::from_secs(70), async {
        loop {
            let b = t.wait_for_new(after).await.unwrap();
            if !b.is_empty() {
                return b;
            }
        }
    })
    .await
    .expect("mail arrives")
}

#[tokio::test]
#[ignore = "needs a local GreenMail in Docker — see the module doc"]
async fn the_transport_works_against_a_real_mailbox() {
    let stamp = std::process::id();
    let me = format!("me{stamp}@proton.test");
    let t = transport(&me);
    let status = t.inbox_status().await.unwrap();
    let mut r = raw(&me).await;
    for mb in ["Sent", "All Mail", "Drafts"] {
        let _ = r.create(mb).await;
    }

    // SMTP in, IMAP fetch out.
    let mail = OutgoingMail::new(&me, vec![me.clone()], vec![], "Lunch?", "Where shall we eat?".into());
    t.send(&mail).await.unwrap();
    let got = wait_for_mail(&t, status.uid_next.saturating_sub(1)).await;
    assert_eq!(got[0].subject, "Lunch?");
    assert_eq!(got[0].message_id.as_deref(), Some(mail.message_id.as_str()));

    // The Sent lookup by X-Pm-Internal-Id.
    r.append("Sent", None, None, b"X-Pm-Internal-Id: abc123==\r\nFrom: x@y.z\r\nSubject: s\r\n\r\nb\r\n")
        .await
        .unwrap();
    assert!(t.in_sent("abc123==").await.unwrap());
    assert!(!t.in_sent("zzz").await.unwrap());

    // Filing: read, moved with a copy of the reply to Folders/Aivyx.
    let reply = OutgoingMail::reply(&me, vec![me.clone()], &got[0], "The usual place.".into());
    t.file_handled(got[0].uid, &reply).await.unwrap();
    r.select("Folders/Aivyx").await.unwrap();
    assert_eq!(r.uid_search("ALL").await.unwrap().len(), 2);
    assert_eq!(r.select("INBOX").await.unwrap().exists, 0);

    // All Mail search + fetch, and a draft.
    r.append("All Mail", None, None, b"From: ann@x.com\r\nSubject: invoice 42\r\nMessage-ID: <inv@x.com>\r\n\r\nplease pay\r\n")
        .await
        .unwrap();
    let hits = t
        .search(&SearchQuery { subject: Some("invoice".into()), max_results: 5, ..Default::default() })
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].from.as_deref(), Some("ann@x.com"));
    assert!(t.fetch(hits[0].uid).await.unwrap().unwrap().text.contains("please pay"));
    t.append_draft(&OutgoingMail::new(&me, vec!["ann@x.com".into()], vec![], "Re: invoice", "paid".into()))
        .await
        .unwrap();
    r.select("Drafts").await.unwrap();
    assert_eq!(r.uid_search("ALL").await.unwrap().len(), 1);

    // IDLE wakes for new mail well before its 60 s timeout.
    let last = r.select("INBOX").await.unwrap().uid_next.unwrap_or(1) - 1;
    let me2 = me.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let m = OutgoingMail::new(&me2, vec![me2.clone()], vec![], "second", "x".into());
        transport(&me2).send(&m).await.unwrap();
    });
    let start = std::time::Instant::now();
    let b = wait_for_mail(&transport(&me), last).await;
    assert_eq!(b[0].subject, "second");
    assert!(start.elapsed() < Duration::from_secs(30), "IDLE took {:?}", start.elapsed());
    r.logout().await.unwrap();
}

/// Answers every turn with its subject, uppercased.
struct Echo;

#[async_trait]
impl TurnRunner for Echo {
    async fn run(&mut self, turn: &Turn) -> Result<Option<String>, String> {
        Ok(Some(turn.message.subject.to_uppercase()))
    }
}

#[tokio::test]
#[ignore = "needs a local GreenMail in Docker — see the module doc"]
async fn the_mail_loop_answers_mail_from_you_and_files_it() {
    let stamp = std::process::id();
    let me = format!("me{stamp}l@proton.test");
    let bot = format!("bot{stamp}l@proton.test");
    let mut r = raw(&me).await;
    let _ = r.create("Sent").await;
    let dir = tempfile::tempdir().unwrap();
    let policy = ProtonPolicy::new(&bot, &[me.clone(), bot.clone()], &[]);
    let shutdown = CancellationToken::new();
    let loop_shutdown = shutdown.clone();
    let loop_dir = dir.path().to_path_buf();
    let loop_user = me.clone();
    let looper = tokio::spawn(async move {
        let t = transport(&loop_user);
        let mut echo = Echo;
        run_mail_loop(&t, &policy, &loop_dir, &mut echo, loop_shutdown).await
    });
    // Let the loop take its "now" cursor before the mail arrives.
    tokio::time::sleep(Duration::from_secs(2)).await;

    // You → the bot address, delivered to your own inbox (one account), and
    // a copy in Sent carrying the same X-Pm-Internal-Id — as Proton does.
    let raw_mail = format!(
        "X-Pm-Internal-Id: pm{stamp}==\r\nFrom: {me}\r\nTo: {bot}\r\nMessage-ID: <q{stamp}@proton.test>\r\n\
         Subject: hello bot\r\n\r\nare you there?\r\n"
    );
    r.append("Sent", None, None, raw_mail.as_bytes()).await.unwrap();
    r.append("INBOX", None, None, raw_mail.as_bytes()).await.unwrap();
    // A forgery: same From, not in Sent.
    let forged = format!(
        "X-Pm-Internal-Id: forged{stamp}==\r\nFrom: {me}\r\nTo: {bot}\r\nSubject: forged\r\n\r\nx\r\n"
    );
    r.append("INBOX", None, None, forged.as_bytes()).await.unwrap();

    // The reply (from the bot address) lands back in this same inbox.
    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    let reply = loop {
        assert!(std::time::Instant::now() < deadline, "no reply arrived");
        r.select("INBOX").await.unwrap();
        let uids = r.uid_search(format!("FROM \"{bot}\"")).await.unwrap();
        if let Some(uid) = uids.into_iter().next() {
            use futures_util::TryStreamExt;
            let f: Vec<_> = r.uid_fetch(uid.to_string(), "BODY.PEEK[]").await.unwrap().try_collect().await.unwrap();
            break aivyx_proton::message::parse(uid, f[0].body().unwrap()).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    assert_eq!(reply.subject, "Re: hello bot");
    assert_eq!(reply.in_reply_to.as_deref(), Some(format!("q{stamp}@proton.test").as_str()));
    assert!(reply.text.contains("HELLO BOT"));
    // The forgery got nothing and stays unread in the inbox; the real one was filed.
    r.select("Folders/Aivyx").await.unwrap();
    assert_eq!(r.uid_search("ALL").await.unwrap().len(), 2, "original + reply copy");
    r.select("INBOX").await.unwrap();
    assert_eq!(r.uid_search("SUBJECT \"forged\" UNSEEN").await.unwrap().len(), 1);

    shutdown.cancel();
    let report = looper.await.unwrap().unwrap();
    assert_eq!(report.answered, 1);
    r.logout().await.unwrap();
}
