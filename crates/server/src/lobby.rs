//! The lobby thread: single owner of sessions, rooms and matches.
//!
//! Every connection thread talks to it through one channel, so there is no
//! shared mutable state and no locks: events are processed one at a time, in
//! arrival order. Simultaneous actions (two players joining the last free
//! slot at once) are therefore serialized naturally — first event wins.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::{SocketAddr, UdpSocket};

use crossbeam_channel::{Receiver, Sender};
use gg_shared::constants::*;
use gg_shared::protocol::*;
use log::{info, warn};

use crate::conn::{ConnId, LobbyEvent};
use crate::game::{self, RoomEvent, SlotStats};
use crate::udp::UdpCtl;
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
    /// Authenticates this player's UDP packets.
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
    /// Channel into this match's game-loop thread.
    ctl: Sender<RoomEvent>,
}

struct Slot {
    slot: u8,
    conn: ConnId,
    token: u64,
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
    udp_ctl: Sender<UdpCtl>,
    /// Cloned into each game loop for sending snapshots.
    udp_socket: UdpSocket,
    /// Handed to game loops so they can report back.
    self_tx: Sender<LobbyEvent>,
    /// Where server-spawned bots connect (this server, via localhost).
    bot_addr: SocketAddr,
    /// Player ids of bot accounts.
    bot_ids: HashSet<u32>,
    bots_spawned: u32,
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
    ServerMsg::Error {
        code: code.into(),
        message: message.into(),
    }
}

