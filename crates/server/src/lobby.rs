//! The lobby thread: single owner of sessions, rooms and matches.
//!
//! Every connection thread talks to it through one channel, so there is no
//! shared mutable state and no locks: events are processed one at a time, in
//! arrival order. Simultaneous actions (two players joining the last free
//! slot at once) are therefore serialized naturally — first event wins.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;

use crossbeam_channel::{Receiver, Sender};
use gg_shared::constants::*;
use gg_shared::protocol::*;
use log::{info, warn};

use crate::conn::{ConnId, LobbyEvent};
use crate::users::UserStore;
use crate::Config;

struct Session {
    addr: SocketAddr,
    tx: Sender<ServerMsg>,
    user: Option<User>,
    room: Option<u32>,
}

struct User {
    player_id: u32,
    username: String,
    /// Authenticates this player's UDP packets (milestone 2).
    #[allow(dead_code)]
    token: u64,
}

struct Room {
    id: u32,
    name: String,
    host: ConnId,
    members: Vec<Member>,
    game: Option<Match>,
}

struct Member {
    conn: ConnId,
    ready: bool,
}

struct Match {
    slots: Vec<Slot>,
}

struct Slot {
    slot: u8,
    conn: ConnId,
    username: String,
    connected: bool,
    round_wins: u8,
    kills: u32,
    deaths: u32,
}

pub struct Lobby {
    config: Config,
    users: UserStore,
    sessions: HashMap<ConnId, Session>,
    rooms: BTreeMap<u32, Room>,
    next_room_id: u32,
}

/// Free function (not a method) so it can be called while another field of
/// `Lobby` is mutably borrowed.
fn send(sessions: &HashMap<ConnId, Session>, conn: ConnId, msg: ServerMsg) {
    if let Some(s) = sessions.get(&conn) {
        // Err only if the writer thread already exited; the reader will
        // report the disconnect, so ignoring is correct here.
        let _ = s.tx.send(msg);
    }
}

fn error(code: &str, message: impl Into<String>) -> ServerMsg {
    ServerMsg::Error { code: code.into(), message: message.into() }
}

impl Lobby {
    pub fn new(config: Config, users: UserStore) -> Self {
        Lobby { config, users, sessions: HashMap::new(), rooms: BTreeMap::new(), next_room_id: 1 }
    }

    pub fn run(mut self, rx: Receiver<LobbyEvent>) {
        for event in rx.iter() {
            match event {
                LobbyEvent::Connected { conn, addr, tx } => {
                    info!("{conn}: connected from {addr}");
                    let _ = tx.send(ServerMsg::Welcome {
                        protocol: PROTOCOL_VERSION,
                        server_name: self.config.server_name.clone(),
                    });
                    self.sessions.insert(conn, Session { addr, tx, user: None, room: None });
                }
                LobbyEvent::Message { conn, msg } => self.handle(conn, msg),
                LobbyEvent::Disconnected { conn, reason } => self.disconnect(conn, reason),
            }
        }
    }

    fn label(&self, conn: ConnId) -> String {
        match self.sessions.get(&conn).and_then(|s| s.user.as_ref()) {
            Some(u) => format!("{conn}({})", u.username),
            None => conn.to_string(),
        }
    }

