//! UDP game-channel packets (docs/PROTOCOL.md §5).
//!
//! Wire format: `"GG"` magic (2 bytes) · protocol version (1 byte) · postcard body.
//! postcard writes the enum variant index first, so the body starts with the
//! packet kind, followed by compact varint-encoded fields.

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::constants::PROTOCOL_VERSION;

pub const MAGIC: [u8; 2] = *b"GG";
pub const HEADER_LEN: usize = 3;
/// Stay under typical path MTU (1500 - IP/UDP/VPN headers) so datagrams are
/// never IP-fragmented: one lost fragment would lose the whole datagram.
pub const MAX_DATAGRAM: usize = 1200;
/// Each Input packet repeats the newest N input frames, so a lost packet
/// costs nothing unless N packets in a row are lost.
pub const INPUT_REDUNDANCY: usize = 3;
pub const MAX_EVENTS_PER_SNAPSHOT: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputFrame {
    /// Increments once per client tick (60 Hz).
    pub seq: u32,
    /// `game::buttons` bitfield.
    pub buttons: u16,
    /// Server tick the client was displaying (lag compensation, milestone 4).
    pub view_tick: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClientPacket {
    /// Binds this UDP source address to the player who owns `token`.
    Hello {
        token: u64,
    },
    Input {
        token: u64,
        frames: Vec<InputFrame>,
        last_snapshot_tick: u32,
        events_ack: u32,
    },
    Ping {
        token: u64,
        client_time_ms: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ServerPacket {
    HelloAck,
    Snapshot(Snapshot),
    Pong { client_time_ms: u32 },
}

/// Full world state. Never a delta, so any single snapshot is enough to
/// rebuild the world and a lost one is simply replaced by the next.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub tick: u32,
    /// Last `InputFrame::seq` the server applied for the receiving client.
    pub ack_input_seq: u32,
    pub round: u8,
    pub phase: RoundPhase,
    /// `(slot, round_wins)`.
    pub scores: Vec<(u8, u8)>,
    pub players: Vec<PlayerState>,
    /// Reliable events this client has not acknowledged yet (§5.4).
    pub events: Vec<Event>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoundPhase {
    Countdown { ms_left: u16 },
    Playing { ms_left: u32 },
    RoundOver { winner: Option<u8>, ms_left: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PlayerState {
    pub slot: u8,
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    pub flags: u8,
}

pub mod flags {
    pub const ALIVE: u8 = 1 << 0;
    pub const FACING_RIGHT: u8 = 1 << 1;
    pub const GROUNDED: u8 = 1 << 2;
    pub const CONNECTED: u8 = 1 << 3;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Per-client, starts at 1, +1 per event. The client ACKs the highest
    /// contiguous id it processed (cumulative ACK, like TCP's ACK number).
    pub id: u32,
    pub kind: EventKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EventKind {
    Kill {
        killer: Option<u8>,
        victim: u8,
        cause: KillCause,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KillCause {
    /// Landed on another packet's header.
    Stomp,
    /// Fell into a pit.
    Fall,
}

pub fn encode<T: Serialize>(packet: &T) -> Result<Vec<u8>, String> {
    let mut buf = Vec::with_capacity(256);
    buf.extend_from_slice(&MAGIC);
    buf.push(PROTOCOL_VERSION);
    let body = postcard::to_allocvec(packet).map_err(|e| e.to_string())?;
    buf.extend_from_slice(&body);
    if buf.len() > MAX_DATAGRAM {
        return Err(format!("datagram too large: {} bytes", buf.len()));
    }
    Ok(buf)
}

/// `None` for anything that is not a valid packet of ours: callers just drop it.
pub fn decode<T: DeserializeOwned>(buf: &[u8]) -> Option<T> {
    if buf.len() < HEADER_LEN || buf[..2] != MAGIC || buf[2] != PROTOCOL_VERSION {
        return None;
    }
    postcard::from_bytes(&buf[HEADER_LEN..]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big_snapshot() -> Snapshot {
        Snapshot {
            tick: 123_456,
            ack_input_seq: 99_999,
            round: 4,
            phase: RoundPhase::Playing { ms_left: 60_000 },
            scores: vec![(1, 4), (2, 3), (3, 4)],
            players: (1..=3)
                .map(|slot| PlayerState {
                    slot,
                    x: 312.5,
                    y: 200.25,
                    vx: -150.0,
                    vy: 480.0,
                    flags: 0b1011,
                })
                .collect(),
            events: (1..=MAX_EVENTS_PER_SNAPSHOT as u32)
                .map(|id| Event {
                    id,
                    kind: EventKind::Kill {
                        killer: Some(1),
                        victim: 2,
                        cause: KillCause::Stomp,
                    },
                })
                .collect(),
        }
    }

    #[test]
    fn roundtrip() {
        let s = ServerPacket::Snapshot(big_snapshot());
        assert_eq!(decode::<ServerPacket>(&encode(&s).unwrap()), Some(s));
        let c = ClientPacket::Input {
            token: u64::MAX,
            frames: vec![
                InputFrame {
                    seq: 7,
                    buttons: 0b101,
                    view_tick: 9
                };
                INPUT_REDUNDANCY
            ],
            last_snapshot_tick: 9,
            events_ack: 3,
        };
        assert_eq!(decode::<ClientPacket>(&encode(&c).unwrap()), Some(c));
    }

    #[test]
    fn worst_case_snapshot_fits_one_datagram() {
        let bytes = encode(&ServerPacket::Snapshot(big_snapshot())).unwrap();
        assert!(bytes.len() < 400, "snapshot is {} bytes", bytes.len());
    }

    #[test]
    fn rejects_foreign_packets() {
        let mut good = encode(&ClientPacket::Hello { token: 1 }).unwrap();
        assert!(decode::<ClientPacket>(&good).is_some());
        good[2] = PROTOCOL_VERSION + 1;
        assert!(decode::<ClientPacket>(&good).is_none());
        assert!(decode::<ClientPacket>(b"GET / HTTP/1.1\r\n").is_none());
        assert!(decode::<ClientPacket>(&[]).is_none());
        assert!(decode::<ClientPacket>(b"GG\x01\xff\xff\xff").is_none());
    }
}
