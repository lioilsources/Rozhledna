//! `u32 LE length` + protobuf message framing for control/input/preview streams.

use prost::Message;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Upper bound for one framed message (clipboard is capped at 1 MB, icons/previews are smaller).
pub const MAX_MESSAGE_LEN: usize = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FramingError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("decode: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("message too large: {0} bytes")]
    TooLarge(usize),
}

pub fn encode_framed<M: Message>(msg: &M) -> Vec<u8> {
    let len = msg.encoded_len();
    let mut buf = Vec::with_capacity(4 + len);
    buf.extend_from_slice(&(len as u32).to_le_bytes());
    msg.encode(&mut buf).expect("Vec has capacity");
    buf
}

pub async fn write_msg<W: AsyncWrite + Unpin, M: Message>(w: &mut W, msg: &M) -> Result<(), FramingError> {
    w.write_all(&encode_framed(msg)).await?;
    Ok(())
}

/// Reads one message. Returns `Ok(None)` on clean EOF at a message boundary.
pub async fn read_msg<R: AsyncRead + Unpin, M: Message + Default>(r: &mut R) -> Result<Option<M>, FramingError> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_MESSAGE_LEN {
        return Err(FramingError::TooLarge(len));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(Some(M::decode(buf.as_slice())?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pb::{Envelope, Ping, envelope::Msg};

    #[tokio::test]
    async fn roundtrip_two_messages() {
        let a = Envelope { seq: 1, reply_to: 0, msg: Some(Msg::Ping(Ping { t0_ns: 42 })) };
        let b = Envelope { seq: 2, reply_to: 1, msg: None };
        let mut wire = Vec::new();
        write_msg(&mut wire, &a).await.unwrap();
        write_msg(&mut wire, &b).await.unwrap();
        let mut r = wire.as_slice();
        assert_eq!(read_msg::<_, Envelope>(&mut r).await.unwrap(), Some(a));
        assert_eq!(read_msg::<_, Envelope>(&mut r).await.unwrap(), Some(b));
        assert_eq!(read_msg::<_, Envelope>(&mut r).await.unwrap(), None);
    }

    #[tokio::test]
    async fn rejects_oversized() {
        let wire = (u32::MAX).to_le_bytes();
        let mut r = wire.as_slice();
        assert!(matches!(read_msg::<_, Envelope>(&mut r).await, Err(FramingError::TooLarge(_))));
    }
}
