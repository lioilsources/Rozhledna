//! One task per agent: connect, authenticate/pair, serve, reconnect with backoff.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rz_net::backoff::Backoff;
use rz_net::control::{Control, ControlSender, ping_now, sample_from_pong};
use rz_net::handshake::{Authenticated, HubUi, PairAsk, hub_handshake};
use rz_net::{Connection, Endpoint, SendStream, client_endpoint_mtls, video};
use rz_proto::clock::{ClockSync, now_ns};
use rz_proto::datagram::{Datagram, seq_newer};
use rz_proto::framing::write_msg;
use rz_proto::pb::{self, envelope::Msg};
use tokio::sync::{mpsc, oneshot};

use crate::events::{AgentId, AgentState, Event, SourceInfo, TileId, TileState, base64};
use crate::tile::Incoming;
use crate::{AgentEntry, Inner};

pub enum AgentCmd {
    Subscribe(TileId),
    Update(TileId),
    Unsubscribe(TileId),
    Input(TileId, pb::input_event::Ev),
    Pointer(TileId, f32, f32),
    Keyframe { tile: TileId, last: u64 },
    Shutdown,
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Keep-alive for held keys (agent releases after 1 s of silence) + clock sync.
const PING_EVERY: Duration = Duration::from_millis(500);

struct Ui {
    inner: Arc<Inner>,
    agent: AgentId,
}

impl HubUi for Ui {
    fn show_code(&self, ask: PairAsk) {
        self.inner.emit(Event::AgentState { agent: self.agent, state: AgentState::Pairing, detail: String::new() });
        self.inner.emit(Event::PairCode { agent: self.agent, name: ask.peer_name, sas: ask.sas });
    }

    async fn confirm_agent(&self, ask: PairAsk) -> bool {
        let request = self.inner.id();
        let (tx, rx) = oneshot::channel();
        self.inner.confirms.lock().unwrap().insert(request, tx);
        self.inner.emit(Event::ConfirmAgent {
            request,
            agent: self.agent,
            name: ask.peer_name,
            fingerprint: ask.peer_fp.to_string(),
            sas: ask.sas,
        });
        tokio::time::timeout(Duration::from_secs(120), rx).await.ok().and_then(|r| r.ok()).unwrap_or(false)
    }
}

enum Exit {
    Shutdown,
    Lost(String),
}

pub async fn agent_task(
    inner: Arc<Inner>,
    agent: AgentId,
    entry: Arc<AgentEntry>,
    mut rx: mpsc::UnboundedReceiver<AgentCmd>,
) {
    let ep = match client_endpoint_mtls(&inner.identity) {
        Ok(e) => e,
        Err(e) => {
            inner.emit(Event::AgentState { agent, state: AgentState::Disconnected, detail: format!("{e:#}") });
            return;
        }
    };
    let ui = Ui { inner: inner.clone(), agent };
    let mut backoff = Backoff::default();
    loop {
        inner.emit(Event::AgentState { agent, state: AgentState::Connecting, detail: String::new() });
        let exit = match connect(&inner, &entry, &ep, &ui).await {
            Ok((conn, ctl, auth)) => {
                backoff.reset();
                *entry.fingerprint.lock().unwrap() = Some(auth.peer_fp);
                inner.emit(Event::AgentInfo {
                    agent,
                    name: auth.peer.name.clone(),
                    fingerprint: auth.peer_fp.to_string(),
                    granted: auth.granted.clone(),
                    newly_paired: auth.newly_paired,
                });
                inner.emit(Event::AgentState { agent, state: AgentState::Connected, detail: String::new() });
                let e = Session::run(inner.clone(), agent, entry.clone(), conn.clone(), ctl, &mut rx).await;
                conn.close(0u32.into(), b"bye");
                e
            }
            Err(e) => Exit::Lost(format!("{e:#}")),
        };
        for t in inner.tiles_of(agent) {
            let p = *t.params.lock().unwrap();
            inner.emit(Event::TileState {
                tile: t.id,
                agent,
                state: TileState::Waiting,
                w: 0,
                h: 0,
                fps: p.fps,
                detail: String::new(),
            });
        }
        let why = match exit {
            Exit::Shutdown => return,
            Exit::Lost(why) => why,
        };
        let delay = backoff.next_delay();
        tracing::info!(agent, "disconnected ({why}); retry in {delay:?}");
        inner.emit(Event::AgentState { agent, state: AgentState::Disconnected, detail: why });
        let sleep = tokio::time::sleep(delay);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                _ = &mut sleep => break,
                c = rx.recv() => match c {
                    None | Some(AgentCmd::Shutdown) => return,
                    // Tiles persist in `inner.tiles` and are (re)subscribed on the next connect.
                    Some(_) => {}
                },
            }
        }
    }
}

