//! System state the hub needs to know (plan 8, 13): TCC permissions, locked screen, display sleep;
//! and the display-sleep assertion held while somebody is watching.

use rz_proto::pb;

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::{c_char, c_void};

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct CGRect {
        pub x: f64,
        pub y: f64,
        pub w: f64,
        pub h: f64,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        pub fn CGSessionCopyCurrentDictionary() -> *const c_void;
        pub fn CGMainDisplayID() -> u32;
        pub fn CGDisplayIsAsleep(display: u32) -> u32;
        pub fn CGDisplayBounds(display: u32) -> CGRect;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        pub fn CFDictionaryGetValue(dict: *const c_void, key: *const c_void) -> *const c_void;
        pub fn CFStringCreateWithCString(alloc: *const c_void, s: *const c_char, encoding: u32) -> *const c_void;
        pub fn CFBooleanGetValue(b: *const c_void) -> u8;
        pub fn CFGetTypeID(cf: *const c_void) -> usize;
        pub fn CFBooleanGetTypeID() -> usize;
        pub fn CFRelease(cf: *const c_void);
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        pub fn IOPMAssertionCreateWithName(kind: *const c_void, level: u32, name: *const c_void, id: *mut u32) -> i32;
        pub fn IOPMAssertionRelease(id: u32) -> i32;
    }

    pub const UTF8: u32 = 0x0800_0100;

    /// Owned CFString from a NUL-terminated literal.
    pub struct CfStr(pub *const c_void);
    impl CfStr {
        pub fn new(s: &std::ffi::CStr) -> Self {
            Self(unsafe { CFStringCreateWithCString(std::ptr::null(), s.as_ptr(), UTF8) })
        }
    }
    impl Drop for CfStr {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CFRelease(self.0) }
            }
        }
    }
}

/// `CGSSessionScreenIsLocked` from the current session dictionary.
#[cfg(target_os = "macos")]
pub fn screen_locked() -> bool {
    use mac::*;
    unsafe {
        let d = CGSessionCopyCurrentDictionary();
        if d.is_null() {
            // No GUI session for this process (e.g. ssh without login): treat as locked.
            return true;
        }
        let key = CfStr::new(c"CGSSessionScreenIsLocked");
        let v = CFDictionaryGetValue(d, key.0);
        let locked = !v.is_null() && CFGetTypeID(v) == CFBooleanGetTypeID() && CFBooleanGetValue(v) != 0;
        CFRelease(d);
        locked
    }
}

#[cfg(target_os = "macos")]
pub fn display_asleep() -> bool {
    unsafe { mac::CGDisplayIsAsleep(mac::CGMainDisplayID()) != 0 }
}

/// Global bounds of a display in points (origin top-left of the main display).
#[cfg(target_os = "macos")]
pub fn display_bounds(id: u32) -> pb::Rect {
    let r = unsafe { mac::CGDisplayBounds(id) };
    pb::Rect { x: r.x, y: r.y, w: r.w, h: r.h }
}

pub fn current() -> pb::AgentStatus {
    #[cfg(target_os = "macos")]
    {
        let (sr, ax) = rz_capture::macos_sck::permissions();
        pb::AgentStatus {
            screen_recording: sr,
            accessibility: ax,
            screen_locked: screen_locked(),
            display_asleep: display_asleep(),
        }
    }
    #[cfg(not(target_os = "macos"))]
    pb::AgentStatus::default()
}

/// Keeps the display awake while held (`PreventUserIdleDisplaySleep`, plan 8). Shared by all active streams.
pub struct DisplayAwake {
    #[cfg(target_os = "macos")]
    id: u32,
}

impl DisplayAwake {
    pub fn new() -> Self {
        #[cfg(target_os = "macos")]
        {
            use mac::*;
            let kind = CfStr::new(c"PreventUserIdleDisplaySleep");
            let name = CfStr::new(c"Rozhledna: a hub is watching this screen");
            let mut id = 0u32;
            let r = unsafe { IOPMAssertionCreateWithName(kind.0, 255, name.0, &mut id) };
            if r != 0 {
                tracing::warn!("IOPMAssertionCreateWithName failed: {r:#x}");
            } else {
                tracing::debug!(id, "display sleep prevented");
            }
            Self { id }
        }
        #[cfg(not(target_os = "macos"))]
        Self {}
    }
}

impl Default for DisplayAwake {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for DisplayAwake {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        if self.id != 0 {
            unsafe { mac::IOPMAssertionRelease(self.id) };
            tracing::debug!(id = self.id, "display sleep allowed again");
        }
    }
}
