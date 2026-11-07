//! Client state machine and screens:
//! Login → Lobby ("The Switch") → Room ("Port") → Match → Results (0xGG / 0xFF).

use std::collections::VecDeque;

use gg_shared::constants::*;
use gg_shared::protocol::*;
use macroquad::prelude::*;

use crate::net::{Connection, NetEvent};
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

struct MatchView {
    map: String,
    slots: Vec<SlotInfo>,
    your_slot: u8,
    started_at: f64,
    countdown_ms: u32,
    log: Vec<(String, Color)>,
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
    ("E_CHECKSUM_MISMATCH", "packet integrity compromised, discarded at hop"),
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
    match_view: Option<MatchView>,
    results: Option<ResultsView>,
    last_ping_sent: f64,
    rtt_ms: Option<f64>,
}

impl App {
    pub fn new(server_addr: String, username: String) -> Self {
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
            match_view: None,
            results: None,
            last_ping_sent: 0.0,
            rtt_ms: None,
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
            self.set_status(format!("connecting to {} ...", self.server_addr), theme::DIM);
            match Connection::connect(&self.server_addr) {
                Ok(c) => self.conn = Some(c),
                Err(e) => {
                    self.set_status(format!("cannot reach {}: {e}", self.server_addr), theme::ERROR);
                    return;
                }
            }
        }
        self.send(msg);
    }

    fn connection_lost(&mut self, why: String) {
        self.conn = None;
        self.me = None;
        self.room = None;
        self.match_view = None;
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
                NetEvent::Msg(msg) => self.on_message(msg),
                NetEvent::Closed(why) => {
                    self.connection_lost(why);
                    return;
                }
            }
        }
        // Application-level heartbeat; also gives us an RTT readout.
        let now = get_time();
        if self.conn.is_some() && now - self.last_ping_sent >= TCP_HEARTBEAT_INTERVAL.as_secs_f64() {
            self.last_ping_sent = now;
            self.send(ClientMsg::Ping { nonce: (now * 1000.0) as u64 });
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
            ServerMsg::Welcome { protocol, server_name } => {
                if protocol != PROTOCOL_VERSION {
                    self.connection_lost(format!(
                        "server speaks protocol v{protocol}, this client v{PROTOCOL_VERSION}"
                    ));
                } else {
                    self.set_status(format!("connected to '{server_name}'"), theme::DIM);
                }
            }
            ServerMsg::AuthOk { player_id, username, .. } => {
                self.status = None;
                self.chat.clear();
                self.me = Some(Me { player_id, username });
                self.password.clear();
                self.screen = Screen::Lobby;
                self.ui.focus = None;
            }
            ServerMsg::AuthError { reason } => self.set_status(reason.describe(), theme::ERROR),
            ServerMsg::RoomList { rooms } => self.rooms = rooms,
            ServerMsg::RoomState { room_id, name, host_id, players } => {
                if self.screen == Screen::Lobby {
                    self.screen = Screen::Room;
                    self.chat.clear();
                    self.status = None;
                }
                self.room = Some(RoomView { room_id, name, host_id, players });
            }
            ServerMsg::LeftRoom => {
                self.room = None;
                self.match_view = None;
                self.chat.clear();
                self.screen = Screen::Lobby;
            }
            ServerMsg::Chat { from, text, .. } => self.push_chat(from, text),
            ServerMsg::MatchStarting { map, slots, your_slot, countdown_ms } => {
                self.match_view = Some(MatchView {
                    map,
                    slots,
                    your_slot,
                    started_at: get_time(),
                    countdown_ms,
                    log: Vec::new(),
                });
                self.screen = Screen::Match;
            }
            ServerMsg::PlayerDisconnected { slot, username, reason } => {
                let why = match reason {
                    DisconnectReason::Closed => "connection closed",
                    DisconnectReason::Timeout => "timed out",
                    DisconnectReason::Error => "protocol error",
                    DisconnectReason::Left => "left the match",
                };
                let line = format!("0x{slot:02X} {username} DROPPED ({why})");
                self.push_chat("*", line.clone());
                if let Some(m) = &mut self.match_view {
                    m.log.push((line, theme::WARN));
                }
            }
            ServerMsg::MatchEnded { reason, winner_slot, rankings } => {
                let your_slot = self.match_view.as_ref().map_or(0, |m| m.your_slot);
                let (error_code, error_text) =
                    LOSS_ERRORS[macroquad::rand::gen_range(0, LOSS_ERRORS.len())];
                self.results = Some(ResultsView { reason, winner_slot, rankings, your_slot, error_code, error_text });
                self.match_view = None;
                self.screen = Screen::Results;
            }
            ServerMsg::Error { message, .. } => self.set_status(message, theme::ERROR),
            ServerMsg::Pong { nonce } => {
                self.rtt_ms = Some((get_time() * 1000.0 - nonce as f64).max(0.0));
            }
        }
    }

    // ---------------------------------------------------------------- draw

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
        let net = match (&self.conn, self.rtt_ms) {
            (None, _) => "offline".to_string(),
            (Some(_), Some(rtt)) => format!("tcp rtt {rtt:.0} ms"),
            (Some(_), None) => "online".to_string(),
        };
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
        self.ui.text_field(F_SERVER, Rect::new(x, 320.0, w, 42.0), "server", &mut self.server_addr, false, 64, Some(F_USER));
        let enter_user =
            self.ui.text_field(F_USER, Rect::new(x, 395.0, w, 42.0), "username", &mut self.username, false, USERNAME_MAX, Some(F_PASS));
        let enter_pass =
            self.ui.text_field(F_PASS, Rect::new(x, 470.0, w, 42.0), "password", &mut self.password, true, PASSWORD_MAX, Some(F_SERVER));

        let login = self.ui.button(Rect::new(x, 535.0, w / 2.0 - 6.0, 44.0), "LOGIN");
        let register = self.ui.button(Rect::new(x + w / 2.0 + 6.0, 535.0, w / 2.0 - 6.0, 44.0), "REGISTER");

        let creds = (self.username.trim().to_string(), self.password.clone());
        if login || enter_pass || enter_user {
            self.connect_and_send(ClientMsg::Login { username: creds.0, password: creds.1 });
        } else if register {
            self.connect_and_send(ClientMsg::Register { username: creds.0, password: creds.1 });
        }
    }

    /// Shared chat panel for lobby and room screens.
    fn draw_chat(&mut self, r: Rect, title: &str) {
        panel(r);
        text(title, r.x + 14.0, r.y + 28.0, 22.0, theme::DIM);
        let mut y = r.y + 60.0;
        for (from, msg) in &self.chat {
            let color = if from == "*" { theme::WARN } else { theme::TEXT };
            let line = if from == "*" { msg.clone() } else { format!("{from}: {msg}") };
            text(&line, r.x + 14.0, y, 20.0, color);
            y += 24.0;
        }
        let field = Rect::new(r.x + 12.0, r.y + r.h - 54.0, r.w - 24.0, 42.0);
        if self.ui.text_field(F_CHAT, field, "", &mut self.chat_input, false, CHAT_MAX, None) {
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
            text(&format!(":{}", room.id), row.x + 12.0, row.y + 32.0, 24.0, theme::ACCENT);
            text(&room.name, row.x + 80.0, row.y + 32.0, 24.0, theme::TEXT);
            let state = if room.in_match {
                "IN MATCH".to_string()
            } else {
                format!("{}/{}", room.players, room.max)
            };
            text(&state, row.x + row.w - 250.0, row.y + 32.0, 22.0, theme::DIM);
            if !room.in_match && room.players < room.max
                && self.ui.button(Rect::new(row.x + row.w - 110.0, row.y + 7.0, 100.0, 36.0), "JOIN")
            {
                join = Some(room.id);
            }
            y += 58.0;
        }
        if self.rooms.is_empty() {
            text("no ports open. create one below.", left.x + 14.0, y + 30.0, 22.0, theme::DIM);
        }

        let by = left.y + left.h - 60.0;
        let submit = self.ui.text_field(
            F_ROOM, Rect::new(left.x + 12.0, by, 420.0, 44.0), "", &mut self.new_room_name, false, ROOM_NAME_MAX, None,
        );
        let create = self.ui.button(Rect::new(left.x + 444.0, by, 160.0, 44.0), "CREATE PORT");
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
        if self.ui.button(Rect::new(right.x, right.y + right.h + 12.0, 140.0, 40.0), "LOGOUT") {
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
            &format!("PACKETS {}/{}  - match starts when everyone is ready (min {MIN_PLAYERS_TO_START})", room.players.len(), MAX_ROOM_PLAYERS),
            left.x + 14.0, left.y + 30.0, 20.0, theme::DIM,
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
            let (label, color) = if p.ready { ("READY", theme::ACCENT) } else { ("waiting", theme::DIM) };
            text(label, left.x + left.w - 130.0, y + 38.0, 24.0, color);
        }

        let by = left.y + left.h - 60.0;
        let ready_label = if my_ready { "NOT READY" } else { "READY" };
        if self.ui.button(Rect::new(left.x + 12.0, by, 200.0, 46.0), ready_label) {
            self.send(ClientMsg::SetReady { ready: !my_ready });
        }
        if self.ui.button_colored(Rect::new(left.x + 224.0, by, 160.0, 46.0), "LEAVE", theme::ERROR) {
            self.send(ClientMsg::LeaveRoom);
        }

        let right = Rect::new(748.0, 84.0, screen_width() - 772.0, screen_height() - 140.0);
        self.draw_chat(right, "PORT CHAT");
    }

    /// Placeholder until the UDP game loop lands (milestone 2): shows the
    /// match roster, countdown and live disconnect notices.
    fn draw_match(&mut self) {
        let Some(m) = &self.match_view else { return };
        self.header(&format!("MATCH  //  map: {}", m.map));
        let cx = screen_width() / 2.0;
        let left_ms = m.countdown_ms as f64 - (get_time() - m.started_at) * 1000.0;
        if left_ms > 0.0 {
            text_centered(&format!("{}", (left_ms / 1000.0).ceil()), cx, 220.0, 120.0, theme::ACCENT);
        } else {
            text_centered("ROUTING...", cx, 200.0, 60.0, theme::ACCENT);
            text_centered("gameplay arrives in milestone 2 (UDP game loop)", cx, 240.0, 22.0, theme::DIM);
        }
        for (i, s) in m.slots.iter().enumerate() {
            let y = 320.0 + i as f32 * 56.0;
            let you = if s.slot == m.your_slot { "  < you" } else { "" };
            let line = format!("0x{:02X}   {}{}", s.slot, s.username, you);
            text(&line, cx - 200.0, y, 32.0, theme::slot_color(s.slot));
        }
        let mut y = 520.0;
        for (line, color) in &m.log {
            text_centered(line, cx, y, 22.0, *color);
            y += 26.0;
        }
        if self.ui.button_colored(Rect::new(cx - 90.0, screen_height() - 110.0, 180.0, 44.0), "FORFEIT", theme::ERROR) {
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
                MatchEndReason::NotEnoughPlayers => "200 OK - delivered by default: everyone else dropped.".to_string(),
            };
            text_centered(&sub, cx, 330.0, 24.0, theme::TEXT);
        } else if let Some(w) = winner {
            text_centered("0xFF", cx, 230.0, 180.0, theme::ERROR);
            let x = cx - 330.0;
            text("FATAL 0xFF: PACKET_DROPPED", x, 290.0, 28.0, theme::ERROR);
            text(&format!("  {}: {}", r.error_code, r.error_text), x, 322.0, 22.0, theme::TEXT);
            text(&format!("    at packet 0x{:02X} (you)", r.your_slot), x, 348.0, 22.0, theme::DIM);
            text(&format!("    delivered instead: 0x{:02X} ({})", w.slot, w.username), x, 374.0, 22.0, theme::DIM);
        } else {
            text_centered("0x00", cx, 230.0, 180.0, theme::DIM);
            text_centered("NULL ROUTE - nobody was delivered", cx, 290.0, 30.0, theme::TEXT);
        }

        // Final rankings.
        let tx = cx - 330.0;
        let mut y = 440.0;
        text("RANK  PACKET  NAME              ROUNDS  KILLS  DEATHS", tx, y, 22.0, theme::DIM);
        for (i, rk) in r.rankings.iter().enumerate() {
            y += 32.0;
            let status = if rk.disconnected { "  (dropped)" } else { "" };
            let line = format!(
                "#{}    0x{:02X}    {:<16}  {:>4}   {:>4}   {:>4}{}",
                i + 1, rk.slot, rk.username, rk.round_wins, rk.kills, rk.deaths, status
            );
            text(&line, tx, y, 22.0, theme::slot_color(rk.slot));
        }

        let back_to = if self.room.is_some() { "BACK TO PORT" } else { "BACK TO LOBBY" };
        if self.ui.button(Rect::new(cx - 110.0, screen_height() - 110.0, 220.0, 46.0), back_to) {
            self.results = None;
            self.screen = if self.room.is_some() { Screen::Room } else { Screen::Lobby };
        }
    }
}
