//! Capture sources (plan 6.6). Phase 0: `testpattern` (no permissions needed) and macOS ScreenCaptureKit displays.

pub mod testpattern;

#[cfg(target_os = "macos")]
pub mod macos_sck;
#[cfg(target_os = "macos")]
pub mod pixbuf;

use std::sync::Arc;

/// A planar NV12 image in CPU memory (Y plane, then interleaved CbCr at half resolution). Video range.
#[derive(Clone)]
pub struct Nv12Image {
    pub width: u32,
    pub height: u32,
    /// `width * height` bytes.
    pub y: Vec<u8>,
    /// `width * height / 2` bytes (CbCr pairs, `width/2 × height/2`).
    pub uv: Vec<u8>,
}

impl Nv12Image {
    pub fn new(width: u32, height: u32) -> Self {
        assert!(width.is_multiple_of(2) && height.is_multiple_of(2), "NV12 needs even dimensions");
        let n = (width * height) as usize;
        Self { width, height, y: vec![16; n], uv: vec![128; n / 2] }
    }
}

pub enum FrameBuffer {
    Nv12(Arc<Nv12Image>),
    #[cfg(target_os = "macos")]
    PixelBuffer(pixbuf::PixelBuffer),
}

pub struct Frame {
    /// Local monotonic clock (`rz_proto::clock::now_ns` domain).
    pub capture_ts_ns: u64,
    pub width: u32,
    pub height: u32,
    pub buffer: FrameBuffer,
}

pub type FrameCallback = Box<dyn FnMut(Frame) + Send + 'static>;

/// A running capture. Frames are delivered on a source-owned thread/queue.
pub trait Capture: Send {
    fn start(&mut self, on_frame: FrameCallback) -> anyhow::Result<()>;
    fn stop(&mut self);
    /// Output size in pixels.
    fn size(&self) -> (u32, u32);
}
