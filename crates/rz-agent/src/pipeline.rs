//! One stream: capture → (throttle) → VideoToolbox H.264 → one QUIC uni stream per frame.
//!
//! Backpressure (plan 6.4, adjusted in Phase 0, DECISIONS.md): more than 2 encoded-but-unsent frames → the next
//! captured frame is dropped *before* encoding and the bitrate drops by 20 %, recovering by 5 %/s. More than 3
//! frames inside the encoder → drop without touching the bitrate (encoder overload, not network).
//! Throttle ([`Throttle`]): SCK delivers up to the display rate; frames are taken on an ideal fps grid with
//! ¼-interval tolerance.
//!
//! Trailing frame: SCK only delivers frames when the content changes, so a frame dropped by the throttle or by
//! backpressure may be the *last* state of the screen (a window switch, a keystroke). It is kept as `pending` and
//! encoded as soon as its slot comes, unless a newer frame replaces it — otherwise the hub would show the previous
//! state until the next change (dogfood: a stale video still after switching to a fullscreen terminal).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use rz_capture::{Capture, Frame};
use rz_codec::videotoolbox::VtEncoder;
use rz_codec::{Encoder, EncoderConfig, EncoderOutput};
use rz_net::control::ControlSender;
use rz_net::{Connection, video};
use rz_proto::frame::{CODEC_H264, FLAG_DISCONTINUITY, FLAG_KEYFRAME, FrameHeader};
use rz_proto::pb::{self, envelope::Msg};
use tokio::sync::mpsc;

use crate::status::DisplayAwake;

/// Limits frames to `fps` on an ideal time grid.
///
/// A frame is accepted when it is at most ¼ interval early for its slot; the next slot is then
/// `max(slot + interval, ts + interval/2)`. Effects: a 120 Hz source gives every second frame; a late frame
/// (sleep overshoot, vsync jitter) does not cause the next on-time frame to be dropped (a plain "≥ ¾ interval
/// since the previous frame" rule did: testpattern 56 fps instead of 60); after a pause the grid restarts.
#[derive(Debug)]
pub struct Throttle {
    interval: u64,
    due: Option<u64>,
}

impl Throttle {
    pub fn new(fps: u32) -> Self {
        Self { interval: 1_000_000_000 / fps.max(1) as u64, due: None }
    }

    /// Whether a frame at `ts` would be accepted (without taking the slot).
    pub fn ready(&self, ts: u64) -> bool {
        self.due.is_none_or(|due| ts + self.interval / 4 >= due)
    }

    pub fn accept(&mut self, ts: u64) -> bool {
        let i = self.interval;
        if !self.ready(ts) {
            return false;
        }
        let base = self.due.map_or(ts + i, |d| d + i);
        self.due = Some(base.max(ts + i / 2));
        true
    }
}

/// [`Throttle`] plus the trailing frame: a frame that misses its slot is kept and released by [`Self::flush`]
/// once a slot comes, unless a newer frame replaces it first.
pub struct Gate<T> {
    throttle: Throttle,
    pending: Option<T>,
}

impl<T> Gate<T> {
    pub fn new(fps: u32) -> Self {
        Self { throttle: Throttle::new(fps), pending: None }
    }

    /// A new frame: `Some` = encode it now, `None` = kept as pending.
    pub fn offer(&mut self, ts: u64, item: T) -> Option<T> {
        if self.throttle.accept(ts) {
            self.pending = None;
            Some(item)
        } else {
            self.pending = Some(item);
            None
        }
    }

    /// The pending frame, if its slot has come at `now`.
    pub fn flush(&mut self, now: u64) -> Option<T> {
        if self.pending.is_some() && self.throttle.accept(now) { self.pending.take() } else { None }
    }

    /// Backpressure refused `item`: keep it unless something newer is already pending.
    pub fn retry(&mut self, item: T) {
        if self.pending.is_none() {
            self.pending = Some(item);
        }
    }
}

/// Bitrate controller (plan 6.4).
struct RateCtl {
    target_kbps: u32,
    current_kbps: u32,
    last_drop: Instant,
}

