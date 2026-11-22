//! Codec abstraction (plan 6.5). Wire format is H.264 Annex B with SPS/PPS in-band on every IDR.

pub mod annexb;
#[cfg(target_os = "macos")]
pub mod videotoolbox;

use rz_capture::Frame;

#[derive(Debug, Clone, Copy)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// 0 = auto (bits-per-pixel heuristic).
    pub bitrate_kbps: u32,
}

impl EncoderConfig {
    /// Plan 6.5: start at 0.12 bpp @ 60 fps for FOCUS.
    pub fn auto_bitrate_kbps(&self, bpp: f64) -> u32 {
        ((self.width as f64 * self.height as f64 * self.fps as f64 * bpp) / 1000.0) as u32
    }

    pub fn effective_bitrate_kbps(&self) -> u32 {
        if self.bitrate_kbps > 0 { self.bitrate_kbps } else { self.auto_bitrate_kbps(0.12) }
    }
}

/// One encoded access unit.
#[derive(Debug, Clone)]
pub struct EncodedFrame {
    /// Annex B; SPS/PPS prepended on keyframes.
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub frame_no: u64,
    pub capture_ts_ns: u64,
    pub encode_done_ts_ns: u64,
    pub width: u32,
    pub height: u32,
}

/// Encoder output. VideoToolbox in low-latency mode may drop frames to hold the bitrate,
/// so frame numbers on the wire must be assigned after encoding, not at capture.
#[derive(Debug, Clone)]
pub enum EncoderOutput {
    Frame(EncodedFrame),
    /// The encoder dropped input frame `frame_no` (or failed to encode it).
    Dropped {
        frame_no: u64,
    },
}

impl EncoderOutput {
    pub fn frame(self) -> Option<EncodedFrame> {
        match self {
            EncoderOutput::Frame(f) => Some(f),
            EncoderOutput::Dropped { .. } => None,
        }
    }
}

pub type EncodedCallback = Box<dyn FnMut(EncoderOutput) + Send + 'static>;

pub trait Encoder: Send {
    /// Submits a frame. Output arrives through the callback given at construction (possibly on another thread).
    fn encode(&mut self, frame: &Frame, frame_no: u64, force_keyframe: bool) -> anyhow::Result<()>;
    fn set_bitrate_kbps(&mut self, kbps: u32) -> anyhow::Result<()>;
    /// Blocks until all submitted frames were emitted.
    fn flush(&mut self) -> anyhow::Result<()>;
}

/// A decoded picture.
pub struct DecodedFrame {
    #[cfg(target_os = "macos")]
    pub pixel_buffer: rz_capture::pixbuf::PixelBuffer,
    pub width: u32,
    pub height: u32,
}

pub trait Decoder: Send {
    /// Decodes one Annex B access unit. Returns `Ok(None)` if the decoder needs a keyframe first.
    fn decode(&mut self, annexb: &[u8]) -> anyhow::Result<Option<DecodedFrame>>;
}