    fn handle(&mut self, conn: ConnId, msg: ClientMsg) {
        let Some(session) = self.sessions.get(&conn) else { return };
        let authed = session.user.is_some();
        let in_room = session.room.is_some();

        match msg {
            ClientMsg::Ping { nonce } => send(&self.sessions, conn, ServerMsg::Pong { nonce }),
            ClientMsg::Register { username, password } if !authed => {
                match self.users.register(&username, &password) {
                    Ok(id) => {
                        info!("{conn}: registered '{username}' as player {id}");
                        self.login(conn, &username, &password);
                    }
                    Err(reason) => send(&self.sessions, conn, ServerMsg::AuthError { reason }),
                }
            }
            ClientMsg::Login { username, password } if !authed => self.login(conn, &username, &password),
            ClientMsg::Logout if authed => {
                info!("{}: logout", self.label(conn));
                self.disconnect(conn, DisconnectReason::Closed);
            }
            ClientMsg::ListRooms if authed && !in_room => {
                send(&self.sessions, conn, ServerMsg::RoomList { rooms: self.room_infos() })
            }
            ClientMsg::CreateRoom { name } if authed && !in_room => self.create_room(conn, name),
            ClientMsg::JoinRoom { room_id } if authed && !in_room => self.join_room(conn, room_id),
            ClientMsg::LeaveRoom if in_room => {
                let room_id = self.sessions.get_mut(&conn).and_then(|s| s.room.take());
                send(&self.sessions, conn, ServerMsg::LeftRoom);
                if let Some(room_id) = room_id {
                    info!("{}: left room {room_id}", self.label(conn));
                    self.leave_room(conn, room_id, DisconnectReason::Left);
                }
            }
            ClientMsg::SetReady { ready } if in_room => self.set_ready(conn, ready),
            ClientMsg::Chat { text } if authed => self.chat(conn, text),
            other => send(
                &self.sessions,
                conn,
                error("bad_state", format!("{} is not allowed right now", other.kind())),
            ),
        }
    }

    fn login(&mut self, conn: ConnId, username: &str, password: &str) {
        let Some((player_id, username)) = self.users.verify(username, password) else {
            warn!("{conn}: failed login for '{username}'");
            send(&self.sessions, conn, ServerMsg::AuthError { reason: AuthErrorReason::BadCredentials });
            return;
        };
        let already = self
            .sessions
            .values()
            .any(|s| s.user.as_ref().is_some_and(|u| u.player_id == player_id));
        if already {
            send(&self.sessions, conn, ServerMsg::AuthError { reason: AuthErrorReason::AlreadyLoggedIn });
            return;
        }
        let token = rand::random::<u64>();
        let Some(session) = self.sessions.get_mut(&conn) else { return };
        session.user = Some(User { player_id, username: username.clone(), token });
        info!("{conn}: logged in as '{username}' (player {player_id}) from {}", session.addr);
        let udp_port = self.config.udp_port;
        send(&self.sessions, conn, ServerMsg::AuthOk { player_id, username, token, udp_port });
        send(&self.sessions, conn, ServerMsg::RoomList { rooms: self.room_infos() });
    }

    /// Idempotent: the reader thread and an explicit Logout may both trigger it.
    fn disconnect(&mut self, conn: ConnId, reason: DisconnectReason) {
        let label = self.label(conn);
        let Some(session) = self.sessions.remove(&conn) else { return };
        info!("{label}: disconnected ({reason:?})");
        if let Some(room_id) = session.room {
            self.leave_room(conn, room_id, reason);
        }
        // `session` (and its `tx`) is dropped here → writer thread closes the socket.
    }

    // ---------------------------------------------------------------- rooms

    fn room_infos(&self) -> Vec<RoomInfo> {
        self.rooms
            .values()
            .map(|r| RoomInfo {
                id: r.id,
                name: r.name.clone(),
                players: r.members.len() as u8,
                max: MAX_ROOM_PLAYERS as u8,
                in_match: r.game.is_some(),
            })
            .collect()
    }

    /// Push the room list to everyone who is logged in and not inside a room.
    fn broadcast_room_list(&self) {
        let rooms = self.room_infos();
        for s in self.sessions.values() {
            if s.user.is_some() && s.room.is_none() {
                let _ = s.tx.send(ServerMsg::RoomList { rooms: rooms.clone() });
            }
        }
    }

    fn player_id(&self, conn: ConnId) -> Option<u32> {
        self.sessions.get(&conn)?.user.as_ref().map(|u| u.player_id)
    }

