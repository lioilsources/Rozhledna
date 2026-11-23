//! One tile = one subscription. Owns a decode thread (reorder → VideoToolbox → sink) that survives reconnects:
//! after a reconnect the agent starts a new stream whose frame 0 carries DISCONTINUITY, which resets the reorder.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use rz_codec::{DecodedFrame, Decoder};
use rz_latency::FrameRecord;
use rz_proto::clock::{ClockSync, now_ns};
use rz_proto::frame::{FLAG_DISCONTINUITY, FrameHeader};
use rz_proto::pb;

use crate::events::{AgentId, Event, TileId};
use crate::reorder::{Reorder, ReorderEvent};

/// Called for every decoded frame (on the tile's decode thread). Keep it cheap: store the newest, notify the UI.
pub type FrameSink = Arc<dyn Fn(TileId, DecodedFrame, FrameRecord) + Send + Sync>;

/// What the UI asked for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileParams {
    pub source_id: u64,
    pub max_w: u32,
    pub max_h: u32,
    pub fps: u32,
    pub focus: bool,
    /// 0 = auto.
    pub bitrate_kbps: u32,
}

impl TileParams {
    pub fn subscribe(&self) -> pb::Subscribe {
        pb::Subscribe {
            source_id: self.source_id,
            tier: if self.focus { pb::Tier::Focus } else { pb::Tier::Live } as i32,
            max_w: self.max_w,
            max_h: self.max_h,
            max_fps: self.fps,
            codec: pb::Codec::H264 as i32,
            bitrate_kbps: self.bitrate_kbps,
        }
    }
}

/// Latest cursor on this tile's source (normalized).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CursorState {
    pub x: f32,
    pub y: f32,
    pub visible: bool,
    pub shape_id: u32,
    pub seq: u32,
}

pub struct Incoming {
    pub hdr: FrameHeader,
    pub payload: Vec<u8>,
    pub recv_ns: u64,
    pub sync: Arc<Mutex<ClockSync>>,
}

pub struct Tile {
    pub id: TileId,
    pub agent: AgentId,
    pub params: Mutex<TileParams>,
    pub frames: mpsc::Sender<Incoming>,
    /// Frames below are useless; set by the decode thread, read by the receive side to `stop()` streams early.
    floor: AtomicU64,
    floor_ts: AtomicU64,
    waiting_key: AtomicBool,
    pub cursor: Mutex<CursorState>,
    /// Agent-side stats (from `StreamStats`).
    pub agent_fps: AtomicU32,
    pub agent_dropped: AtomicU32,
    stop: Arc<AtomicBool>,
}

impl Tile {
    /// Whether a frame with this header is still worth reading.
    pub fn wants(&self, hdr: &FrameHeader) -> bool {
        if hdr.flags & FLAG_DISCONTINUITY != 0 {
            return true;
        }
        // Below the floor *and* not newer than what was released: really stale. A newer frame with a low number
        // belongs to a new epoch (reconnect / resize) whose first frame has not been processed yet.
        let ts = hdr.capture_ts_ns;
        if hdr.frame_no < self.floor.load(Ordering::Acquire) && (ts == 0 || ts <= self.floor_ts.load(Ordering::Acquire))
        {
            return false;
        }
        hdr.is_keyframe() || !self.waiting_key.load(Ordering::Acquire)
    }
}

