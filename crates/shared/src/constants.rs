//! Protocol-wide constants. Keep in sync with `docs/PROTOCOL.md` §6.

use std::time::Duration;

/// Bumped on any incompatible protocol change; checked in `Welcome`.
pub const PROTOCOL_VERSION: u8 = 2;

/// Same port number for TCP and UDP (separate port spaces).
pub const DEFAULT_PORT: u16 = 7777;

/// Largest TCP frame payload we accept. Anything bigger is a protocol error.
pub const MAX_FRAME_LEN: usize = 64 * 1024;

pub const MAX_ROOM_PLAYERS: usize = 3;
pub const MIN_PLAYERS_TO_START: usize = 2;
pub const ROUNDS_TO_WIN: u8 = 5;
pub const MATCH_COUNTDOWN_MS: u32 = 3000;

/// Client sends an application-level `Ping` this often over TCP.
pub const TCP_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);
/// No TCP traffic for this long ⇒ the peer is considered dead.
pub const TCP_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(6);

pub const USERNAME_MIN: usize = 3;
pub const USERNAME_MAX: usize = 16;
pub const PASSWORD_MIN: usize = 3;
pub const PASSWORD_MAX: usize = 64;
pub const ROOM_NAME_MAX: usize = 24;
pub const CHAT_MAX: usize = 200;
