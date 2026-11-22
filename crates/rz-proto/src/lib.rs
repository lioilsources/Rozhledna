//! Rozhledna wire protocol v1 (plan chapter 7).

pub mod clock;
pub mod datagram;
pub mod frame;
pub mod framing;

/// Generated protobuf types (`cargo xtask gen-proto`).
#[allow(clippy::all)]
pub mod pb {
    include!("gen/rozhledna.v1.rs");
}

/// Protocol version carried in `Hello.protocol` and the ALPN.
pub const PROTOCOL_VERSION: u32 = 1;
/// QUIC ALPN identifier.
pub const ALPN: &[u8] = b"rozhledna/1";
/// Default UDP port.
pub const DEFAULT_PORT: u16 = 47470;
