//! Annex B ↔ AVCC (length-prefixed) helpers for H.264.

pub const START_CODE: [u8; 4] = [0, 0, 0, 1];

pub const NAL_SLICE: u8 = 1;
pub const NAL_IDR: u8 = 5;
pub const NAL_SEI: u8 = 6;
pub const NAL_SPS: u8 = 7;
pub const NAL_PPS: u8 = 8;
pub const NAL_AUD: u8 = 9;

pub fn nal_type(nal: &[u8]) -> u8 {
    nal.first().map_or(0, |b| b & 0x1F)
}

/// Splits an Annex B buffer into NAL units (without start codes). Accepts 3- and 4-byte start codes.
pub fn split_nals(buf: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut start: Option<usize> = None;
    while i + 3 <= buf.len() {
        if buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 1 {
            if let Some(s) = start {
                let mut end = i;
                // Trailing zero belongs to a 4-byte start code.
                while end > s && buf[end - 1] == 0 {
                    end -= 1;
                }
                if end > s {
                    out.push(&buf[s..end]);
                }
            }
            i += 3;
            start = Some(i);
        } else {
            i += 1;
        }
    }
    if let Some(s) = start
        && s < buf.len()
    {
        out.push(&buf[s..]);
    }
    out
}

/// Converts AVCC (`nal_len_size`-byte big-endian lengths) to Annex B, appending to `out`.
pub fn avcc_to_annexb(avcc: &[u8], nal_len_size: usize, out: &mut Vec<u8>) -> Result<(), &'static str> {
    let mut i = 0;
    while i < avcc.len() {
        if i + nal_len_size > avcc.len() {
            return Err("truncated NAL length");
        }
        let mut len = 0usize;
        for b in &avcc[i..i + nal_len_size] {
            len = (len << 8) | *b as usize;
        }
        i += nal_len_size;
        if i + len > avcc.len() {
            return Err("truncated NAL");
        }
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(&avcc[i..i + len]);
        i += len;
    }
    Ok(())
}

/// Builds AVCC with 4-byte lengths from NAL units.
pub fn nals_to_avcc<'a>(nals: impl IntoIterator<Item = &'a [u8]>) -> Vec<u8> {
    let mut out = Vec::new();
    for n in nals {
        out.extend_from_slice(&(n.len() as u32).to_be_bytes());
        out.extend_from_slice(n);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_mixed_start_codes() {
        let buf = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 4, 5];
        let nals = split_nals(&buf);
        assert_eq!(nals, vec![&[0x67, 1, 2][..], &[0x68, 3][..], &[0x65, 4, 5][..]]);
        assert_eq!(nals.iter().map(|n| nal_type(n)).collect::<Vec<_>>(), vec![NAL_SPS, NAL_PPS, NAL_IDR]);
    }

    #[test]
    fn avcc_roundtrip() {
        let nals: Vec<&[u8]> = vec![&[0x67, 9, 9], &[0x65, 1, 2, 3, 4]];
        let avcc = nals_to_avcc(nals.iter().copied());
        let mut annexb = Vec::new();
        avcc_to_annexb(&avcc, 4, &mut annexb).unwrap();
        assert_eq!(split_nals(&annexb), nals);
        assert!(avcc_to_annexb(&avcc[..avcc.len() - 1], 4, &mut Vec::new()).is_err());
    }
}
