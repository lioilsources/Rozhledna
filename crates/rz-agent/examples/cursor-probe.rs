//! T1.4 experiment: does `+[NSCursor currentSystemCursor]` still return the system cursor on this macOS?
//! Prints for 5 s (10 Hz): pointer location, whether the cursor is nil, image size, hotspot, TIFF hash.
#![allow(deprecated)]

fn main() {
    #[cfg(target_os = "macos")]
    mac::run();
}

#[cfg(target_os = "macos")]
mod mac {
    use std::hash::{Hash, Hasher};

    use objc2_app_kit::NSCursor;

    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    struct CGPoint {
        x: f64,
        y: f64,
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreate(src: *const std::ffi::c_void) -> *mut std::ffi::c_void;
        fn CGEventGetLocation(e: *mut std::ffi::c_void) -> CGPoint;
    }

    pub fn run() {
        // AppKit cursors need an NSApplication instance.
        let mtm = objc2::MainThreadMarker::new().unwrap();
        let _app = objc2_app_kit::NSApplication::sharedApplication(mtm);
        let std_hash = |c: &NSCursor| {
            let d = c.image().TIFFRepresentation().map(|d| d.to_vec()).unwrap_or_default();
            let mut h = std::collections::hash_map::DefaultHasher::new();
            d.hash(&mut h);
            h.finish()
        };
        println!("arrow  {:016x}", std_hash(&NSCursor::arrowCursor()));
        println!("ibeam  {:016x}", std_hash(&NSCursor::IBeamCursor()));
        println!("hand   {:016x}", std_hash(&NSCursor::pointingHandCursor()));
        for _ in 0..50 {
            let p = unsafe { CGEventGetLocation(CGEventCreate(std::ptr::null())) };
            match NSCursor::currentSystemCursor() {
                None => println!("({:.0},{:.0}) currentSystemCursor = nil", p.x, p.y),
                Some(c) => {
                    let s = c.image().size();
                    let hs = c.hotSpot();
                    println!(
                        "({:.0},{:.0}) cursor {:.0}x{:.0} hotspot ({:.0},{:.0}) hash {:016x}",
                        p.x,
                        p.y,
                        s.width,
                        s.height,
                        hs.x,
                        hs.y,
                        std_hash(&c)
                    );
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}
