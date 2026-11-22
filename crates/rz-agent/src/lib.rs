//! Rozhledna agent (T1.2).
//!
//! - accepts hubs over mutual TLS (`rz_net::server_endpoint_mtls`), LAN addresses only (plan 6.8);
//! - authenticates / pairs them (`rz_net::handshake`), the agent's user confirms via [`AgentUi`];
//! - lists displays, serves any number of streams per hub (`Subscribe` / `UpdateSubscription` / `Unsubscribe`);
//! - reports [`pb::AgentStatus`] (permissions, locked screen, display sleep) after the handshake and on change;
//! - keeps the display awake while at least one stream runs;
//! - advertises itself over mDNS.

pub mod cursor;
pub mod input;
pub mod pipeline;
pub mod sources;
pub mod status;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use anyhow::{Context, Result};
use rz_net::control::{Control, ControlSender, pong_for};
use rz_net::discovery::{Advert, Discovery};
use rz_net::handshake::{AgentUi, LocalInfo, agent_handshake};
use rz_net::trust::TrustStore;
use rz_net::{Connection, Fingerprint, Identity};
use rz_proto::clock::now_ns;
use rz_proto::pb::{self, envelope::Msg};
use tokio::sync::watch;

pub use rz_net::handshake::PairAsk;

use crate::input::{HubInput, Rects, SharedInjector};
use crate::pipeline::Pipeline;
use crate::sources::{CaptureParams, Registry};
use crate::status::DisplayAwake;

/// `Error.code` values sent to the hub.
pub mod err {
    pub const PROTOCOL: u32 = 1;
    /// Screen locked / no GUI session: capture impossible until unlocked (watch `AgentStatus`).
    pub const SCREEN_LOCKED: u32 = 2;
    /// The hub lacks the needed grant (`view`).
    pub const NOT_PERMITTED: u32 = 3;
    pub const UNKNOWN_SOURCE: u32 = 4;
    /// Capture or encoder failed (often: Screen Recording not granted).
    pub const CAPTURE: u32 = 5;
    pub const UNKNOWN_STREAM: u32 = 6;
}

/// Agent-wide limits; a hub's `Subscribe` can only lower them.
#[derive(Clone, Debug)]
pub struct Limits {
    pub max_w: u32,
    pub max_h: u32,
    pub max_fps: u32,
    /// 0 = auto (0.12 bpp).
    pub bitrate_kbps: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_w: 3840, max_h: 2160, max_fps: 60, bitrate_kbps: 0 }
    }
}

/// A connected, authenticated hub (for the tray menu).
#[derive(Clone, Debug)]
pub struct HubInfo {
    pub name: String,
    pub fingerprint: Fingerprint,
    pub addr: SocketAddr,
    pub streams: usize,
}

struct HubEntry {
    info: HubInfo,
    conn: Connection,
}

pub struct Agent<U: AgentUi + 'static> {
    pub identity: Identity,
    pub info: LocalInfo,
    trust: Mutex<TrustStore>,
    registry: Registry,
    limits: Limits,
    ui: U,
    hubs: Mutex<HashMap<u64, HubEntry>>,
    next_conn: AtomicU64,
    awake: Mutex<Weak<DisplayAwake>>,
    injector: SharedInjector,
    cursor: Option<watch::Receiver<cursor::Sample>>,
    /// Source rectangles (global points) for mapping normalized input coordinates; refreshed on every listing.
    rects: Rects,
    /// Bumped whenever the set of hubs or their streams changes (tray refresh).
    changed: watch::Sender<u64>,
}

