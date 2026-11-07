//! Client state machine and screens:
//! Login → Lobby ("The Switch") → Room ("Port") → Match → Results (0xGG / 0xFF).

use std::collections::VecDeque;

use gg_shared::constants::*;
use gg_shared::protocol::*;
use macroquad::prelude::*;

use gg_shared::game::builtin_map;
use gg_shared::udp::{ClientPacket, ServerPacket};

use crate::game_view::GameView;
use crate::net::{now_ms, Connection, NetEvent, UdpLink};
use crate::ui::{self, panel, text, text_centered, theme, Ui};

const F_SERVER: u32 = 1;
const F_USER: u32 = 2;
const F_PASS: u32 = 3;
const F_ROOM: u32 = 4;
const F_CHAT: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    Login,
    Lobby,
    Room,
    Match,
    Results,
}

struct Me {
    player_id: u32,
    username: String,
}

struct RoomView {
    room_id: u32,
    name: String,
    host_id: u32,
    players: Vec<RoomPlayer>,
}

/// State of the UDP channel, shown in the footer.
#[derive(Debug, Clone, Copy, PartialEq)]
enum UdpState {
    Off,
    /// Sending Hello every 200 ms, waiting for HelloAck.
    Pending {
        since: f64,
        last_hello: f64,
    },
    Ok,
    /// No HelloAck within 5 s: probably a firewall. Keep retrying slowly.
    Blocked {
        last_hello: f64,
    },
}

struct ResultsView {
    reason: MatchEndReason,
    winner_slot: Option<u8>,
    rankings: Vec<Ranking>,
    your_slot: u8,
    error_code: &'static str,
    error_text: &'static str,
}

/// Losers get a fake fatal error. Picked at random per match.
const LOSS_ERRORS: &[(&str, &str)] = &[
    ("E_TTL_EXCEEDED", "time to live reached 0 before delivery"),
    (
        "E_CHECKSUM_MISMATCH",
        "packet integrity compromised, discarded at hop",
    ),
    ("E_CONN_REFUSED", "destination host rejected your packet"),
    ("E_SEGFAULT", "core dumped while routing your packet"),
    ("E_BUFFER_OVERFLOW", "too many hits taken, buffer overrun"),
    ("E_NO_ROUTE_TO_HOST", "your packet got lost in the backbone"),
];

pub struct App {
    pub ui: Ui,
    screen: Screen,
    conn: Option<Connection>,
    server_addr: String,
    username: String,
    password: String,
    status: Option<(String, Color)>,
    me: Option<Me>,
    rooms: Vec<RoomInfo>,
    room: Option<RoomView>,
    new_room_name: String,
    chat: VecDeque<(String, String)>,
    chat_input: String,
    game: Option<GameView>,
    results: Option<ResultsView>,
    last_ping_sent: f64,
    rtt_ms: Option<f64>,
    token: u64,
    udp: Option<UdpLink>,
    udp_state: UdpState,
    udp_rtt_ms: Option<f64>,
    last_udp_ping: f64,
    /// Simulated UDP packet loss (0.0-1.0), from `--loss`.
    loss: f32,
    pub auto: Auto,
}

/// Developer shortcuts for quick local testing (`--login`, `--autojoin`).
#[derive(Default)]
pub struct Auto {
    /// Register (or log in if the account exists) at startup.
    pub login: Option<(String, String)>,
    /// Join the first open room (or create one), then ready up.
    pub join: bool,
    /// Play with random inputs (`--bot`), for automated playtests.
    pub bot: bool,
    lobby_since: Option<f64>,
    join_sent: bool,
    ready_sent: bool,
}

impl App {
    pub fn new(server_addr: String, username: String, loss: f32) -> Self {
        let mut ui = Ui::default();
        ui.focus = Some(if username.is_empty() { F_USER } else { F_PASS });
        App {
            ui,
            screen: Screen::Login,
            conn: None,
            server_addr,
            username,
            password: String::new(),
            status: None,
            me: None,
            rooms: Vec::new(),
            room: None,
            new_room_name: String::new(),
            chat: VecDeque::new(),
            chat_input: String::new(),
            game: None,
            results: None,
            last_ping_sent: 0.0,
            rtt_ms: None,
            token: 0,
            udp: None,
            udp_state: UdpState::Off,
            udp_rtt_ms: None,
            last_udp_ping: 0.0,
            loss,
            auto: Auto::default(),
        }
    }

