//! macOS injection via `CGEventPost(kCGHIDEventTap)` (plan 6.7). Needs Accessibility; without it macOS silently
//! drops the events, so [`CgEventInjector::available`] reports `AXIsProcessTrusted`.
//!
//! - Event source `CombinedSessionState` with local-events suppression interval 0: by default macOS ignores the
//!   local mouse/keyboard for 250 ms after every synthetic event, which would make the Mac unusable for the person
//!   sitting at it while a hub moves the pointer.
//! - Modifier keys are posted as `kCGEventFlagsChanged` with the new flags (a keyboard event with a modifier key
//!   code alone does not change the modifier state for other apps).
//! - Mouse moves while a button is held are posted as `…Dragged`.

use std::ffi::c_void;

use crate::{Action, Injector, Mods, hid};

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

type CGEventRef = *mut c_void;
type CGEventSourceRef = *mut c_void;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceCreate(state: i32) -> CGEventSourceRef;
    fn CGEventSourceSetLocalEventsSuppressionInterval(source: CGEventSourceRef, seconds: f64);
    fn CGEventCreateMouseEvent(source: CGEventSourceRef, kind: u32, pos: CGPoint, button: u32) -> CGEventRef;
    fn CGEventCreateKeyboardEvent(source: CGEventSourceRef, key: u16, down: bool) -> CGEventRef;
    fn CGEventCreateScrollWheelEvent2(
        source: CGEventSourceRef,
        units: u32,
        wheel_count: u32,
        wheel1: i32,
        wheel2: i32,
        wheel3: i32,
    ) -> CGEventRef;
    fn CGEventSetType(event: CGEventRef, kind: u32);
    fn CGEventSetFlags(event: CGEventRef, flags: u64);
    fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
    fn CGEventSetDoubleValueField(event: CGEventRef, field: u32, value: f64);
    fn CGEventSetLocation(event: CGEventRef, pos: CGPoint);
    fn CGEventKeyboardSetUnicodeString(event: CGEventRef, len: usize, chars: *const u16);
    fn CGEventPost(tap: u32, event: CGEventRef);
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> u8;
}

const HID_EVENT_TAP: u32 = 0;
const SOURCE_COMBINED_SESSION: i32 = 0;

// CGEventType
const LEFT_DOWN: u32 = 1;
const LEFT_UP: u32 = 2;
const RIGHT_DOWN: u32 = 3;
const RIGHT_UP: u32 = 4;
const MOVED: u32 = 5;
const LEFT_DRAGGED: u32 = 6;
const RIGHT_DRAGGED: u32 = 7;
const FLAGS_CHANGED: u32 = 12;
const OTHER_DOWN: u32 = 25;
const OTHER_UP: u32 = 26;
const OTHER_DRAGGED: u32 = 27;

// CGEventField
const MOUSE_CLICK_STATE: u32 = 1;
const MOUSE_BUTTON_NUMBER: u32 = 3;
const MOUSE_DELTA_X: u32 = 4;
const MOUSE_DELTA_Y: u32 = 5;
const SCROLL_IS_CONTINUOUS: u32 = 88;
const SCROLL_PHASE: u32 = 99;
const SCROLL_POINT_DELTA_1: u32 = 96;
const SCROLL_POINT_DELTA_2: u32 = 97;
const SCROLL_FIXED_DELTA_1: u32 = 93;
const SCROLL_FIXED_DELTA_2: u32 = 94;

/// Protocol phase (NSEventPhase bits) → `CGScrollPhase` (CGEventTypes.h): the numbering differs, NSEventPhase
/// "changed" = 4 is `kCGScrollPhaseEnded`, so passing it through made every trackpad update an "end" and AppKit
/// views (Finder) ignored the gesture.
pub(crate) fn cg_scroll_phase(ns: u32) -> Option<i64> {
    Some(match ns {
        1 => 1,    // began
        4 => 2,    // changed
        8 => 4,    // ended
        16 => 8,   // cancelled
        32 => 128, // may begin
        _ => return None,
    })
}

// CGEventFlags (+ NX device-dependent side bits)
const F_CAPS: u64 = 0x0001_0000;
const F_SHIFT: u64 = 0x0002_0000;
const F_CTRL: u64 = 0x0004_0000;
const F_ALT: u64 = 0x0008_0000;
const F_CMD: u64 = 0x0010_0000;
const F_NON_COALESCED: u64 = 0x0000_0100;
// NX_DEVICE*KEYMASK in HID order of `Mods::sides`: lctrl, lshift, lalt, lcmd, rctrl, rshift, ralt, rcmd.
const SIDE_BITS: [u64; 8] = [0x0001, 0x0002, 0x0020, 0x0008, 0x2000, 0x0004, 0x0040, 0x0010];

fn flags(m: Mods) -> u64 {
    let mut f = F_NON_COALESCED;
    if m.caps {
        f |= F_CAPS;
    }
    if m.shift {
        f |= F_SHIFT;
    }
    if m.ctrl {
        f |= F_CTRL;
    }
    if m.alt {
        f |= F_ALT;
    }
    if m.cmd {
        f |= F_CMD;
    }
    for (i, b) in SIDE_BITS.iter().enumerate() {
        if m.sides & (1 << i) != 0 {
            f |= b;
        }
    }
    f
}

pub struct CgEventInjector {
    source: CGEventSourceRef,
    /// Set `kCGMouseEventDeltaX/Y` on motion (real mice do; `drag-probe` compares with and without).
    deltas: bool,
    last: Option<CGPoint>,
}

// SAFETY: CGEventSource is a CF object; posting from one thread at a time (the Injector is behind a Mutex).
unsafe impl Send for CgEventInjector {}

