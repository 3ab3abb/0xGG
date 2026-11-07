//! In-match client: samples input at a fixed 60 Hz, sends it over UDP with
//! redundancy, consumes snapshots and reliable events, and renders.
//!
//! Milestone 2 renders the latest snapshot as-is (no prediction or
//! interpolation yet), so movement shows the raw network round trip.

use std::collections::VecDeque;

use gg_shared::game::{buttons, Map, Tile, DT, PLAYER_H, PLAYER_W, SNAPSHOT_EVERY, TILE};
use gg_shared::protocol::SlotInfo;
use gg_shared::udp::{
    flags, ClientPacket, EventKind, InputFrame, KillCause, RoundPhase, Snapshot, INPUT_REDUNDANCY,
};
use macroquad::prelude::*;

use crate::ui::{text, text_centered, text_width, theme};

const SCALE: f32 = 2.0;
const FEED_SECONDS: f64 = 6.0;

pub struct GameView {
    pub map: Map,
    pub slots: Vec<SlotInfo>,
    pub your_slot: u8,
    latest: Option<Snapshot>,
    events_ack: u32,
    feed: VecDeque<(f64, String, Color)>,
    // input
    seq: u32,
    history: VecDeque<InputFrame>,
    acc: f32,
    // network stats
    snaps_received: u64,
    snaps_expected: u64,
    snaps_stale: u64,
    rate_window: VecDeque<f64>,
    bot_buttons: u16,
    bot_next_change: f64,
}

fn read_buttons() -> u16 {
    let any = |keys: &[KeyCode]| keys.iter().any(|&k| is_key_down(k) || is_key_pressed(k));
    let mut b = 0;
    if any(&[KeyCode::A, KeyCode::Left]) {
        b |= buttons::LEFT;
    }
    if any(&[KeyCode::D, KeyCode::Right]) {
        b |= buttons::RIGHT;
    }
    if any(&[KeyCode::W, KeyCode::Up, KeyCode::Space]) {
        b |= buttons::JUMP;
    }
    if any(&[KeyCode::S, KeyCode::Down]) {
        b |= buttons::DOWN;
    }
    b
}

fn hex(slot: u8) -> String {
    format!("0x{slot:02X}")
}

impl GameView {
    pub fn new(map: Map, slots: Vec<SlotInfo>, your_slot: u8) -> Self {
        GameView {
            map,
            slots,
            your_slot,
            latest: None,
            events_ack: 0,
            feed: VecDeque::new(),
            seq: 0,
            history: VecDeque::new(),
            acc: 0.0,
            snaps_received: 0,
            snaps_expected: 0,
            snaps_stale: 0,
            rate_window: VecDeque::new(),
            bot_buttons: 0,
            bot_next_change: 0.0,
        }
    }

    fn name(&self, slot: u8) -> &str {
        self.slots
            .iter()
            .find(|s| s.slot == slot)
            .map_or("?", |s| s.username.as_str())
    }

    pub fn log(&mut self, line: String, color: Color) {
        self.feed.push_back((get_time(), line, color));
        while self.feed.len() > 6 {
            self.feed.pop_front();
        }
    }

    pub fn on_snapshot(&mut self, s: Snapshot) {
        let last_tick = self.latest.as_ref().map(|l| l.tick);
        // Reordered or duplicated datagram: a newer state is already applied.
        if last_tick.is_some_and(|t| s.tick <= t) {
            self.snaps_stale += 1;
            return;
        }
        if let Some(t) = last_tick {
            self.snaps_expected += ((s.tick - t) / SNAPSHOT_EVERY).max(1) as u64;
        } else {
            self.snaps_expected += 1;
        }
        self.snaps_received += 1;
        let now = get_time();
        self.rate_window.push_back(now);
        while self.rate_window.front().is_some_and(|&t| now - t > 1.0) {
            self.rate_window.pop_front();
        }

        // Reliable events: process strictly in id order, exactly once.
        let mut events = s.events.clone();
        events.sort_by_key(|e| e.id);
        for e in events {
            if e.id != self.events_ack + 1 {
                continue; // duplicate (already handled) or a gap we wait to fill
            }
            self.events_ack = e.id;
            match e.kind {
                EventKind::Kill {
                    killer,
                    victim,
                    cause,
                } => {
                    let line = match (killer, cause) {
                        (Some(k), KillCause::Stomp) => {
                            format!("{} --[STOMP]--> {} DROPPED", hex(k), hex(victim))
                        }
                        (_, KillCause::Fall) => format!("{} lost in the backbone", hex(victim)),
                        (None, _) => format!("{} DROPPED", hex(victim)),
                    };
                    let color = if victim == self.your_slot {
                        theme::ERROR
                    } else {
                        theme::slot_color(killer.unwrap_or(victim))
                    };
                    self.log(line, color);
                }
            }
        }
        self.latest = Some(s);
    }