    // -------------------------------------------------------------- network

    fn set_status(&mut self, msg: impl Into<String>, color: Color) {
        self.status = Some((msg.into(), color));
    }

    fn send(&mut self, msg: ClientMsg) {
        let Some(conn) = &mut self.conn else { return };
        if let Err(e) = conn.send(&msg) {
            self.connection_lost(format!("send failed: {e}"));
        }
    }

    /// Connect on demand (first Login/Register click), then send.
    fn connect_and_send(&mut self, msg: ClientMsg) {
        if self.conn.is_none() {
            self.set_status(
                format!("connecting to {} ...", self.server_addr),
                theme::DIM,
            );
            match Connection::connect(&self.server_addr) {
                Ok(c) => self.conn = Some(c),
                Err(e) => {
                    self.set_status(
                        format!("cannot reach {}: {e}", self.server_addr),
                        theme::ERROR,
                    );
                    return;
                }
            }
        }
        self.send(msg);
    }

    fn connection_lost(&mut self, why: String) {
        self.conn = None;
        self.udp = None;
        self.udp_state = UdpState::Off;
        self.udp_rtt_ms = None;
        self.me = None;
        self.room = None;
        self.game = None;
        self.rooms.clear();
        self.screen = Screen::Login;
        self.set_status(format!("disconnected: {why}"), theme::ERROR);
    }

    pub fn poll_network(&mut self) {
        let events = match &self.conn {
            Some(c) => c.poll(),
            None => return,
        };
        for ev in events {
            match ev {
                NetEvent::Msg(ServerMsg::Pong { nonce }, arrived) => {
                    self.rtt_ms = Some((arrived - nonce as f64).max(0.0));
                }
                NetEvent::Msg(msg, _) => self.on_message(msg),
                NetEvent::Closed(why) => {
                    self.connection_lost(why);
                    return;
                }
            }
        }
        // Application-level heartbeat; also gives us an RTT readout.
        let now = get_time();
        if self.conn.is_some() && now - self.last_ping_sent >= TCP_HEARTBEAT_INTERVAL.as_secs_f64()
        {
            self.last_ping_sent = now;
            self.send(ClientMsg::Ping {
                nonce: now_ms() as u64,
            });
        }
        self.poll_udp(now);
    }

    fn udp_send(&mut self, packet: ClientPacket) {
        if let Some(udp) = &mut self.udp {
            udp.send(&packet);
        }
    }

    fn poll_udp(&mut self, now: f64) {
        let packets = match &mut self.udp {
            Some(u) => u.poll(),
            None => return,
        };
        for (p, arrived) in packets {
            match p {
                ServerPacket::HelloAck => self.udp_state = UdpState::Ok,
                ServerPacket::Pong { client_time_ms } => {
                    self.udp_rtt_ms = Some((arrived as u32).wrapping_sub(client_time_ms) as f64);
                }
                ServerPacket::Snapshot(s) => {
                    // A snapshot also proves the UDP path works.
                    self.udp_state = UdpState::Ok;
                    if let Some(g) = &mut self.game {
                        g.on_snapshot(s);
                    }
                }
            }
        }

        // Hello handshake: binds our UDP address (as the server sees it,
        // after NAT) to our login token.
        let token = self.token;
        match self.udp_state {
            UdpState::Pending { since, last_hello } => {
                if now - since > 5.0 {
                    self.udp_state = UdpState::Blocked { last_hello: now };
                } else if now - last_hello >= 0.2 {
                    self.udp_state = UdpState::Pending {
                        since,
                        last_hello: now,
                    };
                    self.udp_send(ClientPacket::Hello { token });
                }
            }
            UdpState::Blocked { last_hello } if now - last_hello >= 1.0 => {
                self.udp_state = UdpState::Blocked { last_hello: now };
                self.udp_send(ClientPacket::Hello { token });
            }
            UdpState::Ok if now - self.last_udp_ping >= 1.0 => {
                self.last_udp_ping = now;
                self.udp_send(ClientPacket::Ping {
                    token,
                    client_time_ms: now_ms() as u32,
                });
            }
            _ => {}
        }
    }

