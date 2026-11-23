//! Probe: does an injected drag resize / move a window? (dogfood Phase 1: "windows on M2 cannot be resized")
//!
//! Opens a Finder window with known bounds, drags its right edge (resize) and its title bar (move) with the same
//! events the agent posts, with and without `kCGMouseEventDeltaX/Y`, and prints the bounds before/after.
//! Takes over the mouse for ~10 s. Needs Accessibility for the terminal (or run from the agent bundle).
//!
//! `cargo run -p rz-input --example drag-probe`

#[cfg(target_os = "macos")]
fn main() {
    use std::process::Command;
    use std::thread::sleep;
    use std::time::Duration;

    use rz_input::macos::CgEventInjector;
    use rz_input::{Action, Injector, Mods};

    fn osa(script: &str) -> String {
        let out = Command::new("osascript").arg("-e").arg(script).output().expect("osascript");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
    let set = |b: [i32; 4]| {
        osa(&format!(
            "tell application \"Finder\"\nactivate\nif (count of Finder windows) = 0 then make new Finder window\n\
             set bounds of front Finder window to {{{}, {}, {}, {}}}\nend tell",
            b[0], b[1], b[2], b[3]
        ));
        sleep(Duration::from_millis(600));
    };
    let bounds = || osa("tell application \"Finder\" to get bounds of front Finder window");

    let mut inj = CgEventInjector::new();
    assert!(inj.available(), "no Accessibility for this process");
    let m = Mods::default();
    let drag = |inj: &mut CgEventInjector, from: (f64, f64), to: (f64, f64)| {
        inj.inject(&Action::Move { x: from.0, y: from.1, buttons: 0 }, m);
        sleep(Duration::from_millis(300));
        inj.inject(&Action::Button { button: 0, down: true, x: from.0, y: from.1, click_count: 1 }, m);
        sleep(Duration::from_millis(50));
        for i in 1..=25 {
            let t = i as f64 / 25.0;
            let (x, y) = (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
            inj.inject(&Action::Move { x, y, buttons: 1 }, m);
            sleep(Duration::from_millis(16));
        }
        inj.inject(&Action::Button { button: 0, down: false, x: to.0, y: to.1, click_count: 1 }, m);
        sleep(Duration::from_millis(500));
    };

    let b = [200, 200, 900, 700];
    for deltas in [false, true] {
        inj.set_deltas(deltas);
        for (what, from, to) in [
            ("resize right edge", (899.0, 450.0), (1099.0, 450.0)),
            ("resize edge +2 px outside", (902.0, 450.0), (1102.0, 450.0)),
            ("resize corner", (899.0, 699.0), (1049.0, 799.0)),
            ("move title bar", (550.0, 212.0), (650.0, 262.0)),
        ] {
            set(b);
            let before = bounds();
            drag(&mut inj, from, to);
            println!("deltas={deltas:<5} {what:<28} {before}  →  {}", bounds());
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {}
