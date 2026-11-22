//! Deterministic test source (plan 12): moving pattern + burned-in frame number and timestamp.
//!
//! The frame number is burned in twice:
//! - machine-readable: a row of 40 black/white 16×16 cells (32 bits frame_no + 8 bit check),
//!   robust to H.264 compression → [`decode_frame_no`] reads it back after a decode;
//! - human-readable: 7-segment digits (frame number and capture time in ms), for slow-motion video.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rz_proto::clock::now_ns;

use crate::{Capture, Frame, FrameBuffer, FrameCallback, Nv12Image};

const BLACK: u8 = 16;
const WHITE: u8 = 235;
const CELL: u32 = 16;
const BITS: u32 = 40;
const ORIGIN: u32 = 16;
/// Minimum width so the bit row fits.
pub const MIN_WIDTH: u32 = ORIGIN * 2 + CELL * BITS;
const BAND_H: u32 = 120;

fn check_byte(n: u32) -> u8 {
    n.to_le_bytes().iter().fold(0xA5u8, |a, b| a ^ b)
}

/// Renders frame `n` with timestamp `ts_ns` into `img`.
pub fn render(img: &mut Nv12Image, n: u64, ts_ns: u64) {
    let (w, h) = (img.width, img.height);
    // Background: colour bars moving 8 px/frame.
    let shift = ((n * 8) % w as u64) as u32;
    let bar_w = (w / 8).max(2) & !1;
    let mut yrow = vec![0u8; w as usize];
    let mut uvrow = vec![0u8; w as usize];
    const BARS: [(u8, u8, u8); 8] = [
        (180, 128, 128),
        (168, 44, 136),
        (145, 147, 44),
        (133, 63, 52),
        (63, 193, 204),
        (51, 109, 212),
        (28, 212, 120),
        (40, 128, 128),
    ];
    for x in 0..w {
        let (yy, u, v) = BARS[(((x + shift) % w) / bar_w) as usize % 8];
        yrow[x as usize] = yy;
        if x % 2 == 0 {
            uvrow[x as usize] = u;
            uvrow[x as usize + 1] = v;
        }
    }
    for row in img.y.chunks_exact_mut(w as usize) {
        row.copy_from_slice(&yrow);
    }
    for row in img.uv.chunks_exact_mut(w as usize) {
        row.copy_from_slice(&uvrow);
    }
    // A bouncing white box: obvious motion for judging smoothness.
    let box_s = (h / 6).max(16) & !1;
    let span = (w - box_s) as u64;
    let p = (n * 12) % (2 * span);
    let bx = (if p < span { p } else { 2 * span - p }) as u32 & !1;
    let by = (h / 2).saturating_sub(box_s / 2) & !1;
    fill(img, bx, by, box_s, box_s, WHITE);

    // Info band.
    fill(img, 0, 0, w, BAND_H.min(h) & !1, BLACK);
    let n32 = n as u32;
    let code = n32 as u64 | (check_byte(n32) as u64) << 32;
    for i in 0..BITS {
        let on = (code >> i) & 1 == 1;
        fill(img, ORIGIN + i * CELL, ORIGIN, CELL, CELL, if on { WHITE } else { BLACK });
    }
    let ms = (ts_ns / 1_000_000) % 1_000_000;
    let digits = format!("{:08} {:06}", n % 100_000_000, ms);
    draw_digits(img, ORIGIN, ORIGIN + CELL + 12, &digits, 4);
}

/// Overlays a block of pseudo-random luma noise (rows below the info band, `fraction` of the area).
/// Makes the content hard to compress — a stand-in for busy desktop content in benchmarks.
pub fn add_noise(img: &mut Nv12Image, seed: u64, fraction: f64) {
    let w = img.width as usize;
    let rows = ((img.height - BAND_H.min(img.height)) as f64 * fraction.clamp(0.0, 1.0)) as usize;
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let start = BAND_H as usize * w;
    for px in img.y[start..start + rows * w].iter_mut() {
        // xorshift64
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *px = 16 + (x % 220) as u8;
    }
}

fn fill(img: &mut Nv12Image, x: u32, y: u32, w: u32, h: u32, luma: u8) {
    let (iw, ih) = (img.width, img.height);
    let x1 = (x + w).min(iw);
    let y1 = (y + h).min(ih);
    for yy in y..y1 {
        let o = (yy * iw) as usize;
        img.y[o + x as usize..o + x1 as usize].fill(luma);
    }
    // Neutral chroma for the area.
    for yy in (y / 2)..y1.div_ceil(2) {
        let o = (yy * iw) as usize;
        img.uv[o + (x & !1) as usize..o + (x1.div_ceil(2) * 2).min(iw) as usize].fill(128);
    }
}

// 7-segment: bits a b c d e f g.
const SEG: [u8; 10] = [0x3F, 0x06, 0x5B, 0x4F, 0x66, 0x6D, 0x7D, 0x07, 0x7F, 0x6F];

