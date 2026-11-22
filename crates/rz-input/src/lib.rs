//! Input injection (plan 6.7, T1.3).
//!
//! - [`InputState`]: per-hub state (pressed keys and buttons, modifiers, pointer position). Turns protocol events
//!   into resolved [`Action`]s in global coordinates and can release everything it pressed (`release_all`) —
//!   so a disconnect or a lost grab never leaves a modifier stuck.
//! - [`Injector`]: posts actions to the OS. Backends: [`NullInjector`] (tests, development) and
//!   `macos::CgEventInjector`.

pub mod hid;
#[cfg(target_os = "macos")]
pub mod macos;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use rz_proto::pb;

/// Modifier state, derived from the pressed keys.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub cmd: bool,
    pub caps: bool,
    /// Which side is held (for apps that distinguish left/right): bits l_ctrl, l_shift, l_alt, l_cmd, r_… in HID order.
    pub sides: u8,
}

/// Pressed mouse buttons (bit n = button n).
pub type Buttons = u8;

/// A resolved input action in agent global coordinates (points, origin top-left of the main display).
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Move {
        x: f64,
        y: f64,
        buttons: Buttons,
    },
    Button {
        button: u8,
        down: bool,
        x: f64,
        y: f64,
        click_count: u32,
    },
    /// `hid`: keyboard usage (page 0x07).
    Key {
        hid: u16,
        down: bool,
    },
    Scroll {
        dx: f64,
        dy: f64,
        precise: bool,
        phase: u32,
        x: f64,
        y: f64,
    },
    Text(String),
}

pub trait Injector: Send {
    fn inject(&mut self, action: &Action, mods: Mods);
    /// Whether the OS will accept injected events (macOS: Accessibility granted).
    fn available(&self) -> bool {
        true
    }
}

/// Records actions; for tests and `--source testpattern` development.
#[derive(Clone, Default)]
pub struct NullInjector {
    pub log: Arc<Mutex<Vec<(Action, Mods)>>>,
}

impl Injector for NullInjector {
    fn inject(&mut self, action: &Action, mods: Mods) {
        tracing::trace!(?action, ?mods, "null inject");
        self.log.lock().unwrap().push((action.clone(), mods));
    }
}

/// Maps normalized source coordinates (0..1) to global points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SourceRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl SourceRect {
    pub fn map(&self, nx: f32, ny: f32) -> (f64, f64) {
        let c = |v: f32| (v as f64).clamp(0.0, 1.0);
        // Stay strictly inside: x = x0 + w would be on the next display.
        (self.x + c(nx) * (self.w - 0.5).max(0.0), self.y + c(ny) * (self.h - 0.5).max(0.0))
    }
}

impl From<&pb::Rect> for SourceRect {
    fn from(r: &pb::Rect) -> Self {
        Self { x: r.x, y: r.y, w: r.w, h: r.h }
    }
}

/// Per-hub input state.
#[derive(Debug, Default)]
pub struct InputState {
    keys: BTreeSet<u16>,
    buttons: Buttons,
    caps: bool,
    pos: Option<(f64, f64)>,
}

impl InputState {
    pub fn mods(&self) -> Mods {
        use hid::*;
        let k = |u| self.keys.contains(&u);
        let mut sides = 0u8;
        for (i, u) in (LEFT_CTRL..=RIGHT_GUI).enumerate() {
            if k(u) {
                sides |= 1 << i;
            }
        }
        Mods {
            ctrl: k(LEFT_CTRL) || k(RIGHT_CTRL),
            shift: k(LEFT_SHIFT) || k(RIGHT_SHIFT),
            alt: k(LEFT_ALT) || k(RIGHT_ALT),
            cmd: k(LEFT_GUI) || k(RIGHT_GUI),
            caps: self.caps,
            sides,
        }
    }

    pub fn has_pressed(&self) -> bool {
        !self.keys.is_empty() || self.buttons != 0
    }

    /// Pointer motion (datagram).
    pub fn motion(&mut self, rect: SourceRect, nx: f32, ny: f32) -> Vec<Action> {
        let (x, y) = rect.map(nx, ny);
        self.pos = Some((x, y));
        vec![Action::Move { x, y, buttons: self.buttons }]
    }

