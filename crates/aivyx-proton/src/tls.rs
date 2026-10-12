//! TLS to Proton Mail Bridge, pinned to the certificate captured at
//! `aivyx-pa connect proton`.
//!
//! Bridge serves a self-signed certificate marked as a CA, which webpki
//! refuses as a server certificate (`CaUsedAsEndEntity`) — so it can't be
//! added as a trust root. Instead the SHA-256 of the exact certificate is
//! pinned: any other certificate fails the handshake. The handshake
//! signature is still verified, so the peer must hold the pinned key.
//! Never system trust, never "accept anything" — except [`capturing`],
//! used once by `connect proton` to read the certificate it then pins.

use std::sync::{Arc, Mutex};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};
use tokio_rustls::TlsConnector;

/// In the handshake error when Bridge presents another certificate.
pub const PIN_MISMATCH: &str = "Proton Mail Bridge presented a different certificate";

pub type Pin = [u8; 32];

pub fn fingerprint(der: &[u8]) -> Pin {
    let mut pin = [0u8; 32];
    pin.copy_from_slice(Sha256::digest(der).as_slice());
    pin
}

pub fn pin_to_hex(pin: &Pin) -> String {
    hex::encode(pin)
}

pub fn pin_from_hex(s: &str) -> Result<Pin, String> {
    let bytes = hex::decode(s.trim()).map_err(|e| format!("bad certificate pin: {e}"))?;
    bytes
        .try_into()
        .map_err(|_| "bad certificate pin: expected 32 bytes".to_string())
}

/// The crypto provider the rest of pa uses (both are compiled in, so
/// rustls can't pick a process default on its own).
fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

#[derive(Debug)]
struct Verifier {
    /// `Some`: only this certificate. `None`: capture mode.
    pin: Option<Pin>,
    seen: Arc<Mutex<Option<Vec<u8>>>>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        *self.seen.lock().expect("seen mutex poisoned") = Some(end_entity.as_ref().to_vec());
        match self.pin {
            Some(pin) if fingerprint(end_entity.as_ref()) != pin => {
                Err(rustls::Error::General(PIN_MISMATCH.to_string()))
            }
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

fn connector(pin: Option<Pin>) -> (TlsConnector, Arc<Mutex<Option<Vec<u8>>>>) {
    let provider = provider();
    let seen = Arc::new(Mutex::new(None));
    let verifier = Verifier {
        pin,
        seen: Arc::clone(&seen),
        provider: Arc::clone(&provider),
    };
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("aws-lc-rs supports the default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    (TlsConnector::from(Arc::new(config)), seen)
}

/// Accepts only the certificate whose SHA-256 is `pin`.
pub fn pinned(pin: Pin) -> TlsConnector {
    connector(Some(pin)).0
}

/// Accepts any certificate and records it — only for `connect proton`,
/// which shows the operator the fingerprint and then pins it.
pub fn capturing() -> (TlsConnector, Arc<Mutex<Option<Vec<u8>>>>) {
    connector(None)
}

/// The host part of `host:port` as a TLS server name. The pin is what's
/// checked; the name only has to be well-formed.
pub fn server_name(addr: &str) -> Result<ServerName<'static>, String> {
    let host = match addr.rsplit_once(':') {
        Some((h, _)) => h,
        None => addr,
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    ServerName::try_from(host.to_string()).map_err(|e| format!("bad Bridge address {addr:?}: {e}"))
}

/// True when a connect error is a pin mismatch.
pub fn is_pin_mismatch(e: &std::io::Error) -> bool {
    e.to_string().contains(PIN_MISMATCH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A self-signed CA certificate for `127.0.0.1`, like Bridge's.
    const CERT: &[u8] = include_bytes!("../tests/fixtures/bridge-test-cert.der");
    const KEY: &[u8] = include_bytes!("../tests/fixtures/bridge-test-key.der");

    /// A TLS server on one end of an in-memory pipe; returns the other end.
    fn serve() -> tokio::io::DuplexStream {
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KEY.to_vec()));
        let config = rustls::ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![CertificateDer::from(CERT.to_vec())], key)
            .unwrap();
        let (client, server) = tokio::io::duplex(64 * 1024);
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        tokio::spawn(async move {
            if let Ok(mut tls) = acceptor.accept(server).await {
                let _ = tls.write_all(b"ok").await;
                let _ = tls.flush().await;
                let mut b = [0u8; 1];
                let _ = tls.read(&mut b).await;
            }
        });
        client
    }

    #[tokio::test]
    async fn the_pinned_certificate_connects() {
        let name = server_name("127.0.0.1:1143").unwrap();
        let mut tls = pinned(fingerprint(CERT)).connect(name, serve()).await.unwrap();
        let mut buf = [0u8; 2];
        tls.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ok");
    }

    #[tokio::test]
    async fn any_other_certificate_is_refused() {
        let name = server_name("127.0.0.1:1143").unwrap();
        let err = pinned([0u8; 32]).connect(name, serve()).await.unwrap_err();
        assert!(is_pin_mismatch(&err), "{err}");
    }

    #[tokio::test]
    async fn capture_records_the_certificate() {
        let (c, seen) = capturing();
        c.connect(server_name("localhost:1025").unwrap(), serve()).await.unwrap();
        let der = seen.lock().unwrap().clone().unwrap();
        assert_eq!(fingerprint(&der), fingerprint(CERT));
    }

    #[test]
    fn pins_round_trip_as_hex_and_names_parse() {
        let pin = fingerprint(CERT);
        assert_eq!(pin_from_hex(&pin_to_hex(&pin)).unwrap(), pin);
        assert!(pin_from_hex("abcd").is_err());
        assert!(server_name("[::1]:1143").is_ok());
        assert!(server_name("localhost:1025").is_ok());
    }
}
