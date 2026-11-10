//! In-match client: samples input at a fixed 60 Hz, sends it over UDP with
//! redundancy, consumes snapshots and reliable events, and renders.
//!
//! Your own packet is drawn from client-side prediction, everything else
//! from entity interpolation ~100 ms in the past (see `netcode`). F1 / F2
//! toggle each technique live, to compare against the raw server state.

use std::collections::VecDeque;

use gg_shared::bot::BotBrain;
use gg_shared::game::{buttons, Map, Tile, DT, PLAYER_H, PLAYER_W, SNAPSHOT_EVERY, TILE};
use gg_shared::protocol::SlotInfo;
use gg_shared::udp::{
    flags, ClientPacket, EventKind, InputFrame, KillCause, PlayerState, ProjectileKind, RoundPhase,
    Snapshot, INPUT_REDUNDANCY,
};
use gg_shared::weapons::{Weapon, FIREWALL_H, FIREWALL_W, ITEM_H, ITEM_W};
use macroquad::prelude::*;

use crate::screen::{VH, VW};

use crate::netcode::{Interpolator, Predictor};
use crate::sprites::{self, Art, SEEKER_EXTRA_H};
use crate::ui::{text, text_centered, text_width, theme};

const SCALE: f32 = 2.0;
const FEED_SECONDS: f64 = 6.0;
const TRACER_SECONDS: f64 = 0.15;
const BLAST_SECONDS: f64 = 0.4;
const PICKUP_HINT_RANGE: f32 = 18.0;
/// How long the PACKET DROPPED puff lingers where a player died.
const DROP_SECONDS: f64 = 0.9;
/// Server blade swing length (16 ticks at 60 Hz); the attack animation spans it.
const SWING_SECONDS: f64 = 16.0 / 60.0;
/// Checksum Blade reach in world px past the player's edge (matches the server).
const BLADE_REACH: f32 = 20.0;

pub const CONTROLS: &str =
    "move A/D  jump SPACE  aim up W  drop S+SPACE  shoot J  pick up/throw K  forfeit ESC   (or arrows + Z/X/C)";

enum Effect {
    Tracer { x0: f32, y0: f32, x1: f32, y1: f32 },
    Blast { x: f32, y: f32, radius: f32 },
}

pub struct GameView {
    pub map: Map,
    pub slots: Vec<SlotInfo>,
    pub your_slot: u8,
    latest: Option<Snapshot>,
    /// What gets drawn this frame: interpolated world + predicted self.
    view: Option<Snapshot>,
    events_ack: u32,
    // latency hiding
    pred: Predictor,
    interp: Interpolator,
    predict_on: bool,
    interp_on: bool,
    /// Buttons of the newest local input (aim-up is drawn from it).
    last_buttons: u16,
    feed: VecDeque<(f64, String, Color)>,
    effects: Vec<(f64, Effect)>,
    // input
    seq: u32,
    history: VecDeque<InputFrame>,
    acc: f32,
    // network stats
    snaps_received: u64,
    snaps_expected: u64,
    snaps_stale: u64,
    rate_window: VecDeque<f64>,
    brain: BotBrain,
    // cosmetics
    art: Art,
    /// When each slot's current blade swing started (client clock).
    swing_start: [Option<f64>; 8],
    /// (time, world x, world y, slot) of recent deaths, for the drop puff.
    drops: Vec<(f64, f32, f32, u8)>,
}

fn read_buttons() -> u16 {
    let any = |keys: &[KeyCode]| keys.iter().any(|&k| is_key_down(k) || is_key_pressed(k));
    let mut b = 0;
    for (keys, bit) in [
        (&[KeyCode::A, KeyCode::Left][..], buttons::LEFT),
        (&[KeyCode::D, KeyCode::Right][..], buttons::RIGHT),
        (&[KeyCode::Space, KeyCode::Z][..], buttons::JUMP),
        (&[KeyCode::W, KeyCode::Up][..], buttons::AIM_UP),
        (&[KeyCode::S, KeyCode::Down][..], buttons::DOWN),
        (&[KeyCode::J, KeyCode::X][..], buttons::SHOOT),
        (&[KeyCode::K, KeyCode::C][..], buttons::INTERACT),
    ] {
        if any(keys) {
            b |= bit;
        }
    }
    b
}

fn hex(slot: u8) -> String {
    format!("0x{slot:02X}")
}

