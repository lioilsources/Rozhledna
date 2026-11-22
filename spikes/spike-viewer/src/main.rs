//! Phase 0 spike viewer (T0.4/T0.5): QUIC → VideoToolbox → IOSurface as `CALayer.contents`.
//!
//! Overlay (CATextLayer): rolling P50/P95 per pipeline segment, fps, bitrate, RTT.
//! On exit (window closed, `--duration` elapsed or Ctrl+C) writes the CSV and prints the report.
//!
//! `present_ns` = after `CATransaction::commit` on the main thread. True scanout happens later in
//! WindowServer (≤ 1 refresh); glass-to-glass is measured separately with tools/latency/counter.html.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Parser;
use rz_codec::DecodedFrame;
use rz_latency::FrameRecord;
use rz_proto::clock::now_ns;
use spike_viewer::{Client, ClientOptions, LiveStats};

#[derive(Parser, Debug, Clone)]
#[command(about = "Rozhledna Phase 0 spike viewer")]
struct Args {
    /// Agent address, e.g. 192.168.1.20:47470
    addr: SocketAddr,
    #[arg(long, default_value_t = 3840)]
    max_w: u32,
    #[arg(long, default_value_t = 2160)]
    max_h: u32,
    #[arg(long, default_value_t = 60)]
    fps: u32,
    /// 0 = agent default.
    #[arg(long, default_value_t = 0)]
    bitrate_kbps: u32,
    /// Write per-frame latency records here on exit.
    #[arg(long)]
    csv: Option<PathBuf>,
    /// Stop after N seconds.
    #[arg(long)]
    duration: Option<f64>,
    /// No window: decode only (present_ns = decode_done). For loopback tests.
    #[arg(long)]
    headless: bool,
    /// Check the burned-in frame number of each decoded frame (agent must run --source testpattern).
    #[arg(long)]
    verify_testpattern: bool,
    /// Required in Phase 0.
    #[arg(long)]
    insecure_dev: bool,
}

static RECORDS: Mutex<Vec<FrameRecord>> = Mutex::new(Vec::new());
static CSV: OnceLock<Option<PathBuf>> = OnceLock::new();
static STATS: OnceLock<Arc<Mutex<LiveStats>>> = OnceLock::new();
static VERIFY_OK: AtomicU32 = AtomicU32::new(0);
static VERIFY_BAD: AtomicU32 = AtomicU32::new(0);
static VERIFY_LAST: AtomicU64 = AtomicU64::new(u64::MAX);

fn verify(frame: &DecodedFrame) {
    let n = frame.pixel_buffer.with_luma(|l| rz_capture::testpattern::decode_frame_no(l));
    let last = VERIFY_LAST.load(Ordering::Relaxed);
    match n {
        Some(n) if last == u64::MAX || n as u64 > last => {
            VERIFY_OK.fetch_add(1, Ordering::Relaxed);
            VERIFY_LAST.store(n as u64, Ordering::Relaxed);
        }
        _ => {
            VERIFY_BAD.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Writes the CSV and prints the report exactly once.
fn finish() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let recs = std::mem::take(&mut *RECORDS.lock().unwrap());
        if let Some(Some(path)) = CSV.get() {
            match std::fs::File::create(path).and_then(|mut f| rz_latency::write_csv(&mut f, &recs)) {
                Ok(()) => eprintln!("wrote {} records to {}", recs.len(), path.display()),
                Err(e) => eprintln!("CSV write failed: {e}"),
            }
        }
        print!("{}", rz_latency::report(&recs));
        if let Some(s) = STATS.get() {
            let s = s.lock().unwrap();
            if let Some(a) = &s.agent {
                println!(
                    "agent: {} fps sent (last s), bitrate {} kb/s, dropped total {}",
                    a.fps, a.bitrate_kbps, a.dropped_before_encode
                );
            }
            println!(
                "keyframe requests {}   frames skipped {}   min RTT {:.2} ms{}",
                s.keyframe_requests,
                s.frames_skipped,
                s.min_rtt_ns.unwrap_or(0) as f64 / 1e6,
                s.error.as_ref().map(|e| format!("   error: {e}")).unwrap_or_default()
            );
        }
        let (ok, bad) = (VERIFY_OK.load(Ordering::Relaxed), VERIFY_BAD.load(Ordering::Relaxed));
        if ok + bad > 0 {
            println!("testpattern verify: {ok} ok, {bad} bad");
        }
    });
}

