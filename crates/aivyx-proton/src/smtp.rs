//! A minimal SMTP submission client — just what Bridge needs: EHLO,
//! STARTTLS, AUTH PLAIN, MAIL/RCPT/DATA, QUIT. Our own rather than
//! `lettre`'s transport because the TLS must be pinned (`tls`), which
//! `lettre` can't express. Generic over the stream so the protocol is
//! unit-tested over an in-memory pipe.

use base64::Engine as _;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

#[derive(Debug, Error)]
pub enum SmtpError {
    #[error("smtp i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("smtp server said {code}: {text}")]
    Reply { code: u16, text: String },
    #[error("smtp login rejected: {0}")]
    Auth(String),
    #[error("smtp protocol: {0}")]
    Protocol(String),
}

pub struct Conn<S> {
    stream: BufReader<S>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Conn<S> {
    /// Reads the server greeting and says EHLO.
    pub async fn open(stream: S) -> Result<Self, SmtpError> {
        let mut c = Conn {
            stream: BufReader::new(stream),
        };
        c.expect(220).await?;
        c.ehlo().await?;
        Ok(c)
    }

    /// After STARTTLS there is no new greeting: just EHLO again.
    pub async fn resume_after_tls(stream: S) -> Result<Self, SmtpError> {
        let mut c = Conn {
            stream: BufReader::new(stream),
        };
        c.ehlo().await?;
        Ok(c)
    }

    async fn ehlo(&mut self) -> Result<(), SmtpError> {
        self.command("EHLO aivyx-pa").await?;
        self.expect(250).await.map(|_| ())
    }

    /// Asks to start TLS and hands back the raw stream for the handshake.
    /// Bytes the server sent after its `220` would be injected plaintext,
    /// so they are an error.
    pub async fn starttls(mut self) -> Result<S, SmtpError> {
        self.command("STARTTLS").await?;
        self.expect(220).await?;
        if !self.stream.buffer().is_empty() {
            return Err(SmtpError::Protocol("data after STARTTLS".into()));
        }
        Ok(self.stream.into_inner())
    }

    pub async fn auth_plain(&mut self, user: &str, password: &str) -> Result<(), SmtpError> {
        let token = base64::engine::general_purpose::STANDARD.encode(format!("\0{user}\0{password}"));
        self.command(&format!("AUTH PLAIN {token}")).await?;
        match self.expect(235).await {
            Err(SmtpError::Reply { code: 535, text }) => Err(SmtpError::Auth(text)),
            other => other.map(|_| ()),
        }
    }

    /// `data` is the RFC 5322 message (CRLF line endings).
    pub async fn send(&mut self, from: &str, to: &[String], data: &[u8]) -> Result<(), SmtpError> {
        if to.is_empty() {
            return Err(SmtpError::Protocol("no recipients".into()));
        }
        self.command(&format!("MAIL FROM:<{}>", path(from)?)).await?;
        self.expect(250).await?;
        for rcpt in to {
            self.command(&format!("RCPT TO:<{}>", path(rcpt)?)).await?;
            self.expect_one_of(&[250, 251]).await?;
        }
        self.command("DATA").await?;
        self.expect(354).await?;
        let w = self.stream.get_mut();
        w.write_all(&dot_stuff(data)).await?;
        w.flush().await?;
        self.expect(250).await.map(|_| ())
    }

    pub async fn quit(mut self) {
        if self.command("QUIT").await.is_ok() {
            let _ = self.expect(221).await;
        }
    }

    async fn command(&mut self, line: &str) -> Result<(), SmtpError> {
        if line.contains(['\r', '\n']) {
            return Err(SmtpError::Protocol("line break in a command".into()));
        }
        let w = self.stream.get_mut();
        w.write_all(line.as_bytes()).await?;
        w.write_all(b"\r\n").await?;
        w.flush().await?;
        Ok(())
    }

    async fn expect(&mut self, code: u16) -> Result<Vec<String>, SmtpError> {
        self.expect_one_of(&[code]).await
    }

    /// Reads one (possibly multi-line) reply.
    async fn expect_one_of(&mut self, codes: &[u16]) -> Result<Vec<String>, SmtpError> {
        let mut lines = Vec::new();
        loop {
            let mut line = String::new();
            if self.stream.read_line(&mut line).await? == 0 {
                return Err(SmtpError::Protocol("connection closed".into()));
            }
            let line = line.trim_end_matches(['\r', '\n']).to_string();
            let code: u16 = line
                .get(..3)
                .and_then(|c| c.parse().ok())
                .ok_or_else(|| SmtpError::Protocol(format!("bad reply {line:?}")))?;
            let more = line.as_bytes().get(3) == Some(&b'-');
            lines.push(line);
            if !more {
                if codes.contains(&code) {
                    return Ok(lines);
                }
                return Err(SmtpError::Reply {
                    code,
                    text: lines.join(" / "),
                });
            }
        }
    }
}

/// A bare address for `MAIL FROM` / `RCPT TO`.
fn path(addr: &str) -> Result<&str, SmtpError> {
    let a = addr.trim();
    if a.is_empty() || a.contains(['<', '>', ' ', '\r', '\n']) || !a.contains('@') {
        return Err(SmtpError::Protocol(format!("bad address {addr:?}")));
    }
    Ok(a)
}

/// The DATA payload: a leading `.` on any line doubled, CRLF-terminated,
/// then the `.` end marker.
pub fn dot_stuff(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 8);
    let mut line_start = true;
    for &b in data {
        if line_start && b == b'.' {
            out.push(b'.');
        }
        out.push(b);
        line_start = b == b'\n';
    }
    if !out.ends_with(b"\r\n") {
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b".\r\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::DuplexStream;

    /// A scripted server: for each `(line prefix, reply)` it reads client
    /// lines until one starts with the prefix (`"."` must match exactly —
    /// the DATA end marker), then sends the reply. Returns every line read.
    fn server(
        greeting: bool,
        script: Vec<(&'static str, &'static str)>,
    ) -> (DuplexStream, tokio::task::JoinHandle<Vec<String>>) {
        let (client, srv) = tokio::io::duplex(64 * 1024);
        let h = tokio::spawn(async move {
            let mut srv = BufReader::new(srv);
            if greeting {
                srv.get_mut().write_all(b"220 bridge ready\r\n").await.unwrap();
            }
            let mut seen = Vec::new();
            for (expect, reply) in script {
                loop {
                    let mut line = String::new();
                    if srv.read_line(&mut line).await.unwrap() == 0 {
                        return seen;
                    }
                    let line = line.trim_end_matches(['\r', '\n']).to_string();
                    let hit = if expect == "." { line == "." } else { line.starts_with(expect) };
                    seen.push(line);
                    if hit {
                        break;
                    }
                }
                srv.get_mut().write_all(reply.as_bytes()).await.unwrap();
            }
            seen
        });
        (client, h)
    }

    #[tokio::test]
    async fn a_message_is_submitted() {
        let (stream, srv) = server(
            true,
            vec![
                ("EHLO", "250-bridge\r\n250 AUTH PLAIN\r\n"),
                ("AUTH PLAIN", "235 ok\r\n"),
                ("MAIL FROM:<bot@proton.me>", "250 ok\r\n"),
                ("RCPT TO:<me@proton.me>", "250 ok\r\n"),
                ("DATA", "354 go\r\n"),
                (".", "250 queued\r\n"),
                ("QUIT", "221 bye\r\n"),
            ],
        );
        let mut c = Conn::open(stream).await.unwrap();
        c.auth_plain("user", "pw").await.unwrap();
        c.send("bot@proton.me", &["me@proton.me".to_string()], b"Subject: x\r\n\r\n.hidden\r\nhi")
            .await
            .unwrap();
        c.quit().await;
        let seen = srv.await.unwrap();
        let token = base64::engine::general_purpose::STANDARD.encode("\0user\0pw");
        assert!(seen.contains(&format!("AUTH PLAIN {token}")), "{seen:?}");
        let data = seen.iter().position(|l| l == "DATA").unwrap();
        assert_eq!(&seen[data + 1..data + 6], ["Subject: x", "", "..hidden", "hi", "."]);
    }

    #[tokio::test]
    async fn a_rejected_login_is_auth() {
        let (stream, _srv) = server(
            true,
            vec![
                ("EHLO", "250 bridge\r\n"),
                ("AUTH PLAIN", "535 5.7.8 bad credentials\r\n"),
            ],
        );
        let mut c = Conn::open(stream).await.unwrap();
        assert!(matches!(c.auth_plain("u", "p").await, Err(SmtpError::Auth(_))));
    }

    #[tokio::test]
    async fn starttls_refuses_injected_bytes() {
        let (stream, _srv) = server(
            true,
            vec![
                ("EHLO", "250 bridge\r\n"),
                ("STARTTLS", "220 go ahead\r\n250 injected\r\n"),
            ],
        );
        let c = Conn::open(stream).await.unwrap();
        assert!(matches!(c.starttls().await, Err(SmtpError::Protocol(_))));
    }

    #[tokio::test]
    async fn a_refused_recipient_is_an_error() {
        let (stream, _srv) = server(
            false,
            vec![
                ("EHLO", "250 bridge\r\n"),
                ("MAIL FROM", "250 ok\r\n"),
                ("RCPT TO", "550 no such user\r\n"),
            ],
        );
        let mut c = Conn::resume_after_tls(stream).await.unwrap();
        let e = c.send("a@x.com", &["b@y.com".to_string()], b"x").await.unwrap_err();
        assert!(matches!(e, SmtpError::Reply { code: 550, .. }), "{e}");
    }

    #[test]
    fn dot_stuffing_and_bad_paths() {
        assert_eq!(dot_stuff(b".a\r\nb\r\n.c"), b"..a\r\nb\r\n..c\r\n.\r\n".to_vec());
        assert!(path("a b@x").is_err());
        assert!(path("a@x>\r\nRCPT TO:<evil@x").is_err());
        assert!(path("nobody").is_err());
        assert_eq!(path(" a@x ").unwrap(), "a@x");
    }
}