    /// Advance the fixed 60 Hz input clock; returns the Input packets to send.
    /// Random walk with jumps: enough to exercise movement, stomps and pits.
    fn bot(&mut self) -> u16 {
        use macroquad::rand::gen_range;
        let now = get_time();
        if now >= self.bot_next_change {
            self.bot_next_change = now + gen_range(0.2, 0.9);
            let mut b = match gen_range(0, 5) {
                0 => 0,
                1 | 2 => buttons::LEFT,
                _ => buttons::RIGHT,
            };
            if gen_range(0, 3) == 0 {
                b |= buttons::JUMP;
            }
            if gen_range(0, 8) == 0 {
                b |= buttons::DOWN | buttons::JUMP;
            }
            self.bot_buttons = b;
        }
        self.bot_buttons
    }

    pub fn update(&mut self, token: u64, bot: bool) -> Vec<ClientPacket> {
        // Clamp so a long frame (window drag) doesn't burst-send hundreds of inputs.
        self.acc = (self.acc + get_frame_time()).min(0.25);
        let b = if bot { self.bot() } else { read_buttons() };
        let view_tick = self.latest.as_ref().map_or(0, |s| s.tick);
        let mut out = Vec::new();
        while self.acc >= DT {
            self.acc -= DT;
            self.seq += 1;
            self.history.push_back(InputFrame {
                seq: self.seq,
                buttons: b,
                view_tick,
            });
            while self.history.len() > INPUT_REDUNDANCY {
                self.history.pop_front();
            }
            out.push(ClientPacket::Input {
                token,
                frames: self.history.iter().copied().collect(),
                last_snapshot_tick: view_tick,
                events_ack: self.events_ack,
            });
        }
        out
    }

    pub fn net_stats(&self) -> String {
        let loss = if self.snaps_expected > 0 {
            100.0 * (1.0 - self.snaps_received as f64 / self.snaps_expected as f64)
        } else {
            0.0
        };
        let unacked = self
            .latest
            .as_ref()
            .map_or(0, |s| self.seq.saturating_sub(s.ack_input_seq));
        format!(
            "snapshots {}/s  loss {:.1}%  stale {}  inputs in flight {}",
            self.rate_window.len(),
            loss.max(0.0),
            self.snaps_stale,
            unacked
        )
    }

    // ------------------------------------------------------------- render

    fn origin(&self) -> Vec2 {
        vec2(
            (screen_width() - self.map.pixel_width() * SCALE) / 2.0,
            (screen_height() - self.map.pixel_height() * SCALE) / 2.0,
        )
    }

    pub fn draw(&self) {
        let o = self.origin();
        self.draw_map(o);
        let Some(s) = &self.latest else {
            text_centered(
                "waiting for first snapshot over UDP...",
                screen_width() / 2.0,
                screen_height() / 2.0,
                28.0,
                theme::DIM,
            );
            return;
        };

        for p in &s.players {
            if p.flags & flags::ALIVE == 0 {
                continue;
            }
            let x = o.x + p.x * SCALE;
            let y = o.y + p.y * SCALE;
            let (w, h) = (PLAYER_W * SCALE, PLAYER_H * SCALE);
            let color = theme::slot_color(p.slot);
            // Body + darker "header" band: it's a packet.
            draw_rectangle(x, y, w, h, color);
            draw_rectangle(
                x,
                y,
                w,
                4.0 * SCALE,
                Color::new(color.r * 0.55, color.g * 0.55, color.b * 0.55, 1.0),
            );
            // Eyes look where the packet faces.
            let right = p.flags & flags::FACING_RIGHT != 0;
            let ex = if right { x + w - 10.0 } else { x + 2.0 };
            let pupil = if right { 2.0 } else { 0.0 };
            for dx in [0.0, 5.0] {
                draw_rectangle(ex + dx, y + 10.0, 4.0, 6.0, WHITE);
                draw_rectangle(ex + dx + pupil, y + 12.0, 2.0, 3.0, BLACK);
            }
            let label = hex(p.slot);
            let lc = if p.slot == self.your_slot {
                theme::ACCENT
            } else {
                theme::TEXT
            };
            text_centered(&label, x + w / 2.0, y - 6.0, 18.0, lc);
            if p.slot == self.your_slot {
                let cx = x + w / 2.0;
                draw_triangle(
                    vec2(cx - 5.0, y - 28.0),
                    vec2(cx + 5.0, y - 28.0),
                    vec2(cx, y - 22.0),
                    theme::ACCENT,
                );
            }
        }
        self.draw_hud(s);
    }