fn main() -> Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    anyhow::ensure!(args.insecure_dev, "Phase 0 has no pairing yet: run with --insecure-dev (trusted LAN only)");
    CSV.set(args.csv.clone()).unwrap();
    let opts = ClientOptions {
        addr: args.addr,
        max_w: args.max_w,
        max_h: args.max_h,
        fps: args.fps,
        bitrate_kbps: args.bitrate_kbps,
        name: "spike-viewer".into(),
    };
    if args.headless { run_headless(args, opts) } else { ui::run(args, opts) }
}

fn run_headless(args: Args, opts: ClientOptions) -> Result<()> {
    let check = args.verify_testpattern;
    let mut client = Client::spawn(
        opts,
        Box::new(move |frame, mut rec| {
            if check {
                verify(&frame);
            }
            rec.present_ns = rec.decode_done_ns;
            RECORDS.lock().unwrap().push(rec);
        }),
    )?;
    STATS.set(client.stats.clone()).unwrap();
    let start = Instant::now();
    let limit = args.duration.map(Duration::from_secs_f64);
    while !client.is_stopped() && limit.is_none_or(|l| start.elapsed() < l) {
        std::thread::sleep(Duration::from_millis(50));
    }
    client.stop();
    finish();
    Ok(())
}

mod ui {
    use super::*;
    use dispatch2::DispatchQueue;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, ProtocolObject};
    use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType, NSWindow,
        NSWindowStyleMask,
    };
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};
    use objc2_core_graphics::CGColor;
    use objc2_core_video::CVPixelBuffer;
    use objc2_foundation::{NSNotification, NSObject, NSObjectProtocol, NSString};
    use objc2_quartz_core::{CAAutoresizingMask, CALayer, CATextLayer, CATransaction, kCAGravityResizeAspect};
    use rz_capture::pixbuf::PixelBuffer;

    #[link(name = "CoreVideo", kind = "framework")]
    unsafe extern "C" {
        /// Returns the backing IOSurfaceRef (not retained) or NULL.
        fn CVPixelBufferGetIOSurface(pb: &CVPixelBuffer) -> *const std::ffi::c_void;
    }

    struct Ui {
        video: Retained<CALayer>,
        text: Retained<CATextLayer>,
        /// Keep recently shown buffers alive so the decoder pool does not recycle a visible IOSurface.
        held: VecDeque<PixelBuffer>,
        rolling: VecDeque<FrameRecord>,
        last_overlay: Instant,
        verify: bool,
    }

    thread_local! {
        static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "RzSpikeViewerDelegate"]
        struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl NSApplicationDelegate for Delegate {
            #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
            fn should_terminate(&self, _sender: &NSApplication) -> bool {
                true
            }

            #[unsafe(method(applicationWillTerminate:))]
            fn will_terminate(&self, _n: &NSNotification) {
                finish();
            }
        }
    );

    impl Delegate {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(());
            unsafe { msg_send![super(this), init] }
        }
    }

    fn present(frame: DecodedFrame, mut rec: FrameRecord) {
        UI.with(|ui| {
            let mut ui = ui.borrow_mut();
            let Some(ui) = ui.as_mut() else { return };
            if ui.verify {
                verify(&frame);
            }
            let surface = unsafe { CVPixelBufferGetIOSurface(frame.pixel_buffer.cv()) };
            if surface.is_null() {
                return;
            }
            CATransaction::begin();
            CATransaction::setDisableActions(true);
            // IOSurfaceRef is accepted directly as layer contents.
            let obj: &AnyObject = unsafe { &*(surface as *const AnyObject) };
            unsafe { ui.video.setContents(Some(obj)) };
            CATransaction::commit();
            rec.present_ns = now_ns();
            ui.held.push_back(frame.pixel_buffer);
            while ui.held.len() > 3 {
                ui.held.pop_front();
            }
            RECORDS.lock().unwrap().push(rec);
            ui.rolling.push_back(rec);
            while ui.rolling.len() > 240 {
                ui.rolling.pop_front();
            }
            if ui.last_overlay.elapsed() > Duration::from_millis(250) {
                ui.last_overlay = Instant::now();
                update_overlay(ui);
            }
        });
    }

    fn update_overlay(ui: &Ui) {
        let recs: Vec<FrameRecord> = ui.rolling.iter().copied().collect();
        let mut s = rz_latency::report(&recs);
        if let Some(st) = STATS.get() {
            let st = st.lock().unwrap();
            if let Some(a) = &st.agent {
                s.push_str(&format!(
                    "agent: {} fps sent, target {} kb/s, dropped {}\n",
                    a.fps, a.bitrate_kbps, a.dropped_before_encode
                ));
            }
            s.push_str(&format!(
                "RTT {:.2} ms   keyframe req {}   skipped {}",
                st.min_rtt_ns.unwrap_or(0) as f64 / 1e6,
                st.keyframe_requests,
                st.frames_skipped
            ));
            if let Some(e) = &st.error {
                s.push_str(&format!("\nERROR: {e}"));
            }
        }
        let ns = NSString::from_str(&s);
        unsafe { ui.text.setString(Some(&ns)) };
    }

    pub fn run(args: Args, opts: ClientOptions) -> Result<()> {
        let mtm = MainThreadMarker::new().expect("main thread");
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        let delegate = Delegate::new(mtm);
        app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

        let rect = CGRect::new(CGPoint::new(100.0, 100.0), CGSize::new(1280.0, 720.0));
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Resizable
            | NSWindowStyleMask::Miniaturizable;
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str(&format!("Rozhledna spike — {}", args.addr)));
        let view = window.contentView().expect("content view");
        view.setWantsLayer(true);
        let video = view.layer().expect("layer-backed view");
        video.setContentsGravity(unsafe { kCAGravityResizeAspect });
        video.setBackgroundColor(Some(&CGColor::new_srgb(0.0, 0.0, 0.0, 1.0)));

        let text = CATextLayer::new();
        let scale = window.backingScaleFactor();
        text.setContentsScale(scale);
        text.setFontSize(12.0);
        text.setForegroundColor(Some(&CGColor::new_srgb(1.0, 1.0, 0.6, 1.0)));
        text.setBackgroundColor(Some(&CGColor::new_srgb(0.0, 0.0, 0.0, 0.6)));
        text.setFrame(CGRect::new(CGPoint::new(8.0, 720.0 - 8.0 - 150.0), CGSize::new(560.0, 150.0)));
        text.setAutoresizingMask(CAAutoresizingMask::LayerMaxXMargin | CAAutoresizingMask::LayerMinYMargin);
        unsafe { text.setString(Some(&NSString::from_str("connecting…"))) };
        video.addSublayer(&text);

        UI.with(|u| {
            *u.borrow_mut() = Some(Ui {
                video,
                text,
                held: VecDeque::new(),
                rolling: VecDeque::new(),
                last_overlay: Instant::now(),
                verify: args.verify_testpattern,
            })
        });
        window.center();
        window.makeKeyAndOrderFront(None);
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);

        let client = Client::spawn(
            opts,
            Box::new(|frame, rec| {
                let item = SendBox((frame, rec));
                DispatchQueue::main().exec_async(move || {
                    let SendBox((frame, rec)) = item;
                    present(frame, rec);
                });
            }),
        )?;
        STATS.set(client.stats.clone()).unwrap();

        // Duration limit and Ctrl+C both end the app through the main queue.
        let limit = args.duration;
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async {
                match limit {
                    Some(s) => {
                        tokio::select! {
                            _ = tokio::time::sleep(Duration::from_secs_f64(s)) => {}
                            _ = tokio::signal::ctrl_c() => {}
                        }
                    }
                    None => {
                        let _ = tokio::signal::ctrl_c().await;
                    }
                }
            });
            DispatchQueue::main().exec_async(|| {
                finish();
                std::process::exit(0);
            });
        });

        app.run();
        drop(client);
        finish();
        Ok(())
    }

    /// DecodedFrame holds a Send PixelBuffer; FrameRecord is plain data.
    struct SendBox<T>(T);
    unsafe impl<T> Send for SendBox<T> {}
}