impl<U: AgentUi + 'static> Agent<U> {
    pub fn new(
        identity: Identity,
        name: &str,
        trust: TrustStore,
        registry: Registry,
        limits: Limits,
        ui: U,
        injector: Box<dyn rz_input::Injector>,
    ) -> Arc<Self> {
        let info = LocalInfo {
            name: name.into(),
            is_agent: true,
            is_hub: false,
            features: vec!["agent_status".into(), "cursor_shapes".into()],
        };
        let cursor = cursor::start();
        Arc::new(Self {
            identity,
            info,
            trust: Mutex::new(trust),
            registry,
            limits,
            ui,
            hubs: Mutex::new(HashMap::new()),
            next_conn: AtomicU64::new(1),
            awake: Mutex::new(Weak::new()),
            injector: Arc::new(Mutex::new(injector)),
            cursor,
            rects: Arc::new(Mutex::new(HashMap::new())),
            changed: watch::channel(0).0,
        })
    }

    pub fn ui(&self) -> &U {
        &self.ui
    }

    pub fn hubs(&self) -> Vec<HubInfo> {
        self.hubs.lock().unwrap().values().map(|h| h.info.clone()).collect()
    }

    pub fn subscribe_changes(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    /// „Odpojit vše“ (plan 6.8).
    pub fn disconnect_all(&self) {
        for h in self.hubs.lock().unwrap().values() {
            h.conn.close(0u32.into(), b"disconnected by the agent's user");
        }
    }

    /// Forgets a paired hub (it will have to pair again) and disconnects it.
    pub fn unpair(&self, fp: &Fingerprint) -> Result<bool> {
        for h in self.hubs.lock().unwrap().values() {
            if &h.info.fingerprint == fp {
                h.conn.close(0u32.into(), b"unpaired");
            }
        }
        self.trust.lock().unwrap().remove(fp)
    }

    pub fn trusted_hubs(&self) -> Vec<rz_net::trust::TrustedPeer> {
        self.trust.lock().unwrap().peers().to_vec()
    }

    /// Lists sources and refreshes the rectangle cache used for input mapping.
    fn list_sources(&self) -> Result<Vec<pb::Source>> {
        let l = self.registry.list()?;
        let mut r = self.rects.lock().unwrap();
        r.clear();
        for s in &l {
            if let Some(rect) = &s.rect {
                r.insert(s.id, rect.into());
            }
        }
        Ok(l)
    }

    /// Whether injected input reaches the OS (macOS: Accessibility).
    pub fn input_available(&self) -> bool {
        self.injector.lock().unwrap().available()
    }

    fn bump(&self) {
        self.changed.send_modify(|v| *v += 1);
    }

    fn awake(&self) -> Arc<DisplayAwake> {
        let mut w = self.awake.lock().unwrap();
        if let Some(a) = w.upgrade() {
            return a;
        }
        let a = Arc::new(DisplayAwake::new());
        *w = Arc::downgrade(&a);
        a
    }

    /// Binds, advertises over mDNS (when `mdns`), and serves hubs until the endpoint closes.
    pub async fn serve(self: Arc<Self>, bind: SocketAddr, mdns: bool) -> Result<()> {
        let ep = rz_net::server_endpoint_mtls(bind, &self.identity)?;
        let port = ep.local_addr()?.port();
        let _discovery = if mdns {
            let mut d = Discovery::new()?;
            d.advertise(&Advert {
                name: self.info.name.clone(),
                fingerprint: self.identity.fingerprint,
                port,
                roles: vec!["agent"],
            })?;
            Some(d)
        } else {
            None
        };
        self.accept_loop(ep).await
    }

    pub async fn accept_loop(self: Arc<Self>, ep: rz_net::Endpoint) -> Result<()> {
        while let Some(incoming) = ep.accept().await {
            let remote = incoming.remote_address();
            if !rz_net::lan::is_lan(remote.ip()) {
                tracing::warn!(%remote, "refused: not a LAN address");
                incoming.refuse();
                continue;
            }
            let me = self.clone();
            tokio::spawn(async move {
                let conn = match incoming.await {
                    Ok(c) => c,
                    Err(e) => return tracing::warn!(%remote, "TLS handshake failed: {e}"),
                };
                let c = conn.clone();
                match me.session(conn).await {
                    Ok(()) => tracing::info!(%remote, "hub disconnected"),
                    // The hub closed the connection itself: a normal end, not an error.
                    Err(_) if matches!(c.close_reason(), Some(quinn::ConnectionError::ApplicationClosed(_))) => {
                        tracing::info!(%remote, "hub disconnected")
                    }
                    Err(e) => tracing::warn!(%remote, "session ended: {e:#}"),
                }
            });
        }
        Ok(())
    }

    async fn session(self: Arc<Self>, conn: Connection) -> Result<()> {
        let (send, recv) = tokio::time::timeout(Duration::from_secs(10), conn.accept_bi())
            .await
            .context("hub opened no control stream")??;
        let mut ctl = Control::new(send, recv);
        let auth =
            agent_handshake(&mut ctl, &conn, &self.info, &self.identity.fingerprint, &self.trust, &self.ui).await?;
        let can_view = auth.granted.iter().any(|g| g == "view");
        let can_control = auth.granted.iter().any(|g| g == "control");
        tracing::info!(hub = %auth.peer.name, fp = %auth.peer_fp.short(), ?auth.granted, "hub authenticated");

        let conn_id = self.next_conn.fetch_add(1, Ordering::Relaxed);
        let entry = HubEntry {
            info: HubInfo {
                name: auth.peer.name.clone(),
                fingerprint: auth.peer_fp,
                addr: conn.remote_address(),
                streams: 0,
            },
            conn: conn.clone(),
        };
        self.hubs.lock().unwrap().insert(conn_id, entry);
        self.bump();
        if can_control && self.rects.lock().unwrap().is_empty() {
            let _ = self.list_sources();
        }
        let input = HubInput::new(self.injector.clone(), self.rects.clone(), can_control);
        let input_task = tokio::spawn(input::run(conn.clone(), input.clone()));
        let r = self.clone().serve_hub(conn, ctl, can_view, conn_id, input.clone()).await;
        input_task.abort();
        input.release_all("session ended");
        self.hubs.lock().unwrap().remove(&conn_id);
        self.bump();
        r
    }

    async fn serve_hub(
        self: Arc<Self>,
        conn: Connection,
        ctl: Control,
        can_view: bool,
        conn_id: u64,
        input: Arc<HubInput>,
    ) -> Result<()> {
        let (tx, mut rx) = ctl.split();
        let ctl = Arc::new(tokio::sync::Mutex::new(tx));
        let status_task = tokio::spawn(status_loop(ctl.clone()));
        let streamed: Arc<Mutex<HashMap<u32, u64>>> = Arc::default();
        let cursor_task = self
            .cursor
            .clone()
            .map(|rx| tokio::spawn(cursor_loop(rx, conn.clone(), ctl.clone(), streamed.clone(), self.rects.clone())));
        let mut streams: HashMap<u32, Pipeline> = HashMap::new();
        let mut next_stream = 1u32;
        let res: Result<()> = async {
            while let Some(env) = rx.recv().await? {
                let t1 = now_ns();
                input.touch();
                let Some(msg) = env.msg else { continue };
                let reply = |m: Msg| {
                    let ctl = ctl.clone();
                    async move { ctl.lock().await.reply(env.seq, m).await }
                };
                match msg {
                    Msg::Ping(p) => {
                        reply(pong_for(&p, t1)).await?;
                    }
                    Msg::ListSources(_) => {
                        let m = match self.list_sources() {
                            Ok(sources) => Msg::SourceList(pb::SourceList { sources }),
                            Err(e) => error_msg(err::CAPTURE, format!("list sources: {e:#}")),
                        };
                        reply(m).await?;
                    }
                    Msg::Subscribe(sub) => {
                        let id = next_stream;
                        let m = match self.open_stream(&conn, &ctl, &sub, id, can_view) {
                            Ok(p) => {
                                next_stream += 1;
                                let c = p.config;
                                streams.insert(id, p);
                                Msg::SubscribeAck(pb::SubscribeAck {
                                    stream_id: id,
                                    w: c.w,
                                    h: c.h,
                                    fps: c.fps,
                                    codec: pb::Codec::H264 as i32,
                                })
                            }
                            Err(e) => Msg::Error(e),
                        };
                        reply(m).await?;
                        *streamed.lock().unwrap() = streams.iter().map(|(k, p)| (*k, p.source_id)).collect();
                        self.set_streams(conn_id, streams.len());
                    }
                    Msg::UpdateSubscription(u) => {
                        // Restart the stream with the new limits under the same id (new frame 0 = DISCONTINUITY).
                        let m = match streams.remove(&u.stream_id) {
                            None => error_msg(err::UNKNOWN_STREAM, format!("no stream {}", u.stream_id)),
                            Some(old) => {
                                let sub = pb::Subscribe {
                                    source_id: old.source_id,
                                    tier: u.tier,
                                    max_w: u.max_w,
                                    max_h: u.max_h,
                                    max_fps: u.max_fps,
                                    codec: pb::Codec::H264 as i32,
                                    bitrate_kbps: u.bitrate_kbps,
                                };
                                drop(old);
                                match self.open_stream(&conn, &ctl, &sub, u.stream_id, can_view) {
                                    Ok(p) => {
                                        let c = p.config;
                                        streams.insert(u.stream_id, p);
                                        Msg::SubscribeAck(pb::SubscribeAck {
                                            stream_id: u.stream_id,
                                            w: c.w,
                                            h: c.h,
                                            fps: c.fps,
                                            codec: pb::Codec::H264 as i32,
                                        })
                                    }
                                    Err(e) => Msg::Error(e),
                                }
                            }
                        };
                        reply(m).await?;
                        *streamed.lock().unwrap() = streams.iter().map(|(k, p)| (*k, p.source_id)).collect();
                        self.set_streams(conn_id, streams.len());
                    }
                    Msg::Unsubscribe(u) => {
                        streams.remove(&u.stream_id);
                        *streamed.lock().unwrap() = streams.iter().map(|(k, p)| (*k, p.source_id)).collect();
                        self.set_streams(conn_id, streams.len());
                    }
                    Msg::RequestKeyframe(rk) => {
                        if let Some(p) = streams.get(&rk.stream_id) {
                            tracing::debug!(stream = rk.stream_id, after = rk.last_frame_no, "keyframe requested");
                            p.request_keyframe();
                        }
                    }
                    other => tracing::debug!("ignored {other:?}"),
                }
            }
            Ok(())
        }
        .await;
        status_task.abort();
        if let Some(t) = cursor_task {
            t.abort();
        }
        drop(streams);
        res
    }

    fn set_streams(&self, conn_id: u64, n: usize) {
        if let Some(h) = self.hubs.lock().unwrap().get_mut(&conn_id) {
            h.info.streams = n;
        }
        self.bump();
    }

    /// Opens + starts a stream, or returns the `Error` message for the hub.
    fn open_stream(
        &self,
        conn: &Connection,
        ctl: &Arc<tokio::sync::Mutex<ControlSender>>,
        sub: &pb::Subscribe,
        stream_id: u32,
        can_view: bool,
    ) -> std::result::Result<Pipeline, pb::Error> {
        if !can_view {
            return Err(error(err::NOT_PERMITTED, "this hub has no 'view' permission".into()));
        }
        if matches!(self.registry.backend(), sources::Backend::Sck { .. }) && status::current().screen_locked {
            return Err(error(err::SCREEN_LOCKED, "screen is locked".into()));
        }
        let cap = |req: u32, lim: u32| if req > 0 { req.min(lim) } else { lim };
        let p = CaptureParams {
            max_w: cap(sub.max_w, self.limits.max_w),
            max_h: cap(sub.max_h, self.limits.max_h),
            fps: cap(sub.max_fps, self.limits.max_fps),
        };
        let bitrate = if sub.bitrate_kbps > 0 { sub.bitrate_kbps } else { self.limits.bitrate_kbps };
        let known = self.list_sources().map(|l| l.iter().any(|s| s.id == sub.source_id)).unwrap_or(false);
        if !known {
            return Err(error(err::UNKNOWN_SOURCE, format!("unknown source {}", sub.source_id)));
        }
        let capture = self
            .registry
            .open(sub.source_id, p)
            .map_err(|e| error(err::CAPTURE, format!("open source {}: {e:#}", sub.source_id)))?;
        Pipeline::start(capture, sub.source_id, stream_id, p.fps, bitrate, conn, ctl.clone(), self.awake())
            .map_err(|e| error(err::CAPTURE, format!("start stream: {e:#}")))
    }
}