    fn draw_map(&self, o: Vec2) {
        let t = TILE * SCALE;
        for ty in 0..self.map.height as i32 {
            for tx in 0..self.map.width as i32 {
                let x = o.x + tx as f32 * t;
                let y = o.y + ty as f32 * t;
                match self.map.tile(tx, ty) {
                    Tile::Solid => {
                        draw_rectangle(x, y, t, t, Color::new(0.08, 0.12, 0.16, 1.0));
                        // Circuit-trace edge where the block is exposed on top.
                        if self.map.tile(tx, ty - 1) != Tile::Solid && ty > 0 {
                            draw_rectangle(x, y, t, 3.0, Color::new(0.15, 0.55, 0.35, 1.0));
                        }
                    }
                    Tile::Platform => {
                        draw_rectangle(x, y, t, 6.0, Color::new(0.2, 0.75, 0.5, 1.0));
                        draw_rectangle(
                            x + t / 2.0 - 2.0,
                            y + 6.0,
                            4.0,
                            4.0,
                            Color::new(0.12, 0.4, 0.28, 1.0),
                        );
                    }
                    Tile::Empty => {
                        draw_rectangle(
                            x + t / 2.0 - 1.0,
                            y + t / 2.0 - 1.0,
                            2.0,
                            2.0,
                            Color::new(0.1, 0.14, 0.18, 1.0),
                        );
                    }
                }
            }
        }
    }

    fn draw_hud(&self, s: &Snapshot) {
        let cx = screen_width() / 2.0;

        // Scores: round wins as pips, first to 5.
        let mut x = 24.0;
        for &(slot, wins) in &s.scores {
            let connected = s
                .players
                .iter()
                .any(|p| p.slot == slot && p.flags & flags::CONNECTED != 0);
            let color = if connected {
                theme::slot_color(slot)
            } else {
                theme::DIM
            };
            let label = format!("{} {}", hex(slot), self.name(slot));
            text(&label, x, 34.0, 20.0, color);
            let lw = text_width(&label, 20.0);
            for i in 0..gg_shared::constants::ROUNDS_TO_WIN {
                let px = x + lw + 10.0 + i as f32 * 12.0;
                if i < wins {
                    draw_rectangle(px, 22.0, 9.0, 12.0, color);
                } else {
                    draw_rectangle_lines(px, 22.0, 9.0, 12.0, 2.0, color);
                }
            }
            x += lw + 90.0;
        }

        // Phase.
        match s.phase {
            RoundPhase::Countdown { ms_left } => {
                text_centered(&format!("ROUND {}", s.round), cx, 250.0, 40.0, theme::TEXT);
                text_centered(
                    &format!("{}", (ms_left as f32 / 1000.0).ceil() as u32),
                    cx,
                    340.0,
                    110.0,
                    theme::ACCENT,
                );
            }
            RoundPhase::Playing { ms_left } => {
                let secs = ms_left / 1000;
                text_centered(
                    &format!("R{}  {}:{:02}", s.round, secs / 60, secs % 60),
                    cx,
                    34.0,
                    22.0,
                    theme::DIM,
                );
            }
            RoundPhase::RoundOver { winner, .. } => {
                let (line, color) = match winner {
                    Some(w) if w == self.your_slot => {
                        ("ROUND DELIVERED".to_string(), theme::ACCENT)
                    }
                    Some(w) => (format!("{} DELIVERED", hex(w)), theme::slot_color(w)),
                    None => ("NULL ROUTE - draw".to_string(), theme::DIM),
                };
                text_centered(&line, cx, 300.0, 56.0, color);
            }
        }

        let me_alive = s
            .players
            .iter()
            .any(|p| p.slot == self.your_slot && p.flags & flags::ALIVE != 0);
        if !me_alive && matches!(s.phase, RoundPhase::Playing { .. }) {
            text_centered("PACKET DROPPED", cx, 200.0, 48.0, theme::ERROR);
            text_centered("spectating until next round", cx, 230.0, 20.0, theme::DIM);
        }

        // Kill feed, top right.
        let now = get_time();
        let mut y = 70.0;
        for (t, line, color) in &self.feed {
            if now - t > FEED_SECONDS {
                continue;
            }
            let w = text_width(line, 20.0);
            text(line, screen_width() - w - 40.0, y, 20.0, *color);
            y += 24.0;
        }

        text(
            &self.net_stats(),
            40.0,
            screen_height() - 34.0,
            16.0,
            theme::DIM,
        );
        text(
            "move A/D  jump W/SPACE  drop S+jump  forfeit ESC",
            40.0,
            screen_height() - 50.0,
            16.0,
            theme::DIM,
        );
    }
}