impl Drop for Tile {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// How long to wait for a requested keyframe before asking again.
const KEYFRAME_RETRY_MS: u64 = 500;

/// Requests a keyframe for the tile from its agent.
pub type KeyframeFn = Box<dyn Fn(u64) + Send>;

pub fn spawn(
    id: TileId,
    agent: AgentId,
    params: TileParams,
    sink: FrameSink,
    events: tokio::sync::mpsc::UnboundedSender<Event>,
    request_keyframe: KeyframeFn,
) -> Arc<Tile> {
    let (tx, rx) = mpsc::channel::<Incoming>();
    let stop = Arc::new(AtomicBool::new(false));
    let tile = Arc::new(Tile {
        id,
        agent,
        params: Mutex::new(params),
        frames: tx,
        floor: AtomicU64::new(0),
        floor_ts: AtomicU64::new(0),
        waiting_key: AtomicBool::new(false),
        cursor: Mutex::new(CursorState::default()),
        agent_fps: AtomicU32::new(0),
        agent_dropped: AtomicU32::new(0),
        stop: stop.clone(),
    });
    let weak = Arc::downgrade(&tile);
    std::thread::Builder::new()
        .name(format!("rz-tile-{id}"))
        .spawn(move || decode_loop(id, rx, params.fps, sink, events, request_keyframe, weak, stop))
        .expect("spawn decode thread");
    tile
}

#[allow(clippy::too_many_arguments)]
fn decode_loop(
    id: TileId,
    rx: mpsc::Receiver<Incoming>,
    fps: u32,
    sink: FrameSink,
    events: tokio::sync::mpsc::UnboundedSender<Event>,
    request_keyframe: KeyframeFn,
    tile: std::sync::Weak<Tile>,
    stop: Arc<AtomicBool>,
) {
    #[cfg(target_os = "macos")]
    let mut dec = rz_codec::videotoolbox::VtDecoder::new();
    let interval = Duration::from_secs_f64(1.0 / fps.max(1) as f64);
    let mut reorder: Reorder<Incoming> = Reorder::new(interval);
    let mut last_kf_req: Option<Instant> = None;
    let mut stats = Stats::default();
    let mut stats_at = Instant::now();
    while !stop.load(Ordering::Acquire) {
        match rx.recv_timeout(Duration::from_millis(5)) {
            Ok(inc) => {
                let (n, key, disc) = (inc.hdr.frame_no, inc.hdr.is_keyframe(), inc.hdr.flags & FLAG_DISCONTINUITY != 0);
                stats.bytes += inc.payload.len() as u64;
                let ts = inc.hdr.capture_ts_ns;
                reorder.push_ts(n, key, disc, ts, inc, Instant::now());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        for ev in reorder.poll(Instant::now()) {
            match ev {
                ReorderEvent::Ready(inc) => {
                    #[cfg(target_os = "macos")]
                    let decoded = match dec.decode(&inc.payload) {
                        Ok(Some(d)) => d,
                        Ok(None) => continue,
                        Err(e) => {
                            tracing::warn!(tile = id, "decode frame {}: {e:#}", inc.hdr.frame_no);
                            reorder.need_keyframe();
                            continue;
                        }
                    };
                    #[cfg(not(target_os = "macos"))]
                    let decoded: DecodedFrame = {
                        let _ = &inc;
                        continue;
                    };
                    let done = now_ns();
                    let (cap, enc) = {
                        let s = inc.sync.lock().unwrap();
                        (
                            s.remote_to_local(inc.hdr.capture_ts_ns).unwrap_or(0),
                            s.remote_to_local(inc.hdr.encode_done_ts_ns).unwrap_or(0),
                        )
                    };
                    if cap > 0 {
                        stats.latency_ms.push(done.saturating_sub(cap) as f32 / 1e6);
                    }
                    stats.frames += 1;
                    let rec = FrameRecord {
                        frame_no: inc.hdr.frame_no,
                        keyframe: inc.hdr.is_keyframe(),
                        bytes: inc.payload.len() as u32,
                        width: inc.hdr.width,
                        height: inc.hdr.height,
                        capture_ns: cap,
                        encode_done_ns: enc,
                        recv_ns: inc.recv_ns,
                        decode_done_ns: done,
                        present_ns: 0,
                    };
                    sink(id, decoded, rec);
                }
                ReorderEvent::Skipped(n) => stats.skipped += n,
                ReorderEvent::RequestKeyframe(last) => {
                    // One request per 250 ms: a 4K IDR needs time to encode and transfer.
                    if last_kf_req.is_none_or(|t| t.elapsed() > Duration::from_millis(250)) {
                        last_kf_req = Some(Instant::now());
                        stats.keyframe_requests += 1;
                        request_keyframe(last);
                    }
                }
            }
        }
        // While waiting for a keyframe the receive side drops P-frames unread, so the reorder sees nothing and
        // would not ask again: if the IDR got lost (agent drop, stream reset) the tile froze for good. Re-ask.
        if reorder.waiting_for_keyframe()
            && last_kf_req.is_some_and(|t| t.elapsed() > Duration::from_millis(KEYFRAME_RETRY_MS))
        {
            last_kf_req = Some(Instant::now());
            stats.keyframe_requests += 1;
            request_keyframe(reorder.last_done());
        }
        let Some(t) = tile.upgrade() else { break };
        t.floor.store(reorder.floor(), Ordering::Release);
        t.floor_ts.store(reorder.last_done_ts(), Ordering::Release);
        t.waiting_key.store(reorder.waiting_for_keyframe(), Ordering::Release);
        if stats_at.elapsed() >= Duration::from_secs(1) {
            let dt = stats_at.elapsed().as_secs_f32();
            stats_at = Instant::now();
            if stats.frames > 0 || stats.bytes > 0 || stats.keyframe_requests > 0 {
                let ev = stats.event(id, dt, &t);
                let _ = events.send(ev);
            }
            stats = Stats::default();
        }
    }
}

#[derive(Default)]
struct Stats {
    frames: u32,
    bytes: u64,
    skipped: u32,
    keyframe_requests: u32,
    latency_ms: Vec<f32>,
}

impl Stats {
    fn event(&mut self, tile: TileId, dt: f32, t: &Tile) -> Event {
        self.latency_ms.sort_by(f32::total_cmp);
        let p = |q: f32| {
            if self.latency_ms.is_empty() {
                0.0
            } else {
                self.latency_ms[((self.latency_ms.len() - 1) as f32 * q).round() as usize]
            }
        };
        Event::TileStats {
            tile,
            fps: self.frames as f32 / dt,
            bitrate_kbps: (self.bytes as f32 * 8.0 / dt / 1000.0) as u32,
            latency_p50_ms: p(0.5),
            latency_p95_ms: p(0.95),
            skipped: self.skipped,
            keyframe_requests: self.keyframe_requests,
            agent_fps: t.agent_fps.load(Ordering::Relaxed),
            agent_dropped: t.agent_dropped.load(Ordering::Relaxed),
        }
    }
}
