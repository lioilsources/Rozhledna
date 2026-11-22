//! T0.2: VideoToolbox encode/decode timing for 1080p, 1440p, 4K (testpattern, real-time pacing).
//!
//! `cargo run --release -p rz-codec --example vt-bench -- [--fps 60] [--frames 300] [--bpp 0.12]`

use std::sync::mpsc;
use std::time::{Duration, Instant};

use clap::Parser;
use rz_capture::pixbuf::PixelBuffer;
use rz_capture::testpattern::{add_noise, decode_frame_no, render};
use rz_capture::{Frame, FrameBuffer, Nv12Image};
use rz_codec::videotoolbox::{VtDecoder, VtEncoder};
use rz_codec::{Decoder, Encoder, EncoderConfig};
use rz_latency::summarize;
use rz_proto::clock::now_ns;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value_t = 60)]
    fps: u32,
    #[arg(long, default_value_t = 300)]
    frames: u64,
    /// Bits per pixel for the auto bitrate (plan 6.5: 0.12 FOCUS, 0.06 LIVE).
    #[arg(long, default_value_t = 0.12)]
    bpp: f64,
    /// Only these resolutions, e.g. 2560x1440 (repeatable).
    #[arg(long)]
    res: Vec<String>,
    /// Fraction of the frame covered by fresh random noise each frame (0 = plain testpattern).
    #[arg(long, default_value_t = 0.0)]
    noise: f64,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("warn").init();
    let a = Args::parse();
    let mut sizes = vec![(1920u32, 1080u32), (2560, 1440), (3840, 2160)];
    if !a.res.is_empty() {
        sizes = a
            .res
            .iter()
            .map(|r| {
                let (w, h) = r.split_once('x').expect("WxH");
                (w.parse().unwrap(), h.parse().unwrap())
            })
            .collect();
    }
    println!("VideoToolbox H.264, {} fps, {} frames, {} bpp, noise {}", a.fps, a.frames, a.bpp, a.noise);
    println!(
        "{:>10} {:>8} | {:>7} {:>7} {:>7} | {:>7} {:>7} {:>7} | {:>8} {:>7} {:>6} {:>8}",
        "res",
        "Mb/s tgt",
        "enc P50",
        "enc P95",
        "enc max",
        "dec P50",
        "dec P95",
        "dec max",
        "Mb/s out",
        "IDR kB",
        "ok",
        "enc fps"
    );
    for (w, h) in sizes {
        bench(w, h, &a)?;
    }
    Ok(())
}

fn bench(w: u32, h: u32, a: &Args) -> anyhow::Result<()> {
    let mut cfg = EncoderConfig { width: w, height: h, fps: a.fps, bitrate_kbps: 0 };
    cfg.bitrate_kbps = cfg.auto_bitrate_kbps(a.bpp);
    let (tx, rx) = mpsc::channel();
    let mut enc = VtEncoder::new(cfg, Box::new(move |f| tx.send(f).unwrap()))?;
    // Pre-render a loop of frames so pattern rendering does not distort timing.
    let pool: Vec<PixelBuffer> = (0..a.fps as u64)
        .map(|n| {
            let mut img = Nv12Image::new(w, h);
            render(&mut img, n, 0);
            if a.noise > 0.0 {
                add_noise(&mut img, n + 1, a.noise);
            }
            PixelBuffer::from_nv12(&img)
        })
        .collect::<Result<_, _>>()?;
    let interval = Duration::from_secs_f64(1.0 / a.fps as f64);
    let t0 = Instant::now();
    for n in 0..a.frames {
        if let Some(d) = (t0 + interval * n as u32).checked_duration_since(Instant::now()) {
            std::thread::sleep(d);
        }
        let pb = &pool[(n % pool.len() as u64) as usize];
        let f = Frame { capture_ts_ns: now_ns(), width: w, height: h, buffer: FrameBuffer::PixelBuffer(pb.clone()) };
        enc.encode(&f, n, n == 0)?;
    }
    enc.flush()?;
    let enc_fps = a.frames as f64 / t0.elapsed().as_secs_f64();
    drop(enc);
    let frames: Vec<_> = rx.iter().filter_map(rz_codec::EncoderOutput::frame).collect();

    let mut dec = VtDecoder::new();
    let mut dec_ms = Vec::new();
    let mut ok = 0;
    for f in &frames {
        let t = now_ns();
        if let Some(d) = dec.decode(&f.data)? {
            dec_ms.push((now_ns() - t) as f64 / 1e6);
            let expect = (f.frame_no % pool.len() as u64) as u32;
            if d.pixel_buffer.with_luma(|l| decode_frame_no(l)) == Some(expect) {
                ok += 1;
            }
        }
    }
    let enc = summarize(frames.iter().map(|f| (f.encode_done_ts_ns - f.capture_ts_ns) as f64 / 1e6).collect());
    let decs = summarize(dec_ms);
    let bytes: usize = frames.iter().map(|f| f.data.len()).sum();
    let secs = a.frames as f64 / a.fps as f64;
    let idr = frames.iter().find(|f| f.keyframe).map_or(0, |f| f.data.len());
    println!(
        "{:>10} {:>8.1} | {:>7.2} {:>7.2} {:>7.2} | {:>7.2} {:>7.2} {:>7.2} | {:>8.1} {:>7} {:>3}/{} {:>8.1}",
        format!("{w}x{h}"),
        cfg.bitrate_kbps as f64 / 1000.0,
        enc.p50,
        enc.p95,
        enc.max,
        decs.p50,
        decs.p95,
        decs.max,
        bytes as f64 * 8.0 / secs / 1e6,
        idr / 1024,
        ok,
        frames.len(),
        enc_fps
    );
    Ok(())
}