impl RateCtl {
    fn on_drop(&mut self) -> u32 {
        self.current_kbps = ((self.current_kbps as f64 * 0.8) as u32).max(self.target_kbps / 10);
        self.last_drop = Instant::now();
        self.current_kbps
    }

    fn tick(&mut self, dt: Duration) -> Option<u32> {
        if self.current_kbps >= self.target_kbps || self.last_drop.elapsed() < Duration::from_secs(1) {
            return None;
        }
        let next = ((self.current_kbps as f64 * 1.05f64.powf(dt.as_secs_f64())).ceil() as u32).min(self.target_kbps);
        (next != self.current_kbps).then(|| {
            self.current_kbps = next;
            next
        })
    }
}

struct Shared {
    encoder: Mutex<Option<VtEncoder>>,
    force_keyframe: AtomicBool,
    in_encoder: AtomicUsize,
    unsent: AtomicUsize,
    dropped_encoder_busy: AtomicU32,
    dropped_before_encode: AtomicU32,
    dropped_by_encoder: AtomicU32,
    sent: AtomicU32,
    rate: Mutex<RateCtl>,
    /// Throttle and the newest frame not encoded yet (throttled or refused by backpressure); see the module docs.
    gate: Mutex<Gate<Frame>>,
    input_no: std::sync::atomic::AtomicU64,
}

impl Shared {
    /// Encodes `frame`, or gives it back when backpressure says not now.
    fn submit(&self, frame: Frame) -> Result<(), Frame> {
        let mut guard = self.encoder.lock().unwrap();
        let Some(enc) = guard.as_mut() else { return Ok(()) };
        if self.unsent.load(Ordering::Acquire) > 2 {
            self.dropped_before_encode.fetch_add(1, Ordering::Relaxed);
            let kbps = self.rate.lock().unwrap().on_drop();
            let _ = enc.set_bitrate_kbps(kbps);
            return Err(frame);
        }
        if self.in_encoder.load(Ordering::Acquire) > 3 {
            self.dropped_encoder_busy.fetch_add(1, Ordering::Relaxed);
            return Err(frame);
        }
        let kf = self.force_keyframe.swap(false, Ordering::AcqRel);
        self.in_encoder.fetch_add(1, Ordering::AcqRel);
        let n = self.input_no.fetch_add(1, Ordering::AcqRel);
        if let Err(e) = enc.encode(&frame, n, kf) {
            tracing::warn!("encode: {e}");
            self.in_encoder.fetch_sub(1, Ordering::AcqRel);
            self.force_keyframe.store(kf, Ordering::Release);
        }
        Ok(())
    }

    /// New frame from the capture: encode it if its slot has come, otherwise keep it as the pending one.
    fn on_frame(&self, frame: Frame) {
        let ts = frame.capture_ts_ns;
        let Some(f) = self.gate.lock().unwrap().offer(ts, frame) else { return };
        if let Err(f) = self.submit(f) {
            self.gate.lock().unwrap().retry(f);
        }
    }

    /// Encodes the pending frame once its slot has come (called periodically).
    fn flush_pending(&self, now: u64) {
        let Some(f) = self.gate.lock().unwrap().flush(now) else { return };
        if let Err(f) = self.submit(f) {
            self.gate.lock().unwrap().retry(f);
        }
    }
}

/// Effective stream settings (what the agent actually does).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StreamConfig {
    pub w: u32,
    pub h: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
}

pub struct Pipeline {
    capture: Box<dyn Capture>,
    shared: Arc<Shared>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    pub source_id: u64,
    pub config: StreamConfig,
    _awake: Arc<DisplayAwake>,
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        self.capture.stop();
        if let Ok(mut e) = self.shared.encoder.lock() {
            e.take();
        }
        for t in &self.tasks {
            t.abort();
        }
    }
}

