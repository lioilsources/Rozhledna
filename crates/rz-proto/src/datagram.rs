//! QUIC datagrams (plan 7.5). Fixed little-endian layouts, ≤ 1200 B.

pub const TYPE_POINTER_MOTION: u8 = 0x01;
pub const TYPE_CURSOR_POS: u8 = 0x02;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Datagram {
    /// hub → agent; coordinates normalized 0..1 in source content.
    PointerMotion { source_id: u64, seq: u32, x: f32, y: f32 },
    /// agent → hub.
    CursorPos { source_id: u64, seq: u32, x: f32, y: f32, shape_id: u32, visible: bool },
}

impl Datagram {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(32);
        match *self {
            Datagram::PointerMotion { source_id, seq, x, y } => {
                b.push(TYPE_POINTER_MOTION);
                b.extend_from_slice(&source_id.to_le_bytes());
                b.extend_from_slice(&seq.to_le_bytes());
                b.extend_from_slice(&x.to_le_bytes());
                b.extend_from_slice(&y.to_le_bytes());
            }
            Datagram::CursorPos { source_id, seq, x, y, shape_id, visible } => {
                b.push(TYPE_CURSOR_POS);
                b.extend_from_slice(&source_id.to_le_bytes());
                b.extend_from_slice(&seq.to_le_bytes());
                b.extend_from_slice(&x.to_le_bytes());
                b.extend_from_slice(&y.to_le_bytes());
                b.extend_from_slice(&shape_id.to_le_bytes());
                b.push(visible as u8);
            }
        }
        b
    }

    pub fn decode(b: &[u8]) -> Option<Self> {
        let u32_at = |o: usize| Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?));
        let f32_at = |o: usize| Some(f32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?));
        let source_id = u64::from_le_bytes(b.get(1..9)?.try_into().ok()?);
        match *b.first()? {
            TYPE_POINTER_MOTION if b.len() == 21 => {
                Some(Datagram::PointerMotion { source_id, seq: u32_at(9)?, x: f32_at(13)?, y: f32_at(17)? })
            }
            TYPE_CURSOR_POS if b.len() == 26 => Some(Datagram::CursorPos {
                source_id,
                seq: u32_at(9)?,
                x: f32_at(13)?,
                y: f32_at(17)?,
                shape_id: u32_at(21)?,
                visible: *b.get(25)? != 0,
            }),
            _ => None,
        }
    }
}

/// Wrapping-aware "is `a` newer than `b`" for datagram sequence numbers.
pub fn seq_newer(a: u32, b: u32) -> bool {
    a != b && a.wrapping_sub(b) < u32::MAX / 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for d in [
            Datagram::PointerMotion { source_id: 9, seq: 3, x: 0.25, y: 0.75 },
            Datagram::CursorPos { source_id: 1, seq: u32::MAX, x: 0.5, y: 0.1, shape_id: 4, visible: true },
        ] {
            assert_eq!(Datagram::decode(&d.encode()), Some(d));
        }
        assert_eq!(Datagram::decode(&[0x01, 0, 0]), None);
        assert_eq!(Datagram::decode(&[]), None);
    }

    #[test]
    fn seq_wraps() {
        assert!(seq_newer(1, 0));
        assert!(seq_newer(0, u32::MAX));
        assert!(!seq_newer(5, 5));
        assert!(!seq_newer(4, 5));
    }
}
