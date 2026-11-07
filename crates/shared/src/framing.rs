//! TCP message framing: `[u32 big-endian length][JSON payload]`.
//!
//! TCP is a byte stream, so a single `read()` may return half a message or
//! several messages glued together. The length prefix tells the receiver
//! exactly where each message ends.

use std::io::{self, Read, Write};

use serde::{de::DeserializeOwned, Serialize};

use crate::constants::MAX_FRAME_LEN;

fn invalid(msg: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// Serialize `msg` and write it as one frame.
///
/// Header and payload go out in a single `write_all` so they normally leave
/// in the same TCP segment (we run with `TCP_NODELAY`).
pub fn write_frame<W: Write, T: Serialize>(w: &mut W, msg: &T) -> io::Result<()> {
    let payload = serde_json::to_vec(msg).map_err(invalid)?;
    if payload.len() > MAX_FRAME_LEN {
        return Err(invalid("frame too large"));
    }
    let mut buf = Vec::with_capacity(4 + payload.len());
    buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    buf.extend_from_slice(&payload);
    w.write_all(&buf)?;
    w.flush()
}

/// Block until one full frame has arrived, then decode it.
///
/// Errors:
/// - `UnexpectedEof`: peer closed the connection;
/// - `WouldBlock` / `TimedOut`: read timeout (heartbeat missed);
/// - `InvalidData`: oversized frame or undecodable payload.
pub fn read_frame<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<T> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME_LEN {
        return Err(invalid(format!("frame length {len} exceeds limit")));
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    serde_json::from_slice(&payload).map_err(invalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ClientMsg;

    #[test]
    fn roundtrip_multiple_frames_in_one_buffer() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &ClientMsg::Ping { nonce: 1 }).unwrap();
        write_frame(&mut buf, &ClientMsg::CreateRoom { name: "port 80".into() }).unwrap();
        let mut r = &buf[..];
        assert_eq!(read_frame::<_, ClientMsg>(&mut r).unwrap(), ClientMsg::Ping { nonce: 1 });
        assert_eq!(
            read_frame::<_, ClientMsg>(&mut r).unwrap(),
            ClientMsg::CreateRoom { name: "port 80".into() }
        );
        assert_eq!(read_frame::<_, ClientMsg>(&mut r).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn rejects_oversized_length() {
        let buf = (u32::MAX).to_be_bytes();
        let err = read_frame::<_, ClientMsg>(&mut &buf[..]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn wire_format_is_tagged_json() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &ClientMsg::JoinRoom { room_id: 3 }).unwrap();
        assert_eq!(&buf[4..], br#"{"type":"JoinRoom","room_id":3}"#);
        assert_eq!(u32::from_be_bytes(buf[..4].try_into().unwrap()) as usize, buf.len() - 4);
    }
}