    fn push_chat(&mut self, from: impl Into<String>, text: impl Into<String>) {
        self.chat.push_back((from.into(), text.into()));
        while self.chat.len() > 14 {
            self.chat.pop_front();
        }
    }

    fn on_message(&mut self, msg: ServerMsg) {
        match msg {
            ServerMsg::Welcome {
                protocol,
                server_name,
            } => {
                if protocol != PROTOCOL_VERSION {
                    self.connection_lost(format!(
                        "server speaks protocol v{protocol}, this client v{PROTOCOL_VERSION}"
                    ));
                } else {
                    self.set_status(format!("connected to '{server_name}'"), theme::DIM);
                }
            }
            ServerMsg::AuthOk {
                player_id,
                username,
                token,
                udp_port,
            } => {
                self.token = token;
                let udp_addr = self
                    .conn
                    .as_ref()
                    .and_then(|c| c.peer_addr().ok())
                    .map(|mut a| {
                        a.set_port(udp_port);
                        a
                    });
                match udp_addr.map(|a| UdpLink::open(a, self.loss)) {
                    Some(Ok(link)) => {
                        self.udp = Some(link);
                        self.udp_state = UdpState::Pending {
                            since: get_time(),
                            last_hello: 0.0,
                        };
                    }
                    Some(Err(e)) => {
                        self.set_status(format!("cannot open UDP socket: {e}"), theme::ERROR)
                    }
                    None => {}
                }
                self.status = None;
                self.chat.clear();
                self.me = Some(Me {
                    player_id,
                    username,
                });
                self.password.clear();
                self.screen = Screen::Lobby;
                self.ui.focus = None;
            }
            ServerMsg::AuthError {
                reason: AuthErrorReason::UserExists,
            } if self.auto.login.is_some() => {
                if let Some((username, password)) = self.auto.login.clone() {
                    self.send(ClientMsg::Login { username, password });
                }
            }
            ServerMsg::AuthError { reason } => self.set_status(reason.describe(), theme::ERROR),
            ServerMsg::RoomList { rooms } => self.rooms = rooms,
            ServerMsg::RoomState {
                room_id,
                name,
                host_id,
                players,
            } => {
                if self.screen == Screen::Lobby {
                    self.screen = Screen::Room;
                    self.chat.clear();
                    self.status = None;
                }
                self.room = Some(RoomView {
                    room_id,
                    name,
                    host_id,
                    players,
                });
            }
            ServerMsg::LeftRoom => {
                self.room = None;
                self.game = None;
                self.chat.clear();
                self.screen = Screen::Lobby;
            }
            ServerMsg::Chat { from, text, .. } => self.push_chat(from, text),
            ServerMsg::MatchStarting {
                map,
                slots,
                your_slot,
                ..
            } => match builtin_map(&map) {
                Some(m) => {
                    self.game = Some(GameView::new(m, slots, your_slot));
                    self.screen = Screen::Match;
                    self.ui.focus = None;
                }
                None => self.set_status(
                    format!("unknown map '{map}': update your client"),
                    theme::ERROR,
                ),
            },
            ServerMsg::PlayerDisconnected {
                slot,
                username,
                reason,
            } => {
                let why = match reason {
                    DisconnectReason::Closed => "connection closed",
                    DisconnectReason::Timeout => "timed out",
                    DisconnectReason::Error => "protocol error",
                    DisconnectReason::Left => "left the match",
                };
                let line = format!("0x{slot:02X} {username} DROPPED ({why})");
                self.push_chat("*", line.clone());
                if let Some(g) = &mut self.game {
                    g.log(line, theme::WARN);
                }
            }
            ServerMsg::MatchEnded {
                reason,
                winner_slot,
                rankings,
            } => {
                let your_slot = self.game.as_ref().map_or(0, |g| g.your_slot);
                let (error_code, error_text) =
                    LOSS_ERRORS[macroquad::rand::gen_range(0, LOSS_ERRORS.len())];
                self.results = Some(ResultsView {
                    reason,
                    winner_slot,
                    rankings,
                    your_slot,
                    error_code,
                    error_text,
                });
                self.game = None;
                self.screen = Screen::Results;
            }
            ServerMsg::Error { message, .. } => self.set_status(message, theme::ERROR),
            ServerMsg::Pong { .. } => {} // handled with its arrival time in poll_network
        }
    }

