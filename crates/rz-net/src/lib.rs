//! QUIC transport for Rozhledna (plan 6.4).
//!
//! - Phase 0: ephemeral certificate, no client auth (`--insecure-dev`), frame-per-stream video, control stream,
//!   clock sync.
//! - Phase 1 (T1.1): persistent identity, mutual TLS, trust store, SAS pairing (`handshake`), mDNS discovery,
//!   reconnect backoff.

pub mod backoff;
pub mod control;
pub mod discovery;
pub mod handshake;
pub mod identity;
pub mod lan;
pub mod pair;
pub mod tls;
pub mod trust;
pub mod video;

pub use identity::{Fingerprint, Identity};

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

/// Server endpoint (agent side) with an ephemeral certificate and no client auth (Phase 0, `--insecure-dev`).
pub fn server_endpoint(bind: SocketAddr) -> Result<Endpoint> {
    let id = Identity::generate()?;
    server_with(bind, tls::server_crypto(&id)?, &id)
}

/// Server endpoint that requires a client certificate (Phase 1 agent).
pub fn server_endpoint_mtls(bind: SocketAddr, id: &Identity) -> Result<Endpoint> {
    server_with(bind, tls::server_crypto_mtls(id)?, id)
}

fn server_with(bind: SocketAddr, crypto: rustls::ServerConfig, id: &Identity) -> Result<Endpoint> {
    let mut cfg = quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(crypto)?));
    cfg.transport_config(Arc::new(transport_config()));
    let ep = Endpoint::server(cfg, bind).with_context(|| format!("bind {bind}"))?;
    tracing::info!(fp = %id.fingerprint.short(), "listening on {}", ep.local_addr()?);
    Ok(ep)
}

/// Client endpoint (hub side) presenting `id` as its certificate (Phase 1 hub).
pub fn client_endpoint_mtls(id: &Identity) -> Result<Endpoint> {
    client_with(tls::client_crypto(Some(id))?)
}

fn client_with(crypto: rustls::ClientConfig) -> Result<Endpoint> {
    let mut cfg = quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(crypto)?));
    cfg.transport_config(Arc::new(transport_config()));
    let mut ep = Endpoint::client("0.0.0.0:0".parse().unwrap())?;
    ep.set_default_client_config(cfg);
    Ok(ep)
}

/// Client endpoint (hub side). `insecure_dev` accepts any server certificate.
pub fn client_endpoint(insecure_dev: bool) -> Result<Endpoint> {
    anyhow::ensure!(insecure_dev, "without an identity only --insecure-dev is possible; use client_endpoint_mtls");
    client_with(tls::insecure_client_crypto()?)
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
