//! TLS setup (plan 6.3). Both sides present a self-signed certificate (mutual TLS 1.3). The verifiers accept any
//! certificate but still check the handshake signature, so the peer provably holds the key of the fingerprint it
//! shows. Whether that fingerprint is trusted is decided by the application layer (`handshake`), not by TLS.

use std::sync::Arc;

use anyhow::Result;
use rustls::DigitallySignedStruct;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};

pub use crate::identity::Identity;

/// Phase 0 name; the same type as [`Identity`].
pub type DevIdentity = Identity;

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Server without client authentication (Phase 0 spikes, `--insecure-dev`).
pub fn server_crypto(id: &Identity) -> Result<rustls::ServerConfig> {
    let mut cfg = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(vec![id.cert.clone()], PrivateKeyDer::Pkcs8(id.key.clone_key()))?;
    cfg.alpn_protocols = vec![rz_proto::ALPN.to_vec()];
    Ok(cfg)
}

/// Server that requires a client certificate (agent in Phase 1+).
pub fn server_crypto_mtls(id: &Identity) -> Result<rustls::ServerConfig> {
    let p = provider();
    let mut cfg = rustls::ServerConfig::builder_with_provider(p.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(Arc::new(AcceptAny(p)))
        .with_single_cert(vec![id.cert.clone()], PrivateKeyDer::Pkcs8(id.key.clone_key()))?;
    cfg.alpn_protocols = vec![rz_proto::ALPN.to_vec()];
    Ok(cfg)
}

/// Client accepting any server certificate; presents `id` as client certificate when given.
pub fn client_crypto(id: Option<&Identity>) -> Result<rustls::ClientConfig> {
    let p = provider();
    let b = rustls::ClientConfig::builder_with_provider(p.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAny(p)));
    let mut cfg = match id {
        Some(id) => b.with_client_auth_cert(vec![id.cert.clone()], PrivateKeyDer::Pkcs8(id.key.clone_key()))?,
        None => b.with_no_client_auth(),
    };
    cfg.alpn_protocols = vec![rz_proto::ALPN.to_vec()];
    Ok(cfg)
}

pub fn insecure_client_crypto() -> Result<rustls::ClientConfig> {
    client_crypto(None)
}

/// Accepts any certificate but still checks the handshake signature against it.
#[derive(Debug)]
struct AcceptAny(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 not supported".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

impl ClientCertVerifier for AcceptAny {
    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 not supported".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