    // ---------------------------------------------------------------- draw

    /// Drive `--login` / `--autojoin`. Called once per frame.
    pub fn run_auto(&mut self) {
        if let Some((username, password)) = self.auto.login.clone() {
            if self.screen == Screen::Login && self.conn.is_none() && self.status.is_none() {
                self.username = username.clone();
                self.connect_and_send(ClientMsg::Register { username, password });
            }
        }
        if !self.auto.join {
            return;
        }
        let now = get_time();
        match self.screen {
            Screen::Lobby => {
                // Give the pushed room list half a second to arrive.
                let since = *self.auto.lobby_since.get_or_insert(now);
                if !self.auto.join_sent && now - since > 0.5 {
                    self.auto.join_sent = true;
                    match self.rooms.iter().find(|r| !r.in_match && r.players < r.max) {
                        Some(r) => {
                            let room_id = r.id;
                            self.send(ClientMsg::JoinRoom { room_id });
                        }
                        None => self.send(ClientMsg::CreateRoom { name: "dev".into() }),
                    }
                }
            }
            Screen::Room if !self.auto.ready_sent => {
                self.auto.ready_sent = true;
                self.send(ClientMsg::SetReady { ready: true });
            }
            _ => {}
        }
    }

    pub fn draw(&mut self) {
        match self.screen {
            Screen::Login => self.draw_login(),
            Screen::Lobby => self.draw_lobby(),
            Screen::Room => self.draw_room(),
            Screen::Match => self.draw_match(),
            Screen::Results => self.draw_results(),
        }
        self.draw_footer();
    }

    fn draw_footer(&self) {
        let y = screen_height() - 14.0;
        if let Some((msg, color)) = &self.status {
            text(msg, 24.0, y, 20.0, *color);
        }
        let tcp = match (&self.conn, self.rtt_ms) {
            (None, _) => "offline".to_string(),
            (Some(_), Some(rtt)) => format!("tcp rtt {rtt:.0} ms"),
            (Some(_), None) => "tcp ok".to_string(),
        };
        let udp = match (self.udp_state, self.udp_rtt_ms) {
            (UdpState::Off, _) => String::new(),
            (UdpState::Pending { .. }, _) => "  |  udp handshake...".to_string(),
            (UdpState::Blocked { .. }, _) => "  |  UDP BLOCKED (check firewall)".to_string(),
            (UdpState::Ok, Some(rtt)) => format!("  |  udp rtt {rtt:.0} ms"),
            (UdpState::Ok, None) => "  |  udp ok".to_string(),
        };
        let loss = if self.loss > 0.0 {
            format!("  |  SIMULATED LOSS {:.0}%", self.loss * 100.0)
        } else {
            String::new()
        };
        let net = format!("{tcp}{udp}{loss}");
        let w = ui::text_width(&net, 18.0);
        text(&net, screen_width() - w - 24.0, y, 18.0, theme::DIM);
    }

    fn header(&self, title: &str) {
        text("0xGG", 24.0, 46.0, 40.0, theme::ACCENT);
        text(title, 130.0, 44.0, 26.0, theme::TEXT);
        if let Some(me) = &self.me {
            let who = format!("{}  #{}", me.username, me.player_id);
            let w = ui::text_width(&who, 22.0);
            text(&who, screen_width() - w - 24.0, 42.0, 22.0, theme::DIM);
        }
        draw_line(24.0, 64.0, screen_width() - 24.0, 64.0, 1.0, theme::BORDER);
    }

