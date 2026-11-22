//! C ABI over `rz-hub-core` for the Flutter hub (T1.5/T1.6). The Swift plugin calls these via `@_silgen_name`;
//! the header is `include/rz_hub.h`.
//!
//! Model:
//! - events: the UI polls [`rz_hub_next_event`] (JSON string, free with [`rz_hub_free_string`]);
//! - frames: each tile's decode thread stores the newest `CVPixelBuffer` and calls `on_frame(ctx, tile)`; the
//!   Flutter texture's `copyPixelBuffer` then takes it with [`rz_hub_take_frame`] (+1 retained). Frames replaced
//!   before Flutter took them are simply dropped — "decode everything, show only the newest" (dogfood finding);
//! - identity + trust store: `state_dir` (default `~/Library/Application Support/Rozhledna/hub`).
//!
//! All functions are safe to call from any thread. Ids are `u64`; 0 means "none / error".

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::ptr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rz_hub_core::{Hub, TileParams};
use rz_net::identity::{Identity, default_dir};
use rz_net::trust::TrustStore;
use rz_proto::pb::{self, input_event::Ev};

pub type FrameCallback = extern "C" fn(ctx: *mut c_void, tile: u64);

#[cfg(target_os = "macos")]
type Pixels = rz_capture::pixbuf::PixelBuffer;
#[cfg(not(target_os = "macos"))]
type Pixels = ();

pub struct RzHub {
    hub: Hub,
    latest: Arc<Mutex<HashMap<u64, Pixels>>>,
}

struct Ctx(*mut c_void);
// SAFETY: the caller guarantees `ctx` outlives the hub and its callback is thread-safe.
unsafe impl Send for Ctx {}
unsafe impl Sync for Ctx {}

unsafe fn str_arg<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() { None } else { unsafe { CStr::from_ptr(p) }.to_str().ok() }
}

/// Creates the hub. `state_dir` may be NULL (default location); `name` is shown in agents' pairing dialogs.
/// Returns NULL on error (see the log).
///
/// # Safety
/// Strings NUL-terminated or NULL; `ctx` valid until [`rz_hub_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rz_hub_new(
    state_dir: *const c_char,
    name: *const c_char,
    on_frame: FrameCallback,
    ctx: *mut c_void,
) -> *mut RzHub {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .try_init();
    let r = (|| -> anyhow::Result<RzHub> {
        let dir = match unsafe { str_arg(state_dir) } {
            Some(d) if !d.is_empty() => d.into(),
            _ => default_dir("hub")?,
        };
        let id = Identity::load_or_create(&dir)?;
        let trust = TrustStore::load(&dir)?;
        let name = unsafe { str_arg(name) }
            .filter(|s| !s.is_empty())
            .map(String::from)
            .unwrap_or_else(rz_net::discovery::hostname);
        let latest: Arc<Mutex<HashMap<u64, Pixels>>> = Arc::default();
        let l2 = latest.clone();
        let ctx = Ctx(ctx);
        let sink: rz_hub_core::FrameSink = Arc::new(move |tile, frame, _rec| {
            #[cfg(target_os = "macos")]
            l2.lock().unwrap().insert(tile, frame.pixel_buffer);
            #[cfg(not(target_os = "macos"))]
            let _ = (&l2, frame);
            let c = &ctx;
            on_frame(c.0, tile);
        });
        tracing::info!(fp = %id.fingerprint, state = %dir.display(), "hub starting");
        Ok(RzHub { hub: Hub::new(id, trust, &name, sink)?, latest })
    })();
    match r {
        Ok(h) => Box::into_raw(Box::new(h)),
        Err(e) => {
            tracing::error!("rz_hub_new: {e:#}");
            ptr::null_mut()
        }
    }
}

/// # Safety
/// `h` from [`rz_hub_new`]; not used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rz_hub_free(h: *mut RzHub) {
    if !h.is_null() {
        drop(unsafe { Box::from_raw(h) });
    }
}

fn hub<'a>(h: *mut RzHub) -> Option<&'a RzHub> {
    unsafe { h.as_ref() }
}

