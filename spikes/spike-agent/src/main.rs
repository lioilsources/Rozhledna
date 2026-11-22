//! Phase 0 spike agent (T0.4): capture → VideoToolbox H.264 → QUIC, one uni stream per frame.
//!
//! Serves one hub at a time. Protocol subset: Hello/HelloAck, Subscribe/SubscribeAck,
//! RequestKeyframe, Unsubscribe, Ping/Pong, StreamStats. No pairing (`--insecure-dev` only).
//!
//! Backpressure (plan 6.4): if more than 2 encoded frames are not yet handed to QUIC, the next captured
//! frame is dropped *before* encoding and the bitrate drops by 20 %; when calm it recovers by 5 %/s
//! towards the target. Frames still inside the encoder are counted separately (a 4K encode takes
//! ~22 ms on M1 Pro, i.e. >1 frame at 60 Hz): more than 3 there drops the frame without touching the
//! bitrate, since it is encoder overload, not network congestion (DECISIONS.md, 2026-11-22).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use rz_capture::Capture;
use rz_capture::testpattern::TestPattern;
use rz_codec::videotoolbox::VtEncoder;
use rz_codec::{Encoder, EncoderConfig, EncoderOutput};
use rz_net::control::{Control, ControlSender, pong_for};
use rz_net::{Connection, video};
use rz_proto::clock::now_ns;
use rz_proto::frame::{CODEC_H264, FLAG_DISCONTINUITY, FLAG_KEYFRAME, FrameHeader};
use rz_proto::pb::{self, envelope::Msg};
use tokio::sync::mpsc;

#[derive(Copy, Clone, ValueEnum, Debug)]
enum SourceKind {
    /// ScreenCaptureKit display capture (needs Screen Recording permission).
    Sck,
    /// Synthetic moving pattern with burned-in frame number (no permissions).
    Testpattern,
}

#[derive(Parser, Debug)]
#[command(about = "Rozhledna Phase 0 spike agent")]
struct Args {
    #[arg(long, default_value = "0.0.0.0:47470")]
    bind: SocketAddr,
    #[arg(long, value_enum, default_value = "sck")]
    source: SourceKind,
    /// CGDirectDisplayID (default: main display). See --list-displays.
    #[arg(long)]
    display: Option<u32>,
    /// Upper bound for the stream size (the hub's Subscribe can lower it).
    #[arg(long, default_value_t = 3840)]
    max_w: u32,
    #[arg(long, default_value_t = 2160)]
    max_h: u32,
    #[arg(long, default_value_t = 60)]
    fps: u32,
    /// 0 = auto (0.12 bpp).
    #[arg(long, default_value_t = 0)]
    bitrate_kbps: u32,
    /// SCK `minimumFrameInterval` = 1/N s (experiment T0.x fps); default = --fps, 0 = no limit.
    #[arg(long)]
    sck_interval_hz: Option<u32>,
    /// Testpattern size.
    #[arg(long, default_value = "2560x1440")]
    tp_size: String,
    /// Required in Phase 0: no pairing, accept any hub.
    #[arg(long)]
    insecure_dev: bool,
    #[arg(long)]
    list_displays: bool,
    /// Log to a file (useful when started from the .app bundle).
    #[arg(long)]
    log_file: Option<std::path::PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    init_logging(args.log_file.as_deref())?;
    if args.list_displays {
        for d in rz_capture::macos_sck::list_displays()? {
            println!(
                "display {}: {}x{} pt, {}x{} px, {:.0} Hz",
                d.id, d.width, d.height, d.pixel_width, d.pixel_height, d.refresh_hz
            );
        }
        return Ok(());
    }
    if !args.insecure_dev {
        bail!("Phase 0 has no pairing yet: run with --insecure-dev (trusted LAN only)");
    }
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(serve(args))
}

fn init_logging(file: Option<&std::path::Path>) -> Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let b = tracing_subscriber::fmt().with_env_filter(filter);
    match file {
        Some(p) => b.with_ansi(false).with_writer(Mutex::new(std::fs::File::create(p)?)).init(),
        None => b.init(),
    }
    Ok(())
}

async fn serve(args: Args) -> Result<()> {
    let (sr, ax) = rz_capture::macos_sck::permissions();
    tracing::info!(screen_recording = sr, accessibility = ax, "TCC state");
    if matches!(args.source, SourceKind::Sck) && !sr {
        tracing::warn!(
            "Screen Recording not granted for this process (or its responsible terminal); SCK capture will fail. \
             Grant it in System Settings → Privacy & Security, or use --source testpattern"
        );
    }
    let ep = rz_net::server_endpoint(args.bind)?;
    let args = Arc::new(args);
    while let Some(incoming) = ep.accept().await {
        let conn = match incoming.await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("handshake failed: {e}");
                continue;
            }
        };
        let peer = conn.remote_address();
        tracing::info!(%peer, "hub connected");
        // One hub at a time in the spike: serve it to completion.
        if let Err(e) = session(conn, args.clone()).await {
            tracing::warn!(%peer, "session ended: {e:#}");
        } else {
            tracing::info!(%peer, "session closed");
        }
    }
    Ok(())
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
    /// Called every `dt`; returns a new bitrate if it changed.
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
    /// Submitted to the encoder, not yet out of it.
    in_encoder: AtomicUsize,
    /// Encoded, not yet handed to QUIC.
    unsent: AtomicUsize,
    dropped_encoder_busy: AtomicU32,
    dropped_before_encode: AtomicU32,
    dropped_by_encoder: AtomicU32,
    sent: AtomicU32,
    rate: Mutex<RateCtl>,
}

