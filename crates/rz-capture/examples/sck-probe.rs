//! T0.7 / T0.8 probe: ScreenCaptureKit window capture behaviour and TCC state.
//!
//! ```text
//! cargo run -p rz-capture --example sck-probe -- permissions
//! cargo run -p rz-capture --example sck-probe -- windows [--all]
//! cargo run -p rz-capture --example sck-probe -- window <ID> [--secs 3] [--children on|off] [--dump DIR]
//! cargo run -p rz-capture --example sck-probe -- display [--secs 3] [--dump DIR]
//! ```
//! `window` prints: frames received, first-frame delay, frame size, mean luma, share of black pixels.
//! With `--dump`, the last frame is written as a BMP so menus/sheets/popups can be checked by eye.
//! Results go to docs/probes/macos-window-capture.md.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use objc2_core_video::kCVPixelFormatType_32BGRA;
use rz_capture::macos_sck::{ScCapture, ScOptions, list_windows, permissions};
use rz_capture::pixbuf::PixelBuffer;
use rz_capture::{Capture, FrameBuffer};

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Copy, Clone, ValueEnum)]
enum OnOff {
    On,
    Off,
}

#[derive(Subcommand)]
enum Cmd {
    /// Screen Recording + Accessibility state (no prompt).
    Permissions,
    /// List windows. Default filter: layer 0, ≥ 64×64 pt (plan T3.1).
    Windows {
        #[arg(long)]
        all: bool,
    },
    /// Capture one window for a few seconds.
    Window {
        id: u32,
        #[arg(long, default_value_t = 3.0)]
        secs: f64,
        #[arg(long, value_enum)]
        children: Option<OnOff>,
        #[arg(long, default_value_t = 2.0)]
        scale: f64,
        #[arg(long)]
        dump: Option<PathBuf>,
    },
    /// Capture the main display for a few seconds.
    Display {
        #[arg(long, default_value_t = 3.0)]
        secs: f64,
        #[arg(long)]
        dump: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("warn").init();
    match Args::parse().cmd {
        Cmd::Permissions => {
            let (sr, ax) = permissions();
            println!("Screen Recording: {}", if sr { "granted" } else { "NOT granted" });
            println!("Accessibility:    {}", if ax { "granted" } else { "NOT granted" });
            println!("(attributed to the responsible process — the terminal when run from a shell)");
        }
        Cmd::Windows { all } => {
            let mut ws = list_windows()?;
            ws.sort_by_key(|w| (w.app_name.clone(), w.id));
            println!("{:>7} {:>5} {:>3} {:>3} {:>11}  {:<24} title", "id", "layer", "on", "act", "size(pt)", "app");
            for w in ws.iter().filter(|w| all || (w.layer == 0 && w.width >= 64.0 && w.height >= 64.0)) {
                println!(
                    "{:>7} {:>5} {:>3} {:>3} {:>11}  {:<24} {}",
                    w.id,
                    w.layer,
                    if w.on_screen { "y" } else { "-" },
                    if w.active { "y" } else { "-" },
                    format!("{:.0}x{:.0}", w.width, w.height),
                    truncate(&w.app_name, 24),
                    w.title
                );
            }
        }
        Cmd::Window { id, secs, children, scale, dump } => {
            let opts = ScOptions {
                pixel_format: kCVPixelFormatType_32BGRA,
                include_child_windows: children.map(|c| matches!(c, OnOff::On)),
                shows_cursor: false,
                min_interval_hz: None,
            };
            probe(ScCapture::window(id, scale, 7680, 4320, 60, opts)?, secs, dump)?;
        }
        Cmd::Display { secs, dump } => {
            let opts = ScOptions { pixel_format: kCVPixelFormatType_32BGRA, ..Default::default() };
            probe(ScCapture::display(None, 7680, 4320, 60, opts)?, secs, dump)?;
        }
    }
    Ok(())
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

struct Probe {
    frames: u32,
    first: Option<Duration>,
    last: Option<PixelBuffer>,
    sizes: std::collections::BTreeSet<(u32, u32)>,
}

fn probe(mut cap: ScCapture, secs: f64, dump: Option<PathBuf>) -> Result<()> {
    let start = Instant::now();
    let st = Arc::new(Mutex::new(Probe { frames: 0, first: None, last: None, sizes: Default::default() }));
    let s2 = st.clone();
    cap.start(Box::new(move |f| {
        let mut p = s2.lock().unwrap();
        p.frames += 1;
        p.first.get_or_insert(start.elapsed());
        p.sizes.insert((f.width, f.height));
        if let FrameBuffer::PixelBuffer(pb) = f.buffer {
            p.last = Some(pb);
        }
    }))?;
    std::thread::sleep(Duration::from_secs_f64(secs));
    cap.stop();
    let p = st.lock().unwrap();
    println!("requested size: {:?}", cap.size());
    println!("frames: {} in {secs:.1} s (SCK only sends frames when content changes)", p.frames);
    println!("first frame after: {:?}", p.first);
    println!("frame sizes seen: {:?}", p.sizes);
    if let Some(pb) = &p.last {
        let (w, h) = (pb.width(), pb.height());
        let (mean, black) = pb.with_luma(|l| {
            let (mut sum, mut blk, mut n) = (0u64, 0u64, 0u64);
            for y in (0..h).step_by(4) {
                for x in (0..w).step_by(4) {
                    let v = l(x, y) as u64;
                    sum += v;
                    blk += (v < 8) as u64;
                    n += 1;
                }
            }
            (sum as f64 / n as f64, blk as f64 / n as f64)
        });
        println!("last frame: {w}x{h}, mean luma {mean:.1}, black pixels {:.1} %", black * 100.0);
        if let Some(dir) = dump {
            std::fs::create_dir_all(&dir)?;
            let path = dir.join(format!("frame-{}.bmp", rz_proto::clock::now_ns()));
            write_bmp(pb, &path)?;
            println!("dumped {}", path.display());
        }
    } else {
        println!("no frame received (minimized / other Space / no permission?)");
    }
    Ok(())
}

/// 32-bit top-down BMP from a BGRA pixel buffer.
fn write_bmp(pb: &PixelBuffer, path: &std::path::Path) -> Result<()> {
    let (w, h) = (pb.width(), pb.height());
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    pb.with_bgra_rows(|row| px.extend_from_slice(&row[..(w * 4) as usize]));
    let mut f = Vec::with_capacity(54 + px.len());
    let size = 54 + px.len() as u32;
    f.extend_from_slice(b"BM");
    f.extend_from_slice(&size.to_le_bytes());
    f.extend_from_slice(&[0; 4]);
    f.extend_from_slice(&54u32.to_le_bytes());
    f.extend_from_slice(&40u32.to_le_bytes());
    f.extend_from_slice(&(w as i32).to_le_bytes());
    f.extend_from_slice(&(-(h as i32)).to_le_bytes());
    f.extend_from_slice(&1u16.to_le_bytes());
    f.extend_from_slice(&32u16.to_le_bytes());
    f.extend_from_slice(&[0; 24]);
    f.extend_from_slice(&px);
    std::fs::write(path, f)?;
    Ok(())
}
