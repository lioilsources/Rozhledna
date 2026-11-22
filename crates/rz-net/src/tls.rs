//! TLS setup. Phase 0: ephemeral self-signed Ed25519 cert, client accepts anything (dev only).

use std::sync::Arc;

use anyhow::Result;
use rustls::DigitallySignedStruct;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};

pub struct DevIdentity {
    pub cert: CertificateDer<'static>,
    pub key: PrivatePkcs8KeyDer<'static>,
}

impl DevIdentity {
    pub fn generate() -> Result<Self> {
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519)?;
        let params = rcgen::CertificateParams::new(vec!["rozhledna".into()])?;
        let cert = params.self_signed(&key)?;
        Ok(Self { cert: cert.der().clone(), key: PrivatePkcs8KeyDer::from(key.serialize_der()) })
    }

    /// Short hex fingerprint for logs (first 16 hex chars of a non-cryptographic digest in Phase 0).
    pub fn fingerprint_hex(&self) -> String {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.cert.as_ref().hash(&mut h);
        format!("{:016x}", h.finish())
    }
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

pub fn server_crypto(id: &DevIdentity) -> Result<rustls::ServerConfig> {
    let mut cfg = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(vec![id.cert.clone()], PrivateKeyDer::Pkcs8(id.key.clone_key()))?;
    cfg.alpn_protocols = vec![rz_proto::ALPN.to_vec()];
    Ok(cfg)
}

pub fn insecure_client_crypto() -> Result<rustls::ClientConfig> {
    let p = provider();
    let mut cfg = rustls::ClientConfig::builder_with_provider(p.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAny(p)))
        .with_no_client_auth();
    cfg.alpn_protocols = vec![rz_proto::ALPN.to_vec()];
    Ok(cfg)
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
