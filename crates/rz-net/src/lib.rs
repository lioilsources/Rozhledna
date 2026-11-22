//! QUIC transport for Rozhledna (plan 6.4).
//!
//! Phase 0 scope: dev self-signed certificate + insecure verifier (`--insecure-dev`),
//! frame-per-stream video, length-prefixed control stream, clock sync.
//! Identity, trust store and SAS pairing arrive in Phase 1 (T1.1).

pub mod control;
pub mod tls;
pub mod video;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{Endpoint, TransportConfig, VarInt};

pub use quinn::{Connection, RecvStream, SendStream};

/// Transport tuned for LAN (plan 6.4).
pub fn transport_config() -> TransportConfig {
    let mut t = TransportConfig::default();
    t.max_concurrent_uni_streams(VarInt::from_u32(4096))
        .max_concurrent_bidi_streams(VarInt::from_u32(16))
        .stream_receive_window(VarInt::from_u32(8 * 1024 * 1024))
        .receive_window(VarInt::from_u32(64 * 1024 * 1024))
        .send_window(64 * 1024 * 1024)
        .keep_alive_interval(Some(Duration::from_secs(2)))
        .max_idle_timeout(Some(Duration::from_secs(10).try_into().unwrap()))
        .datagram_receive_buffer_size(Some(1024 * 1024))
        .datagram_send_buffer_size(1024 * 1024)
        .initial_rtt(Duration::from_millis(5));
    // Large initial congestion window: on a LAN, slow start would throttle the first keyframes.
    let mut cc = quinn::congestion::CubicConfig::default();
    cc.initial_window(4 * 1024 * 1024);
    t.congestion_controller_factory(Arc::new(cc));
    t
}

/// Server endpoint (agent side) with a dev self-signed certificate.
pub fn server_endpoint(bind: SocketAddr) -> Result<Endpoint> {
    let id = tls::DevIdentity::generate()?;
    let crypto = tls::server_crypto(&id)?;
    let mut cfg = quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(crypto)?));
    cfg.transport_config(Arc::new(transport_config()));
    let ep = Endpoint::server(cfg, bind).with_context(|| format!("bind {bind}"))?;
    tracing::info!(fp = %id.fingerprint_hex(), "listening on {}", ep.local_addr()?);
    Ok(ep)
}

/// Client endpoint (hub side). `insecure_dev` accepts any server certificate.
pub fn client_endpoint(insecure_dev: bool) -> Result<Endpoint> {
    anyhow::ensure!(insecure_dev, "Phase 0 only supports --insecure-dev (pairing lands in Phase 1)");
    let crypto = tls::insecure_client_crypto()?;
    let mut cfg = quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(crypto)?));
    cfg.transport_config(Arc::new(transport_config()));
    let mut ep = Endpoint::client("0.0.0.0:0".parse().unwrap())?;
    ep.set_default_client_config(cfg);
    Ok(ep)
}

pub async fn connect(ep: &Endpoint, addr: SocketAddr) -> Result<Connection> {
    let conn = ep.connect(addr, "rozhledna")?.await.with_context(|| format!("connect {addr}"))?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rz_proto::frame::{FLAG_KEYFRAME, FrameHeader};

    #[tokio::test(flavor = "multi_thread")]
    async fn loopback_frames_and_control() -> Result<()> {
        let server = server_endpoint("127.0.0.1:0".parse()?)?;
        let addr = server.local_addr()?;
        let srv = tokio::spawn(async move {
            let conn = server.accept().await.unwrap().await.unwrap();
            let (send, recv) = conn.accept_bi().await.unwrap();
            let mut ctl = control::Control::new(send, recv);
            // Answer one ping, then send 3 frames.
            let env = ctl.recv().await.unwrap().unwrap();
            control::answer_ping(&mut ctl, &env).await.unwrap();
            for n in 0..3u64 {
                let hdr = FrameHeader { stream_id: 1, frame_no: n, flags: FLAG_KEYFRAME, ..Default::default() };
                video::send_frame(&conn, hdr, &vec![n as u8; 100_000], 0).await.unwrap();
            }
            conn.closed().await;
        });

        let client = client_endpoint(true)?;
        let conn = connect(&client, addr).await?;
        let (send, recv) = conn.open_bi().await?;
        let mut ctl = control::Control::new(send, recv);
        let mut sync = rz_proto::clock::ClockSync::default();
        control::ping_once(&mut ctl, &mut sync).await?;
        let off = sync.offset_ns().unwrap();
        assert!(off.abs() < 5_000_000, "same host → offset ≈ 0, got {off}");

        let mut got = Vec::new();
        for _ in 0..3 {
            let s = conn.accept_uni().await?;
            let (h, p) = video::recv_frame(s).await?;
            assert_eq!(p.len(), 100_000);
            assert!(p.iter().all(|&b| b == h.frame_no as u8));
            got.push(h.frame_no);
        }
        got.sort();
        assert_eq!(got, vec![0, 1, 2]);
        conn.close(0u32.into(), b"done");
        srv.await?;
        Ok(())
    }
}
