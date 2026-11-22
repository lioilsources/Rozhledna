//! Video frame header (plan 7.3): 44 bytes, little-endian, at the start of each video uni stream.

use thiserror::Error;

pub const MAGIC: [u8; 4] = *b"RZVF";
pub const HEADER_LEN: usize = 44;

pub const FLAG_KEYFRAME: u16 = 1 << 0;
pub const FLAG_DISCONTINUITY: u16 = 1 << 1;

/// Codec ids in the header (same values as `pb::Codec`).
pub const CODEC_H264: u8 = 0;
pub const CODEC_HEVC: u8 = 1;
pub const CODEC_AV1: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameHeader {
    pub stream_id: u32,
    pub frame_no: u64,
    /// Agent monotonic clock, ns.
    pub capture_ts_ns: u64,
    /// Agent monotonic clock, ns.
    pub encode_done_ts_ns: u64,
    pub flags: u16,
    pub codec: u8,
    pub width: u16,
    pub height: u16,
    pub payload_len: u32,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum HeaderError {
    #[error("bad magic {0:?}")]
    BadMagic([u8; 4]),
    #[error("short header: {0} bytes")]
    Short(usize),
}

impl FrameHeader {
    pub fn is_keyframe(&self) -> bool {
        self.flags & FLAG_KEYFRAME != 0
    }

    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[0..4].copy_from_slice(&MAGIC);
        b[4..8].copy_from_slice(&self.stream_id.to_le_bytes());
        b[8..16].copy_from_slice(&self.frame_no.to_le_bytes());
        b[16..24].copy_from_slice(&self.capture_ts_ns.to_le_bytes());
        b[24..32].copy_from_slice(&self.encode_done_ts_ns.to_le_bytes());
        b[32..34].copy_from_slice(&self.flags.to_le_bytes());
        b[34] = self.codec;
        b[35] = 0;
        b[36..38].copy_from_slice(&self.width.to_le_bytes());
        b[38..40].copy_from_slice(&self.height.to_le_bytes());
        b[40..44].copy_from_slice(&self.payload_len.to_le_bytes());
        b
    }

    pub fn decode(b: &[u8]) -> Result<Self, HeaderError> {
        if b.len() < HEADER_LEN {
            return Err(HeaderError::Short(b.len()));
        }
        let magic: [u8; 4] = b[0..4].try_into().unwrap();
        if magic != MAGIC {
            return Err(HeaderError::BadMagic(magic));
        }
        let u16_at = |o: usize| u16::from_le_bytes(b[o..o + 2].try_into().unwrap());
        let u32_at = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let u64_at = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
        Ok(Self {
            stream_id: u32_at(4),
            frame_no: u64_at(8),
            capture_ts_ns: u64_at(16),
            encode_done_ts_ns: u64_at(24),
            flags: u16_at(32),
            codec: b[34],
            width: u16_at(36),
            height: u16_at(38),
            payload_len: u32_at(40),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_layout() {
        let h = FrameHeader {
            stream_id: 7,
            frame_no: 0x0102_0304_0506_0708,
            capture_ts_ns: 11,
            encode_done_ts_ns: 22,
            flags: FLAG_KEYFRAME,
            codec: CODEC_H264,
            width: 2560,
            height: 1440,
            payload_len: 12345,
        };
        let b = h.encode();
        assert_eq!(&b[0..4], b"RZVF");
        assert_eq!(b[8], 0x08, "frame_no is little-endian at offset 8");
        assert_eq!(u16::from_le_bytes([b[36], b[37]]), 2560);
        assert_eq!(FrameHeader::decode(&b).unwrap(), h);
        assert!(h.is_keyframe());
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(FrameHeader::decode(&[0; 10]), Err(HeaderError::Short(10)));
        let mut b = FrameHeader::default().encode();
        b[0] = b'X';
        assert!(matches!(FrameHeader::decode(&b), Err(HeaderError::BadMagic(_))));
    }
}
