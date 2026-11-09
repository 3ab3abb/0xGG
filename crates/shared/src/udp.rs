//! UDP game-channel packets (docs/PROTOCOL.md §5).
//!
//! Wire format: `"GG"` magic (2 bytes) · protocol version (1 byte) · postcard body.
//! postcard writes the enum variant index first, so the body starts with the
//! packet kind, followed by compact varint-encoded fields.

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::constants::PROTOCOL_VERSION;
use crate::game::PlayerSim;
use crate::weapons::Weapon;

pub const MAGIC: [u8; 2] = *b"GG";
pub const HEADER_LEN: usize = 3;
/// Stay under typical path MTU (1500 - IP/UDP/VPN headers) so datagrams are
/// never IP-fragmented: one lost fragment would lose the whole datagram.
pub const MAX_DATAGRAM: usize = 1200;
/// Each Input packet repeats the newest N input frames, so a lost packet
/// costs nothing unless N packets in a row are lost.
pub const INPUT_REDUNDANCY: usize = 3;
pub const MAX_EVENTS_PER_SNAPSHOT: usize = 20;
pub const MAX_PROJECTILES_PER_SNAPSHOT: usize = 32;

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
    /// Weapons lying on the map or flying after a throw.
    pub items: Vec<ItemState>,
    pub projectiles: Vec<ProjectileState>,
    pub firewalls: Vec<FirewallState>,
    /// Reliable events this client has not acknowledged yet (§5.4).
    pub events: Vec<Event>,
    /// The receiving client's own full physics state (including hidden
    /// timers: coyote, jump buffer, drop-through, stun, previous buttons),
    /// right after applying `ack_input_seq`. The client resets its
    /// prediction to this and replays newer inputs (reconciliation).
    /// `None` while the client is dead or not in the match.
    pub you: Option<PlayerSim>,
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
    pub weapon: Option<Weapon>,
    pub ammo: u8,
}

pub mod flags {
    pub const ALIVE: u8 = 1 << 0;
    pub const FACING_RIGHT: u8 = 1 << 1;
    pub const GROUNDED: u8 = 1 << 2;
    pub const CONNECTED: u8 = 1 << 3;
    pub const AIMING_UP: u8 = 1 << 4;
    pub const STUNNED: u8 = 1 << 5;
    /// Checksum Blade swing active (hits and reflects).
    pub const SWINGING: u8 = 1 << 6;
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ItemState {
    pub id: u16,
    pub weapon: Weapon,
    pub ammo: u8,
    pub x: f32,
    pub y: f32,
    pub flying: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectileKind {
    Pellet,
    Bomb,
    /// Smaller child bomb from a fork.
    Shard,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ProjectileState {
    pub id: u16,
    pub kind: ProjectileKind,
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FirewallState {
    pub id: u16,
    pub x: f32,
    pub y: f32,
    pub ms_left: u16,
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
    /// Hitscan tracer (Ping Pistol, or a reflected shot), for drawing.
    Shot {
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
    },
    Explosion {
        x: f32,
        y: f32,
        radius: f32,
    },
    /// Hit by a thrown item.
    Stun {
        by: u8,
        victim: u8,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KillCause {
    /// Landed on another packet's header.
    Stomp,
    /// Fell into a pit.
    Fall,
    Weapon(Weapon),
    /// Own shot sent back by a Checksum Blade.
    Reflect,
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

    /// Everything maxed out: the largest snapshot the server can produce.
    fn big_snapshot() -> Snapshot {
        Snapshot {
            tick: 123_456,
            ack_input_seq: 99_999,
            round: 4,
            phase: RoundPhase::Playing { ms_left: 60_000 },
            scores: vec![(1, 4), (2, 3), (3, 4)],
            you: Some(PlayerSim {
                x: 312.5,
                y: 200.25,
                vx: -150.0,
                vy: 480.0,
                facing_right: true,
                grounded: true,
                coyote: 6,
                jump_buffer: 6,
                drop_timer: 10,
                prev_buttons: 0x7f,
                stun: 40,
            }),
            players: (1..=3)
                .map(|slot| PlayerState {
                    slot,
                    x: 312.5,
                    y: 200.25,
                    vx: -150.0,
                    vy: 480.0,
                    flags: 0b111_1011,
                    weapon: Some(Weapon::BroadcastShotgun),
                    ammo: 2,
                })
                .collect(),
            items: (0..10)
                .map(|id| ItemState {
                    id,
                    weapon: Weapon::ForkBomb,
                    ammo: 1,
                    x: 100.5,
                    y: 200.5,
                    flying: true,
                })
                .collect(),
            projectiles: (0..MAX_PROJECTILES_PER_SNAPSHOT as u16)
                .map(|id| ProjectileState {
                    id: id + 300,
                    kind: ProjectileKind::Pellet,
                    x: 321.5,
                    y: 111.25,
                })
                .collect(),
            firewalls: (0..6)
                .map(|id| FirewallState {
                    id,
                    x: 10.0,
                    y: 20.0,
                    ms_left: 4000,
                })
                .collect(),
            events: (1..=MAX_EVENTS_PER_SNAPSHOT as u32)
                .map(|id| Event {
                    id: id + 70_000,
                    kind: EventKind::Shot {
                        x0: 1.5,
                        y0: 2.5,
                        x1: 300.5,
                        y1: 2.5,
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
        println!("worst-case snapshot: {} bytes", bytes.len());
        assert!(
            bytes.len() <= MAX_DATAGRAM,
            "snapshot is {} bytes",
            bytes.len()
        );
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
