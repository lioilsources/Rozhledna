//! Viewer client core, shared by the native spike viewer (T0.4) and the Flutter spike (T0.6).
//!
//! Runs networking on its own tokio runtime thread and decoding on a dedicated thread.
//! Decoded frames are handed to a callback together with a latency record (timestamps mapped into the
//! local clock via Ping/Pong clock sync). The callback fills in `present_ns`.

pub mod reorder;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use rz_codec::videotoolbox::VtDecoder;
use rz_codec::{DecodedFrame, Decoder};
use rz_latency::FrameRecord;
use rz_net::control::{Control, ping_now, sample_from_pong};
use rz_proto::clock::{ClockSync, now_ns};
use rz_proto::frame::FrameHeader;
use rz_proto::pb::{self, envelope::Msg};

pub use reorder::{Reorder, ReorderEvent};

#[derive(Debug, Clone)]
pub struct ClientOptions {
    pub addr: SocketAddr,
    pub max_w: u32,
    pub max_h: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub name: String,
}

/// Live numbers for overlays.
#[derive(Debug, Clone, Default)]
pub struct LiveStats {
    pub stream: Option<pb::SubscribeAck>,
    pub agent: Option<pb::StreamStats>,
    pub min_rtt_ns: Option<i64>,
    pub clock_offset_ns: Option<i64>,
    pub keyframe_requests: u32,
    pub frames_skipped: u32,
    pub error: Option<String>,
}

pub type DecodedCallback = Box<dyn FnMut(DecodedFrame, FrameRecord) + Send + 'static>;

pub struct Client {
    stop: Arc<AtomicBool>,
    pub stats: Arc<Mutex<LiveStats>>,
    net: Option<JoinHandle<()>>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Client {
    pub fn spawn(opts: ClientOptions, on_decoded: DecodedCallback) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Mutex::new(LiveStats::default()));
        let (sd_tx, sd_rx) = tokio::sync::oneshot::channel();
        let (st, s2) = (stop.clone(), stats.clone());
        let net = std::thread::Builder::new().name("rz-net".into()).spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
            let r = rt.block_on(async {
                tokio::select! {
                    r = run(opts, on_decoded, s2.clone(), st.clone()) => r,
                    _ = sd_rx => Ok(()),
                }
            });
            if let Err(e) = r {
                tracing::error!("client: {e:#}");
                s2.lock().unwrap().error = Some(format!("{e:#}"));
            }
            st.store(true, Ordering::SeqCst);
        })?;
        Ok(Self { stop, stats, net: Some(net), shutdown: Some(sd_tx) })
    }

    pub fn is_stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(s) = self.shutdown.take() {
            let _ = s.send(());
        }
        if let Some(t) = self.net.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Incoming {
    hdr: FrameHeader,
    payload: Vec<u8>,
    recv_ns: u64,
}

