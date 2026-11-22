//! One stream: capture → (throttle) → VideoToolbox H.264 → one QUIC uni stream per frame.
//!
//! Backpressure (plan 6.4, adjusted in Phase 0, DECISIONS.md): more than 2 encoded-but-unsent frames → the next
//! captured frame is dropped *before* encoding and the bitrate drops by 20 %, recovering by 5 %/s. More than 3
//! frames inside the encoder → drop without touching the bitrate (encoder overload, not network).
//! Throttle ([`Throttle`]): SCK delivers up to the display rate; frames are taken on an ideal fps grid with
//! ¼-interval tolerance.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use rz_capture::Capture;
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

    pub fn accept(&mut self, ts: u64) -> bool {
        let i = self.interval;
        if let Some(due) = self.due
            && ts + i / 4 < due
        {
            return false;
        }
        let base = self.due.map_or(ts + i, |d| d + i);
        self.due = Some(base.max(ts + i / 2));
        true
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
        });
        let tasks = vec![
            tokio::spawn(sender(conn.clone(), rx, shared.clone(), stream_id)),
            tokio::spawn(rate_and_stats(shared.clone(), stream_id, ctl)),
        ];

        let sh = shared.clone();
        let mut input_no = 0u64;
        let mut throttle = Throttle::new(fps);
        capture.start(Box::new(move |frame| {
            if !throttle.accept(frame.capture_ts_ns) {
                return;
            }
            let mut guard = sh.encoder.lock().unwrap();
            let Some(enc) = guard.as_mut() else { return };
            if sh.unsent.load(Ordering::Acquire) > 2 {
                sh.dropped_before_encode.fetch_add(1, Ordering::Relaxed);
                let kbps = sh.rate.lock().unwrap().on_drop();
                let _ = enc.set_bitrate_kbps(kbps);
                return;
            }
            if sh.in_encoder.load(Ordering::Acquire) > 3 {
                sh.dropped_encoder_busy.fetch_add(1, Ordering::Relaxed);
                return;
            }
            let kf = sh.force_keyframe.swap(false, Ordering::AcqRel);
            sh.in_encoder.fetch_add(1, Ordering::AcqRel);
            if let Err(e) = enc.encode(&frame, input_no, kf) {
                tracing::warn!("encode: {e}");
                sh.in_encoder.fetch_sub(1, Ordering::AcqRel);
                sh.force_keyframe.store(kf, Ordering::Release);
            }
            input_no += 1;
        }))?;
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
    use super::Throttle;

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
    fn pause_then_resume() {
        let mut t = Throttle::new(60);
        assert!(t.accept(0));
        assert!(t.accept(5000 * MS), "after a pause the next frame goes through");
        assert!(!t.accept(5000 * MS + 4 * MS), "but not a burst right behind it");
        assert!(t.accept(5000 * MS + 17 * MS));
    }
}