impl Pipeline {
    /// Starts streaming `capture` (not yet started) as `stream_id`. `bitrate_kbps` 0 = auto (0.12 bpp).
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        mut capture: Box<dyn Capture>,
        source_id: u64,
        stream_id: u32,
        fps: u32,
        bitrate_kbps: u32,
        conn: &Connection,
        ctl: Arc<tokio::sync::Mutex<ControlSender>>,
        awake: Arc<DisplayAwake>,
    ) -> Result<Self> {
        let (w, h) = capture.size();
        let cfg = EncoderConfig { width: w, height: h, fps, bitrate_kbps };
        let target = cfg.effective_bitrate_kbps();
        let (tx, rx) = mpsc::unbounded_channel::<EncoderOutput>();
        let encoder = VtEncoder::new(cfg, Box::new(move |out| drop(tx.send(out))))?;
        let shared = Arc::new(Shared {
            encoder: Mutex::new(Some(encoder)),
            force_keyframe: AtomicBool::new(true),
            in_encoder: AtomicUsize::new(0),
            unsent: AtomicUsize::new(0),
            dropped_encoder_busy: AtomicU32::new(0),
            dropped_before_encode: AtomicU32::new(0),
            dropped_by_encoder: AtomicU32::new(0),
            sent: AtomicU32::new(0),
            rate: Mutex::new(RateCtl { target_kbps: target, current_kbps: target, last_drop: Instant::now() }),
            gate: Mutex::new(Gate::new(fps)),
            input_no: std::sync::atomic::AtomicU64::new(0),
        });
        let tasks = vec![
            tokio::spawn(sender(conn.clone(), rx, shared.clone(), stream_id)),
            tokio::spawn(rate_and_stats(shared.clone(), stream_id, ctl)),
            tokio::spawn(flush_pending(shared.clone(), fps)),
        ];

        let sh = shared.clone();
        capture.start(Box::new(move |frame| sh.on_frame(frame)))?;
        let config = StreamConfig { w, h, fps, bitrate_kbps: target };
        tracing::info!(stream_id, source_id, w, h, fps, kbps = target, "streaming");
        Ok(Self { capture, shared, tasks, source_id, config, _awake: awake })
    }

    pub fn request_keyframe(&self) {
        self.shared.force_keyframe.store(true, Ordering::Release);
    }
}

/// Assigns contiguous wire frame numbers after encoding (the encoder may drop frames itself) and sends each frame
/// on its own uni stream. Frame 0 carries DISCONTINUITY so the hub resets its reorder state.
async fn sender(conn: Connection, mut rx: mpsc::UnboundedReceiver<EncoderOutput>, sh: Arc<Shared>, stream_id: u32) {
    let mut wire_no = 0u64;
    while let Some(out) = rx.recv().await {
        let f = match out {
            EncoderOutput::Frame(f) => f,
            EncoderOutput::Dropped { .. } => {
                sh.dropped_by_encoder.fetch_add(1, Ordering::Relaxed);
                sh.in_encoder.fetch_sub(1, Ordering::AcqRel);
                continue;
            }
        };
        sh.in_encoder.fetch_sub(1, Ordering::AcqRel);
        sh.unsent.fetch_add(1, Ordering::AcqRel);
        let mut flags = if f.keyframe { FLAG_KEYFRAME } else { 0 };
        if wire_no == 0 {
            flags |= FLAG_DISCONTINUITY;
        }
        let hdr = FrameHeader {
            stream_id,
            frame_no: wire_no,
            capture_ts_ns: f.capture_ts_ns,
            encode_done_ts_ns: f.encode_done_ts_ns,
            flags,
            codec: CODEC_H264,
            width: f.width as u16,
            height: f.height as u16,
            payload_len: 0,
        };
        wire_no += 1;
        let (conn, sh) = (conn.clone(), sh.clone());
        tokio::spawn(async move {
            let prio = if hdr.is_keyframe() { 2 } else { 1 };
            if let Err(e) = video::send_frame(&conn, hdr, &f.data, prio).await {
                tracing::debug!("send frame {}: {e}", hdr.frame_no);
            } else {
                sh.sent.fetch_add(1, Ordering::Relaxed);
            }
            sh.unsent.fetch_sub(1, Ordering::AcqRel);
        });
    }
}

/// Sends the trailing frame: checks a few times per frame interval.
async fn flush_pending(sh: Arc<Shared>, fps: u32) {
    let period = Duration::from_secs_f64(1.0 / fps.max(1) as f64 / 4.0).max(Duration::from_millis(2));
    let mut iv = tokio::time::interval(period);
    iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        iv.tick().await;
        let sh = sh.clone();
        // The encoder call may block briefly; keep it off the async workers.
        let _ = tokio::task::spawn_blocking(move || sh.flush_pending(rz_proto::clock::now_ns())).await;
    }
}