    fn push_room_state(&self, room_id: u32) {
        let Some(room) = self.rooms.get(&room_id) else { return };
        let players = room
            .members
            .iter()
            .filter_map(|m| {
                let u = self.sessions.get(&m.conn)?.user.as_ref()?;
                Some(RoomPlayer { player_id: u.player_id, username: u.username.clone(), ready: m.ready })
            })
            .collect();
        let msg = ServerMsg::RoomState {
            room_id,
            name: room.name.clone(),
            host_id: self.player_id(room.host).unwrap_or(0),
            players,
        };
        for m in &room.members {
            send(&self.sessions, m.conn, msg.clone());
        }
    }

    fn create_room(&mut self, conn: ConnId, name: String) {
        let name = name.trim().to_string();
        if name.is_empty() || name.chars().count() > ROOM_NAME_MAX {
            send(&self.sessions, conn, error("invalid_name", format!("room name must be 1-{ROOM_NAME_MAX} chars")));
            return;
        }
        let id = self.next_room_id;
        self.next_room_id += 1;
        self.rooms.insert(
            id,
            Room { id, name: name.clone(), host: conn, members: vec![Member { conn, ready: false }], game: None },
        );
        if let Some(s) = self.sessions.get_mut(&conn) {
            s.room = Some(id);
        }
        info!("{}: created room {id} '{name}'", self.label(conn));
        self.push_room_state(id);
        self.broadcast_room_list();
    }

    fn join_room(&mut self, conn: ConnId, room_id: u32) {
        let Some(room) = self.rooms.get_mut(&room_id) else {
            send(&self.sessions, conn, error("room_not_found", "that room no longer exists"));
            return;
        };
        if room.game.is_some() {
            send(&self.sessions, conn, error("room_in_match", "a match is already running in that room"));
            return;
        }
        if room.members.len() >= MAX_ROOM_PLAYERS {
            send(&self.sessions, conn, error("room_full", "that room is full"));
            return;
        }
        room.members.push(Member { conn, ready: false });
        if let Some(s) = self.sessions.get_mut(&conn) {
            s.room = Some(room_id);
        }
        info!("{}: joined room {room_id}", self.label(conn));
        self.push_room_state(room_id);
        self.broadcast_room_list();
    }

    /// Remove `conn` from a room, whether it left voluntarily or disconnected.
    /// The caller has already cleared `session.room` (or removed the session).
    fn leave_room(&mut self, conn: ConnId, room_id: u32, reason: DisconnectReason) {
        let Some(room) = self.rooms.get_mut(&room_id) else { return };
        room.members.retain(|m| m.conn != conn);

        let mut end_match = false;
        if let Some(game) = &mut room.game {
            if let Some(slot) = game.slots.iter_mut().find(|s| s.conn == conn && s.connected) {
                slot.connected = false;
                info!("room {room_id}: slot 0x{:02X} ({}) dropped: {reason:?}", slot.slot, slot.username);
                let notice =
                    ServerMsg::PlayerDisconnected { slot: slot.slot, username: slot.username.clone(), reason };
                for m in &room.members {
                    send(&self.sessions, m.conn, notice.clone());
                }
            }
            end_match = game.slots.iter().filter(|s| s.connected).count() < MIN_PLAYERS_TO_START;
        }

        if room.members.is_empty() {
            info!("room {room_id} is empty, closing it");
            self.rooms.remove(&room_id);
        } else {
            if room.host == conn {
                room.host = room.members[0].conn;
            }
            if end_match {
                self.end_match(room_id, MatchEndReason::NotEnoughPlayers);
            }
            self.push_room_state(room_id);
        }
        self.broadcast_room_list();
    }

    fn set_ready(&mut self, conn: ConnId, ready: bool) {
        let Some(room_id) = self.sessions.get(&conn).and_then(|s| s.room) else { return };
        let Some(room) = self.rooms.get_mut(&room_id) else { return };
        if room.game.is_some() {
            return; // readiness is meaningless mid-match
        }
        if let Some(m) = room.members.iter_mut().find(|m| m.conn == conn) {
            m.ready = ready;
        }
        let start = room.members.len() >= MIN_PLAYERS_TO_START && room.members.iter().all(|m| m.ready);
        self.push_room_state(room_id);
        if start {
            self.start_match(room_id);
        }
    }

