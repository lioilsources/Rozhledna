//! Trust store (plan 6.3): fingerprints of paired peers, JSON file with mode 0600.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::identity::{Fingerprint, create_private_dir, write_private};

/// Permissions a hub gets on an agent (plan 6.3). Default: everything (D9).
pub const ALL_GRANTS: &[&str] = &["view", "control", "clipboard", "window_control"];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TrustedPeer {
    /// `sha256:<hex>`
    pub fingerprint: String,
    pub name: String,
    /// Agent side: what this hub may do. Hub side: empty.
    #[serde(default)]
    pub granted: Vec<String>,
    pub added_unix: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    peers: Vec<TrustedPeer>,
}

pub struct TrustStore {
    path: Option<PathBuf>,
    peers: Vec<TrustedPeer>,
}

impl TrustStore {
    /// In-memory store (tests, `--insecure-dev`).
    pub fn memory() -> Self {
        Self { path: None, peers: Vec::new() }
    }

    /// Loads `dir/trusted.json`; a missing file is an empty store.
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join("trusted.json");
        let peers = match std::fs::read(&path) {
            Ok(b) => serde_json::from_slice::<File>(&b).with_context(|| format!("parse {}", path.display()))?.peers,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        };
        Ok(Self { path: Some(path), peers })
    }

    pub fn get(&self, fp: &Fingerprint) -> Option<&TrustedPeer> {
        let s = fp.to_string();
        self.peers.iter().find(|p| p.fingerprint == s)
    }

    pub fn peers(&self) -> &[TrustedPeer] {
        &self.peers
    }

    /// Adds or replaces the peer and saves.
    pub fn insert(&mut self, fp: &Fingerprint, name: &str, granted: &[String]) -> Result<()> {
        self.remove_no_save(fp);
        let added_unix =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        self.peers.push(TrustedPeer {
            fingerprint: fp.to_string(),
            name: name.into(),
            granted: granted.to_vec(),
            added_unix,
        });
        self.save()
    }

    /// Removes the peer and saves; returns whether it was there.
    pub fn remove(&mut self, fp: &Fingerprint) -> Result<bool> {
        let had = self.remove_no_save(fp);
        self.save()?;
        Ok(had)
    }

    fn remove_no_save(&mut self, fp: &Fingerprint) -> bool {
        let s = fp.to_string();
        let n = self.peers.len();
        self.peers.retain(|p| p.fingerprint != s);
        self.peers.len() != n
    }

    fn save(&self) -> Result<()> {
        let Some(path) = &self.path else { return Ok(()) };
        if let Some(d) = path.parent() {
            create_private_dir(d)?;
        }
        let json = serde_json::to_vec_pretty(&File { peers: self.peers.clone() })?;
        write_private(path, &json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_reload_remove() -> Result<()> {
        let d = crate::identity::test_dir("trust");
        let fp = Fingerprint([7; 32]);
        let mut t = TrustStore::load(&d)?;
        assert!(t.get(&fp).is_none());
        t.insert(&fp, "hub-a", &["view".into()])?;
        t.insert(&fp, "hub-a2", &["view".into(), "control".into()])?; // replace, not duplicate
        let t2 = TrustStore::load(&d)?;
        assert_eq!(t2.peers().len(), 1);
        assert_eq!(t2.get(&fp).unwrap().name, "hub-a2");
        let mut t3 = t2;
        assert!(t3.remove(&fp)?);
        assert!(TrustStore::load(&d)?.get(&fp).is_none());
        std::fs::remove_dir_all(&d)?;
        Ok(())
    }
}
