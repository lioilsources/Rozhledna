//! Application-level authentication on the control stream (plan 6.3).
//!
//! TLS accepts any certificate; this layer decides. The connection is `UNPAIRED` until the peer fingerprint is in
//! the trust store.
//!
//! ```text
//! hub                                   agent
//!  ── Hello ───────────────────────────►  fp(hub) trusted?  yes → HelloAck{paired, granted}   (done)
//!                                         no  → HelloAck{paired:false}
//!  ── PairRequest{hub_name} ──────────►   both show SAS; agent's user confirms
//!  ◄─ PairResult{accepted, granted} ───   agent stores fp(hub); hub stores fp(agent)
//! ```
//! When the agent trusts the hub but the hub does not know the agent (e.g. reinstalled hub), the hub's user
//! confirms locally (`HubUi::confirm_agent`).

use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use quinn::Connection;
use rz_proto::pb::{self, envelope::Msg};

use crate::control::Control;
use crate::identity::Fingerprint;
use crate::pair::{connection_sas, peer_fingerprint};
use crate::trust::{ALL_GRANTS, TrustStore};

/// How long the agent waits for its user to confirm a pairing.
pub const PAIR_TIMEOUT: Duration = Duration::from_secs(120);

/// Who we are, for `Hello`.
#[derive(Clone, Debug)]
pub struct LocalInfo {
    pub name: String,
    pub is_agent: bool,
    pub is_hub: bool,
    pub features: Vec<String>,
}

impl LocalInfo {
    pub fn hello(&self, fp: &Fingerprint) -> pb::Hello {
        pb::Hello {
            protocol: rz_proto::PROTOCOL_VERSION,
            node_id: fp.node_id().to_vec(),
            name: self.name.clone(),
            os: if cfg!(target_os = "macos") {
                pb::Os::Macos
            } else if cfg!(target_os = "linux") {
                pb::Os::Linux
            } else if cfg!(target_os = "windows") {
                pb::Os::Windows
            } else {
                pb::Os::Unknown
            } as i32,
            os_version: String::new(),
            app_version: env!("CARGO_PKG_VERSION").into(),
            is_agent: self.is_agent,
            is_hub: self.is_hub,
            encode_codecs: if self.is_agent { vec![pb::Codec::H264 as i32] } else { vec![] },
            decode_codecs: if self.is_hub { vec![pb::Codec::H264 as i32] } else { vec![] },
            features: self.features.clone(),
        }
    }
}

/// A pairing request shown to the user.
#[derive(Clone, Debug)]
pub struct PairAsk {
    pub peer_name: String,
    pub peer_fp: Fingerprint,
    pub sas: String,
}

/// Agent UI: "Hub X wants to connect, code 123 456 — Allow?"
pub trait AgentUi: Send + Sync {
    fn confirm_hub(&self, ask: PairAsk) -> impl Future<Output = bool> + Send;
}

/// Hub UI.
pub trait HubUi: Send + Sync {
    /// The agent is asking its user; show the same code. Called once, non-blocking.
    fn show_code(&self, ask: PairAsk);
    /// The agent already trusts us but we do not know it: ask our user.
    fn confirm_agent(&self, ask: PairAsk) -> impl Future<Output = bool> + Send;
}

/// Result of a successful handshake.
#[derive(Clone, Debug)]
pub struct Authenticated {
    pub peer: pb::Hello,
    pub peer_fp: Fingerprint,
    /// Permissions of the hub on the agent.
    pub granted: Vec<String>,
    /// True when pairing happened in this handshake (first contact).
    pub newly_paired: bool,
}

async fn recv_msg(ctl: &mut Control) -> Result<(u64, Msg)> {
    let env = ctl.recv().await?.context("peer closed the control stream")?;
    Ok((env.seq, env.msg.context("empty envelope")?))
}

fn check_protocol(h: &pb::Hello) -> Result<()> {
    if h.protocol != rz_proto::PROTOCOL_VERSION {
        bail!("protocol {} not supported (we speak {})", h.protocol, rz_proto::PROTOCOL_VERSION);
    }
    Ok(())
}

