//! The C ABI the Swift plugin uses, driven from Rust against a real agent (testpattern): events as JSON, frame
//! callback, `take_frame` (+1 CVPixelBuffer), input by macOS key code, cursor, free. macOS only.
#![cfg(target_os = "macos")]

use std::ffi::{CStr, CString, c_void};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rz_agent::sources::{Backend, Registry};
use rz_agent::{Agent, Limits, PairAsk};
use rz_hub_ffi::*;
use rz_input::{Action, NullInjector};
use rz_net::Identity;
use rz_net::handshake::AgentUi;
use rz_net::trust::TrustStore;

struct Accept;
impl AgentUi for Accept {
    async fn confirm_hub(&self, _: PairAsk) -> bool {
        true
    }
}

static FRAMES: AtomicU64 = AtomicU64::new(0);
extern "C" fn on_frame(_ctx: *mut c_void, _tile: u64) {
    FRAMES.fetch_add(1, Ordering::Relaxed);
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
}
#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    fn CVPixelBufferGetWidth(pb: *const c_void) -> usize;
}

#[test]
fn c_abi_end_to_end() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let null = NullInjector::default();
    let agent = Agent::new(
        Identity::generate().unwrap(),
        "ffi-agent",
        TrustStore::memory(),
        Registry::new(Backend::Testpattern { w: 1344, h: 756, count: 1 }),
        Limits::default(),
        Accept,
        Box::new(null.clone()),
    );
    let ep =
        rt.block_on(async { rz_net::server_endpoint_mtls("127.0.0.1:0".parse().unwrap(), &agent.identity) }).unwrap();
    let addr = ep.local_addr().unwrap();
    rt.spawn(agent.accept_loop(ep));

    let dir = std::env::temp_dir().join(format!("rz-ffi-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cdir = CString::new(dir.to_str().unwrap()).unwrap();
    let h = unsafe { rz_hub_new(cdir.as_ptr(), c"ffi-hub".as_ptr(), on_frame, std::ptr::null_mut()) };
    assert!(!h.is_null());
    let fp = rz_hub_fingerprint(h);
    assert!(unsafe { CStr::from_ptr(fp) }.to_str().unwrap().starts_with("sha256:"));
    unsafe { rz_hub_free_string(fp) };

    let caddr = CString::new(addr.to_string()).unwrap();
    let a = unsafe { rz_hub_add_agent(h, caddr.as_ptr()) };
    assert_ne!(a, 0);
    // Events until the agent's sources arrive.
    let t = Instant::now();
    let mut seen = Vec::new();
    while !seen.iter().any(|s: &String| s.contains(r#""type":"sources""#)) {
        assert!(t.elapsed() < Duration::from_secs(10), "events so far: {seen:?}");
        let e = rz_hub_next_event(h, 100);
        if !e.is_null() {
            seen.push(unsafe { CStr::from_ptr(e) }.to_str().unwrap().to_string());
            unsafe { rz_hub_free_string(e) };
        }
    }
    assert!(seen.iter().any(|s| s.contains(r#""type":"pair_code""#)));

    let tile = rz_hub_subscribe(h, a, 1, 1344, 756, 30, true, 0);
    assert_ne!(tile, 0);
    let t = Instant::now();
    while FRAMES.load(Ordering::Relaxed) < 10 {
        assert!(t.elapsed() < Duration::from_secs(10), "no frames");
        let e = rz_hub_next_event(h, 50);
        if !e.is_null() {
            unsafe { rz_hub_free_string(e) };
        }
    }
    let pb = rz_hub_take_frame(h, tile);
    assert!(!pb.is_null(), "a frame is waiting after the callback");
    assert_eq!(unsafe { CVPixelBufferGetWidth(pb) }, 1344);
    unsafe { CFRelease(pb) };
    // Taken: the slot is empty until the next frame (newest-wins slot).
    let again = rz_hub_take_frame(h, tile);
    if !again.is_null() {
        unsafe { CFRelease(again) }; // a new frame may already have arrived; that is fine too
    }

    // Input via macOS key code (grab path): kVK_ANSI_A = 0x00 → HID 0x04 on the agent.
    assert!(rz_hub_key_mac(h, tile, 0x00, true));
    assert!(rz_hub_key_mac(h, tile, 0x00, false));
    rz_hub_pointer(h, tile, 0.25, 0.75);
    let t = Instant::now();
    loop {
        let log = null.log.lock().unwrap().clone();
        if log.iter().any(|(a, _)| *a == Action::Key { hid: 0x04, down: false })
            && log.iter().any(|(a, _)| matches!(a, Action::Move { .. }))
        {
            break;
        }
        assert!(t.elapsed() < Duration::from_secs(5), "input did not arrive: {log:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut c = RzCursor::default();
    // The cursor may or may not be known yet (needs a CursorPos datagram); the call itself must be safe.
    let _ = unsafe { rz_hub_cursor(h, tile, &mut c) };

    rz_hub_unsubscribe(h, tile);
    unsafe { rz_hub_free(h) };
    let _ = std::fs::remove_dir_all(&dir);
}
