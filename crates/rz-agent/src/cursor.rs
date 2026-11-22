//! Cursor tracker (plan 8, T1.4). The cursor is drawn locally on the hub (D6), so the video never contains it
//! (`showsCursor=false`); the agent sends its position (`CursorPos` datagrams) and shape (`CursorShape`).
//!
//! macOS: position from `CGEventGetLocation(CGEventCreate(NULL))` at 60 Hz, shape from
//! `+[NSCursor currentSystemCursor]` at 30 Hz (deprecated but still working on macOS 27, DECISIONS.md). Shapes are
//! identified by a hash of the image's TIFF data; standard AppKit cursors map to a `kind`, anything else is sent
//! as PNG with its hotspot.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use rz_proto::pb;
use tokio::sync::watch;

/// A cursor shape; `id` is stable for the agent's lifetime.
#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    pub id: u32,
    pub kind: String,
    /// Empty for a known `kind`.
    pub png: Vec<u8>,
    /// In PNG pixels.
    pub hotspot: (u32, u32),
}

impl Shape {
    pub fn to_pb(&self) -> pb::CursorShape {
        pb::CursorShape {
            shape_id: self.id,
            kind: self.kind.clone(),
            png: self.png.clone(),
            hotspot_x: self.hotspot.0,
            hotspot_y: self.hotspot.1,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    /// Global points.
    pub x: f64,
    pub y: f64,
    pub shape: Option<Arc<Shape>>,
}

/// Starts the sampler thread (once per agent). Returns `None` where cursor tracking is not available.
pub fn start() -> Option<watch::Receiver<Sample>> {
    #[cfg(target_os = "macos")]
    {
        let (tx, rx) = watch::channel(Sample { x: 0.0, y: 0.0, shape: None });
        std::thread::Builder::new().name("rz-cursor".into()).spawn(move || mac::run(tx)).ok()?;
        Some(rx)
    }
    #[cfg(not(target_os = "macos"))]
    None
}

#[cfg(target_os = "macos")]
mod mac {
    #![allow(deprecated)] // currentSystemCursor, resize*Cursor

    use std::hash::{Hash, Hasher};

    use objc2::rc::autoreleasepool;
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSCursor};
    use objc2_foundation::NSDictionary;

    use super::*;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreate(src: *const std::ffi::c_void) -> *mut std::ffi::c_void;
        fn CGEventGetLocation(e: *mut std::ffi::c_void) -> CGPoint;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: *const std::ffi::c_void);
    }
    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {
        static NSApp: *const std::ffi::c_void;
    }

    fn location() -> (f64, f64) {
        unsafe {
            let e = CGEventCreate(std::ptr::null());
            if e.is_null() {
                return (0.0, 0.0);
            }
            let p = CGEventGetLocation(e);
            CFRelease(e);
            (p.x, p.y)
        }
    }

    fn tiff_hash(c: &NSCursor) -> Option<(u64, Vec<u8>)> {
        let d = c.image().TIFFRepresentation()?.to_vec();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        d.hash(&mut h);
        Some((h.finish(), d))
    }

    fn known() -> HashMap<u64, &'static str> {
        let mut m = HashMap::new();
        let mut add = |c: objc2::rc::Retained<NSCursor>, k: &'static str| {
            if let Some((h, _)) = tiff_hash(&c) {
                m.entry(h).or_insert(k);
            }
        };
        add(NSCursor::arrowCursor(), "arrow");
        add(NSCursor::IBeamCursor(), "ibeam");
        add(NSCursor::pointingHandCursor(), "pointer");
        add(NSCursor::crosshairCursor(), "crosshair");
        add(NSCursor::openHandCursor(), "grab");
        add(NSCursor::closedHandCursor(), "grabbing");
        add(NSCursor::resizeLeftRightCursor(), "resize-h");
        add(NSCursor::resizeUpDownCursor(), "resize-v");
        add(NSCursor::columnResizeCursor(), "resize-h");
        add(NSCursor::rowResizeCursor(), "resize-v");
        add(NSCursor::operationNotAllowedCursor(), "not-allowed");
        add(NSCursor::dragCopyCursor(), "copy");
        m
    }

    fn png_of(tiff: &[u8]) -> Option<(Vec<u8>, u32)> {
        let data = objc2_foundation::NSData::with_bytes(tiff);
        let rep = NSBitmapImageRep::imageRepWithData(&data)?;
        let props = NSDictionary::new();
        let png = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &props) }?;
        Some((png.to_vec(), rep.pixelsWide() as u32))
    }

    pub fn run(tx: watch::Sender<Sample>) {
        let Some(mtm) = objc2::MainThreadMarker::new() else {
            // Not on the main thread: AppKit's cursor images need an NSApplication instance, which main() creates.
            return run_inner(tx);
        };
        let _ = objc2_app_kit::NSApplication::sharedApplication(mtm);
        run_inner(tx)
    }

    fn run_inner(tx: watch::Sender<Sample>) {
        // Without an NSApplication (tests, odd embeddings) the standard cursors are NULL (objc2 would panic).
        let kinds = if unsafe { !NSApp.is_null() } { autoreleasepool(|_| known()) } else { HashMap::new() };
        if kinds.is_empty() {
            tracing::warn!("cursor: AppKit cursors unavailable (no NSApplication?); shapes disabled");
        }
        let mut ids: HashMap<u64, Arc<Shape>> = HashMap::new();
        let mut shape: Option<Arc<Shape>> = None;
        let mut warned_nil = false;
        let mut tick = 0u64;
        loop {
            if tx.is_closed() {
                return;
            }
            let (x, y) = location();
            if tick.is_multiple_of(2) {
                autoreleasepool(|_| match NSCursor::currentSystemCursor() {
                    None => {
                        if !warned_nil {
                            tracing::warn!("cursor: currentSystemCursor returned nil; shape tracking off");
                            warned_nil = true;
                        }
                    }
                    Some(c) => {
                        let Some((h, tiff)) = tiff_hash(&c) else { return };
                        let s = match ids.get(&h) {
                            Some(s) => s.clone(),
                            None => {
                                let hs = c.hotSpot();
                                let size = c.image().size();
                                let id = ids.len() as u32 + 1;
                                let s = Arc::new(match kinds.get(&h) {
                                    Some(k) => Shape {
                                        id,
                                        kind: (*k).into(),
                                        png: vec![],
                                        hotspot: (hs.x as u32, hs.y as u32),
                                    },
                                    None => {
                                        let (png, px_w) = png_of(&tiff).unwrap_or_default();
                                        let scale = if size.width > 0.0 { px_w as f64 / size.width } else { 1.0 };
                                        Shape {
                                            id,
                                            kind: String::new(),
                                            png,
                                            hotspot: ((hs.x * scale) as u32, (hs.y * scale) as u32),
                                        }
                                    }
                                });
                                tracing::debug!(id, kind = %s.kind, png = s.png.len(), "new cursor shape");
                                ids.insert(h, s.clone());
                                s
                            }
                        };
                        shape = Some(s);
                    }
                });
            }
            tick += 1;
            let sample = Sample { x, y, shape: shape.clone() };
            tx.send_if_modified(|cur| {
                if *cur != sample {
                    *cur = sample;
                    true
                } else {
                    false
                }
            });
            std::thread::sleep(Duration::from_micros(16_667));
        }
    }
}