    fn draw_login(&mut self) {
        let cx = screen_width() / 2.0;
        text_centered("30 78 47 47", cx, 120.0, 22.0, theme::DIM);
        text_centered("0xGG", cx, 210.0, 110.0, theme::ACCENT);
        text_centered("Last packet standing.", cx, 250.0, 26.0, theme::TEXT);

        let w = 360.0;
        let x = cx - w / 2.0;
        self.ui.text_field(
            F_SERVER,
            Rect::new(x, 320.0, w, 42.0),
            "server",
            &mut self.server_addr,
            false,
            64,
            Some(F_USER),
        );
        let enter_user = self.ui.text_field(
            F_USER,
            Rect::new(x, 395.0, w, 42.0),
            "username",
            &mut self.username,
            false,
            USERNAME_MAX,
            Some(F_PASS),
        );
        let enter_pass = self.ui.text_field(
            F_PASS,
            Rect::new(x, 470.0, w, 42.0),
            "password",
            &mut self.password,
            true,
            PASSWORD_MAX,
            Some(F_SERVER),
        );

        let login = self
            .ui
            .button(Rect::new(x, 535.0, w / 2.0 - 6.0, 44.0), "LOGIN");
        let register = self.ui.button(
            Rect::new(x + w / 2.0 + 6.0, 535.0, w / 2.0 - 6.0, 44.0),
            "REGISTER",
        );

        let creds = (self.username.trim().to_string(), self.password.clone());
        if login || enter_pass || enter_user {
            self.connect_and_send(ClientMsg::Login {
                username: creds.0,
                password: creds.1,
            });
        } else if register {
            self.connect_and_send(ClientMsg::Register {
                username: creds.0,
                password: creds.1,
            });
        }
    }

    /// Shared chat panel for lobby and room screens.
    fn draw_chat(&mut self, r: Rect, title: &str) {
        panel(r);
        text(title, r.x + 14.0, r.y + 28.0, 22.0, theme::DIM);
        let mut y = r.y + 60.0;
        for (from, msg) in &self.chat {
            let color = if from == "*" {
                theme::WARN
            } else {
                theme::TEXT
            };
            let line = if from == "*" {
                msg.clone()
            } else {
                format!("{from}: {msg}")
            };
            text(&line, r.x + 14.0, y, 20.0, color);
            y += 24.0;
        }
        let field = Rect::new(r.x + 12.0, r.y + r.h - 54.0, r.w - 24.0, 42.0);
        if self.ui.text_field(
            F_CHAT,
            field,
            "",
            &mut self.chat_input,
            false,
            CHAT_MAX,
            None,
        ) {
            let text = std::mem::take(&mut self.chat_input);
            if !text.trim().is_empty() {
                self.send(ClientMsg::Chat { text });
            }
        }
    }