struct Pipeline {
    capture: Box<dyn Capture>,
    shared: Arc<Shared>,
    stream_id: u32,
    tasks: Vec<tokio::task::JoinHandle<()>>,
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

fn parse_size(s: &str) -> Result<(u32, u32)> {
    let (w, h) = s.split_once('x').context("size must be WxH")?;
    Ok((w.parse()?, h.parse()?))
}

fn start_pipeline(
    args: &Args,
    sub: &pb::Subscribe,
    stream_id: u32,
    conn: &Connection,
    ctl: Arc<tokio::sync::Mutex<ControlSender>>,
) -> Result<(Pipeline, pb::SubscribeAck)> {
    let max_w = if sub.max_w > 0 { sub.max_w.min(args.max_w) } else { args.max_w };
    let max_h = if sub.max_h > 0 { sub.max_h.min(args.max_h) } else { args.max_h };
    let fps = if sub.max_fps > 0 { sub.max_fps.min(args.fps) } else { args.fps };
    let mut capture: Box<dyn Capture> = match args.source {
        SourceKind::Sck => {
            let opts = rz_capture::macos_sck::ScOptions { min_interval_hz: args.sck_interval_hz, ..Default::default() };
            Box::new(rz_capture::macos_sck::ScCapture::display(args.display, max_w, max_h, fps, opts)?)
        }
        SourceKind::Testpattern => {
            let (w, h) = parse_size(&args.tp_size)?;
            let (w, h) = rz_capture::macos_sck::fit(w, h, max_w, max_h);
            Box::new(TestPattern::new(w, h, fps))
        }
    };
    let (w, h) = capture.size();
    let bitrate = if sub.bitrate_kbps > 0 { sub.bitrate_kbps } else { args.bitrate_kbps };
    let cfg = EncoderConfig { width: w, height: h, fps, bitrate_kbps: bitrate };
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
        tokio::spawn(rate_and_stats(shared.clone(), stream_id, fps, ctl)),
    ];

    // Capture callback: runs on the capture thread / SCK queue.
    let sh = shared.clone();
    let mut input_no = 0u64;
    capture.start(Box::new(move |frame| {
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

    let ack = pb::SubscribeAck { stream_id, w, h, fps, codec: pb::Codec::H264 as i32 };
    tracing::info!(stream_id, w, h, fps, kbps = target, "streaming");
    Ok((Pipeline { capture, shared, stream_id, tasks }, ack))
}

/// Assigns contiguous wire frame numbers after encoding and sends each frame on its own uni stream.
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
        // Each frame is independent; a slow frame must not block the next one.
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

async fn rate_and_stats(sh: Arc<Shared>, stream_id: u32, _fps: u32, ctl: Arc<tokio::sync::Mutex<ControlSender>>) {
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

async fn session(conn: Connection, args: Arc<Args>) -> Result<()> {
    let (send, recv) = conn.accept_bi().await?;
    let (ctl_tx, mut ctl_rx) = Control::new(send, recv).split();
    let ctl = Arc::new(tokio::sync::Mutex::new(ctl_tx));
    let mut pipeline: Option<Pipeline> = None;
    let mut next_stream_id = 1u32;
    while let Some(env) = ctl_rx.recv().await? {
        let t1 = now_ns();
        let Some(msg) = env.msg else { continue };
        match msg {
            Msg::Ping(p) => {
                ctl.lock().await.reply(env.seq, pong_for(&p, t1)).await?;
            }
            Msg::Hello(h) => {
                tracing::info!(name = h.name, os = h.os, app = h.app_version, "hello");
                let ack = pb::HelloAck { paired: true, granted: vec!["view".into()] };
                ctl.lock().await.reply(env.seq, Msg::HelloAck(ack)).await?;
            }
            Msg::ListSources(_) => {
                let list = pb::SourceList { sources: vec![] }; // Phase 1
                ctl.lock().await.reply(env.seq, Msg::SourceList(list)).await?;
            }
            Msg::Subscribe(sub) => {
                pipeline = None; // one stream in the spike
                let id = next_stream_id;
                next_stream_id += 1;
                match start_pipeline(&args, &sub, id, &conn, ctl.clone()) {
                    Ok((p, ack)) => {
                        pipeline = Some(p);
                        ctl.lock().await.reply(env.seq, Msg::SubscribeAck(ack)).await?;
                    }
                    Err(e) => {
                        tracing::error!("subscribe failed: {e:#}");
                        let err = pb::Error { code: 1, message: format!("{e:#}") };
                        ctl.lock().await.reply(env.seq, Msg::Error(err)).await?;
                    }
                }
            }
            Msg::RequestKeyframe(rk) => {
                if let Some(p) = &pipeline
                    && p.stream_id == rk.stream_id
                {
                    tracing::debug!(after = rk.last_frame_no, "keyframe requested");
                    p.shared.force_keyframe.store(true, Ordering::Release);
                }
            }
            Msg::Unsubscribe(u) => {
                if pipeline.as_ref().is_some_and(|p| p.stream_id == u.stream_id) {
                    pipeline = None;
                }
            }
            other => tracing::debug!("ignored {other:?}"),
        }
    }
    drop(pipeline);
    Ok(())
}