/// This hub's fingerprint (`sha256:…`). Free with [`rz_hub_free_string`].
#[unsafe(no_mangle)]
pub extern "C" fn rz_hub_fingerprint(h: *mut RzHub) -> *mut c_char {
    hub(h).map_or(ptr::null_mut(), |h| into_c(h.hub.fingerprint().to_string()))
}

/// Next event as JSON, waiting up to `timeout_ms`; NULL if none. Free with [`rz_hub_free_string`].
#[unsafe(no_mangle)]
pub extern "C" fn rz_hub_next_event(h: *mut RzHub, timeout_ms: u32) -> *mut c_char {
    let Some(h) = hub(h) else { return ptr::null_mut() };
    match h.hub.next_event(Duration::from_millis(timeout_ms as u64)) {
        Some(e) => into_c(e.to_json()),
        None => ptr::null_mut(),
    }
}

fn into_c(s: String) -> *mut c_char {
    CString::new(s).map_or(ptr::null_mut(), CString::into_raw)
}

/// # Safety
/// `s` from this library (or NULL).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rz_hub_free_string(s: *mut c_char) {
    if !s.is_null() {
        drop(unsafe { CString::from_raw(s) });
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn rz_hub_start_discovery(h: *mut RzHub) -> bool {
    hub(h).is_some_and(|h| h.hub.start_discovery().map_err(|e| tracing::error!("discovery: {e:#}")).is_ok())
}

/// `addr`: `host:port` or `host` (default port). Returns the agent id, 0 on error.
///
/// # Safety
/// `addr` NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rz_hub_add_agent(h: *mut RzHub, addr: *const c_char) -> u64 {
    use std::net::ToSocketAddrs;
    let (Some(h), Some(a)) = (hub(h), unsafe { str_arg(addr) }) else { return 0 };
    let a = if a.contains(':') { a.to_string() } else { format!("{a}:{}", rz_proto::DEFAULT_PORT) };
    match a
        .to_socket_addrs()
        .ok()
        .and_then(|mut i| i.find(|s| s.is_ipv4()).or_else(|| a.to_socket_addrs().ok()?.next()))
    {
        Some(sa) => h.hub.add_agent(sa),
        None => {
            tracing::warn!("cannot resolve {a}");
            0
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn rz_hub_remove_agent(h: *mut RzHub, agent: u64) {
    if let Some(h) = hub(h) {
        h.hub.remove_agent(agent);
    }
}

/// Returns the tile id, 0 on error. `bitrate_kbps` 0 = auto.
#[unsafe(no_mangle)]
pub extern "C" fn rz_hub_subscribe(
    h: *mut RzHub,
    agent: u64,
    source_id: u64,
    max_w: u32,
    max_h: u32,
    fps: u32,
    focus: bool,
    bitrate_kbps: u32,
) -> u64 {
    let Some(h) = hub(h) else { return 0 };
    let p = TileParams { source_id, max_w, max_h, fps, focus, bitrate_kbps };
    h.hub.subscribe(agent, p).unwrap_or_else(|e| {
        tracing::warn!("subscribe: {e:#}");
        0
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn rz_hub_update_tile(
    h: *mut RzHub,
    tile: u64,
    source_id: u64,
    max_w: u32,
    max_h: u32,
    fps: u32,
    focus: bool,
    bitrate_kbps: u32,
) -> bool {
    let Some(h) = hub(h) else { return false };
    h.hub.update_tile(tile, TileParams { source_id, max_w, max_h, fps, focus, bitrate_kbps }).is_ok()
}

#[unsafe(no_mangle)]
pub extern "C" fn rz_hub_unsubscribe(h: *mut RzHub, tile: u64) {
    if let Some(h) = hub(h) {
        h.hub.unsubscribe(tile);
        h.latest.lock().unwrap().remove(&tile);
    }
}

/// Newest decoded frame of the tile as a +1 retained `CVPixelBufferRef` (BGRA, IOSurface), NULL if none new.
#[unsafe(no_mangle)]
pub extern "C" fn rz_hub_take_frame(h: *mut RzHub, tile: u64) -> *mut c_void {
    let Some(h) = hub(h) else { return ptr::null_mut() };
    #[cfg(target_os = "macos")]
    {
        let Some(pb) = h.latest.lock().unwrap().remove(&tile) else { return ptr::null_mut() };
        objc2_core_foundation::CFRetained::into_raw(pb.0).as_ptr().cast()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (h, tile);
        ptr::null_mut()
    }
}

/// `hid`: USB HID keyboard usage (Flutter `PhysicalKeyboardKey.usbHidUsage`, page 0x07 included or not).
#[unsafe(no_mangle)]
pub extern "C" fn rz_hub_key(h: *mut RzHub, tile: u64, hid: u32, down: bool) {
    if let Some(h) = hub(h) {
        h.hub.input(tile, Ev::Key(pb::Key { hid_usage: hid, down, modifiers: 0 }));
    }
}

/// `button`: 0 left, 1 right, 2 middle, 3 back, 4 forward; `x`, `y` normalized 0..1 in the tile's source.
#[unsafe(no_mangle)]
pub extern "C" fn rz_hub_button(h: *mut RzHub, tile: u64, button: u32, down: bool, x: f32, y: f32, clicks: u32) {
    if let Some(h) = hub(h) {
        h.hub.input(tile, Ev::Button(pb::Button { button, down, x, y, click_count: clicks }));
    }
}

/// See the `Scroll` message for units and signs.
#[unsafe(no_mangle)]
pub extern "C" fn rz_hub_scroll(h: *mut RzHub, tile: u64, dx: f32, dy: f32, precise: bool, phase: u32) {
    if let Some(h) = hub(h) {
        h.hub.input(tile, Ev::Scroll(pb::Scroll { dx, dy, precise, phase }));
    }
}

/// # Safety
/// `utf8` NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rz_hub_text(h: *mut RzHub, tile: u64, utf8: *const c_char) {
    if let (Some(h), Some(t)) = (hub(h), unsafe { str_arg(utf8) }) {
        h.hub.input(tile, Ev::Text(pb::Text { utf8: t.into() }));
    }
}

/// Pointer position, normalized 0..1 (datagram; send on every move).
#[unsafe(no_mangle)]
pub extern "C" fn rz_hub_pointer(h: *mut RzHub, tile: u64, x: f32, y: f32) {
    if let Some(h) = hub(h) {
        h.hub.pointer(tile, x, y);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn rz_hub_release_all(h: *mut RzHub, tile: u64) {
    if let Some(h) = hub(h) {
        h.hub.release_all(tile);
    }
}

#[repr(C)]
#[derive(Default)]
pub struct RzCursor {
    pub x: f32,
    pub y: f32,
    pub visible: bool,
    pub shape_id: u32,
}

/// Latest remote cursor on the tile; false if unknown.
///
/// # Safety
/// `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rz_hub_cursor(h: *mut RzHub, tile: u64, out: *mut RzCursor) -> bool {
    let (Some(h), Some(out)) = (hub(h), unsafe { out.as_mut() }) else { return false };
    match h.hub.cursor(tile) {
        Some(c) if c.seq != 0 => {
            *out = RzCursor { x: c.x, y: c.y, visible: c.visible, shape_id: c.shape_id };
            true
        }
        _ => false,
    }
}

/// Answer to a `confirm_agent` event.
#[unsafe(no_mangle)]
pub extern "C" fn rz_hub_answer(h: *mut RzHub, request: u64, accept: bool) {
    if let Some(h) = hub(h) {
        h.hub.answer(request, accept);
    }
}

/// Forgets a paired agent (`sha256:…`).
///
/// # Safety
/// `fingerprint` NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rz_hub_forget(h: *mut RzHub, fingerprint: *const c_char) -> bool {
    let (Some(h), Some(fp)) = (hub(h), unsafe { str_arg(fingerprint) }) else { return false };
    rz_net::Fingerprint::parse(fp).ok().and_then(|fp| h.hub.forget(&fp).ok()).unwrap_or(false)
}
