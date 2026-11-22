//! mDNS/DNS-SD discovery (plan 6.2): service `_rozhledna._udp.local.`, TXT `id name os v fp roles`.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};

use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::identity::Fingerprint;
use crate::lan::is_lan;

pub const SERVICE_TYPE: &str = "_rozhledna._udp.local.";

/// What a node announces.
#[derive(Clone, Debug)]
pub struct Advert {
    pub name: String,
    pub fingerprint: Fingerprint,
    pub port: u16,
    pub roles: Vec<&'static str>,
}

/// A node seen on the network.
#[derive(Clone, Debug, PartialEq)]
pub struct Peer {
    /// mDNS full name (key for `Lost`).
    pub fullname: String,
    pub name: String,
    pub id: String,
    pub os: String,
    pub protocol: String,
    /// First 16 hex of the certificate fingerprint; full check happens in TLS + handshake.
    pub fp_short: String,
    pub roles: Vec<String>,
    /// LAN addresses only (plan 6.8), sorted.
    pub addrs: Vec<SocketAddr>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Discovered {
    Found(Peer),
    Lost { fullname: String },
}

fn os_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    }
}

pub struct Discovery {
    daemon: ServiceDaemon,
    registered: Option<String>,
}

impl Discovery {
    pub fn new() -> Result<Self> {
        Ok(Self { daemon: ServiceDaemon::new().context("start mDNS daemon")?, registered: None })
    }

    /// Announces this node. The instance name is `<name>-<fp_short>` so two machines with the same hostname differ.
    pub fn advertise(&mut self, ad: &Advert) -> Result<()> {
        let host = hostname();
        let instance = format!("{}-{}", ad.name, &ad.fingerprint.short()[..8]);
        let id = hex::encode(ad.fingerprint.node_id());
        let v = rz_proto::PROTOCOL_VERSION.to_string();
        let fp = ad.fingerprint.short();
        let roles = ad.roles.join(",");
        let props = [
            ("id", id.as_str()),
            ("name", ad.name.as_str()),
            ("os", os_name()),
            ("v", v.as_str()),
            ("fp", fp.as_str()),
            ("roles", roles.as_str()),
        ];
        let info = ServiceInfo::new(SERVICE_TYPE, &instance, &format!("{host}.local."), "", ad.port, &props[..])?
            .enable_addr_auto();
        let fullname = info.get_fullname().to_string();
        self.daemon.register(info)?;
        tracing::info!(%fullname, port = ad.port, "mDNS advertised");
        self.registered = Some(fullname);
        Ok(())
    }

    /// Browses for peers. Events arrive on a tokio channel; a background thread pumps the mDNS receiver.
    pub fn browse(&self) -> Result<tokio::sync::mpsc::UnboundedReceiver<Discovered>> {
        let rx = self.daemon.browse(SERVICE_TYPE)?;
        let (tx, out) = tokio::sync::mpsc::unbounded_channel();
        let own = self.registered.clone();
        std::thread::Builder::new().name("rz-mdns".into()).spawn(move || {
            let mut seen: HashMap<String, Peer> = HashMap::new();
            while let Ok(ev) = rx.recv() {
                let d = match ev {
                    ServiceEvent::ServiceResolved(info) => {
                        let fullname = info.get_fullname().to_string();
                        if own.as_deref() == Some(fullname.as_str()) {
                            continue;
                        }
                        let port = info.get_port();
                        let mut addrs: Vec<SocketAddr> = info
                            .get_addresses_v4()
                            .into_iter()
                            .map(|ip| SocketAddr::new(IpAddr::V4(ip), port))
                            .filter(|a| is_lan(a.ip()))
                            .collect();
                        addrs.sort();
                        let p = |k: &str| info.get_property_val_str(k).unwrap_or_default().to_string();
                        let peer = Peer {
                            fullname: fullname.clone(),
                            name: p("name"),
                            id: p("id"),
                            os: p("os"),
                            protocol: p("v"),
                            fp_short: p("fp"),
                            roles: p("roles").split(',').filter(|s| !s.is_empty()).map(String::from).collect(),
                            addrs,
                        };
                        if peer.addrs.is_empty() || seen.get(&fullname) == Some(&peer) {
                            continue;
                        }
                        seen.insert(fullname, peer.clone());
                        Discovered::Found(peer)
                    }
                    ServiceEvent::ServiceRemoved(_, fullname) => {
                        if seen.remove(&fullname).is_none() {
                            continue;
                        }
                        Discovered::Lost { fullname }
                    }
                    _ => continue,
                };
                if tx.send(d).is_err() {
                    break;
                }
            }
        })?;
        Ok(out)
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        if let Some(f) = self.registered.take() {
            let _ = self.daemon.unregister(&f);
        }
        let _ = self.daemon.shutdown();
    }
}

/// Short host name (without `.local`).
pub fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: buf is valid for its length; gethostname NUL-terminates on success.
    let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
    let s = if ok {
        let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..n]).into_owned()
    } else {
        "rozhledna".into()
    };
    s.trim_end_matches(".local").split('.').next().unwrap_or("rozhledna").to_string()
}
