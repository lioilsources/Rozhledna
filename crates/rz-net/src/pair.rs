//! SAS pairing code (plan 6.3).
//!
//! `SAS = 6 digits of SHA-256(min(fpA, fpB) || max(fpA, fpB) || TLS exporter("rozhledna-pair", 32 B))`.
//! The exporter binds the code to this TLS session: a man in the middle has two different sessions, so the two
//! screens show different codes.

use anyhow::{Context, Result};
use quinn::Connection;
use rustls::pki_types::CertificateDer;
use sha2::{Digest, Sha256};

use crate::identity::Fingerprint;

pub const EXPORTER_LABEL: &[u8] = b"rozhledna-pair";

/// Fingerprint of the certificate the peer presented in the TLS handshake.
pub fn peer_fingerprint(conn: &Connection) -> Result<Fingerprint> {
    let any = conn.peer_identity().context("peer presented no certificate")?;
    let certs =
        any.downcast::<Vec<CertificateDer<'static>>>().map_err(|_| anyhow::anyhow!("unexpected peer identity type"))?;
    let leaf = certs.first().context("empty peer certificate chain")?;
    Ok(Fingerprint::of_cert(leaf.as_ref()))
}

pub fn sas(a: &Fingerprint, b: &Fingerprint, exporter: &[u8; 32]) -> String {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    let mut h = Sha256::new();
    h.update(lo.0);
    h.update(hi.0);
    h.update(exporter);
    let d = h.finalize();
    let n = u32::from_be_bytes([d[0], d[1], d[2], d[3]]) % 1_000_000;
    format!("{n:06}")
}

/// SAS for this connection; both ends compute the same value.
pub fn connection_sas(conn: &Connection, local: &Fingerprint) -> Result<String> {
    let peer = peer_fingerprint(conn)?;
    let mut ex = [0u8; 32];
    conn.export_keying_material(&mut ex, EXPORTER_LABEL, b"").map_err(|e| anyhow::anyhow!("TLS exporter: {e:?}"))?;
    Ok(sas(local, &peer, &ex))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symmetric_and_six_digits() {
        let (a, b) = (Fingerprint([1; 32]), Fingerprint([2; 32]));
        let ex = [9u8; 32];
        let s = sas(&a, &b, &ex);
        assert_eq!(s, sas(&b, &a, &ex));
        assert_eq!(s.len(), 6);
        assert!(s.chars().all(|c| c.is_ascii_digit()));
        assert_ne!(s, sas(&a, &b, &[8u8; 32]), "different session → different code");
    }
}
