//! Video: one QUIC unidirectional stream per frame (plan 6.4, 7.3).

use anyhow::{Context, Result, ensure};
use quinn::{Connection, RecvStream};
use rz_proto::frame::{FrameHeader, HEADER_LEN};

/// Max accepted payload (a 4K keyframe at high bitrate stays well below this).
pub const MAX_PAYLOAD: usize = 32 * 1024 * 1024;

/// Opens a uni stream, writes header + payload, finishes it.
/// `priority`: higher is sent first (FOCUS > LIVE > preview).
/// Returns once the data is handed to quinn (not when acked).
pub async fn send_frame(conn: &Connection, mut hdr: FrameHeader, payload: &[u8], priority: i32) -> Result<()> {
    hdr.payload_len = payload.len() as u32;
    let mut s = conn.open_uni().await?;
    s.set_priority(priority)?;
    s.write_all(&hdr.encode()).await?;
    s.write_all(payload).await?;
    s.finish()?;
    Ok(())
}

/// Reads one whole frame from a video stream.
pub async fn recv_frame(mut s: RecvStream) -> Result<(FrameHeader, Vec<u8>)> {
    let mut hb = [0u8; HEADER_LEN];
    s.read_exact(&mut hb).await.context("frame header")?;
    let hdr = FrameHeader::decode(&hb)?;
    let len = hdr.payload_len as usize;
    ensure!(len <= MAX_PAYLOAD, "payload too large: {len}");
    let mut payload = vec![0u8; len];
    s.read_exact(&mut payload).await.context("frame payload")?;
    Ok((hdr, payload))
}

/// Reads just the header (the hub decides from it whether the rest is still wanted).
pub async fn recv_header(s: &mut RecvStream) -> Result<FrameHeader> {
    let mut hb = [0u8; HEADER_LEN];
    s.read_exact(&mut hb).await.context("frame header")?;
    let hdr = FrameHeader::decode(&hb)?;
    ensure!((hdr.payload_len as usize) <= MAX_PAYLOAD, "payload too large: {}", hdr.payload_len);
    Ok(hdr)
}

/// Reads the payload announced by `hdr`.
pub async fn recv_payload(s: &mut RecvStream, hdr: &FrameHeader) -> Result<Vec<u8>> {
    let mut payload = vec![0u8; hdr.payload_len as usize];
    s.read_exact(&mut payload).await.context("frame payload")?;
    Ok(payload)
}