fn with_alpha(c: Color, a: f32) -> Color {
    Color::new(c.r, c.g, c.b, a)
}

/// Draw a weapon icon. `(x, y)` is the grip in screen space; the weapon
/// points right, left or up. Shapes are in world pixels (scaled here).
fn draw_weapon(w: Weapon, x: f32, y: f32, right: bool, up: bool, alpha: f32) {
    // dx runs along the barrel, dy across it.
    let rect = |dx: f32, dy: f32, len: f32, thick: f32, c: Color| {
        let (dx, dy, len, thick) = (dx * SCALE, dy * SCALE, len * SCALE, thick * SCALE);
        let c = with_alpha(c, alpha);
        if up {
            draw_rectangle(x + dy, y - dx - len, thick, len, c);
        } else if right {
            draw_rectangle(x + dx, y + dy, len, thick, c);
        } else {
            draw_rectangle(x - dx - len, y + dy, len, thick, c);
        }
    };
    let metal = Color::new(0.75, 0.8, 0.85, 1.0);
    let dark = Color::new(0.3, 0.33, 0.38, 1.0);
    match w {
        Weapon::PingPistol => {
            rect(0.0, -1.5, 7.0, 3.0, metal);
            rect(0.0, 1.0, 2.0, 3.0, dark);
        }
        Weapon::BroadcastShotgun => {
            rect(-2.0, -1.0, 4.0, 3.0, Color::new(0.55, 0.35, 0.2, 1.0));
            rect(2.0, -1.0, 11.0, 2.0, metal);
        }
        Weapon::ForkBomb => {
            draw_circle(
                x,
                y,
                3.5 * SCALE,
                with_alpha(Color::new(0.85, 0.2, 0.25, 1.0), alpha),
            );
            draw_rectangle(
                x - 1.0,
                y - 5.0 * SCALE,
                2.0,
                2.0 * SCALE,
                with_alpha(theme::WARN, alpha),
            );
        }
        Weapon::Firewall => {
            let c = Color::new(1.0, 0.45, 0.15, 1.0);
            rect(0.0, -3.0, 6.0, 6.0, c);
            rect(0.0, -0.5, 6.0, 1.0, Color::new(0.5, 0.15, 0.05, 1.0));
        }
        Weapon::ChecksumBlade => {
            rect(0.0, -1.0, 3.0, 2.0, dark);
            rect(3.0, -0.5, 11.0, 1.5, Color::new(0.4, 1.0, 0.9, 1.0));
        }
    }
}

impl GameView {
    pub fn new(map: Map, slots: Vec<SlotInfo>, your_slot: u8) -> Self {
        GameView {
            map,
            slots,
            your_slot,
            latest: None,
            view: None,
            events_ack: 0,
            pred: Predictor::default(),
            interp: Interpolator::default(),
            predict_on: true,
            interp_on: true,
            last_buttons: 0,
            feed: VecDeque::new(),
            effects: Vec::new(),
            seq: 0,
            history: VecDeque::new(),
            acc: 0.0,
            snaps_received: 0,
            snaps_expected: 0,
            snaps_stale: 0,
            rate_window: VecDeque::new(),
            brain: BotBrain::new(macroquad::rand::rand() as u64 ^ ((get_time() * 1e9) as u64)),
            art: Art::load(theme::slot_color),
            swing_start: [None; 8],
            drops: Vec::new(),
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
            self.on_event(e.kind, now);
        }
        self.track_animation(&s, now);
        self.interp.push(&s, now);
        self.pred.on_snapshot(&s, &self.map, self.predict_on);
        self.latest = Some(s);
    }

    /// Derive cosmetic animation state (swing start, deaths) from snapshot deltas.
    fn track_animation(&mut self, s: &Snapshot, now: f64) {
        for p in &s.players {
            let Some(slot) = self.swing_start.get_mut(p.slot as usize) else {
                continue;
            };
            let swinging = p.flags & flags::ALIVE != 0 && p.flags & flags::SWINGING != 0;
            match (swinging, *slot) {
                (true, None) => *slot = Some(now),
                (false, Some(_)) => *slot = None,
                _ => {}
            }
            let was_alive = self.latest.as_ref().and_then(|l| {
                l.players
                    .iter()
                    .find(|q| q.slot == p.slot)
                    .map(|q| (q.flags & flags::ALIVE != 0, q.x, q.y))
            });
            if let Some((true, x, y)) = was_alive {
                if p.flags & flags::ALIVE == 0 {
                    self.drops
                        .push((now, x + PLAYER_W / 2.0, y + PLAYER_H / 2.0, p.slot));
                }
            }
        }
    }