impl CgEventInjector {
    pub fn new() -> Self {
        let source = unsafe { CGEventSourceCreate(SOURCE_COMBINED_SESSION) };
        if !source.is_null() {
            unsafe { CGEventSourceSetLocalEventsSuppressionInterval(source, 0.0) };
        }
        Self { source, deltas: false, last: None }
    }

    /// Whether motion events carry `kCGMouseEventDeltaX/Y` (see `examples/drag-probe.rs`).
    pub fn set_deltas(&mut self, on: bool) {
        self.deltas = on;
    }

    fn post(&self, e: CGEventRef, m: Mods) {
        if e.is_null() {
            return;
        }
        unsafe {
            CGEventSetFlags(e, flags(m));
            CGEventPost(HID_EVENT_TAP, e);
            CFRelease(e);
        }
    }
}

impl Default for CgEventInjector {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CgEventInjector {
    fn drop(&mut self) {
        if !self.source.is_null() {
            unsafe { CFRelease(self.source) };
        }
    }
}

impl Injector for CgEventInjector {
    fn available(&self) -> bool {
        unsafe { AXIsProcessTrusted() != 0 }
    }

    fn inject(&mut self, action: &Action, m: Mods) {
        let src = self.source;
        unsafe {
            match *action {
                Action::Move { x, y, buttons } => {
                    let (kind, button) = if buttons & 1 != 0 {
                        (LEFT_DRAGGED, 0)
                    } else if buttons & 2 != 0 {
                        (RIGHT_DRAGGED, 1)
                    } else if buttons != 0 {
                        (OTHER_DRAGGED, buttons.trailing_zeros())
                    } else {
                        (MOVED, 0)
                    };
                    let e = CGEventCreateMouseEvent(src, kind, CGPoint { x, y }, button);
                    if self.deltas
                        && !e.is_null()
                        && let Some(p) = self.last
                    {
                        CGEventSetIntegerValueField(e, MOUSE_DELTA_X, (x - p.x).round() as i64);
                        CGEventSetIntegerValueField(e, MOUSE_DELTA_Y, (y - p.y).round() as i64);
                    }
                    self.last = Some(CGPoint { x, y });
                    self.post(e, m);
                }
                Action::Button { button, down, x, y, click_count } => {
                    let kind = match (button, down) {
                        (0, true) => LEFT_DOWN,
                        (0, false) => LEFT_UP,
                        (1, true) => RIGHT_DOWN,
                        (1, false) => RIGHT_UP,
                        (_, true) => OTHER_DOWN,
                        (_, false) => OTHER_UP,
                    };
                    self.last = Some(CGPoint { x, y });
                    let e = CGEventCreateMouseEvent(src, kind, CGPoint { x, y }, button as u32);
                    if !e.is_null() {
                        CGEventSetIntegerValueField(e, MOUSE_CLICK_STATE, click_count as i64);
                        CGEventSetIntegerValueField(e, MOUSE_BUTTON_NUMBER, button as i64);
                    }
                    self.post(e, m);
                }
                Action::Key { hid: u, down } => {
                    let Some(code) = hid::to_mac(u) else {
                        tracing::debug!(hid = u, "no macOS key code");
                        return;
                    };
                    let e = CGEventCreateKeyboardEvent(src, code, down);
                    if !e.is_null() && (hid::is_modifier(u) || u == hid::CAPS_LOCK) {
                        CGEventSetType(e, FLAGS_CHANGED);
                    }
                    self.post(e, m);
                }
                Action::Scroll { dx, dy, precise, phase, x, y } => {
                    // Units: 0 = pixel, 1 = line. CG wheel1 = vertical (positive = up/away), wheel2 = horizontal.
                    let units = if precise { 0 } else { 1 };
                    let e = CGEventCreateScrollWheelEvent2(src, units, 2, dy.round() as i32, dx.round() as i32, 0);
                    if !e.is_null() {
                        CGEventSetLocation(e, CGPoint { x, y });
                        if precise {
                            CGEventSetIntegerValueField(e, SCROLL_IS_CONTINUOUS, 1);
                            CGEventSetDoubleValueField(e, SCROLL_POINT_DELTA_1, dy);
                            CGEventSetDoubleValueField(e, SCROLL_POINT_DELTA_2, dx);
                            CGEventSetDoubleValueField(e, SCROLL_FIXED_DELTA_1, dy);
                            CGEventSetDoubleValueField(e, SCROLL_FIXED_DELTA_2, dx);
                            if let Some(p) = cg_scroll_phase(phase) {
                                CGEventSetIntegerValueField(e, SCROLL_PHASE, p);
                            }
                        }
                    }
                    self.post(e, m);
                }
                Action::Text(ref t) => {
                    // A keyboard event carrying a Unicode string; ≤ 20 UTF-16 units per event.
                    let units: Vec<u16> = t.encode_utf16().collect();
                    for chunk in units.chunks(20) {
                        for down in [true, false] {
                            let e = CGEventCreateKeyboardEvent(src, 0, down);
                            if !e.is_null() {
                                CGEventKeyboardSetUnicodeString(e, chunk.len(), chunk.as_ptr());
                            }
                            self.post(e, Mods::default());
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn scroll_phase_mapping() {
        use super::cg_scroll_phase as m;
        assert_eq!((m(1), m(4), m(8), m(16), m(32)), (Some(1), Some(2), Some(4), Some(8), Some(128)));
        assert_eq!(m(0), None, "mouse wheel: no phase");
        assert_eq!(m(2), None, "stationary has no CG counterpart");
    }
}