impl Lobby {
    pub fn new(
        config: Config,
        users: UserStore,
        udp_ctl: Sender<UdpCtl>,
        udp_socket: UdpSocket,
        self_tx: Sender<LobbyEvent>,
        bot_addr: SocketAddr,
    ) -> Self {
        Lobby {
            config,
            users,
            sessions: HashMap::new(),
            rooms: BTreeMap::new(),
            next_room_id: 1,
            udp_ctl,
            udp_socket,
            self_tx,
            bot_addr,
            bot_ids: HashSet::new(),
            bots_spawned: 0,
        }
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
                    self.sessions.insert(
                        conn,
                        Session {
                            addr,
                            tx,
                            user: None,
                            room: None,
                        },
                    );
                }
                LobbyEvent::Message { conn, msg } => self.handle(conn, msg),
                LobbyEvent::Disconnected { conn, reason } => self.disconnect(conn, reason),
                LobbyEvent::MatchFinished {
                    room_id,
                    reason,
                    stats,
                } => self.match_finished(room_id, reason, stats),
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
        let Some(session) = self.sessions.get(&conn) else {
            return;
        };
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
            ClientMsg::Login { username, password } if !authed => {
                self.login(conn, &username, &password)
            }
            ClientMsg::Logout if authed => {
                info!("{}: logout", self.label(conn));
                self.disconnect(conn, DisconnectReason::Closed);
            }
            ClientMsg::ListRooms if authed && !in_room => send(
                &self.sessions,
                conn,
                ServerMsg::RoomList {
                    rooms: self.room_infos(),
                },
            ),
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
            ClientMsg::StartMatch if in_room => self.host_start(conn),
            ClientMsg::AddBot if in_room => self.add_bot(conn),
            ClientMsg::RemoveBot if in_room => self.remove_bot(conn),
            ClientMsg::Chat { text } if authed => self.chat(conn, text),
            other => send(
                &self.sessions,
                conn,
                error(
                    "bad_state",
                    format!("{} is not allowed right now", other.kind()),
                ),
            ),
        }
    }

    fn login(&mut self, conn: ConnId, username: &str, password: &str) {
        let Some((player_id, username)) = self.users.verify(username, password) else {
            warn!("{conn}: failed login for '{username}'");
            send(
                &self.sessions,
                conn,
                ServerMsg::AuthError {
                    reason: AuthErrorReason::BadCredentials,
                },
            );
            return;
        };
        let already = self
            .sessions
            .values()
            .any(|s| s.user.as_ref().is_some_and(|u| u.player_id == player_id));
        if already {
            send(
                &self.sessions,
                conn,
                ServerMsg::AuthError {
                    reason: AuthErrorReason::AlreadyLoggedIn,
                },
            );
            return;
        }
        let token = rand::random::<u64>();
        let Some(session) = self.sessions.get_mut(&conn) else {
            return;
        };
        session.user = Some(User {
            player_id,
            username: username.clone(),
            token,
        });
        let _ = self.udp_ctl.send(UdpCtl::Register {
            token,
            label: format!("{conn}({username})"),
        });
        info!(
            "{conn}: logged in as '{username}' (player {player_id}) from {}",
            session.addr
        );
        let udp_port = self.config.udp_port;
        send(
            &self.sessions,
            conn,
            ServerMsg::AuthOk {
                player_id,
                username,
                token,
                udp_port,
            },
        );
        send(
            &self.sessions,
            conn,
            ServerMsg::RoomList {
                rooms: self.room_infos(),
            },
        );
    }

    /// Idempotent: the reader thread and an explicit Logout may both trigger it.
    fn disconnect(&mut self, conn: ConnId, reason: DisconnectReason) {
        let label = self.label(conn);
        let Some(session) = self.sessions.remove(&conn) else {
            return;
        };
        info!("{label}: disconnected ({reason:?})");
        if let Some(user) = &session.user {
            let _ = self.udp_ctl.send(UdpCtl::Unregister { token: user.token });
        }
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
                let _ = s.tx.send(ServerMsg::RoomList {
                    rooms: rooms.clone(),
                });
            }
        }
    }

    fn player_id(&self, conn: ConnId) -> Option<u32> {
        self.sessions.get(&conn)?.user.as_ref().map(|u| u.player_id)
    }

    fn push_room_state(&self, room_id: u32) {
        let Some(room) = self.rooms.get(&room_id) else {
            return;
        };
        let players = room
            .members
            .iter()
            .filter_map(|m| {
                let u = self.sessions.get(&m.conn)?.user.as_ref()?;
                Some(RoomPlayer {
                    player_id: u.player_id,
                    username: u.username.clone(),
                    ready: m.ready,
                    is_bot: self.bot_ids.contains(&u.player_id),
                })
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
            send(
                &self.sessions,
                conn,
                error(
                    "invalid_name",
                    format!("room name must be 1-{ROOM_NAME_MAX} chars"),
                ),
            );
            return;
        }
        let id = self.next_room_id;
        self.next_room_id += 1;
        self.rooms.insert(
            id,
            Room {
                id,
                name: name.clone(),
                host: conn,
                members: vec![Member { conn, ready: false }],
                game: None,
            },
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
            send(
                &self.sessions,
                conn,
                error("room_not_found", "that room no longer exists"),
            );
            return;
        };
        if room.game.is_some() {
            send(
                &self.sessions,
                conn,
                error("room_in_match", "a match is already running in that room"),
            );
            return;
        }
        if room.members.len() >= MAX_ROOM_PLAYERS {
            send(
                &self.sessions,
                conn,
                error("room_full", "that room is full"),
            );
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
        let Some(room) = self.rooms.get_mut(&room_id) else {
            return;
        };
        room.members.retain(|m| m.conn != conn);

        if let Some(game) = &mut room.game {
            if let Some(slot) = game
                .slots
                .iter_mut()
                .find(|s| s.conn == conn && s.connected)
            {
                slot.connected = false;
                info!(
                    "room {room_id}: slot 0x{:02X} ({}) dropped: {reason:?}",
                    slot.slot, slot.username
                );
                let notice = ServerMsg::PlayerDisconnected {
                    slot: slot.slot,
                    username: slot.username.clone(),
                    reason,
                };
                for m in &room.members {
                    send(&self.sessions, m.conn, notice.clone());
                }
                // The game loop removes the packet and ends the match itself
                // if fewer than 2 players remain (→ MatchFinished).
                let _ = game.ctl.send(RoomEvent::PlayerLeft { slot: slot.slot });
                let _ = self.udp_ctl.send(UdpCtl::Route {
                    token: slot.token,
                    route: None,
                });
            }
        }

        if room.members.is_empty() {
            info!("room {room_id} is empty, closing it");
            self.rooms.remove(&room_id);
        } else {
            if room.host == conn {
                room.host = room.members[0].conn;
            }
            self.push_room_state(room_id);
        }
        self.broadcast_room_list();
    }

    fn set_ready(&mut self, conn: ConnId, ready: bool) {
        let Some(room_id) = self.sessions.get(&conn).and_then(|s| s.room) else {
            return;
        };
        let Some(room) = self.rooms.get_mut(&room_id) else {
            return;
        };
        if room.game.is_some() {
            return; // readiness is meaningless mid-match
        }
        if let Some(m) = room.members.iter_mut().find(|m| m.conn == conn) {
            m.ready = ready;
        }
        // Auto-start only when the room is full; with 2 players the host
        // decides (StartMatch), so a third player still has time to join.
        let start = room.members.len() == MAX_ROOM_PLAYERS && room.members.iter().all(|m| m.ready);
        self.push_room_state(room_id);
        if start {
            self.start_match(room_id);
        }
    }

    /// Checks shared by host-only room commands. Returns the room id.
    fn host_room(&self, conn: ConnId, what: &str) -> Result<u32, ServerMsg> {
        let room_id = self.sessions.get(&conn).and_then(|s| s.room);
        let room = room_id.and_then(|id| self.rooms.get(&id));
        match room {
            None => Err(error("bad_state", "not in a room")),
            Some(r) if r.host != conn => {
                Err(error("not_host", format!("only the host can {what}")))
            }
            Some(r) if r.game.is_some() => Err(error("room_in_match", "a match is running")),
            Some(r) => Ok(r.id),
        }
    }

    /// Spawn a headless bot client that connects back over localhost and
    /// joins this room like any other player.
    fn add_bot(&mut self, conn: ConnId) {
        let room_id = match self.host_room(conn, "add bots") {
            Ok(id) => id,
            Err(e) => return send(&self.sessions, conn, e),
        };
        if self.rooms[&room_id].members.len() >= MAX_ROOM_PLAYERS {
            return send(&self.sessions, conn, error("room_full", "the port is full"));
        }
        self.bots_spawned += 1;
        let username = format!("{}{}", gg_shared::bot::BOT_PREFIX, self.bots_spawned);
        let password = format!("{:032x}", rand::random::<u128>());
        let id = self.users.add_bot(&username, &password);
        self.bot_ids.insert(id);
        info!("{}: added {username} to room {room_id}", self.label(conn));
        if let Err(e) = crate::bot::spawn(self.bot_addr, username, password, room_id) {
            warn!("could not start bot: {e}");
        }
    }

    /// Disconnect the most recently added bot in this room.
    fn remove_bot(&mut self, conn: ConnId) {
        let room_id = match self.host_room(conn, "remove bots") {
            Ok(id) => id,
            Err(e) => return send(&self.sessions, conn, e),
        };
        let bot_conn = self.rooms[&room_id]
            .members
            .iter()
            .rev()
            .map(|m| m.conn)
            .find(|c| {
                self.player_id(*c)
                    .is_some_and(|id| self.bot_ids.contains(&id))
            });
        match bot_conn {
            // Dropping its session closes the bot's socket; its thread exits.
            Some(b) => self.disconnect(b, DisconnectReason::Left),
            None => send(&self.sessions, conn, error("no_bot", "no bot in this port")),
        }
    }

    fn host_start(&mut self, conn: ConnId) {
        let Some(room_id) = self.sessions.get(&conn).and_then(|s| s.room) else {
            return;
        };
        let Some(room) = self.rooms.get(&room_id) else {
            return;
        };
        let problem = if room.host != conn {
            Some("only the host can start the match")
        } else if room.game.is_some() {
            Some("a match is already running")
        } else if room.members.len() < MIN_PLAYERS_TO_START {
            Some("need at least 2 packets to start")
        } else if !room.members.iter().all(|m| m.ready) {
            Some("everyone must be ready")
        } else {
            None
        };
        match problem {
            Some(msg) => send(&self.sessions, conn, error("cannot_start", msg)),
            None => self.start_match(room_id),
        }
    }

    // -------------------------------------------------------------- matches

    fn start_match(&mut self, room_id: u32) {
        const MAP: &str = "router";
        let Some(map) = gg_shared::game::builtin_map(MAP) else {
            warn!("built-in map '{MAP}' failed to load");
            return;
        };
        let Some(room) = self.rooms.get_mut(&room_id) else {
            return;
        };
        let mut slots = Vec::new();
        for (i, m) in room.members.iter().enumerate() {
            let Some(user) = self.sessions.get(&m.conn).and_then(|s| s.user.as_ref()) else {
                continue;
            };
            slots.push(Slot {
                slot: i as u8 + 1,
                conn: m.conn,
                token: user.token,
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
                player_id: self.sessions[&s.conn]
                    .user
                    .as_ref()
                    .map_or(0, |u| u.player_id),
                username: s.username.clone(),
            })
            .collect();
        for s in &slots {
            send(
                &self.sessions,
                s.conn,
                ServerMsg::MatchStarting {
                    map: MAP.into(),
                    slots: slot_infos.clone(),
                    your_slot: s.slot,
                    countdown_ms: MATCH_COUNTDOWN_MS,
                },
            );
        }
        let socket = match self.udp_socket.try_clone() {
            Ok(s) => s,
            Err(e) => {
                warn!("room {room_id}: cannot clone UDP socket: {e}");
                return;
            }
        };
        let (ctl, rx) = crossbeam_channel::unbounded();
        let roster = slots.iter().map(|s| (s.slot, s.username.clone())).collect();
        if let Err(e) = game::spawn(room_id, map, roster, socket, rx, self.self_tx.clone()) {
            warn!("room {room_id}: cannot start game loop: {e}");
            return;
        }
        // Route each player's UDP inputs to this game loop.
        for s in &slots {
            let _ = self.udp_ctl.send(UdpCtl::Route {
                token: s.token,
                route: Some((ctl.clone(), s.slot)),
            });
        }
        let names: Vec<_> = slots
            .iter()
            .map(|s| format!("0x{:02X}={}", s.slot, s.username))
            .collect();
        info!("room {room_id}: match started [{}]", names.join(", "));
        room.game = Some(Match { slots, ctl });
        self.broadcast_room_list();
    }

    fn match_finished(&mut self, room_id: u32, reason: MatchEndReason, stats: Vec<SlotStats>) {
        let Some(game) = self.rooms.get_mut(&room_id).and_then(|r| r.game.as_mut()) else {
            return;
        };
        for st in stats {
            if let Some(s) = game.slots.iter_mut().find(|s| s.slot == st.slot) {
                s.round_wins = st.round_wins;
                s.kills = st.kills;
                s.deaths = st.deaths;
            }
        }
        self.end_match(room_id, reason);
        self.push_room_state(room_id);
    }

    fn end_match(&mut self, room_id: u32, reason: MatchEndReason) {
        let Some(room) = self.rooms.get_mut(&room_id) else {
            return;
        };
        let Some(game) = room.game.take() else { return };
        for s in &game.slots {
            let _ = self.udp_ctl.send(UdpCtl::Route {
                token: s.token,
                route: None,
            });
        }

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
            b.round_wins
                .cmp(&a.round_wins)
                .then(b.kills.cmp(&a.kills))
                .then(a.deaths.cmp(&b.deaths))
        });
        let winner_slot = match reason {
            MatchEndReason::Normal => rankings.first().map(|r| r.slot),
            // Last player still connected wins by default.
            MatchEndReason::NotEnoughPlayers => {
                game.slots.iter().find(|s| s.connected).map(|s| s.slot)
            }
        };
        // Stable sort: winner first, everyone else keeps score order.
        rankings.sort_by_key(|r| Some(r.slot) != winner_slot);

        info!("room {room_id}: match ended ({reason:?}), winner {winner_slot:?}");
        let msg = ServerMsg::MatchEnded {
            reason,
            winner_slot,
            rankings,
        };
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
        let Some(session) = self.sessions.get(&conn) else {
            return;
        };
        let Some(user) = &session.user else { return };
        let from = user.username.clone();
        match session.room.and_then(|id| self.rooms.get(&id)) {
            Some(room) => {
                let msg = ServerMsg::Chat {
                    from,
                    text,
                    scope: ChatScope::Room,
                };
                for m in &room.members {
                    send(&self.sessions, m.conn, msg.clone());
                }
            }
            None => {
                let msg = ServerMsg::Chat {
                    from,
                    text,
                    scope: ChatScope::Lobby,
                };
                for s in self.sessions.values() {
                    if s.user.is_some() && s.room.is_none() {
                        let _ = s.tx.send(msg.clone());
                    }
                }
            }
        }
    }
}
