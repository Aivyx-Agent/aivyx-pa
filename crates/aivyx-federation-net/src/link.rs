//! The Noise `XX` link (A19 §2).
//!
//! Wire format: every Noise message (handshake or transport) is a 2-byte
//! big-endian length followed by the message (Noise caps a message at
//! 65 535 bytes). After the handshake, one application frame is the
//! plaintext `u32 BE length || payload`, split into chunks of at most
//! `65535 - 16` plaintext bytes, each sent as one Noise transport message.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::NetError;
use crate::card::fingerprint;
use crate::frame::MAX_FRAME;
use crate::key::TransportKey;

/// The one Noise protocol the link speaks.
pub const NOISE_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_SHA256";

/// Upper bound on connect + the whole handshake.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Noise's maximum message size.
const MAX_NOISE_MSG: usize = 65_535;
/// ChaChaPoly tag length.
const TAG_LEN: usize = 16;
/// Largest plaintext chunk that fits one Noise transport message.
const MAX_CHUNK: usize = MAX_NOISE_MSG - TAG_LEN;
/// Length prefix of an application frame.
const LEN_PREFIX: usize = 4;

/// An established, mutually authenticated Noise session over TCP.
pub struct Link {
    stream: TcpStream,
    transport: snow::TransportState,
    remote: [u8; 32],
    /// Scratch buffer for one Noise message (ciphertext).
    msg: Vec<u8>,
    /// Scratch buffer for one decrypted chunk.
    plain: Vec<u8>,
}

impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Link")
            .field("remote", &fingerprint(&self.remote))
            .finish_non_exhaustive()
    }
}

fn handshake_err(e: snow::Error) -> NetError {
    NetError::Handshake(e.to_string())
}

fn builder(key: &TransportKey) -> Result<snow::Builder<'_>, NetError> {
    let params = NOISE_PARAMS
        .parse()
        .map_err(|e| NetError::Handshake(format!("noise params: {e}")))?;
    snow::Builder::new(params)
        .local_private_key(key.private_bytes())
        .map_err(handshake_err)
}