    fn draw_lobby(&mut self) {
        self.header("THE SWITCH  //  lobby");
        let left = Rect::new(24.0, 84.0, 700.0, screen_height() - 140.0);
        panel(left);
        text("OPEN PORTS", left.x + 14.0, left.y + 30.0, 22.0, theme::DIM);

        let mut join = None;
        let mut y = left.y + 50.0;
        for room in self.rooms.clone() {
            let row = Rect::new(left.x + 12.0, y, left.w - 24.0, 50.0);
            draw_rectangle(row.x, row.y, row.w, row.h, theme::PANEL_HI);
            text(
                &format!(":{}", room.id),
                row.x + 12.0,
                row.y + 32.0,
                24.0,
                theme::ACCENT,
            );
            text(&room.name, row.x + 80.0, row.y + 32.0, 24.0, theme::TEXT);
            let state = if room.in_match {
                "IN MATCH".to_string()
            } else {
                format!("{}/{}", room.players, room.max)
            };
            text(
                &state,
                row.x + row.w - 250.0,
                row.y + 32.0,
                22.0,
                theme::DIM,
            );
            if !room.in_match
                && room.players < room.max
                && self.ui.button(
                    Rect::new(row.x + row.w - 110.0, row.y + 7.0, 100.0, 36.0),
                    "JOIN",
                )
            {
                join = Some(room.id);
            }
            y += 58.0;
        }
        if self.rooms.is_empty() {
            text(
                "no ports open. create one below.",
                left.x + 14.0,
                y + 30.0,
                22.0,
                theme::DIM,
            );
        }

        let by = left.y + left.h - 60.0;
        let submit = self.ui.text_field(
            F_ROOM,
            Rect::new(left.x + 12.0, by, 420.0, 44.0),
            "",
            &mut self.new_room_name,
            false,
            ROOM_NAME_MAX,
            None,
        );
        let create = self
            .ui
            .button(Rect::new(left.x + 444.0, by, 160.0, 44.0), "CREATE PORT");
        if self.new_room_name.is_empty() && self.ui.focus != Some(F_ROOM) {
            text("room name...", left.x + 24.0, by + 29.0, 22.0, theme::DIM);
        }
        if (create || submit) && !self.new_room_name.trim().is_empty() {
            let name = std::mem::take(&mut self.new_room_name);
            self.send(ClientMsg::CreateRoom { name });
        }
        if let Some(room_id) = join {
            self.send(ClientMsg::JoinRoom { room_id });
        }

        let right = Rect::new(748.0, 84.0, screen_width() - 772.0, screen_height() - 200.0);
        self.draw_chat(right, "LOBBY CHAT");
        if self.ui.button(
            Rect::new(right.x, right.y + right.h + 12.0, 140.0, 40.0),
            "LOGOUT",
        ) {
            self.send(ClientMsg::Logout);
            self.connection_lost("logged out".into());
            self.status = None;
        }
    }

    fn draw_room(&mut self) {
        let Some(room) = &self.room else { return };
        self.header(&format!("PORT :{}  //  {}", room.room_id, room.name));
        let my_id = self.me.as_ref().map_or(0, |m| m.player_id);

        let left = Rect::new(24.0, 84.0, 700.0, screen_height() - 140.0);
        panel(left);
        text(
            &format!(
                "PACKETS {}/{}  - starts when full and all ready, or host starts with {MIN_PLAYERS_TO_START}+",
                room.players.len(),
                MAX_ROOM_PLAYERS
            ),
            left.x + 14.0,
            left.y + 30.0,
            20.0,
            theme::DIM,
        );
        let mut my_ready = false;
        for (i, p) in room.players.iter().enumerate() {
            let y = left.y + 56.0 + i as f32 * 70.0;
            let slot = i as u8 + 1;
            draw_rectangle(left.x + 12.0, y, left.w - 24.0, 60.0, theme::PANEL_HI);
            draw_rectangle(left.x + 12.0, y, 8.0, 60.0, theme::slot_color(slot));
            let mut name = p.username.clone();
            if p.player_id == room.host_id {
                name.push_str("  [host]");
            }
            if p.player_id == my_id {
                name.push_str("  (you)");
                my_ready = p.ready;
            }
            text(&name, left.x + 36.0, y + 38.0, 26.0, theme::TEXT);
            let (label, color) = if p.ready {
                ("READY", theme::ACCENT)
            } else {
                ("waiting", theme::DIM)
            };
            text(label, left.x + left.w - 130.0, y + 38.0, 24.0, color);
        }

        let n = room.players.len();
        let can_start = room.host_id == my_id
            && (MIN_PLAYERS_TO_START..MAX_ROOM_PLAYERS).contains(&n)
            && room.players.iter().all(|p| p.ready);
        let by = left.y + left.h - 60.0;
        let ready_label = if my_ready { "NOT READY" } else { "READY" };
        if self
            .ui
            .button(Rect::new(left.x + 12.0, by, 200.0, 46.0), ready_label)
        {
            self.send(ClientMsg::SetReady { ready: !my_ready });
        }
        if self.ui.button_colored(
            Rect::new(left.x + 224.0, by, 160.0, 46.0),
            "LEAVE",
            theme::ERROR,
        ) {
            self.send(ClientMsg::LeaveRoom);
        }
        if can_start {
            let label = format!("START ({n}/{MAX_ROOM_PLAYERS})");
            if self
                .ui
                .button(Rect::new(left.x + left.w - 212.0, by, 200.0, 46.0), &label)
            {
                self.send(ClientMsg::StartMatch);
            }
        }

        let right = Rect::new(748.0, 84.0, screen_width() - 772.0, screen_height() - 140.0);
        self.draw_chat(right, "PORT CHAT");
    }

