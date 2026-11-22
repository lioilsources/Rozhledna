//! Node identity (plan 6.3): Ed25519 key + self-signed certificate, stored as files with 0600 permissions.
//!
//! The fingerprint is SHA-256 of the certificate DER. `node_id` (Hello, mDNS `id=`) is its first 16 bytes,
//! so there is no separate UUID to keep in sync with the key.

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};

/// SHA-256 of a certificate DER.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Fingerprint(pub [u8; 32]);

impl Fingerprint {
    pub fn of_cert(der: &[u8]) -> Self {
        Self(Sha256::digest(der).into())
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// First 16 hex characters (mDNS `fp=`, logs).
    pub fn short(&self) -> String {
        self.to_hex()[..16].to_string()
    }

    pub fn node_id(&self) -> [u8; 16] {
        self.0[..16].try_into().unwrap()
    }

    /// Parses `sha256:<64 hex>` or bare `<64 hex>`.
    pub fn parse(s: &str) -> Result<Self> {
        let h = s.trim().strip_prefix("sha256:").unwrap_or(s.trim());
        let v = hex::decode(h).context("fingerprint: not hex")?;
        let a: [u8; 32] = v.try_into().map_err(|_| anyhow::anyhow!("fingerprint: expected 32 bytes"))?;
        Ok(Self(a))
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", self.to_hex())
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({})", self.short())
    }
}

pub struct Identity {
    pub cert: CertificateDer<'static>,
    pub key: PrivatePkcs8KeyDer<'static>,
    pub fingerprint: Fingerprint,
}

const CERT_FILE: &str = "identity.cert.der";
const KEY_FILE: &str = "identity.key.der";

impl Identity {
    /// New ephemeral identity (tests, Phase 0 spikes).
    pub fn generate() -> Result<Self> {
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519)?;
        let params = rcgen::CertificateParams::new(vec!["rozhledna".into()])?;
        let cert = params.self_signed(&key)?;
        Ok(Self::from_parts(cert.der().to_vec(), key.serialize_der()))
    }

    fn from_parts(cert: Vec<u8>, key: Vec<u8>) -> Self {
        let fingerprint = Fingerprint::of_cert(&cert);
        Self { cert: CertificateDer::from(cert), key: PrivatePkcs8KeyDer::from(key), fingerprint }
    }

    /// Loads the identity from `dir`, or creates it (dir 0700, files 0600) on first start.
    pub fn load_or_create(dir: &Path) -> Result<Self> {
        let (cp, kp) = (dir.join(CERT_FILE), dir.join(KEY_FILE));
        match (cp.exists(), kp.exists()) {
            (true, true) => {
                check_private(&kp)?;
                let cert = std::fs::read(&cp).with_context(|| format!("read {}", cp.display()))?;
                let key = std::fs::read(&kp).with_context(|| format!("read {}", kp.display()))?;
                Ok(Self::from_parts(cert, key))
            }
            (false, false) => {
                create_private_dir(dir)?;
                let id = Self::generate()?;
                write_private(&kp, id.key.secret_pkcs8_der())?;
                write_private(&cp, id.cert.as_ref())?;
                tracing::info!(fp = %id.fingerprint.short(), "created identity in {}", dir.display());
                Ok(id)
            }
            _ => bail!("incomplete identity in {} (one of {CERT_FILE}/{KEY_FILE} missing)", dir.display()),
        }
    }
}

/// `~/Library/Application Support/Rozhledna/<role>` on macOS, `$XDG_CONFIG_HOME/rozhledna/<role>` elsewhere.
/// `RZ_STATE_DIR` overrides the base (tests, several instances on one machine).
pub fn default_dir(role: &str) -> Result<PathBuf> {
    if let Some(d) = std::env::var_os("RZ_STATE_DIR") {
        return Ok(PathBuf::from(d).join(role));
    }
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME not set")?);
    let base = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Rozhledna")
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or(home.join(".config")).join("rozhledna")
    };
    Ok(base.join(role))
}

pub(crate) fn create_private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Writes atomically (temp file + rename) with mode 0600.
pub(crate) fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(&tmp).with_context(|| format!("create {}", tmp.display()))?;
    f.write_all(data)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

fn check_private(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)?.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            bail!("{} has mode {mode:o}; the private key must be 0600", path.display());
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn test_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rz-test-{name}-{}-{}", std::process::id(), rz_proto::clock::now_ns()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persist_and_reload() -> Result<()> {
        let d = test_dir("identity");
        let a = Identity::load_or_create(&d)?;
        let b = Identity::load_or_create(&d)?;
        assert_eq!(a.fingerprint, b.fingerprint);
        assert_eq!(Fingerprint::parse(&a.fingerprint.to_string())?, a.fingerprint);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(d.join(KEY_FILE))?.permissions().mode() & 0o777, 0o600);
            std::fs::set_permissions(d.join(KEY_FILE), std::fs::Permissions::from_mode(0o644))?;
            assert!(Identity::load_or_create(&d).is_err(), "world-readable key must be refused");
        }
        std::fs::remove_dir_all(&d)?;
        Ok(())
    }
}