/// Agent side: expects `Hello` as the first message.
pub async fn agent_handshake(
    ctl: &mut Control,
    conn: &Connection,
    local: &LocalInfo,
    local_fp: &Fingerprint,
    trust: &Mutex<TrustStore>,
    ui: &impl AgentUi,
) -> Result<Authenticated> {
    let (seq, msg) = recv_msg(ctl).await?;
    let Msg::Hello(peer) = msg else { bail!("expected Hello first") };
    if let Err(e) = check_protocol(&peer) {
        ctl.reply(seq, Msg::Error(pb::Error { code: 1, message: e.to_string() })).await?;
        return Err(e);
    }
    let peer_fp = peer_fingerprint(conn)?;
    let me = Some(local.hello(local_fp));
    let known = trust.lock().unwrap().get(&peer_fp).map(|p| p.granted.clone());
    if let Some(granted) = known {
        ctl.reply(seq, Msg::HelloAck(pb::HelloAck { paired: true, granted: granted.clone(), agent: me })).await?;
        return Ok(Authenticated { peer, peer_fp, granted, newly_paired: false });
    }
    ctl.reply(seq, Msg::HelloAck(pb::HelloAck { paired: false, granted: vec![], agent: me })).await?;

    let (seq, msg) = recv_msg(ctl).await?;
    let Msg::PairRequest(req) = msg else {
        bail!("unpaired hub {} sent {msg:?} instead of PairRequest", peer_fp.short())
    };
    let sas = connection_sas(conn, local_fp)?;
    let name = if req.hub_name.is_empty() { peer.name.clone() } else { req.hub_name.clone() };
    let ask = PairAsk { peer_name: name.clone(), peer_fp, sas };
    let ok = tokio::time::timeout(PAIR_TIMEOUT, ui.confirm_hub(ask)).await.unwrap_or(false);
    if !ok {
        let r = pb::PairResult { accepted: false, granted: vec![], reason: "declined".into() };
        ctl.reply(seq, Msg::PairResult(r)).await?;
        bail!("pairing with {name} declined");
    }
    let granted: Vec<String> = ALL_GRANTS.iter().map(|s| s.to_string()).collect();
    trust.lock().unwrap().insert(&peer_fp, &name, &granted)?;
    ctl.reply(seq, Msg::PairResult(pb::PairResult { accepted: true, granted: granted.clone(), reason: String::new() }))
        .await?;
    tracing::info!(hub = %name, fp = %peer_fp.short(), "paired");
    Ok(Authenticated { peer, peer_fp, granted, newly_paired: true })
}

/// Hub side: sends `Hello`, pairs if needed.
pub async fn hub_handshake(
    ctl: &mut Control,
    conn: &Connection,
    local: &LocalInfo,
    local_fp: &Fingerprint,
    trust: &Mutex<TrustStore>,
    ui: &impl HubUi,
) -> Result<Authenticated> {
    ctl.send(Msg::Hello(local.hello(local_fp))).await?;
    let (_, msg) = recv_msg(ctl).await?;
    let ack = match msg {
        Msg::HelloAck(a) => a,
        Msg::Error(e) => bail!("agent refused Hello: {}", e.message),
        m => bail!("expected HelloAck, got {m:?}"),
    };
    let peer_fp = peer_fingerprint(conn)?;
    let peer = ack.agent.clone().context("HelloAck without agent Hello")?;
    check_protocol(&peer)?;
    let known = trust.lock().unwrap().get(&peer_fp).is_some();
    let name = if peer.name.is_empty() { conn.remote_address().to_string() } else { peer.name.clone() };

    if ack.paired {
        if known {
            return Ok(Authenticated { peer, peer_fp, granted: ack.granted, newly_paired: false });
        }
        let sas = connection_sas(conn, local_fp)?;
        if !ui.confirm_agent(PairAsk { peer_name: name.clone(), peer_fp, sas }).await {
            bail!("agent {name} not trusted by this hub");
        }
        trust.lock().unwrap().insert(&peer_fp, &name, &[])?;
        return Ok(Authenticated { peer, peer_fp, granted: ack.granted, newly_paired: true });
    }

    let sas = connection_sas(conn, local_fp)?;
    ui.show_code(PairAsk { peer_name: name.clone(), peer_fp, sas });
    ctl.send(Msg::PairRequest(pb::PairRequest { hub_name: local.name.clone() })).await?;
    let (_, msg) = tokio::time::timeout(PAIR_TIMEOUT + Duration::from_secs(5), recv_msg(ctl))
        .await
        .context("pairing timed out")??;
    let Msg::PairResult(r) = msg else { bail!("expected PairResult, got {msg:?}") };
    if !r.accepted {
        bail!("agent declined pairing: {}", r.reason);
    }
    trust.lock().unwrap().insert(&peer_fp, &name, &[])?;
    Ok(Authenticated { peer, peer_fp, granted: r.granted, newly_paired: true })
}