    fn draw_match(&mut self) {
        let packets = match &mut self.game {
            Some(g) => g.update(self.token, self.auto.bot),
            None => return,
        };
        for p in packets {
            self.udp_send(p);
        }
        if let Some(g) = &self.game {
            g.draw();
        }
        if is_key_pressed(KeyCode::Escape) {
            self.send(ClientMsg::LeaveRoom);
        }
    }

    fn draw_results(&mut self) {
        let Some(r) = &self.results else { return };
        let cx = screen_width() / 2.0;
        let won = r.winner_slot == Some(r.your_slot);
        let winner = r.rankings.iter().find(|x| Some(x.slot) == r.winner_slot);

        if won {
            text_centered("0xGG", cx, 230.0, 180.0, theme::ACCENT);
            text_centered("DELIVERED", cx, 290.0, 40.0, theme::ACCENT);
            let sub = match r.reason {
                MatchEndReason::Normal => "200 OK - last packet standing.".to_string(),
                MatchEndReason::NotEnoughPlayers => {
                    "200 OK - delivered by default: everyone else dropped.".to_string()
                }
            };
            text_centered(&sub, cx, 330.0, 24.0, theme::TEXT);
        } else if let Some(w) = winner {
            text_centered("0xFF", cx, 230.0, 180.0, theme::ERROR);
            let x = cx - 330.0;
            text("FATAL 0xFF: PACKET_DROPPED", x, 290.0, 28.0, theme::ERROR);
            text(
                &format!("  {}: {}", r.error_code, r.error_text),
                x,
                322.0,
                22.0,
                theme::TEXT,
            );
            text(
                &format!("    at packet 0x{:02X} (you)", r.your_slot),
                x,
                348.0,
                22.0,
                theme::DIM,
            );
            text(
                &format!("    delivered instead: 0x{:02X} ({})", w.slot, w.username),
                x,
                374.0,
                22.0,
                theme::DIM,
            );
        } else {
            text_centered("0x00", cx, 230.0, 180.0, theme::DIM);
            text_centered(
                "NULL ROUTE - nobody was delivered",
                cx,
                290.0,
                30.0,
                theme::TEXT,
            );
        }

        // Final rankings.
        let tx = cx - 330.0;
        let mut y = 440.0;
        text(
            "RANK  PACKET  NAME              ROUNDS  KILLS  DEATHS",
            tx,
            y,
            22.0,
            theme::DIM,
        );
        for (i, rk) in r.rankings.iter().enumerate() {
            y += 32.0;
            let status = if rk.disconnected { "  (dropped)" } else { "" };
            let line = format!(
                "#{}    0x{:02X}    {:<16}  {:>4}   {:>4}   {:>4}{}",
                i + 1,
                rk.slot,
                rk.username,
                rk.round_wins,
                rk.kills,
                rk.deaths,
                status
            );
            text(&line, tx, y, 22.0, theme::slot_color(rk.slot));
        }

        let back_to = if self.room.is_some() {
            "BACK TO PORT"
        } else {
            "BACK TO LOBBY"
        };
        if self.ui.button(
            Rect::new(cx - 110.0, screen_height() - 110.0, 220.0, 46.0),
            back_to,
        ) {
            self.results = None;
            self.screen = if self.room.is_some() {
                Screen::Room
            } else {
                Screen::Lobby
            };
        }
    }
}