async fn run(
    opts: ClientOptions,
    on_decoded: DecodedCallback,
    stats: Arc<Mutex<LiveStats>>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let ep = rz_net::client_endpoint(true)?;
    let conn = rz_net::connect(&ep, opts.addr).await?;
    tracing::info!(addr = %opts.addr, "connected");
    let (send, recv) = conn.open_bi().await?;
    let mut ctl = Control::new(send, recv);
    ctl.send(Msg::Hello(pb::Hello {
        protocol: rz_proto::PROTOCOL_VERSION,
        node_id: vec![0; 16],
        name: opts.name.clone(),
        os: pb::Os::Macos as i32,
        os_version: String::new(),
        app_version: env!("CARGO_PKG_VERSION").into(),
        is_agent: false,
        is_hub: true,
        encode_codecs: vec![],
        decode_codecs: vec![pb::Codec::H264 as i32],
        features: vec![],
    }))
    .await?;
    ctl.send(Msg::Subscribe(pb::Subscribe {
        source_id: 0,
        tier: pb::Tier::Focus as i32,
        max_w: opts.max_w,
        max_h: opts.max_h,
        max_fps: opts.fps,
        codec: pb::Codec::H264 as i32,
        bitrate_kbps: opts.bitrate_kbps,
    }))
    .await?;
    let ack = loop {
        match ctl.recv().await?.and_then(|e| e.msg) {
            Some(Msg::SubscribeAck(a)) => break a,
            Some(Msg::Error(e)) => bail!("agent refused subscribe: {}", e.message),
            Some(_) => continue,
            None => bail!("agent closed control stream"),
        }
    };
    tracing::info!(stream = ack.stream_id, w = ack.w, h = ack.h, fps = ack.fps, "subscribed");
    stats.lock().unwrap().stream = Some(ack);
    let (mut ctl_tx, mut ctl_rx) = ctl.split();

    let sync = Arc::new(Mutex::new(ClockSync::default()));
    let (kf_tx, mut kf_rx) = tokio::sync::mpsc::unbounded_channel::<u64>();

    // Control reader: Pongs → clock sync, StreamStats → overlay.
    let (sy, st) = (sync.clone(), stats.clone());
    let reader = tokio::spawn(async move {
        while let Ok(Some(env)) = ctl_rx.recv().await {
            match env.msg {
                Some(Msg::Pong(p)) => {
                    let mut s = sy.lock().unwrap();
                    s.add(sample_from_pong(&p));
                    let mut ls = st.lock().unwrap();
                    ls.min_rtt_ns = s.min_rtt_ns();
                    ls.clock_offset_ns = s.offset_ns();
                }
                Some(Msg::StreamStats(s)) => st.lock().unwrap().agent = Some(s),
                _ => {}
            }
        }
    });
    // Control writer: pings (burst of 5, then every 2 s) and keyframe requests.
    let stream_id = ack.stream_id;
    let writer = tokio::spawn(async move {
        let start = Instant::now();
        let mut n = 0u32;
        loop {
            let wait = if n < 5 { Duration::from_millis(50) } else { Duration::from_secs(2) };
            tokio::select! {
                _ = tokio::time::sleep(wait) => {
                    n += 1;
                    if ctl_tx.send(ping_now()).await.is_err() { break; }
                }
                Some(last) = kf_rx.recv() => {
                    let rk = pb::RequestKeyframe { stream_id, last_frame_no: last };
                    if ctl_tx.send(Msg::RequestKeyframe(rk)).await.is_err() { break; }
                }
            }
            let _ = start;
        }
    });

    // Decode thread.
    let (in_tx, in_rx) = mpsc::channel::<Incoming>();
    let interval = Duration::from_secs_f64(1.0 / ack.fps.max(1) as f64);
    let (sy, st, stp) = (sync.clone(), stats.clone(), stop.clone());
    let decode = std::thread::Builder::new()
        .name("rz-decode".into())
        .spawn(move || decode_loop(in_rx, interval, on_decoded, sy, st, stp, kf_tx))?;

    // Video: one uni stream per frame.
    let result = loop {
        if stop.load(Ordering::Relaxed) {
            break Ok(());
        }
        let s = match conn.accept_uni().await {
            Ok(s) => s,
            Err(e) => break Err(e).context("connection"),
        };
        let tx = in_tx.clone();
        tokio::spawn(async move {
            match rz_net::video::recv_frame(s).await {
                Ok((hdr, payload)) => {
                    let _ = tx.send(Incoming { hdr, payload, recv_ns: now_ns() });
                }
                Err(e) => tracing::debug!("frame stream: {e:#}"),
            }
        });
    };
    drop(in_tx);
    reader.abort();
    writer.abort();
    stop.store(true, Ordering::SeqCst);
    let _ = decode.join();
    result
}

fn decode_loop(
    rx: mpsc::Receiver<Incoming>,
    interval: Duration,
    mut on_decoded: DecodedCallback,
    sync: Arc<Mutex<ClockSync>>,
    stats: Arc<Mutex<LiveStats>>,
    stop: Arc<AtomicBool>,
    kf_tx: tokio::sync::mpsc::UnboundedSender<u64>,
) {
    let mut dec = VtDecoder::new();
    let mut reorder: Reorder<Incoming> = Reorder::new(interval);
    let mut last_kf_req: Option<Instant> = None;
    while !stop.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_millis(5)) {
            Ok(inc) => {
                let (n, key, disc) = (inc.hdr.frame_no, inc.hdr.is_keyframe(), inc.hdr.flags & 2 != 0);
                reorder.push(n, key, disc, inc, Instant::now());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        for ev in reorder.poll(Instant::now()) {
            match ev {
                ReorderEvent::Ready(inc) => {
                    let decoded = match dec.decode(&inc.payload) {
                        Ok(Some(d)) => d,
                        Ok(None) => continue,
                        Err(e) => {
                            tracing::warn!("decode frame {}: {e:#}", inc.hdr.frame_no);
                            reorder.need_keyframe();
                            continue;
                        }
                    };
                    let done = now_ns();
                    let s = sync.lock().unwrap();
                    let (Some(cap), Some(enc)) =
                        (s.remote_to_local(inc.hdr.capture_ts_ns), s.remote_to_local(inc.hdr.encode_done_ts_ns))
                    else {
                        continue; // not synced yet (first ~50 ms)
                    };
                    drop(s);
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
                    on_decoded(decoded, rec);
                }
                ReorderEvent::Skipped(n) => stats.lock().unwrap().frames_skipped += n,
                ReorderEvent::RequestKeyframe(last) => {
                    // Rate-limit: one request per 250 ms (a 4K IDR needs time to encode + transfer).
                    if last_kf_req.is_none_or(|t| t.elapsed() > Duration::from_millis(250)) {
                        last_kf_req = Some(Instant::now());
                        stats.lock().unwrap().keyframe_requests += 1;
                        let _ = kf_tx.send(last);
                    }
                }
            }
        }
    }
}