async fn write_msg(stream: &mut TcpStream, msg: &[u8]) -> Result<(), NetError> {
    let len =
        u16::try_from(msg.len()).map_err(|_| NetError::Frame("noise message too long".into()))?;
    let mut out = Vec::with_capacity(2 + msg.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(msg);
    stream.write_all(&out).await?;
    Ok(())
}

/// Read one length-prefixed Noise message into `buf`; returns its length.
async fn read_msg(stream: &mut TcpStream, buf: &mut [u8]) -> Result<usize, NetError> {
    let mut len = [0u8; 2];
    stream.read_exact(&mut len).await?;
    let len = u16::from_be_bytes(len) as usize;
    stream.read_exact(&mut buf[..len]).await?;
    Ok(len)
}

fn remote_key(hs: &snow::HandshakeState) -> Result<[u8; 32], NetError> {
    hs.get_remote_static()
        .and_then(|k| <[u8; 32]>::try_from(k).ok())
        .ok_or_else(|| NetError::Handshake("remote static key missing or not 32 bytes".into()))
}

impl Link {
    /// Connect to `addr` as the initiator and complete the handshake. The
    /// remote must present `expect_remote`; otherwise this fails with
    /// [`NetError::UnknownPeer`] before sending our own static key (msg 3).
    pub async fn connect(
        addr: &str,
        key: &TransportKey,
        expect_remote: [u8; 32],
    ) -> Result<Link, NetError> {
        tokio::time::timeout(
            HANDSHAKE_TIMEOUT,
            Self::connect_inner(addr, key, expect_remote),
        )
        .await
        .map_err(|_| NetError::Timeout("handshake"))?
    }

    async fn connect_inner(
        addr: &str,
        key: &TransportKey,
        expect_remote: [u8; 32],
    ) -> Result<Link, NetError> {
        let mut stream = TcpStream::connect(addr).await?;
        let _ = stream.set_nodelay(true);
        let mut hs = builder(key)?.build_initiator().map_err(handshake_err)?;
        let mut msg = vec![0u8; MAX_NOISE_MSG];
        let mut plain = vec![0u8; MAX_NOISE_MSG];

        // -> e
        let n = hs.write_message(&[], &mut msg).map_err(handshake_err)?;
        write_msg(&mut stream, &msg[..n]).await?;
        // <- e, ee, s, es
        let n = read_msg(&mut stream, &mut msg).await?;
        hs.read_message(&msg[..n], &mut plain)
            .map_err(handshake_err)?;
        let remote = remote_key(&hs)?;
        if remote != expect_remote {
            return Err(NetError::UnknownPeer {
                fingerprint: fingerprint(&remote),
            });
        }
        // -> s, se
        let n = hs.write_message(&[], &mut msg).map_err(handshake_err)?;
        write_msg(&mut stream, &msg[..n]).await?;

        let transport = hs.into_transport_mode().map_err(handshake_err)?;
        Ok(Link {
            stream,
            transport,
            remote,
            msg,
            plain,
        })
    }

    /// Complete the handshake as the responder on an accepted `stream`. The
    /// initiator's static key must satisfy `known`; otherwise this fails with
    /// [`NetError::UnknownPeer`] and no application frame is read.
    pub async fn accept(
        stream: TcpStream,
        key: &TransportKey,
        known: impl Fn(&[u8; 32]) -> bool,
    ) -> Result<Link, NetError> {
        tokio::time::timeout(HANDSHAKE_TIMEOUT, Self::accept_inner(stream, key, known))
            .await
            .map_err(|_| NetError::Timeout("handshake"))?
    }

    async fn accept_inner(
        mut stream: TcpStream,
        key: &TransportKey,
        known: impl Fn(&[u8; 32]) -> bool,
    ) -> Result<Link, NetError> {
        let _ = stream.set_nodelay(true);
        let mut hs = builder(key)?.build_responder().map_err(handshake_err)?;
        let mut msg = vec![0u8; MAX_NOISE_MSG];
        let mut plain = vec![0u8; MAX_NOISE_MSG];

        // <- e
        let n = read_msg(&mut stream, &mut msg).await?;
        hs.read_message(&msg[..n], &mut plain)
            .map_err(handshake_err)?;
        // -> e, ee, s, es
        let n = hs.write_message(&[], &mut msg).map_err(handshake_err)?;
        write_msg(&mut stream, &msg[..n]).await?;
        // <- s, se
        let n = read_msg(&mut stream, &mut msg).await?;
        hs.read_message(&msg[..n], &mut plain)
            .map_err(handshake_err)?;

        let remote = remote_key(&hs)?;
        if !known(&remote) {
            return Err(NetError::UnknownPeer {
                fingerprint: fingerprint(&remote),
            });
        }
        let transport = hs.into_transport_mode().map_err(handshake_err)?;
        Ok(Link {
            stream,
            transport,
            remote,
            msg,
            plain,
        })
    }

    /// The authenticated remote static (X25519) key.
    pub fn remote_static(&self) -> [u8; 32] {
        self.remote
    }

    /// Send one application frame of at most [`MAX_FRAME`] bytes.
    pub async fn send(&mut self, payload: &[u8]) -> Result<(), NetError> {
        if payload.len() > MAX_FRAME {
            return Err(NetError::Frame(format!(
                "payload of {} bytes exceeds the {MAX_FRAME}-byte cap",
                payload.len()
            )));
        }
        let mut plain = Vec::with_capacity(LEN_PREFIX + payload.len());
        plain.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        plain.extend_from_slice(payload);
        for chunk in plain.chunks(MAX_CHUNK) {
            self.write_chunk(chunk).await?;
        }
        Ok(())
    }

    /// Encrypt `plain` (at most `MAX_CHUNK` bytes) as one Noise transport
    /// message and write it.
    pub(crate) async fn write_chunk(&mut self, plain: &[u8]) -> Result<(), NetError> {
        let n = self
            .transport
            .write_message(plain, &mut self.msg)
            .map_err(|e| NetError::Frame(format!("encrypt: {e}")))?;
        write_msg(&mut self.stream, &self.msg[..n]).await
    }

    /// Read and decrypt one chunk into `self.plain`; returns its length.
    async fn read_chunk(&mut self) -> Result<usize, NetError> {
        let n = read_msg(&mut self.stream, &mut self.msg).await?;
        self.transport
            .read_message(&self.msg[..n], &mut self.plain)
            .map_err(|_| NetError::Frame("chunk failed to decrypt".into()))
    }

    /// Receive one application frame. No timeout of its own; callers wrap it.
    /// An announced length over [`MAX_FRAME`] is refused before any of the
    /// payload is read or allocated.
    pub async fn recv(&mut self) -> Result<Vec<u8>, NetError> {
        let n = self.read_chunk().await?;
        if n < LEN_PREFIX {
            return Err(NetError::Frame(
                "first chunk is shorter than the length prefix".into(),
            ));
        }
        let mut len = [0u8; LEN_PREFIX];
        len.copy_from_slice(&self.plain[..LEN_PREFIX]);
        let len = u32::from_be_bytes(len) as usize;
        if len > MAX_FRAME {
            return Err(NetError::Frame(format!(
                "announced frame of {len} bytes exceeds the {MAX_FRAME}-byte cap"
            )));
        }
        let mut out = Vec::with_capacity(len);
        out.extend_from_slice(&self.plain[LEN_PREFIX..n]);
        while out.len() < len {
            let n = self.read_chunk().await?;
            if n == 0 {
                return Err(NetError::Frame("empty continuation chunk".into()));
            }
            out.extend_from_slice(&self.plain[..n]);
        }
        if out.len() != len {
            return Err(NetError::Frame(
                "frame is longer than its announced length".into(),
            ));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card::fingerprint;
    use crate::key::TransportKey;
    use crate::{MAX_FRAME, NetError};
    use tokio::io::AsyncWriteExt;
    use tokio::net::{TcpListener, TcpStream};

    async fn listener() -> (TcpListener, String) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        (l, addr)
    }

    #[tokio::test]
    async fn paired_keys_complete_the_handshake_and_carry_a_frame() {
        let server = TransportKey::generate().unwrap();
        let client = TransportKey::generate().unwrap();
        let (l, addr) = listener().await;
        let cpub = client.public();
        let spub = server.public();
        let srv = tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let mut link = Link::accept(s, &server, move |k| *k == cpub).await.unwrap();
            assert_eq!(link.remote_static(), cpub);
            assert_eq!(link.recv().await.unwrap(), b"hello");
            link.send(b"world").await.unwrap();
        });
        let mut link = Link::connect(&addr, &client, spub).await.unwrap();
        assert_eq!(link.remote_static(), spub);
        link.send(b"hello").await.unwrap();
        assert_eq!(link.recv().await.unwrap(), b"world");
        srv.await.unwrap();
    }

    #[tokio::test]
    async fn an_unknown_client_key_is_refused_with_its_fingerprint() {
        let server = TransportKey::generate().unwrap();
        let client = TransportKey::generate().unwrap();
        let (l, addr) = listener().await;
        let spub = server.public();
        let expected = fingerprint(&client.public());
        let srv = tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            Link::accept(s, &server, |_| false).await
        });
        // The client may or may not see the refusal; only the server's verdict matters.
        let _client_side = Link::connect(&addr, &client, spub).await;
        match srv.await.unwrap() {
            Err(NetError::UnknownPeer { fingerprint }) => assert_eq!(fingerprint, expected),
            other => panic!("expected UnknownPeer, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_server_with_the_wrong_key_is_refused_by_the_client() {
        let server = TransportKey::generate().unwrap();
        let client = TransportKey::generate().unwrap();
        let other = TransportKey::generate().unwrap();
        let (l, addr) = listener().await;
        let spub = server.public();
        let srv = tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let _ = Link::accept(s, &server, |_| true).await;
        });
        match Link::connect(&addr, &client, other.public()).await {
            Err(NetError::UnknownPeer { fingerprint: f }) => {
                assert_eq!(f, fingerprint(&spub));
            }
            other => panic!("expected UnknownPeer, got {other:?}"),
        }
        srv.await.unwrap();
    }

    #[tokio::test]
    async fn a_client_that_rejects_msg2_never_sends_msg3() {
        use tokio::io::AsyncReadExt;
        let server = TransportKey::generate().unwrap();
        let client = TransportKey::generate().unwrap();
        let other = TransportKey::generate().unwrap();
        let (l, addr) = listener().await;
        // A hand-driven responder: read msg1, write msg2, then watch for msg3.
        let srv = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut hs = builder(&server).unwrap().build_responder().unwrap();
            let mut msg = vec![0u8; MAX_NOISE_MSG];
            let mut plain = vec![0u8; MAX_NOISE_MSG];
            let n = read_msg(&mut s, &mut msg).await.unwrap();
            hs.read_message(&msg[..n], &mut plain).unwrap();
            let n = hs.write_message(&[], &mut msg).unwrap();
            write_msg(&mut s, &msg[..n]).await.unwrap();
            let mut rest = Vec::new();
            let read = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut rest))
                .await
                .expect("the client closes instead of hanging");
            // EOF (or a reset) with nothing after msg2: msg3 was never sent.
            assert!(
                read.is_err() || rest.is_empty(),
                "client sent {} bytes after msg2",
                rest.len()
            );
        });
        let res = Link::connect(&addr, &client, other.public()).await;
        assert!(matches!(res, Err(NetError::UnknownPeer { .. })), "{res:?}");
        srv.await.unwrap();
    }

    #[tokio::test]
    async fn a_frame_over_the_cap_is_refused() {
        let server = TransportKey::generate().unwrap();
        let client = TransportKey::generate().unwrap();
        let (l, addr) = listener().await;
        let spub = server.public();
        let srv = tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let mut link = Link::accept(s, &server, |_| true).await.unwrap();
            link.recv().await
        });
        let mut link = Link::connect(&addr, &client, spub).await.unwrap();
        assert!(matches!(
            link.send(&vec![0u8; MAX_FRAME + 1]).await,
            Err(NetError::Frame(_))
        ));
        // A peer that announces an over-cap length is refused before the payload is read.
        let announced = (MAX_FRAME as u32 + 1).to_be_bytes();
        link.write_chunk(&announced).await.unwrap();
        assert!(matches!(srv.await.unwrap(), Err(NetError::Frame(_))));
    }

    #[tokio::test]
    async fn a_one_mib_frame_round_trips() {
        let server = TransportKey::generate().unwrap();
        let client = TransportKey::generate().unwrap();
        let (l, addr) = listener().await;
        let spub = server.public();
        let payload: Vec<u8> = (0..MAX_FRAME).map(|i| (i % 251) as u8).collect();
        let expect = payload.clone();
        let srv = tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let mut link = Link::accept(s, &server, |_| true).await.unwrap();
            let got = link.recv().await.unwrap();
            link.send(&got).await.unwrap();
        });
        let mut link = Link::connect(&addr, &client, spub).await.unwrap();
        link.send(&payload).await.unwrap();
        assert_eq!(link.recv().await.unwrap(), expect);
        srv.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_client_times_out_the_handshake() {
        let server = TransportKey::generate().unwrap();
        let (l, addr) = listener().await;
        let _silent = TcpStream::connect(&addr).await.unwrap();
        let (s, _) = l.accept().await.unwrap();
        let start = tokio::time::Instant::now();
        let res = Link::accept(s, &server, |_| true).await;
        assert!(
            matches!(res, Err(NetError::Timeout("handshake"))),
            "{res:?}"
        );
        assert!(start.elapsed() <= HANDSHAKE_TIMEOUT + Duration::from_secs(2));
    }

    #[tokio::test]
    async fn truncated_or_random_bytes_never_panic() {
        use rand::{Rng, RngCore};
        let server = TransportKey::generate().unwrap();
        let (l, addr) = listener().await;
        for _ in 0..50 {
            let len = rand::thread_rng().gen_range(1..2048);
            let mut junk = vec![0u8; len];
            rand::thread_rng().fill_bytes(&mut junk);
            let mut raw = TcpStream::connect(&addr).await.unwrap();
            let (s, _) = l.accept().await.unwrap();
            let _ = raw.write_all(&junk).await;
            let _ = raw.shutdown().await;
            let res = Link::accept(s, &server, |_| true).await;
            assert!(res.is_err());
            drop(raw);
        }
    }
}