/// Tries the configured address, then addresses mDNS knows for the agent's fingerprint.
async fn connect(
    inner: &Arc<Inner>,
    entry: &AgentEntry,
    ep: &Endpoint,
    ui: &Ui,
) -> Result<(Connection, Control, Authenticated)> {
    let mut addrs = vec![*entry.addr.lock().unwrap()];
    if let Some(fp) = *entry.fingerprint.lock().unwrap()
        && let Some(found) = inner.discovered.lock().unwrap().get(&fp.short())
    {
        addrs.extend(found.iter().filter(|a| !addrs.contains(a)).copied().collect::<Vec<SocketAddr>>());
    }
    let mut last_err = None;
    for addr in addrs {
        let attempt = async {
            let conn = ep.connect(addr, "rozhledna")?.await?;
            anyhow::Ok(conn)
        };
        let conn = match tokio::time::timeout(CONNECT_TIMEOUT, attempt).await {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => {
                last_err = Some(e.context(format!("connect {addr}")));
                continue;
            }
            Err(_) => {
                last_err = Some(anyhow::anyhow!("connect {addr}: timed out"));
                continue;
            }
        };
        let (send, recv) = conn.open_bi().await?;
        let mut ctl = Control::new(send, recv);
        let auth = hub_handshake(&mut ctl, &conn, &inner.info, &inner.identity.fingerprint, &inner.trust, ui).await?;
        if addr != *entry.addr.lock().unwrap() {
            tracing::info!(%addr, "agent reached at a new address");
            *entry.addr.lock().unwrap() = addr;
        }
        return Ok((conn, ctl, auth));
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no address")))
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Msg>>>>;

struct Session {
    inner: Arc<Inner>,
    agent: AgentId,
    entry: Arc<AgentEntry>,
    conn: Connection,
    tx: Arc<tokio::sync::Mutex<ControlSender>>,
    pending: Pending,
    /// stream_id → tile
    streams: Arc<Mutex<HashMap<u32, TileId>>>,
    sync: Arc<Mutex<ClockSync>>,
    input: Option<SendStream>,
    motion_seq: u32,
}

impl Session {
    async fn run(
        inner: Arc<Inner>,
        agent: AgentId,
        entry: Arc<AgentEntry>,
        conn: Connection,
        ctl: Control,
        rx: &mut mpsc::UnboundedReceiver<AgentCmd>,
    ) -> Exit {
        let (tx, crx) = ctl.split();
        let mut s = Session {
            inner,
            agent,
            entry,
            conn,
            tx: Arc::new(tokio::sync::Mutex::new(tx)),
            pending: Arc::default(),
            streams: Arc::default(),
            sync: Arc::default(),
            input: None,
            motion_seq: 0,
        };
        let (unlock_tx, mut unlock_rx) = mpsc::unbounded_channel::<()>();
        let tasks = vec![
            tokio::spawn(reader(
                s.inner.clone(),
                agent,
                crx,
                s.pending.clone(),
                s.streams.clone(),
                s.sync.clone(),
                unlock_tx,
            )),
            tokio::spawn(pinger(s.tx.clone())),
            tokio::spawn(video_rx(s.inner.clone(), s.conn.clone(), s.streams.clone(), s.sync.clone())),
            tokio::spawn(datagram_rx(s.inner.clone(), agent, s.conn.clone())),
        ];
        let exit = s.serve(rx, &mut unlock_rx).await;
        for t in tasks {
            t.abort();
        }
        exit
    }

    async fn serve(
        &mut self,
        rx: &mut mpsc::UnboundedReceiver<AgentCmd>,
        unlock: &mut mpsc::UnboundedReceiver<()>,
    ) -> Exit {
        if let Err(e) = self.list_sources().await {
            return Exit::Lost(format!("list sources: {e:#}"));
        }
        for t in self.inner.tiles_of(self.agent) {
            self.subscribe(t.id).await;
        }
        loop {
            tokio::select! {
                r = self.conn.closed() => return Exit::Lost(r.to_string()),
                Some(()) = unlock.recv() => {
                    // Screen unlocked: retry tiles that failed (typically SCREEN_LOCKED).
                    let live: Vec<TileId> = self.streams.lock().unwrap().values().copied().collect();
                    for t in self.inner.tiles_of(self.agent) {
                        if !live.contains(&t.id) {
                            self.subscribe(t.id).await;
                        }
                    }
                }
                c = rx.recv() => {
                    let Some(c) = c else { return Exit::Shutdown };
                    match c {
                        AgentCmd::Shutdown => return Exit::Shutdown,
                        AgentCmd::Subscribe(t) => self.subscribe(t).await,
                        AgentCmd::Update(t) => self.update(t).await,
                        AgentCmd::Unsubscribe(t) => self.unsubscribe(t).await,
                        AgentCmd::Input(t, ev) => self.input(t, ev).await,
                        AgentCmd::Pointer(t, x, y) => self.pointer(t, x, y),
                        AgentCmd::Keyframe { tile, last } => {
                            if let Some(sid) = self.stream_of(tile) {
                                let rk = pb::RequestKeyframe { stream_id: sid, last_frame_no: last };
                                let _ = self.tx.lock().await.send(Msg::RequestKeyframe(rk)).await;
                            }
                        }
                    }
                }
            }
        }
    }

    async fn request(&self, msg: Msg) -> Result<Msg> {
        let (otx, orx) = oneshot::channel();
        {
            let mut tx = self.tx.lock().await;
            let seq = tx.next_seq();
            self.pending.lock().unwrap().insert(seq, otx);
            tx.send(msg).await?;
        }
        let r = tokio::time::timeout(REQUEST_TIMEOUT, orx).await.context("agent did not answer")?;
        Ok(r?)
    }

    async fn list_sources(&self) -> Result<()> {
        match self.request(Msg::ListSources(pb::ListSources { include_windows: false })).await? {
            Msg::SourceList(l) => {
                let sources: Vec<SourceInfo> = l.sources.iter().map(SourceInfo::from).collect();
                *self.entry.sources.lock().unwrap() = l.sources;
                self.inner.emit(Event::Sources { agent: self.agent, sources });
                Ok(())
            }
            Msg::Error(e) => bail!("{} (code {})", e.message, e.code),
            m => bail!("unexpected {m:?}"),
        }
    }

    fn stream_of(&self, tile: TileId) -> Option<u32> {
        self.streams.lock().unwrap().iter().find(|(_, t)| **t == tile).map(|(s, _)| *s)
    }

    fn tile_state(&self, tile: TileId, state: TileState, ack: Option<&pb::SubscribeAck>, detail: String) {
        let (w, h, fps) = ack.map_or((0, 0, 0), |a| (a.w, a.h, a.fps));
        self.inner.emit(Event::TileState { tile, agent: self.agent, state, w, h, fps, detail });
    }

    async fn subscribe(&mut self, tile: TileId) {
        let Some(t) = self.inner.tiles.lock().unwrap().get(&tile).cloned() else { return };
        if self.stream_of(tile).is_some() {
            return;
        }
        let sub = t.params.lock().unwrap().subscribe();
        match self.request(Msg::Subscribe(sub)).await {
            Ok(Msg::SubscribeAck(a)) => {
                self.streams.lock().unwrap().insert(a.stream_id, tile);
                self.tile_state(tile, TileState::Live, Some(&a), String::new());
            }
            Ok(Msg::Error(e)) => {
                self.tile_state(tile, TileState::Error, None, format!("{} (code {})", e.message, e.code))
            }
            Ok(m) => self.tile_state(tile, TileState::Error, None, format!("unexpected {m:?}")),
            Err(e) => self.tile_state(tile, TileState::Error, None, format!("{e:#}")),
        }
    }

    async fn update(&mut self, tile: TileId) {
        let Some(t) = self.inner.tiles.lock().unwrap().get(&tile).cloned() else { return };
        let Some(sid) = self.stream_of(tile) else { return self.subscribe(tile).await };
        let p = *t.params.lock().unwrap();
        let u = pb::UpdateSubscription {
            stream_id: sid,
            tier: p.subscribe().tier,
            max_w: p.max_w,
            max_h: p.max_h,
            max_fps: p.fps,
            bitrate_kbps: p.bitrate_kbps,
        };
        match self.request(Msg::UpdateSubscription(u)).await {
            Ok(Msg::SubscribeAck(a)) => self.tile_state(tile, TileState::Live, Some(&a), String::new()),
            Ok(Msg::Error(e)) => {
                self.streams.lock().unwrap().remove(&sid);
                self.tile_state(tile, TileState::Error, None, format!("{} (code {})", e.message, e.code));
            }
            Ok(m) => tracing::warn!("update: unexpected {m:?}"),
            Err(e) => tracing::warn!("update: {e:#}"),
        }
    }

    async fn unsubscribe(&mut self, tile: TileId) {
        if let Some(sid) = self.stream_of(tile) {
            self.streams.lock().unwrap().remove(&sid);
            let _ = self.tx.lock().await.send(Msg::Unsubscribe(pb::Unsubscribe { stream_id: sid })).await;
        }
    }

    fn source_of(&self, tile: TileId) -> Option<u64> {
        self.inner.tiles.lock().unwrap().get(&tile).map(|t| t.params.lock().unwrap().source_id)
    }

    async fn input(&mut self, tile: TileId, ev: pb::input_event::Ev) {
        let Some(source_id) = self.source_of(tile) else { return };
        let e = pb::InputEvent { source_id, ev: Some(ev) };
        for attempt in 0..2 {
            if self.input.is_none() {
                match self.conn.open_uni().await {
                    Ok(s) => {
                        // Input before video: a click must not wait behind a keyframe.
                        let _ = s.set_priority(10);
                        self.input = Some(s);
                    }
                    Err(e) => return tracing::debug!("open input stream: {e}"),
                }
            }
            let s = self.input.as_mut().unwrap();
            match write_msg(s, &e).await {
                Ok(()) => return,
                Err(err) => {
                    tracing::debug!(attempt, "input stream: {err}");
                    self.input = None;
                }
            }
        }
    }

    fn pointer(&mut self, tile: TileId, x: f32, y: f32) {
        let Some(source_id) = self.source_of(tile) else { return };
        self.motion_seq = self.motion_seq.wrapping_add(1);
        let d = Datagram::PointerMotion { source_id, seq: self.motion_seq, x, y };
        let _ = self.conn.send_datagram(d.encode().into());
    }
}

async fn reader(
    inner: Arc<Inner>,
    agent: AgentId,
    mut crx: rz_net::control::ControlReceiver,
    pending: Pending,
    streams: Arc<Mutex<HashMap<u32, TileId>>>,
    sync: Arc<Mutex<ClockSync>>,
    unlock: mpsc::UnboundedSender<()>,
) {
    let mut locked = false;
    while let Ok(Some(env)) = crx.recv().await {
        let Some(msg) = env.msg else { continue };
        if env.reply_to != 0
            && let Some(tx) = pending.lock().unwrap().remove(&env.reply_to)
        {
            let _ = tx.send(msg);
            continue;
        }
        match msg {
            Msg::Pong(p) => sync.lock().unwrap().add(sample_from_pong(&p)),
            Msg::AgentStatus(s) => {
                if locked && !s.screen_locked {
                    let _ = unlock.send(());
                }
                locked = s.screen_locked;
                inner.emit(Event::Status {
                    agent,
                    screen_recording: s.screen_recording,
                    accessibility: s.accessibility,
                    screen_locked: s.screen_locked,
                    display_asleep: s.display_asleep,
                });
            }
            Msg::StreamStats(s) => {
                let tile = streams.lock().unwrap().get(&s.stream_id).copied();
                if let Some(t) = tile.and_then(|t| inner.tiles.lock().unwrap().get(&t).cloned()) {
                    t.agent_fps.store(s.fps, std::sync::atomic::Ordering::Relaxed);
                    t.agent_dropped.store(s.dropped_before_encode, std::sync::atomic::Ordering::Relaxed);
                }
            }
            Msg::CursorShape(c) => inner.emit(Event::CursorShape {
                agent,
                shape_id: c.shape_id,
                kind: c.kind,
                png_base64: base64(&c.png),
                hotspot_x: c.hotspot_x,
                hotspot_y: c.hotspot_y,
            }),
            Msg::Error(e) => tracing::warn!(agent, code = e.code, "agent error: {}", e.message),
            other => tracing::debug!("unhandled {other:?}"),
        }
    }
}

async fn pinger(tx: Arc<tokio::sync::Mutex<ControlSender>>) {
    // A short burst first for a quick clock offset, then the keep-alive rate.
    for _ in 0..5 {
        if tx.lock().await.send(ping_now()).await.is_err() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mut iv = tokio::time::interval(PING_EVERY);
    loop {
        iv.tick().await;
        if tx.lock().await.send(ping_now()).await.is_err() {
            return;
        }
    }
}

async fn video_rx(
    inner: Arc<Inner>,
    conn: Connection,
    streams: Arc<Mutex<HashMap<u32, TileId>>>,
    sync: Arc<Mutex<ClockSync>>,
) {
    while let Ok(mut s) = conn.accept_uni().await {
        let (inner, streams, sync) = (inner.clone(), streams.clone(), sync.clone());
        tokio::spawn(async move {
            let hdr = match video::recv_header(&mut s).await {
                Ok(h) => h,
                Err(e) => return tracing::debug!("frame header: {e:#}"),
            };
            let tile = streams.lock().unwrap().get(&hdr.stream_id).copied();
            let Some(tile) = tile.and_then(|t| inner.tiles.lock().unwrap().get(&t).cloned()) else {
                let _ = s.stop(0u32.into());
                return;
            };
            if !tile.wants(&hdr) {
                // Plan 6.4: the reorder already skipped past this frame (or waits for a keyframe).
                let _ = s.stop(0u32.into());
                return;
            }
            match video::recv_payload(&mut s, &hdr).await {
                Ok(payload) => {
                    let _ = tile.frames.send(Incoming { hdr, payload, recv_ns: now_ns(), sync });
                }
                Err(e) => tracing::debug!("frame {}: {e:#}", hdr.frame_no),
            }
        });
    }
}

async fn datagram_rx(inner: Arc<Inner>, agent: AgentId, conn: Connection) {
    while let Ok(d) = conn.read_datagram().await {
        if let Some(Datagram::CursorPos { source_id, seq, x, y, shape_id, visible }) = Datagram::decode(&d) {
            for t in inner.tiles_of(agent) {
                if t.params.lock().unwrap().source_id != source_id {
                    continue;
                }
                let mut c = t.cursor.lock().unwrap();
                if c.seq == 0 || seq_newer(seq, c.seq) {
                    *c = crate::tile::CursorState { x, y, visible, shape_id, seq };
                }
            }
        }
    }
}
