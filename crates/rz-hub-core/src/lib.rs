//! Rozhledna hub core (T1.5).
//!
//! - [`Hub`]: agents (added by address or found over mDNS), tiles (subscriptions), input, pairing answers,
//!   events for the UI ([`Event`], JSON over the FFI).
//! - One task per agent ([`session`]): connect → handshake/pairing → `ListSources` → (re)subscribe all its tiles →
//!   receive video / control / datagrams; on loss reconnect with backoff (`rz_net::backoff`), tiles stay.
//! - One decode thread per tile ([`tile`]): reorder (plan 6.4) → VideoToolbox → [`FrameSink`].
//! - Receive side stops in-flight streams the reorder can no longer use (plan 6.4, `Tile::wants`).
//! - Pings every 500 ms: clock sync, and the keep-alive the agent needs while keys are held (DECISIONS T1.3).

pub mod events;
pub mod reorder;
mod session;
pub mod tile;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rz_net::discovery::{Discovered, Discovery};
use rz_net::handshake::LocalInfo;
use rz_net::trust::TrustStore;
use rz_net::{Fingerprint, Identity};
use rz_proto::pb;
use tokio::sync::mpsc;

pub use events::{AgentId, AgentState, Event, SourceInfo, TileId, TileState};
pub use tile::{CursorState, FrameSink, TileParams};

use crate::session::AgentCmd;
use crate::tile::Tile;

pub(crate) struct AgentEntry {
    pub addr: Mutex<SocketAddr>,
    pub cmd: mpsc::UnboundedSender<AgentCmd>,
    pub fingerprint: Mutex<Option<Fingerprint>>,
    pub sources: Mutex<Vec<pb::Source>>,
}

pub(crate) struct Inner {
    pub identity: Identity,
    pub info: LocalInfo,
    pub trust: Mutex<TrustStore>,
    pub events: mpsc::UnboundedSender<Event>,
    pub agents: Mutex<HashMap<AgentId, Arc<AgentEntry>>>,
    pub tiles: Mutex<HashMap<TileId, Arc<Tile>>>,
    pub confirms: Mutex<HashMap<u64, tokio::sync::oneshot::Sender<bool>>>,
    /// mDNS: fp_short → addresses (for reconnecting to an agent whose address changed).
    pub discovered: Mutex<HashMap<String, Vec<SocketAddr>>>,
    next_id: AtomicU64,
    pub sink: FrameSink,
}

impl Inner {
    pub fn id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn emit(&self, e: Event) {
        let _ = self.events.send(e);
    }

    pub fn tiles_of(&self, agent: AgentId) -> Vec<Arc<Tile>> {
        self.tiles.lock().unwrap().values().filter(|t| t.agent == agent).cloned().collect()
    }
}

pub struct Hub {
    inner: Arc<Inner>,
    events: Mutex<mpsc::UnboundedReceiver<Event>>,
    rt: tokio::runtime::Runtime,
    discovery: Mutex<Option<Discovery>>,
}

impl Hub {
    /// `name` is shown to agents in the pairing dialog.
    pub fn new(identity: Identity, trust: TrustStore, name: &str, sink: FrameSink) -> Result<Self> {
        let rt =
            tokio::runtime::Builder::new_multi_thread().worker_threads(2).thread_name("rz-hub").enable_all().build()?;
        let (tx, rx) = mpsc::unbounded_channel();
        let inner = Arc::new(Inner {
            identity,
            info: LocalInfo { name: name.into(), is_agent: false, is_hub: true, features: vec![] },
            trust: Mutex::new(trust),
            events: tx,
            agents: Mutex::new(HashMap::new()),
            tiles: Mutex::new(HashMap::new()),
            confirms: Mutex::new(HashMap::new()),
            discovered: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            sink,
        });
        Ok(Self { inner, events: Mutex::new(rx), rt, discovery: Mutex::new(None) })
    }

    pub fn fingerprint(&self) -> Fingerprint {
        self.inner.identity.fingerprint
    }

    /// Starts browsing mDNS; `AgentFound` / `AgentLost` events follow.
    pub fn start_discovery(&self) -> Result<()> {
        let d = Discovery::new()?;
        let mut rx = d.browse()?;
        let inner = self.inner.clone();
        self.rt.spawn(async move {
            while let Some(ev) = rx.recv().await {
                match ev {
                    Discovered::Found(p) => {
                        if !p.roles.iter().any(|r| r == "agent") {
                            continue;
                        }
                        inner.discovered.lock().unwrap().insert(p.fp_short.clone(), p.addrs.clone());
                        inner.emit(Event::AgentFound {
                            key: p.fullname,
                            name: p.name,
                            os: p.os,
                            fp_short: p.fp_short,
                            addrs: p.addrs.iter().map(|a| a.to_string()).collect(),
                        });
                    }
                    Discovered::Lost { fullname } => inner.emit(Event::AgentLost { key: fullname }),
                }
            }
        });
        *self.discovery.lock().unwrap() = Some(d);
        Ok(())
    }