    fn on_event(&mut self, kind: EventKind, now: f64) {
        match kind {
            EventKind::Kill {
                killer,
                victim,
                cause,
            } => {
                let v = hex(victim);
                let line = match (killer, cause) {
                    (_, KillCause::Fall) => format!("{v} lost in the backbone"),
                    (Some(k), KillCause::Weapon(Weapon::ForkBomb)) if k == victim => {
                        format!("{v} fork()ed itself")
                    }
                    (Some(k), _) if k == victim => format!("{v} self-dropped"),
                    (Some(k), KillCause::Stomp) => format!("{} --[STOMP]--> {v} DROPPED", hex(k)),
                    (Some(k), KillCause::Reflect) => {
                        format!("{} --[REFLECT]--> {v} DROPPED", hex(k))
                    }
                    (Some(k), KillCause::Weapon(w)) => {
                        format!("{} --[{}]--> {v} DROPPED", hex(k), w.tag())
                    }
                    (None, _) => format!("{v} DROPPED"),
                };
                let color = if victim == self.your_slot {
                    theme::ERROR
                } else {
                    theme::slot_color(killer.unwrap_or(victim))
                };
                self.log(line, color);
            }
            EventKind::Stun { by, victim } => {
                self.log(
                    format!("{} --[THROW]--> {} STUNNED", hex(by), hex(victim)),
                    theme::DIM,
                );
            }
            EventKind::Shot { x0, y0, x1, y1 } => {
                self.effects.push((now, Effect::Tracer { x0, y0, x1, y1 }));
            }
            EventKind::Explosion { x, y, radius } => {
                self.effects.push((now, Effect::Blast { x, y, radius }));
            }
        }
    }

