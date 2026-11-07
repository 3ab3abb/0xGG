//! TCP control-channel messages (docs/PROTOCOL.md §4).
//!
//! Serialized as JSON with a `"type"` tag, e.g. `{"type":"JoinRoom","room_id":3}`.

use serde::{Deserialize, Serialize};

/// Client → Server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMsg {
    Register { username: String, password: String },
    Login { username: String, password: String },
    /// Server cleans up and closes the connection.
    Logout,
    ListRooms,
    CreateRoom { name: String },
    JoinRoom { room_id: u32 },
    LeaveRoom,
    SetReady { ready: bool },
    Chat { text: String },
    Ping { nonce: u64 },
}

impl ClientMsg {
    /// Message name without its fields (safe to log: never includes passwords).
    pub fn kind(&self) -> &'static str {
        match self {
            ClientMsg::Register { .. } => "Register",
            ClientMsg::Login { .. } => "Login",
            ClientMsg::Logout => "Logout",
            ClientMsg::ListRooms => "ListRooms",
            ClientMsg::CreateRoom { .. } => "CreateRoom",
            ClientMsg::JoinRoom { .. } => "JoinRoom",
            ClientMsg::LeaveRoom => "LeaveRoom",
            ClientMsg::SetReady { .. } => "SetReady",
            ClientMsg::Chat { .. } => "Chat",
            ClientMsg::Ping { .. } => "Ping",
        }
    }
}

/// Server → Client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerMsg {
    Welcome { protocol: u8, server_name: String },
    AuthOk { player_id: u32, username: String, token: u64, udp_port: u16 },
    AuthError { reason: AuthErrorReason },
    /// Pushed to every client in the lobby whenever the room list changes.
    RoomList { rooms: Vec<RoomInfo> },
    /// Pushed to every room member whenever the room changes.
    RoomState { room_id: u32, name: String, host_id: u32, players: Vec<RoomPlayer> },
    /// Confirms `LeaveRoom`; the client is back in the lobby.
    LeftRoom,
    Chat { from: String, text: String, scope: ChatScope },
    MatchStarting { map: String, slots: Vec<SlotInfo>, your_slot: u8, countdown_ms: u32 },
    PlayerDisconnected { slot: u8, username: String, reason: DisconnectReason },
    MatchEnded { reason: MatchEndReason, winner_slot: Option<u8>, rankings: Vec<Ranking> },
    Error { code: String, message: String },
    Pong { nonce: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthErrorReason {
    BadCredentials,
    UserExists,
    AlreadyLoggedIn,
    InvalidUsername,
    InvalidPassword,
}

impl AuthErrorReason {
    pub fn describe(self) -> &'static str {
        match self {
            AuthErrorReason::BadCredentials => "wrong username or password",
            AuthErrorReason::UserExists => "username already taken",
            AuthErrorReason::AlreadyLoggedIn => "this account is already logged in",
            AuthErrorReason::InvalidUsername => "username must be 3-16 chars: letters, digits, _",
            AuthErrorReason::InvalidPassword => "password must be 3-64 chars",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoomInfo {
    pub id: u32,
    pub name: String,
    pub players: u8,
    pub max: u8,
    pub in_match: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoomPlayer {
    pub player_id: u32,
    pub username: String,
    pub ready: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatScope {
    Lobby,
    Room,
}

/// In-match identity: `slot` is the hex id shown in game (0x01..0x03).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlotInfo {
    pub slot: u8,
    pub player_id: u32,
    pub username: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisconnectReason {
    /// Connection closed (clean close, crash, reset).
    Closed,
    /// Heartbeat missed: silent drop (Wi-Fi off, cable pulled...).
    Timeout,
    /// Peer violated the protocol (bad frame).
    Error,
    /// Player left the room voluntarily.
    Left,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchEndReason {
    Normal,
    NotEnoughPlayers,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ranking {
    pub slot: u8,
    pub username: String,
    pub round_wins: u8,
    pub kills: u32,
    pub deaths: u32,
    pub disconnected: bool,
}