    /// Connects to an agent and keeps the connection up (reconnect with backoff) until `remove_agent`.
    pub fn add_agent(&self, addr: SocketAddr) -> AgentId {
        let id = self.inner.id();
        let (tx, rx) = mpsc::unbounded_channel();
        let entry = Arc::new(AgentEntry {
            addr: Mutex::new(addr),
            cmd: tx,
            fingerprint: Mutex::new(None),
            sources: Mutex::new(Vec::new()),
        });
        self.inner.agents.lock().unwrap().insert(id, entry.clone());
        self.rt.spawn(session::agent_task(self.inner.clone(), id, entry, rx));
        id
    }

    pub fn remove_agent(&self, agent: AgentId) {
        if let Some(a) = self.inner.agents.lock().unwrap().remove(&agent) {
            let _ = a.cmd.send(AgentCmd::Shutdown);
        }
        self.inner.tiles.lock().unwrap().retain(|_, t| t.agent != agent);
    }

    pub fn sources(&self, agent: AgentId) -> Vec<SourceInfo> {
        let a = self.inner.agents.lock().unwrap().get(&agent).cloned();
        a.map(|a| a.sources.lock().unwrap().iter().map(SourceInfo::from).collect()).unwrap_or_default()
    }

    /// Creates a tile; it streams whenever the agent is connected.
    pub fn subscribe(&self, agent: AgentId, params: TileParams) -> Result<TileId> {
        let a = self.inner.agents.lock().unwrap().get(&agent).cloned().context("unknown agent")?;
        let id = self.inner.id();
        let cmd = a.cmd.clone();
        let t = tile::spawn(
            id,
            agent,
            params,
            self.inner.sink.clone(),
            self.inner.events.clone(),
            Box::new(move |last| {
                let _ = cmd.send(AgentCmd::Keyframe { tile: id, last });
            }),
        );
        self.inner.tiles.lock().unwrap().insert(id, t);
        let _ = a.cmd.send(AgentCmd::Subscribe(id));
        Ok(id)
    }

    /// Changes size / fps / tier (agent restarts the stream under the same id).
    pub fn update_tile(&self, tile: TileId, params: TileParams) -> Result<()> {
        let t = self.inner.tiles.lock().unwrap().get(&tile).cloned().context("unknown tile")?;
        *t.params.lock().unwrap() = params;
        self.send(t.agent, AgentCmd::Update(tile));
        Ok(())
    }

    pub fn unsubscribe(&self, tile: TileId) {
        if let Some(t) = self.inner.tiles.lock().unwrap().remove(&tile) {
            self.send(t.agent, AgentCmd::Unsubscribe(tile));
        }
    }

    /// Reliable input to the tile's source (`source_id` is filled in).
    pub fn input(&self, tile: TileId, ev: pb::input_event::Ev) {
        if let Some(t) = self.inner.tiles.lock().unwrap().get(&tile) {
            self.send(t.agent, AgentCmd::Input(tile, ev));
        }
    }

    /// Pointer position, normalized 0..1 in the tile's source (datagram, latest wins).
    pub fn pointer(&self, tile: TileId, x: f32, y: f32) {
        if let Some(t) = self.inner.tiles.lock().unwrap().get(&tile) {
            self.send(t.agent, AgentCmd::Pointer(tile, x, y));
        }
    }

    /// Releases everything this hub holds on the tile's agent (focus lost, grab released).
    pub fn release_all(&self, tile: TileId) {
        self.input(tile, pb::input_event::Ev::ReleaseAll(true));
    }

    pub fn cursor(&self, tile: TileId) -> Option<CursorState> {
        self.inner.tiles.lock().unwrap().get(&tile).map(|t| *t.cursor.lock().unwrap())
    }

    /// Answer to `Event::ConfirmAgent`.
    pub fn answer(&self, request: u64, accept: bool) {
        if let Some(tx) = self.inner.confirms.lock().unwrap().remove(&request) {
            let _ = tx.send(accept);
        }
    }

    /// Removes a paired agent from the trust store (next connect pairs again).
    pub fn forget(&self, fp: &Fingerprint) -> Result<bool> {
        self.inner.trust.lock().unwrap().remove(fp)
    }

    pub fn trusted(&self) -> Vec<rz_net::trust::TrustedPeer> {
        self.inner.trust.lock().unwrap().peers().to_vec()
    }

    /// Next event, waiting up to `timeout`.
    pub fn next_event(&self, timeout: std::time::Duration) -> Option<Event> {
        let mut rx = self.events.lock().unwrap();
        if let Ok(e) = rx.try_recv() {
            return Some(e);
        }
        self.rt.block_on(async { tokio::time::timeout(timeout, rx.recv()).await.ok().flatten() })
    }

    fn send(&self, agent: AgentId, cmd: AgentCmd) {
        if let Some(a) = self.inner.agents.lock().unwrap().get(&agent) {
            let _ = a.cmd.send(cmd);
        }
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        for a in self.inner.agents.lock().unwrap().values() {
            let _ = a.cmd.send(AgentCmd::Shutdown);
        }
        self.inner.tiles.lock().unwrap().clear();
    }
}
