//! C ABI for the Flutter spike (T0.6). Called from Swift via `@_silgen_name`.
//!
//! Model: the decode thread stores the newest frame in a slot and calls `on_frame(ctx)`; the
//! Swift `FlutterTexture.copyPixelBuffer` then calls [`rz_spike_take_latest`], which returns a +1
//! retained `CVPixelBufferRef` and stamps `present_ns` for that frame. Frames replaced in the slot
//! before Flutter asked for them are counted as "not presented".

use std::ffi::{CStr, c_char, c_void};
use std::ptr;
use std::sync::{Arc, Mutex};

use rz_capture::pixbuf::PixelBuffer;
use rz_latency::FrameRecord;
use spike_viewer::{Client, ClientOptions};

pub type FrameCallback = extern "C" fn(ctx: *mut c_void);

#[derive(Default)]
struct Slot {
    latest: Option<(PixelBuffer, FrameRecord)>,
    records: Vec<FrameRecord>,
    replaced_unpresented: u64,
}

pub struct Handle {
    client: Client,
    slot: Arc<Mutex<Slot>>,
}

struct Ctx(*mut c_void);
// SAFETY: the Swift side passes a pointer to an object that outlives the client and whose callback is thread-safe.
unsafe impl Send for Ctx {}

/// Starts a client. Returns NULL on error. `addr` is "host:port".
///
/// # Safety
/// `addr` must be a valid NUL-terminated string; `ctx` must stay valid until `rz_spike_stop`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rz_spike_start(
    addr: *const c_char,
    max_w: u32,
    max_h: u32,
    fps: u32,
    on_frame: FrameCallback,
    ctx: *mut c_void,
) -> *mut Handle {
    let _ = tracing_subscriber::fmt().with_env_filter("info").try_init();
    let Ok(addr) = unsafe { CStr::from_ptr(addr) }.to_str() else { return ptr::null_mut() };
    let Ok(addr) = addr.parse() else {
        tracing::error!("bad address {addr}");
        return ptr::null_mut();
    };
    let opts = ClientOptions { addr, max_w, max_h, fps, bitrate_kbps: 0, name: "flutter-spike".into() };
    let slot = Arc::new(Mutex::new(Slot::default()));
    let s2 = slot.clone();
    let ctx = Ctx(ctx);
    let client = Client::spawn(
        opts,
        Box::new(move |frame, rec| {
            let ctx = &ctx;
            {
                let mut s = s2.lock().unwrap();
                if s.latest.replace((frame.pixel_buffer, rec)).is_some() {
                    s.replaced_unpresented += 1;
                }
            }
            on_frame(ctx.0);
        }),
    );
    match client {
        Ok(client) => Box::into_raw(Box::new(Handle { client, slot })),
        Err(e) => {
            tracing::error!("start: {e:#}");
            ptr::null_mut()
        }
    }
}

/// Returns the newest frame as a +1 retained `CVPixelBufferRef`, or NULL if none is pending.
///
/// # Safety
/// `h` must come from `rz_spike_start` and not be stopped.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rz_spike_take_latest(h: *mut Handle) -> *mut c_void {
    let Some(h) = (unsafe { h.as_ref() }) else { return ptr::null_mut() };
    let mut s = h.slot.lock().unwrap();
    let Some((pb, mut rec)) = s.latest.take() else { return ptr::null_mut() };
    rec.present_ns = rz_proto::clock::now_ns();
    s.records.push(rec);
    objc2_core_foundation::CFRetained::into_raw(pb.0).as_ptr().cast()
}

/// Writes a one-line-per-segment summary of the last ~240 presented frames into `buf` (NUL-terminated).
///
/// # Safety
/// `h` valid; `buf` points to `len` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rz_spike_stats(h: *mut Handle, buf: *mut c_char, len: usize) -> usize {
    let Some(h) = (unsafe { h.as_ref() }) else { return 0 };
    let text = {
        let s = h.slot.lock().unwrap();
        let tail = &s.records[s.records.len().saturating_sub(240)..];
        let mut t = rz_latency::report(tail);
        t.push_str(&format!("frames replaced before Flutter took them: {}\n", s.replaced_unpresented));
        let ls = h.client.stats.lock().unwrap();
        t.push_str(&format!(
            "RTT {:.2} ms   keyframe req {}{}",
            ls.min_rtt_ns.unwrap_or(0) as f64 / 1e6,
            ls.keyframe_requests,
            ls.error.as_ref().map(|e| format!("\nERROR: {e}")).unwrap_or_default()
        ));
        t
    };
    unsafe { write_c(&text, buf, len) }
}

unsafe fn write_c(text: &str, buf: *mut c_char, len: usize) -> usize {
    if buf.is_null() || len == 0 {
        return 0;
    }
    let n = text.len().min(len - 1);
    unsafe {
        ptr::copy_nonoverlapping(text.as_ptr(), buf.cast(), n);
        *buf.add(n) = 0;
    }
    n
}

/// Stops the client, writes all records to `csv_path` (if not NULL) and the report into `buf`.
/// Frees the handle.
///
/// # Safety
/// `h` from `rz_spike_start`; `csv_path` NULL or a valid string; `buf` `len` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rz_spike_stop(h: *mut Handle, csv_path: *const c_char, buf: *mut c_char, len: usize) -> usize {
    if h.is_null() {
        return 0;
    }
    let mut h = unsafe { Box::from_raw(h) };
    h.client.stop();
    let s = std::mem::take(&mut *h.slot.lock().unwrap());
    let mut text = rz_latency::report(&s.records);
    text.push_str(&format!("frames replaced before Flutter took them: {}\n", s.replaced_unpresented));
    if !csv_path.is_null()
        && let Ok(p) = unsafe { CStr::from_ptr(csv_path) }.to_str()
    {
        match std::fs::File::create(p).and_then(|mut f| rz_latency::write_csv(&mut f, &s.records)) {
            Ok(()) => text.push_str(&format!("wrote {} records to {p}\n", s.records.len())),
            Err(e) => text.push_str(&format!("CSV write failed: {e}\n")),
        }
    }
    unsafe { write_c(&text, buf, len) }
}