fn draw_digits(img: &mut Nv12Image, x0: u32, y0: u32, s: &str, scale: u32) {
    let (dw, dh, t) = (6 * scale, 10 * scale, scale.max(2));
    let mut x = x0;
    for ch in s.chars() {
        if let Some(d) = ch.to_digit(10) {
            let m = SEG[d as usize];
            let seg = |img: &mut Nv12Image, i: u8, rx: u32, ry: u32, rw: u32, rh: u32| {
                if m & (1 << i) != 0 {
                    fill(img, x + rx, y0 + ry, rw, rh, WHITE);
                }
            };
            seg(img, 0, 0, 0, dw, t); // a
            seg(img, 1, dw - t, 0, t, dh / 2); // b
            seg(img, 2, dw - t, dh / 2, t, dh / 2); // c
            seg(img, 3, 0, dh - t, dw, t); // d
            seg(img, 4, 0, dh / 2, t, dh / 2); // e
            seg(img, 5, 0, 0, t, dh / 2); // f
            seg(img, 6, 0, dh / 2 - t / 2, dw, t); // g
        }
        x += dw + 3 * scale;
    }
}

/// Reads the burned-in frame number back. `luma(x, y)` returns brightness 0..=255 (full or video range).
/// Returns `None` if the check byte does not match.
pub fn decode_frame_no(luma: impl Fn(u32, u32) -> u8) -> Option<u32> {
    let mut code = 0u64;
    for i in 0..BITS {
        let cx = ORIGIN + i * CELL + CELL / 2;
        let cy = ORIGIN + CELL / 2;
        let mut sum = 0u32;
        for dy in 0..4 {
            for dx in 0..4 {
                sum += luma(cx - 2 + dx, cy - 2 + dy) as u32;
            }
        }
        if sum / 16 > 128 {
            code |= 1 << i;
        }
    }
    let n = code as u32;
    (check_byte(n) == (code >> 32) as u8).then_some(n)
}

/// Threaded test source ticking at `fps`.
pub struct TestPattern {
    width: u32,
    height: u32,
    fps: u32,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl TestPattern {
    pub fn new(width: u32, height: u32, fps: u32) -> Self {
        assert!(width >= MIN_WIDTH, "testpattern needs width >= {MIN_WIDTH}");
        Self { width: width & !1, height: height & !1, fps: fps.max(1), stop: Arc::default(), thread: None }
    }

    /// Renders one frame synchronously (for tests).
    pub fn frame(&self, n: u64) -> Frame {
        let ts = now_ns();
        let mut img = Nv12Image::new(self.width, self.height);
        render(&mut img, n, ts);
        make_frame(img, ts)
    }
}

fn make_frame(img: Nv12Image, ts: u64) -> Frame {
    let (width, height) = (img.width, img.height);
    #[cfg(target_os = "macos")]
    let buffer = match crate::pixbuf::PixelBuffer::from_nv12(&img) {
        Ok(pb) => FrameBuffer::PixelBuffer(pb),
        Err(e) => {
            tracing::warn!("CVPixelBuffer alloc failed ({e}); using CPU frame");
            FrameBuffer::Nv12(Arc::new(img))
        }
    };
    #[cfg(not(target_os = "macos"))]
    let buffer = FrameBuffer::Nv12(Arc::new(img));
    Frame { capture_ts_ns: ts, width, height, buffer }
}

impl Capture for TestPattern {
    fn start(&mut self, mut on_frame: FrameCallback) -> anyhow::Result<()> {
        anyhow::ensure!(self.thread.is_none(), "already started");
        self.stop.store(false, Ordering::SeqCst);
        let (w, h, fps, stop) = (self.width, self.height, self.fps, self.stop.clone());
        self.thread = Some(std::thread::Builder::new().name("testpattern".into()).spawn(move || {
            let interval = Duration::from_secs_f64(1.0 / fps as f64);
            let t0 = Instant::now();
            let mut n = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let deadline = t0 + interval * n as u32;
                if let Some(d) = deadline.checked_duration_since(Instant::now()) {
                    std::thread::sleep(d);
                }
                let ts = now_ns();
                let mut img = Nv12Image::new(w, h);
                render(&mut img, n, ts);
                on_frame(make_frame(img, ts));
                n += 1;
            }
        })?);
        Ok(())
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

impl Drop for TestPattern {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burned_frame_number_roundtrips() {
        for n in [0u64, 1, 59, 12345, u32::MAX as u64] {
            let mut img = Nv12Image::new(1280, 720);
            render(&mut img, n, 123_456_789);
            let got = decode_frame_no(|x, y| img.y[(y * img.width + x) as usize]);
            assert_eq!(got, Some(n as u32));
        }
    }

    #[test]
    fn garbage_is_rejected() {
        assert_eq!(decode_frame_no(|x, _| if x % 3 == 0 { 255 } else { 0 }), None);
    }

    #[test]
    fn ticks() {
        let mut tp = TestPattern::new(1280, 720, 120);
        let (tx, rx) = std::sync::mpsc::channel();
        tp.start(Box::new(move |f| {
            let _ = tx.send(f.capture_ts_ns);
        }))
        .unwrap();
        let a = rx.recv().unwrap();
        let b = rx.recv().unwrap();
        tp.stop();
        assert!(b > a);
    }
}
