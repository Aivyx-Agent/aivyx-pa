//! The client side of the federation link: [`ping`] and [`request`].

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use aivyx_federation::identity::Identity;
use aivyx_ipc::federation::RelayRequest;

use crate::NetError;
use crate::card::PeerCard;
use crate::frame::{RequestFrame, ResponseFrame};
use crate::key::TransportKey;
use crate::link::Link;

/// How long [`ping`] waits for the empty reply.
const PING_REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// Connect, complete the handshake against `peer_x25519`, send an empty
/// frame and expect an empty frame back. Proves reachability and key match.
pub async fn ping(addr: &str, key: &TransportKey, peer_x25519: [u8; 32]) -> Result<(), NetError> {
    let mut link = Link::connect(addr, key, peer_x25519).await?;
    link.send(&[]).await?;
    let reply = tokio::time::timeout(PING_REPLY_TIMEOUT, link.recv())
        .await
        .map_err(|_| NetError::Timeout("reply"))??;
    if !reply.is_empty() {
        return Err(NetError::Frame("ping reply was not empty".into()));
    }
    Ok(())
}

/// Send one signed relay request to `peer` and wait up to `reply_timeout`
/// for its [`ResponseFrame`]. The card is verified first; the link only
/// completes if the remote presents the card's transport key.
pub async fn request(
    addr: &str,
    key: &TransportKey,
    peer: &PeerCard,
    identity: &Identity,
    req: &RelayRequest,
    reply_timeout: Duration,
) -> Result<ResponseFrame, NetError> {
    let x = peer.verify()?;
    let mut link = Link::connect(addr, key, x).await?;
    let (header, body) = identity
        .sign_relay(&peer.instance_id, req)
        .await
        .map_err(|e| NetError::Key(format!("sign request: {e}")))?;
    let frame = RequestFrame {
        header,
        body: STANDARD.encode(&body),
    };
    let bytes =
        serde_json::to_vec(&frame).map_err(|e| NetError::Frame(format!("encode request: {e}")))?;
    link.send(&bytes).await?;
    let reply = tokio::time::timeout(reply_timeout, link.recv())
        .await
        .map_err(|_| NetError::Timeout("reply"))??;
    serde_json::from_slice(&reply).map_err(|e| NetError::Frame(format!("decode response: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card::PeerCard;
    use crate::frame::{RequestFrame, ResponseFrame};
    use crate::key::TransportKey;
    use crate::link::Link;
    use aivyx_federation::identity::Identity;
    use aivyx_ipc::federation::{RelayRequest, RelayResponse};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn ping_round_trips_an_empty_frame() {
        let server = TransportKey::generate().unwrap();
        let client = TransportKey::generate().unwrap();
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let spub = server.public();
        let srv = tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let mut link = Link::accept(s, &server, |_| true).await.unwrap();
            assert!(link.recv().await.unwrap().is_empty());
            link.send(&[]).await.unwrap();
        });
        ping(&addr, &client, spub).await.unwrap();
        srv.await.unwrap();
    }

    #[tokio::test]
    async fn request_sends_a_verifiable_signed_frame() {
        let server_key = TransportKey::generate().unwrap();
        let server_id = Identity::generate("desktop".into()).unwrap();
        let card = PeerCard::issue(&server_id, &server_key).await.unwrap();
        let client_key = TransportKey::generate().unwrap();
        let client_id = Identity::generate("laptop".into()).unwrap();
        let client_pub = client_id.public_key_base64();
        let req = RelayRequest::Chat {
            message: "hello".into(),
        };
        let expect = req.clone();

        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let srv = tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let mut link = Link::accept(s, &server_key, |_| true).await.unwrap();
            let raw = link.recv().await.unwrap();
            let frame: RequestFrame = serde_json::from_slice(&raw).unwrap();
            let body = base64::engine::general_purpose::STANDARD
                .decode(&frame.body)
                .unwrap();
            Identity::verify_request(&client_pub, "desktop", &frame.header, &body).unwrap();
            let got: RelayRequest = serde_json::from_slice(&body).unwrap();
            assert_eq!(got, expect);
            let reply = ResponseFrame::Ok {
                response: RelayResponse::Chat { reply: "hi".into() },
            };
            link.send(&serde_json::to_vec(&reply).unwrap())
                .await
                .unwrap();
        });
        let resp = request(
            &addr,
            &client_key,
            &card,
            &client_id,
            &req,
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        match resp {
            ResponseFrame::Ok {
                response: RelayResponse::Chat { reply },
            } => assert_eq!(reply, "hi"),
            other => panic!("unexpected {other:?}"),
        }
        srv.await.unwrap();
    }
}