    /// A reliable input event. `rect` is the target source's rectangle (None: unknown source → ignored except
    /// keys, text and release_all, which do not need coordinates).
    pub fn event(&mut self, ev: &pb::InputEvent, rect: Option<SourceRect>) -> Vec<Action> {
        use pb::input_event::Ev;
        match &ev.ev {
            Some(Ev::Key(k)) => {
                let Some(u) = hid::keyboard_usage(k.hid_usage) else { return vec![] };
                if k.down {
                    if u == hid::CAPS_LOCK && !self.keys.contains(&u) {
                        self.caps = !self.caps;
                    }
                    // Repeat (down while down) is passed through: the OS sees autorepeat from the hub.
                    self.keys.insert(u);
                } else if !self.keys.remove(&u) {
                    return vec![]; // up without down: drop, nothing to release
                }
                vec![Action::Key { hid: u, down: k.down }]
            }
            Some(Ev::Button(b)) => {
                let Some(rect) = rect else { return vec![] };
                let button = b.button.min(7) as u8;
                let (x, y) = rect.map(b.x, b.y);
                self.pos = Some((x, y));
                let bit = 1u8 << button;
                if b.down {
                    self.buttons |= bit;
                } else if self.buttons & bit == 0 {
                    return vec![];
                } else {
                    self.buttons &= !bit;
                }
                vec![Action::Button { button, down: b.down, x, y, click_count: b.click_count.max(1) }]
            }
            Some(Ev::Scroll(s)) => {
                let (x, y) = self.pos.unwrap_or_default();
                vec![Action::Scroll { dx: s.dx as f64, dy: s.dy as f64, precise: s.precise, phase: s.phase, x, y }]
            }
            Some(Ev::Text(t)) if !t.utf8.is_empty() => vec![Action::Text(t.utf8.clone())],
            Some(Ev::ReleaseAll(true)) => self.release_all(),
            _ => vec![],
        }
    }

    /// Key-ups and button-ups for everything this hub holds; modifiers last.
    pub fn release_all(&mut self) -> Vec<Action> {
        let mut out = Vec::new();
        let (x, y) = self.pos.unwrap_or_default();
        for b in 0..8u8 {
            if self.buttons & (1 << b) != 0 {
                out.push(Action::Button { button: b, down: false, x, y, click_count: 1 });
            }
        }
        self.buttons = 0;
        let (mods, rest): (Vec<u16>, Vec<u16>) = self.keys.iter().partition(|u| hid::is_modifier(**u));
        for u in rest.into_iter().chain(mods) {
            out.push(Action::Key { hid: u, down: false });
        }
        self.keys.clear();
        out
    }

    /// Posts actions with the current modifier state. `event()` has already updated the state, so a key-down of
    /// Shift carries Shift and its key-up does not; after `release_all` everything goes out without modifiers.
    pub fn apply(&self, inj: &mut dyn Injector, actions: &[Action]) {
        let m = self.mods();
        for a in actions {
            inj.inject(a, m);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pb::input_event::Ev;

    fn key(u: u32, down: bool) -> pb::InputEvent {
        pb::InputEvent { source_id: 1, ev: Some(Ev::Key(pb::Key { hid_usage: u, down, modifiers: 0 })) }
    }

    const R: SourceRect = SourceRect { x: 100.0, y: 0.0, w: 1000.0, h: 500.0 };

    #[test]
    fn modifiers_and_release_all() {
        let mut s = InputState::default();
        let mut inj = NullInjector::default();
        for e in [key(0xE3, true), key(0xE1, true), key(0x04, true)] {
            let a = s.event(&e, Some(R));
            s.apply(&mut inj, &a);
        }
        let log = inj.log.lock().unwrap().clone();
        assert!(log[2].1.cmd && log[2].1.shift, "Cmd+Shift+A carries both modifiers");
        assert!(s.has_pressed());

        let a = s.release_all();
        assert_eq!(
            a,
            vec![
                Action::Key { hid: 0x04, down: false },
                Action::Key { hid: 0xE1, down: false },
                Action::Key { hid: 0xE3, down: false },
            ],
            "normal keys first, then modifiers"
        );
        assert!(!s.has_pressed());
        assert!(s.event(&key(0x04, false), Some(R)).is_empty(), "stray key-up after release is dropped");
    }

    #[test]
    fn mapping_and_drag() {
        let mut s = InputState::default();
        assert_eq!(R.map(0.0, 0.0), (100.0, 0.0));
        let (x, y) = R.map(1.0, 1.0);
        assert!(x < 1100.0 && y < 500.0, "never on the neighbouring display");
        let b = |down| pb::InputEvent {
            source_id: 1,
            ev: Some(Ev::Button(pb::Button { button: 0, down, x: 0.5, y: 0.5, click_count: 1 })),
        };
        s.event(&b(true), Some(R));
        let Action::Move { x, y, buttons } = s.motion(R, 0.6, 0.5)[0] else { panic!() };
        assert!((x - (100.0 + 0.6 * 999.5)).abs() < 1e-3 && (y - 0.5 * 499.5).abs() < 1e-3);
        assert_eq!(buttons, 1, "move while the left button is held = drag");
        assert_eq!(s.release_all().len(), 1, "held button released");
    }
}