fn error(code: u32, message: String) -> pb::Error {
    tracing::warn!(code, "{message}");
    pb::Error { code, message }
}

fn error_msg(code: u32, message: String) -> Msg {
    Msg::Error(error(code, message))
}

/// Sends the cursor to one hub: `CursorShape` (control, once per shape) and a `CursorPos` datagram per streamed
/// source on every change (≤ 60 Hz), position normalized to that source, `visible` when inside it.
async fn cursor_loop(
    mut rx: watch::Receiver<cursor::Sample>,
    conn: Connection,
    ctl: Arc<tokio::sync::Mutex<ControlSender>>,
    streamed: Arc<Mutex<HashMap<u32, u64>>>,
    rects: Rects,
) {
    use rz_proto::datagram::Datagram;
    let mut sent_shapes = std::collections::HashSet::new();
    let mut seq = 0u32;
    // Also resend once a second: a hub that just subscribed must learn where a resting cursor is.
    let mut heartbeat = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            r = rx.changed() => if r.is_err() { return },
            _ = heartbeat.tick() => {}
        }
        let s = rx.borrow_and_update().clone();
        let mut sources: Vec<u64> = streamed.lock().unwrap().values().copied().collect();
        if sources.is_empty() {
            continue;
        }
        sources.sort_unstable();
        sources.dedup();
        let shape_id = s.shape.as_ref().map_or(0, |sh| sh.id);
        if let Some(sh) = &s.shape
            && sent_shapes.insert(sh.id)
            && ctl.lock().await.send(Msg::CursorShape(sh.to_pb())).await.is_err()
        {
            return;
        }
        for src in sources {
            let Some(r) = rects.lock().unwrap().get(&src).copied() else { continue };
            if r.w <= 0.0 || r.h <= 0.0 {
                continue;
            }
            let (nx, ny) = ((s.x - r.x) / r.w, (s.y - r.y) / r.h);
            let visible = (0.0..1.0).contains(&nx) && (0.0..1.0).contains(&ny);
            seq = seq.wrapping_add(1);
            let d = Datagram::CursorPos { source_id: src, seq, x: nx as f32, y: ny as f32, shape_id, visible };
            if conn.send_datagram(d.encode().into()).is_err() {
                return;
            }
        }
    }
}

/// Sends `AgentStatus` now and whenever it changes (polled every 2 s).
async fn status_loop(ctl: Arc<tokio::sync::Mutex<ControlSender>>) {
    let mut last: Option<pb::AgentStatus> = None;
    let mut iv = tokio::time::interval(Duration::from_secs(2));
    loop {
        iv.tick().await;
        let now = status::current();
        if last.as_ref() != Some(&now) {
            if ctl.lock().await.send(Msg::AgentStatus(now)).await.is_err() {
                return;
            }
            last = Some(now);
        }
    }
}