    /// Advance the fixed 60 Hz input clock; returns the Input packets to send.
    pub fn update(&mut self, token: u64, bot: bool) -> Vec<ClientPacket> {
        // Clamp so a long frame (window drag) doesn't burst-send hundreds of inputs.
        self.acc = (self.acc + get_frame_time()).min(0.25);
        let b = if bot {
            let slot = self.your_slot;
            self.latest
                .as_ref()
                .map_or(0, |s| self.brain.think(s, slot, get_time()))
        } else {
            read_buttons()
        };
        // Demo toggles: compare with and without each technique.
        if is_key_pressed(KeyCode::F1) {
            self.predict_on = !self.predict_on;
            if !self.predict_on {
                self.pred.sim = None; // restarts from the next snapshot when re-enabled
            }
        }
        if is_key_pressed(KeyCode::F2) {
            self.interp_on = !self.interp_on;
        }
        let now = get_time();
        let latest_tick = self.latest.as_ref().map_or(0, |s| s.tick);
        // The tick we actually display for others (for server lag compensation).
        let view_tick = match self.interp.render_tick(now) {
            Some(t) if self.interp_on => (t.max(0.0) as u32).min(latest_tick),
            _ => latest_tick,
        };
        self.last_buttons = b;
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
            // Prediction: apply this input locally now, don't wait for the server.
            self.pred.on_input(self.seq, b, &self.map);
            out.push(ClientPacket::Input {
                token,
                frames: self.history.iter().copied().collect(),
                last_snapshot_tick: view_tick,
                events_ack: self.events_ack,
            });
        }
        self.pred.decay(get_frame_time());
        self.view = self.build_view(now);
        self.effects.retain(|(t, e)| {
            now - t
                < match e {
                    Effect::Tracer { .. } => TRACER_SECONDS,
                    Effect::Blast { .. } => BLAST_SECONDS,
                }
        });
        self.drops.retain(|d| now - d.0 < DROP_SECONDS);
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
        let on = |b: bool| if b { "ON" } else { "off" };
        format!(
            "snapshots {}/s  loss {:.1}%  stale {}  inputs in flight {}   |   [F1] prediction {} (last fix {:.1} px)   [F2] interpolation {}",
            self.rate_window.len(),
            loss.max(0.0),
            self.snaps_stale,
            unacked,
            on(self.predict_on),
            self.pred.last_error,
            on(self.interp_on),
        )
    }

    /// The world to draw: others interpolated in the past (or the raw latest
    /// snapshot), our own packet at its predicted position.
    fn build_view(&self, now: f64) -> Option<Snapshot> {
        let latest = self.latest.as_ref()?;
        let mut v = if self.interp_on {
            self.interp.sample(now).unwrap_or_else(|| latest.clone())
        } else {
            latest.clone()
        };
        // Our own packet always comes from the newest snapshot (weapon, alive)
        // and, when predicting, takes the predicted position and pose.
        let Some(mut me) = latest
            .players
            .iter()
            .find(|p| p.slot == self.your_slot)
            .copied()
        else {
            return Some(v);
        };
        if let (Some(sim), Some((x, y))) = (self.pred.sim, self.pred.draw_pos()) {
            (me.x, me.y, me.vx, me.vy) = (x, y, sim.vx, sim.vy);
            let set = |f: &mut u8, bit: u8, on: bool| {
                if on {
                    *f |= bit
                } else {
                    *f &= !bit
                }
            };
            set(&mut me.flags, flags::FACING_RIGHT, sim.facing_right);
            set(&mut me.flags, flags::GROUNDED, sim.grounded);
            let aiming = self.last_buttons & buttons::AIM_UP != 0 && sim.stun == 0;
            set(&mut me.flags, flags::AIMING_UP, aiming);
        }
        match v.players.iter_mut().find(|p| p.slot == self.your_slot) {
            Some(p) => *p = me,
            None => v.players.push(me),
        }
        Some(v)
    }

    // ------------------------------------------------------------- render

    fn origin(&self) -> Vec2 {
        vec2(
            (VW - self.map.pixel_width() * SCALE) / 2.0,
            (VH - self.map.pixel_height() * SCALE) / 2.0,
        )
    }

    pub fn draw(&self) {
        let o = self.origin();
        let w2s = |x: f32, y: f32| vec2(o.x + x * SCALE, o.y + y * SCALE);
        self.draw_map(o);
        let (Some(hud), Some(s)) = (&self.latest, &self.view) else {
            text_centered(
                "waiting for first snapshot over UDP...",
                VW / 2.0,
                VH / 2.0,
                28.0,
                theme::DIM,
            );
            return;
        };
        let now = get_time();

        // Firewalls: flicker, fade during the last second.
        for f in &s.firewalls {
            let p = w2s(f.x, f.y);
            let fade = (f.ms_left as f32 / 1000.0).min(1.0);
            let flick = 0.75 + 0.25 * ((now * 20.0 + f.id as f64).sin() as f32);
            let c = Color::new(1.0, 0.35 * flick, 0.1, 0.85 * fade);
            draw_rectangle(p.x, p.y, FIREWALL_W * SCALE, FIREWALL_H * SCALE, c);
            for k in 0..5 {
                let yy = p.y + k as f32 * FIREWALL_H * SCALE / 5.0;
                draw_line(
                    p.x,
                    yy,
                    p.x + FIREWALL_W * SCALE,
                    yy,
                    1.0,
                    Color::new(0.4, 0.05, 0.0, fade),
                );
            }
        }

        // Items: weapons on the floor (bobbing) or flying after a throw.
        for it in &s.items {
            let bob = if it.flying {
                0.0
            } else {
                ((now * 3.0 + it.id as f64).sin() as f32) * 2.0
            };
            let p = w2s(it.x + ITEM_W / 2.0 - 4.0, it.y + ITEM_H / 2.0);
            let empty = it.weapon.max_ammo() > 0 && it.ammo == 0;
            let alpha = if empty { 0.4 } else { 1.0 };
            if !self.draw_weapon_sprite(it.weapon, vec2(p.x, p.y + bob), true, false, alpha) {
                draw_weapon(it.weapon, p.x, p.y + bob, true, false, alpha);
            }
        }

        for p in &s.players {
            if p.flags & flags::ALIVE != 0 {
                self.draw_player(p, w2s(p.x, p.y), now);
            }
        }

        for pr in &s.projectiles {
            let p = w2s(pr.x, pr.y);
            match pr.kind {
                ProjectileKind::Pellet => {
                    draw_rectangle(p.x - 2.0, p.y - 2.0, 4.0, 4.0, theme::WARN)
                }
                ProjectileKind::Bomb | ProjectileKind::Shard => {
                    let r = if pr.kind == ProjectileKind::Bomb {
                        4.0
                    } else {
                        3.0
                    } * SCALE;
                    let blink = (now * 12.0) as i64 % 2 == 0;
                    if pr.kind == ProjectileKind::Bomb {
                        let c = if blink {
                            WHITE
                        } else {
                            Color::new(0.75, 0.6, 0.6, 1.0)
                        };
                        sprites::draw_vector(
                            &self.art.fork_bomb,
                            p,
                            vec2(0.5, 0.5),
                            SCALE,
                            0.0,
                            false,
                            c,
                        );
                        continue;
                    }
                    let c = if blink {
                        Color::new(1.0, 0.3, 0.3, 1.0)
                    } else {
                        Color::new(0.7, 0.1, 0.15, 1.0)
                    };
                    draw_circle(p.x, p.y, r, c);
                }
            }
        }

        for (t, e) in &self.effects {
            let age = (now - t) as f32;
            match *e {
                Effect::Tracer { x0, y0, x1, y1 } => {
                    let a = 1.0 - age / TRACER_SECONDS as f32;
                    let (p0, p1) = (w2s(x0, y0), w2s(x1, y1));
                    draw_line(
                        p0.x,
                        p0.y,
                        p1.x,
                        p1.y,
                        3.0,
                        Color::new(1.0, 1.0, 0.8, a.max(0.0)),
                    );
                    if age < 0.07 {
                        let angle = (p1.y - p0.y).atan2(p1.x - p0.x);
                        sprites::draw_vector(
                            &self.art.muzzle,
                            p0,
                            vec2(0.05, 0.5),
                            SCALE * 0.8,
                            angle,
                            false,
                            WHITE,
                        );
                    }
                }
                Effect::Blast { x, y, radius } => {
                    let k = age / BLAST_SECONDS as f32;
                    let p = w2s(x, y);
                    let r = radius * SCALE * (0.5 + 0.5 * k.min(1.0));
                    draw_circle(
                        p.x,
                        p.y,
                        r,
                        Color::new(1.0, 0.55, 0.1, (0.7 * (1.0 - k)).max(0.0)),
                    );
                    draw_circle_lines(
                        p.x,
                        p.y,
                        r,
                        2.0,
                        Color::new(1.0, 0.9, 0.5, (1.0 - k).max(0.0)),
                    );
                }
            }
        }

        // PACKET DROPPED: a puff in the victim's colour where it died.
        for &(t, x, y, slot) in &self.drops {
            let age = (now - t) as f32;
            let k = age / DROP_SECONDS as f32;
            let frame = ((age / 0.12) as usize).min(2);
            let c = theme::slot_color(slot);
            let a = if k < 0.6 { 1.0 } else { (1.0 - k) / 0.4 };
            let p = w2s(x, y);
            sprites::draw_vector(
                &self.art.drops[frame],
                p,
                vec2(0.5, 0.6),
                SCALE,
                0.0,
                false,
                with_alpha(c, a),
            );
            text_centered(
                "PACKET DROPPED",
                p.x,
                p.y - 34.0 - age * 20.0,
                18.0,
                with_alpha(theme::ERROR, a),
            );
        }

        self.draw_hud(hud, s);
    }

    /// Draw a held or dropped weapon with pack art; false if it has none
    /// (Firewall and Checksum Blade keep their procedural look).
    fn draw_weapon_sprite(&self, w: Weapon, grip: Vec2, right: bool, up: bool, alpha: f32) -> bool {
        let (tex, anchor) = match w {
            Weapon::PingPistol => (&self.art.pistol, vec2(0.25, 0.55)),
            Weapon::BroadcastShotgun => (&self.art.shotgun, vec2(0.3, 0.55)),
            Weapon::ForkBomb => (&self.art.fork_bomb, vec2(0.5, 0.5)),
            _ => return false,
        };
        let c = with_alpha(WHITE, alpha);
        if up && w != Weapon::ForkBomb {
            sprites::draw_vector(
                tex,
                grip,
                anchor,
                SCALE,
                -std::f32::consts::FRAC_PI_2,
                false,
                c,
            );
        } else {
            sprites::draw_vector(tex, grip, anchor, SCALE, 0.0, !right, c);
        }
        true
    }

    /// Seeker character for this player; returns false when the art isn't installed.
    fn draw_seeker(&self, p: &PlayerState, x: f32, y: f32, now: f64) -> bool {
        let Some(sk) = self.art.seeker(p.slot) else {
            return false;
        };
        let right = p.flags & flags::FACING_RIGHT != 0;
        let up = p.flags & flags::AIMING_UP != 0;
        let feet = vec2(x + PLAYER_W * SCALE / 2.0, y + PLAYER_H * SCALE);
        // De-sync idle/run cycles between players.
        let t = now + p.slot as f64 * 0.37;
        let swing = self
            .swing_start
            .get(p.slot as usize)
            .copied()
            .flatten()
            .filter(|_| p.flags & flags::SWINGING != 0);
        if let Some(start) = swing {
            let f = (((now - start) / SWING_SECONDS) * 8.0) as usize;
            sk.attack_body.draw(f, feet, right, SCALE, WHITE);
            // The pack's slash is ~62 px long; shrink it to the real hit reach.
            let k = (PLAYER_W / 2.0 + BLADE_REACH) / 62.0 * SCALE;
            if up {
                let s = &sk.attack_slash;
                let size = vec2(s.fw, s.fh) * k;
                let pivot = feet - vec2(0.0, PLAYER_H * SCALE / 2.0);
                let pos = pivot - vec2(s.pivot.x, s.pivot.y - 8.0) * k;
                draw_texture_ex(
                    &s.tex,
                    pos.x,
                    pos.y,
                    WHITE,
                    DrawTextureParams {
                        dest_size: Some(size),
                        source: Some(Rect::new(f.min(7) as f32 * s.fw, 0.0, s.fw, s.fh)),
                        rotation: -std::f32::consts::FRAC_PI_2,
                        pivot: Some(pivot),
                        ..Default::default()
                    },
                );
            } else {
                sk.attack_slash.draw(f, feet, right, k, WHITE);
            }
            return true;
        }
        if p.flags & flags::STUNNED != 0 {
            sk.hurt
                .draw(((t / 0.06) as usize) % sk.hurt.n, feet, right, SCALE, WHITE);
        } else if p.flags & flags::GROUNDED == 0 {
            let f = if p.vy < -40.0 {
                1 + (((p.vy + 340.0) / 300.0).clamp(0.0, 1.0) * 11.0) as usize
            } else if p.vy < 60.0 {
                13 + (p.vy > 10.0) as usize
            } else {
                15 + ((t / 0.04) as usize) % 11
            };
            sk.jump.draw(f, feet, right, SCALE, WHITE);
        } else if p.vx.abs() > 20.0 {
            sk.run
                .draw(((t / 0.07) as usize) % sk.run.n, feet, right, SCALE, WHITE);
        } else {
            sk.idle
                .draw(((t / 0.1) as usize) % sk.idle.n, feet, right, SCALE, WHITE);
        }
        true
    }

    fn draw_player(&self, p: &PlayerState, pos: Vec2, now: f64) {
        let (x, y) = (pos.x, pos.y);
        let (w, h) = (PLAYER_W * SCALE, PLAYER_H * SCALE);
        let color = theme::slot_color(p.slot);
        let right = p.flags & flags::FACING_RIGHT != 0;
        let up = p.flags & flags::AIMING_UP != 0;

        let seeker = self.draw_seeker(p, x, y, now);
        if !seeker {
            // Body + darker "header" band: it's a packet.
            draw_rectangle(x, y, w, h, color);
            draw_rectangle(
                x,
                y,
                w,
                4.0 * SCALE,
                Color::new(color.r * 0.55, color.g * 0.55, color.b * 0.55, 1.0),
            );
            // Eyes look where the packet faces (or up when aiming up).
            let ex = if right { x + w - 10.0 } else { x + 2.0 };
            let (px, py) = if up {
                (1.0, 0.0)
            } else if right {
                (2.0, 2.0)
            } else {
                (0.0, 2.0)
            };
            for dx in [0.0, 5.0] {
                draw_rectangle(ex + dx, y + 10.0, 4.0, 6.0, WHITE);
                draw_rectangle(ex + dx + px, y + 10.0 + py, 2.0, 3.0, BLACK);
            }
        }

        if let Some(wpn) = p.weapon {
            let gx = if up {
                x + w / 2.0 - 2.0
            } else if right {
                x + w - 4.0
            } else {
                x + 4.0
            };
            let gy = if up { y + 4.0 } else { y + h / 2.0 + 2.0 };
            let swinging = p.flags & flags::SWINGING != 0;
            // While swinging, Seeker's attack animation shows the blade itself.
            if !(seeker && swinging && wpn == Weapon::ChecksumBlade)
                && !self.draw_weapon_sprite(wpn, vec2(gx, gy), right, up, 1.0)
            {
                draw_weapon(wpn, gx, gy, right, up, 1.0);
            }
            if wpn == Weapon::ChecksumBlade && swinging && !seeker {
                let reach = 20.0 * SCALE;
                let c = Color::new(0.4, 1.0, 0.9, 0.35);
                if up {
                    draw_rectangle(
                        x + w / 2.0 - 10.0 * SCALE,
                        y - reach,
                        20.0 * SCALE,
                        reach,
                        c,
                    );
                } else if right {
                    draw_rectangle(x + w, y - 3.0 * SCALE, reach, h + 6.0 * SCALE, c);
                } else {
                    draw_rectangle(x - reach, y - 3.0 * SCALE, reach, h + 6.0 * SCALE, c);
                }
            }
        }

        let mut label_y = y - 6.0 - if seeker { SEEKER_EXTRA_H * SCALE } else { 0.0 };
        if p.flags & flags::STUNNED != 0 && (now * 8.0) as i64 % 2 == 0 {
            text_centered("ERR", x + w / 2.0, label_y - 18.0, 18.0, theme::WARN);
            label_y -= 34.0;
        }
        let lc = if p.slot == self.your_slot {
            theme::ACCENT
        } else {
            theme::TEXT
        };
        text_centered(&hex(p.slot), x + w / 2.0, label_y, 18.0, lc);
        if p.slot == self.your_slot {
            let cx = x + w / 2.0;
            let ty = label_y - 22.0;
            draw_triangle(
                vec2(cx - 5.0, ty),
                vec2(cx + 5.0, ty),
                vec2(cx, ty + 6.0),
                theme::ACCENT,
            );
        }
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
        // Weapon spawner pads.
        for &(cx, fy) in &self.map.weapon_spawns {
            let p = vec2(o.x + cx * SCALE, o.y + fy * SCALE);
            draw_rectangle(
                p.x - 8.0 * SCALE,
                p.y - 2.0,
                16.0 * SCALE,
                2.0,
                Color::new(0.3, 0.5, 0.8, 0.8),
            );
        }
    }

    /// `s`: newest snapshot (scores, phase). `view`: what's drawn (positions).
    fn draw_hud(&self, s: &Snapshot, view: &Snapshot) {
        let cx = VW / 2.0;

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

        let me = view.players.iter().find(|p| p.slot == self.your_slot);
        let me_alive = me.is_some_and(|p| p.flags & flags::ALIVE != 0);
        if !me_alive && matches!(s.phase, RoundPhase::Playing { .. }) {
            text_centered("PACKET DROPPED", cx, 200.0, 48.0, theme::ERROR);
            text_centered("spectating until next round", cx, 230.0, 20.0, theme::DIM);
        }

        // Weapon + ammo, bottom right.
        if let Some(me) = me.filter(|_| me_alive) {
            let line = match me.weapon {
                Some(w) if w.max_ammo() == 0 => format!("{}  [unlimited]", w.name()),
                Some(w) if me.ammo == 0 => format!("{}  EMPTY - K to throw", w.name()),
                Some(w) => format!(
                    "{}  {}{}",
                    w.name(),
                    "|".repeat(me.ammo as usize),
                    ".".repeat((w.max_ammo() - me.ammo) as usize)
                ),
                None => {
                    let near = view.items.iter().any(|it| {
                        let dx = it.x + ITEM_W / 2.0 - (me.x + PLAYER_W / 2.0);
                        let dy = it.y + ITEM_H / 2.0 - (me.y + PLAYER_H / 2.0);
                        (dx * dx + dy * dy).sqrt() <= PICKUP_HINT_RANGE
                    });
                    if near {
                        "K to pick up".to_string()
                    } else {
                        "unarmed".to_string()
                    }
                }
            };
            let w = text_width(&line, 22.0);
            text(&line, VW - w - 40.0, VH - 38.0, 22.0, theme::TEXT);
        }

        // Kill feed, top right.
        let now = get_time();
        let mut y = 70.0;
        for (t, line, color) in &self.feed {
            if now - t > FEED_SECONDS {
                continue;
            }
            let w = text_width(line, 20.0);
            text(line, VW - w - 40.0, y, 20.0, *color);
            y += 24.0;
        }

        text(&self.net_stats(), 40.0, VH - 34.0, 16.0, theme::DIM);
        text(CONTROLS, 40.0, VH - 50.0, 16.0, theme::DIM);
    }
}