async fn rate_and_stats(sh: Arc<Shared>, stream_id: u32, ctl: Arc<tokio::sync::Mutex<ControlSender>>) {
    let dt = Duration::from_millis(250);
    let mut iv = tokio::time::interval(dt);
    let mut last_stats = Instant::now();
    let mut last_sent = 0u32;
    loop {
        iv.tick().await;
        if let Some(kbps) = sh.rate.lock().unwrap().tick(dt)
            && let Some(e) = sh.encoder.lock().unwrap().as_mut()
        {
            let _ = e.set_bitrate_kbps(kbps);
        }
        if last_stats.elapsed() >= Duration::from_secs(1) {
            let secs = last_stats.elapsed().as_secs_f64();
            last_stats = Instant::now();
            let sent = sh.sent.load(Ordering::Relaxed);
            let fps = ((sent - last_sent) as f64 / secs).round() as u32;
            last_sent = sent;
            let stats = pb::StreamStats {
                stream_id,
                fps,
                bitrate_kbps: sh.rate.lock().unwrap().current_kbps,
                dropped_before_encode: sh.dropped_before_encode.load(Ordering::Relaxed)
                    + sh.dropped_encoder_busy.load(Ordering::Relaxed)
                    + sh.dropped_by_encoder.load(Ordering::Relaxed),
                encode_ms_p50_x100: 0,
            };
            if ctl.lock().await.send(Msg::StreamStats(stats)).await.is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Gate, Throttle};

    const MS: u64 = 1_000_000;

    fn run(fps: u32, ts_us: &[u64]) -> usize {
        let mut t = Throttle::new(fps);
        ts_us.iter().filter(|&&us| t.accept(us * 1000)).count()
    }

    #[test]
    fn source_120hz_gives_60() {
        let ts: Vec<u64> = (0..120).map(|i| i * 8333).collect();
        assert_eq!(run(60, &ts), 60);
    }

    #[test]
    fn jittery_60hz_keeps_all() {
        // ±2 ms jitter and one 6 ms late frame followed by an on-time one.
        let mut ts: Vec<u64> = (0..60).map(|i| i * 16667 + if i % 2 == 0 { 2000 } else { 0 }).collect();
        ts[30] += 6000;
        assert_eq!(run(60, &ts), 60);
    }

    #[test]
    fn trailing_frame_is_not_lost() {
        // A window switch: two changes 5 ms apart, then the screen stays still. The second (final) state must
        // still go out once its slot comes, without any further frame from the capture.
        let mut g = Gate::new(60);
        assert_eq!(g.offer(0, 'a'), Some('a'));
        assert_eq!(g.offer(5 * MS, 'b'), None, "too early, kept");
        assert_eq!(g.flush(8 * MS), None, "slot not here yet");
        assert_eq!(g.flush(13 * MS), Some('b'), "slot came: the final state goes out");
        assert_eq!(g.flush(40 * MS), None, "nothing more to send");
    }

    #[test]
    fn newer_frame_replaces_pending() {
        let mut g = Gate::new(60);
        assert_eq!(g.offer(0, 1), Some(1));
        assert_eq!(g.offer(3 * MS, 2), None);
        assert_eq!(g.offer(6 * MS, 3), None);
        assert_eq!(g.offer(17 * MS, 4), Some(4), "on-time frame wins, pending dropped");
        assert_eq!(g.flush(40 * MS), None);
        g.retry(5);
        assert_eq!(g.flush(40 * MS), Some(5), "refused by backpressure → sent later");
    }

    #[test]
    fn pause_then_resume() {
        let mut t = Throttle::new(60);
        assert!(t.accept(0));
        assert!(t.accept(5000 * MS), "after a pause the next frame goes through");
        assert!(!t.accept(5000 * MS + 4 * MS), "but not a burst right behind it");
        assert!(t.accept(5000 * MS + 17 * MS));
    }
}