    // -------------------------------------------------------------- matches

    fn start_match(&mut self, room_id: u32) {
        let Some(room) = self.rooms.get_mut(&room_id) else { return };
        let mut slots = Vec::new();
        for (i, m) in room.members.iter().enumerate() {
            let Some(user) = self.sessions.get(&m.conn).and_then(|s| s.user.as_ref()) else { continue };
            slots.push(Slot {
                slot: i as u8 + 1,
                conn: m.conn,
                username: user.username.clone(),
                connected: true,
                round_wins: 0,
                kills: 0,
                deaths: 0,
            });
        }
        let slot_infos: Vec<SlotInfo> = slots
            .iter()
            .map(|s| SlotInfo {
                slot: s.slot,
                player_id: self.sessions[&s.conn].user.as_ref().map_or(0, |u| u.player_id),
                username: s.username.clone(),
            })
            .collect();
        for s in &slots {
            send(
                &self.sessions,
                s.conn,
                ServerMsg::MatchStarting {
                    map: "router".into(),
                    slots: slot_infos.clone(),
                    your_slot: s.slot,
                    countdown_ms: MATCH_COUNTDOWN_MS,
                },
            );
        }
        let names: Vec<_> = slots.iter().map(|s| format!("0x{:02X}={}", s.slot, s.username)).collect();
        info!("room {room_id}: match started [{}]", names.join(", "));
        room.game = Some(Match { slots });
        self.broadcast_room_list();
    }

    fn end_match(&mut self, room_id: u32, reason: MatchEndReason) {
        let Some(room) = self.rooms.get_mut(&room_id) else { return };
        let Some(game) = room.game.take() else { return };

        let mut rankings: Vec<Ranking> = game
            .slots
            .iter()
            .map(|s| Ranking {
                slot: s.slot,
                username: s.username.clone(),
                round_wins: s.round_wins,
                kills: s.kills,
                deaths: s.deaths,
                disconnected: !s.connected,
            })
            .collect();
        rankings.sort_by(|a, b| {
            b.round_wins.cmp(&a.round_wins).then(b.kills.cmp(&a.kills)).then(a.deaths.cmp(&b.deaths))
        });
        let winner_slot = match reason {
            MatchEndReason::Normal => rankings.first().map(|r| r.slot),
            // Last player still connected wins by default.
            MatchEndReason::NotEnoughPlayers => game.slots.iter().find(|s| s.connected).map(|s| s.slot),
        };
        // Stable sort: winner first, everyone else keeps score order.
        rankings.sort_by_key(|r| Some(r.slot) != winner_slot);

        info!("room {room_id}: match ended ({reason:?}), winner {winner_slot:?}");
        let msg = ServerMsg::MatchEnded { reason, winner_slot, rankings };
        for m in &mut room.members {
            m.ready = false;
            send(&self.sessions, m.conn, msg.clone());
        }
        self.broadcast_room_list();
    }

    // ----------------------------------------------------------------- chat

    fn chat(&mut self, conn: ConnId, text: String) {
        let text = text.trim().to_string();
        if text.is_empty() || text.chars().count() > CHAT_MAX {
            return;
        }
        let Some(session) = self.sessions.get(&conn) else { return };
        let Some(user) = &session.user else { return };
        let from = user.username.clone();
        match session.room.and_then(|id| self.rooms.get(&id)) {
            Some(room) => {
                let msg = ServerMsg::Chat { from, text, scope: ChatScope::Room };
                for m in &room.members {
                    send(&self.sessions, m.conn, msg.clone());
                }
            }
            None => {
                let msg = ServerMsg::Chat { from, text, scope: ChatScope::Lobby };
                for s in self.sessions.values() {
                    if s.user.is_some() && s.room.is_none() {
                        let _ = s.tx.send(msg.clone());
                    }
                }
            }
        }
    }
}
