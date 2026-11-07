//! Platformer simulation shared by the server (authoritative) and the client
//! (prediction, milestone 4). It is deterministic given the same inputs:
//! fixed timestep, no randomness, no wall-clock time.
//!
//! World units are pixels; the map is a grid of `TILE`-sized cells.
//! Positions are the top-left corner of an axis-aligned bounding box.

use serde::{Deserialize, Serialize};

pub const TICK_RATE: u32 = 60;
pub const DT: f32 = 1.0 / TICK_RATE as f32;
/// Snapshots go out every N ticks (60 / 2 = 30 Hz).
pub const SNAPSHOT_EVERY: u32 = 2;

pub const TILE: f32 = 16.0;
pub const PLAYER_W: f32 = 12.0;
pub const PLAYER_H: f32 = 14.0;

const RUN_SPEED: f32 = 150.0;
const GROUND_ACCEL: f32 = 1600.0;
const AIR_ACCEL: f32 = 1000.0;
const GRAVITY: f32 = 980.0;
const JUMP_VEL: f32 = -340.0;
/// Releasing jump early clamps upward speed to this: short hop vs full jump.
const JUMP_CUT_VEL: f32 = -140.0;
const MAX_FALL: f32 = 480.0;
pub const STOMP_BOUNCE: f32 = -260.0;
/// Grace ticks to still jump after walking off a ledge.
const COYOTE_TICKS: u8 = 6;
/// A jump pressed slightly before landing still fires on landing.
const JUMP_BUFFER_TICKS: u8 = 6;
const DROP_THROUGH_TICKS: u8 = 10;

/// Input button bitfield (`InputFrame::buttons`).
pub mod buttons {
    pub const LEFT: u16 = 1 << 0;
    pub const RIGHT: u16 = 1 << 1;
    pub const JUMP: u16 = 1 << 2;
    pub const DOWN: u16 = 1 << 3;
    pub const AIM_UP: u16 = 1 << 4;
    pub const SHOOT: u16 = 1 << 5;
    /// Pick up the nearest weapon, or throw the held one.
    pub const INTERACT: u16 = 1 << 6;
}

// ------------------------------------------------------------------ maps

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tile {
    Empty,
    Solid,
    /// One-way: solid only when landing on it from above; DOWN+JUMP drops through.
    Platform,
}

#[derive(Debug, Clone)]
pub struct Map {
    pub name: String,
    pub width: usize,
    pub height: usize,
    tiles: Vec<Tile>,
    /// Player spawn positions (top-left, pixels).
    pub spawns: Vec<(f32, f32)>,
    /// Weapon spawner positions: (center x, floor y) of the cell.
    pub weapon_spawns: Vec<(f32, f32)>,
}

const MAP_ROUTER: &str = include_str!("../../../assets/maps/router.txt");

/// Maps are compiled into both binaries, so client and server always agree.
pub fn builtin_map(name: &str) -> Option<Map> {
    let src = match name {
        "router" => MAP_ROUTER,
        _ => return None,
    };
    Map::parse(name, src).ok()
}

impl Map {
    pub fn parse(name: &str, src: &str) -> Result<Map, String> {
        let lines: Vec<&str> = src
            .lines()
            .filter(|l| !l.starts_with("//"))
            .map(str::trim_end)
            .filter(|l| !l.is_empty())
            .collect();
        let height = lines.len();
        let width = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        if width == 0 || height == 0 {
            return Err("empty map".into());
        }
        let mut tiles = Vec::with_capacity(width * height);
        let mut spawns = Vec::new();
        let mut weapon_spawns = Vec::new();
        for (y, line) in lines.iter().enumerate() {
            let chars: Vec<char> = line.chars().collect();
            for x in 0..width {
                // Entities stand on the bottom of their cell, centered.
                let px = x as f32 * TILE + (TILE - PLAYER_W) / 2.0;
                let py = (y + 1) as f32 * TILE - PLAYER_H;
                tiles.push(match chars.get(x).copied().unwrap_or('.') {
                    '#' => Tile::Solid,
                    '=' => Tile::Platform,
                    'S' => {
                        spawns.push((px, py));
                        Tile::Empty
                    }
                    'W' => {
                        weapon_spawns.push(((x as f32 + 0.5) * TILE, (y + 1) as f32 * TILE));
                        Tile::Empty
                    }
                    _ => Tile::Empty,
                });
            }
        }
        if spawns.len() < 3 {
            return Err(format!(
                "map '{name}' needs at least 3 spawns, has {}",
                spawns.len()
            ));
        }
        Ok(Map {
            name: name.to_string(),
            width,
            height,
            tiles,
            spawns,
            weapon_spawns,
        })
    }

    /// Outside the map: left/right/top behave as walls, below is a bottomless pit.
    pub fn tile(&self, tx: i32, ty: i32) -> Tile {
        if tx < 0 || tx >= self.width as i32 || ty < 0 {
            return Tile::Solid;
        }
        if ty >= self.height as i32 {
            return Tile::Empty;
        }
        self.tiles[ty as usize * self.width + tx as usize]
    }

    pub fn pixel_width(&self) -> f32 {
        self.width as f32 * TILE
    }

    pub fn pixel_height(&self) -> f32 {
        self.height as f32 * TILE
    }
}

// --------------------------------------------------------------- players

/// Full physical state of one packet. Everything `step` reads is in here,
/// which is what makes client-side re-simulation possible.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct PlayerSim {
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    pub facing_right: bool,
    pub grounded: bool,
    pub coyote: u8,
    pub jump_buffer: u8,
    pub drop_timer: u8,
    pub prev_buttons: u16,
    /// Ticks left stunned (hit by a thrown item): inputs are ignored.
    pub stun: u8,
}

fn approach(v: f32, target: f32, delta: f32) -> f32 {
    if v < target {
        (v + delta).min(target)
    } else {
        (v - delta).max(target)
    }
}

/// Inclusive tile index range covered by the span `[a, a + len)`.
fn tile_span(a: f32, len: f32) -> std::ops::RangeInclusive<i32> {
    (a / TILE).floor() as i32..=((a + len - 0.001) / TILE).floor() as i32
}

#[allow(clippy::too_many_arguments)]
pub fn overlap(ax: f32, ay: f32, aw: f32, ah: f32, bx: f32, by: f32, bw: f32, bh: f32) -> bool {
    ax < bx + bw && bx < ax + aw && ay < by + bh && by < ay + ah
}

impl PlayerSim {
    pub fn spawn_at(x: f32, y: f32, facing_right: bool) -> Self {
        PlayerSim {
            x,
            y,
            facing_right,
            ..Default::default()
        }
    }

    pub fn fell_out(&self, map: &Map) -> bool {
        self.y > map.pixel_height() + 64.0
    }

    fn standing_on_platform_only(&self, map: &Map) -> bool {
        let ty = ((self.y + PLAYER_H) / TILE).floor() as i32;
        let below: Vec<Tile> = tile_span(self.x, PLAYER_W)
            .map(|tx| map.tile(tx, ty))
            .collect();
        self.grounded && below.contains(&Tile::Platform) && !below.contains(&Tile::Solid)
    }

    /// Advance one tick with the given buttons held.
    pub fn step(&mut self, map: &Map, b: u16) {
        use buttons::*;
        let b = if self.stun > 0 {
            self.stun -= 1;
            0
        } else {
            b
        };
        let held = |m: u16| b & m != 0;
        let pressed = |m: u16| b & m != 0 && self.prev_buttons & m == 0;

        // Horizontal intent.
        let dir = held(RIGHT) as i32 - held(LEFT) as i32;
        if dir != 0 {
            self.facing_right = dir > 0;
        }
        let accel = if self.grounded {
            GROUND_ACCEL
        } else {
            AIR_ACCEL
        };
        self.vx = approach(self.vx, dir as f32 * RUN_SPEED, accel * DT);

        // Jump: coyote time + input buffering make it feel forgiving.
        if pressed(JUMP) {
            self.jump_buffer = JUMP_BUFFER_TICKS;
        } else {
            self.jump_buffer = self.jump_buffer.saturating_sub(1);
        }
        if self.grounded {
            self.coyote = COYOTE_TICKS;
        } else {
            self.coyote = self.coyote.saturating_sub(1);
        }
        if held(DOWN) && pressed(JUMP) && self.standing_on_platform_only(map) {
            self.drop_timer = DROP_THROUGH_TICKS;
            self.jump_buffer = 0;
            self.grounded = false;
        } else if self.jump_buffer > 0 && self.coyote > 0 {
            self.vy = JUMP_VEL;
            self.jump_buffer = 0;
            self.coyote = 0;
            self.grounded = false;
        }
        if !held(JUMP) && self.vy < JUMP_CUT_VEL {
            self.vy = JUMP_CUT_VEL;
        }
        self.vy = (self.vy + GRAVITY * DT).min(MAX_FALL);
        self.drop_timer = self.drop_timer.saturating_sub(1);

        // Move X, resolve against solid tiles.
        self.x += self.vx * DT;
        let rows = tile_span(self.y, PLAYER_H);
        if self.vx > 0.0 {
            let tx = ((self.x + PLAYER_W - 0.001) / TILE).floor() as i32;
            if rows.clone().any(|ty| map.tile(tx, ty) == Tile::Solid) {
                self.x = tx as f32 * TILE - PLAYER_W;
                self.vx = 0.0;
            }
        } else if self.vx < 0.0 {
            let tx = (self.x / TILE).floor() as i32;
            if rows.clone().any(|ty| map.tile(tx, ty) == Tile::Solid) {
                self.x = (tx + 1) as f32 * TILE;
                self.vx = 0.0;
            }
        }

        // Move Y, resolve against solids and one-way platforms.
        let prev_bottom = self.y + PLAYER_H;
        self.y += self.vy * DT;
        self.grounded = false;
        let cols = tile_span(self.x, PLAYER_W);
        if self.vy > 0.0 {
            let ty = ((self.y + PLAYER_H - 0.001) / TILE).floor() as i32;
            let top = ty as f32 * TILE;
            let land = cols.clone().any(|tx| match map.tile(tx, ty) {
                Tile::Solid => true,
                Tile::Platform => self.drop_timer == 0 && prev_bottom <= top + 0.5,
                Tile::Empty => false,
            });
            if land {
                self.y = top - PLAYER_H;
                self.vy = 0.0;
                self.grounded = true;
            }
        } else if self.vy < 0.0 {
            let ty = (self.y / TILE).floor() as i32;
            if cols.clone().any(|tx| map.tile(tx, ty) == Tile::Solid) {
                self.y = (ty + 1) as f32 * TILE;
                self.vy = 0.0;
            }
        }

        self.prev_buttons = b;
    }
}

#[cfg(test)]
mod tests {
    use super::buttons::*;
    use super::*;

    fn router() -> Map {
        builtin_map("router").expect("router map parses")
    }

    fn settle(map: &Map, p: &mut PlayerSim) {
        for _ in 0..60 {
            p.step(map, 0);
        }
    }

    #[test]
    fn map_parses_with_spawns() {
        let m = router();
        assert_eq!((m.width, m.height), (40, 22));
        assert!(m.spawns.len() >= 3);
    }

    #[test]
    fn spawns_are_standing_on_ground() {
        let m = router();
        for &(x, y) in &m.spawns {
            let mut p = PlayerSim::spawn_at(x, y, true);
            settle(&m, &mut p);
            assert!(p.grounded, "spawn ({x},{y}) is not on ground");
            assert!((p.y - y).abs() < 0.01, "spawn ({x},{y}) fell to {}", p.y);
        }
    }

    #[test]
    fn walks_and_jumps() {
        let m = router();
        let (x, y) = m.spawns[0];
        let mut p = PlayerSim::spawn_at(x, y, true);
        settle(&m, &mut p);
        for _ in 0..10 {
            p.step(&m, RIGHT);
        }
        assert!(p.x > x + 5.0);
        p.step(&m, JUMP);
        assert!(p.vy < 0.0 && !p.grounded);
        let mut peak = p.y;
        for _ in 0..20 {
            p.step(&m, JUMP);
            peak = peak.min(p.y);
        }
        assert!(y - peak > 40.0, "jump too low: {}", y - peak);
    }

    #[test]
    fn walls_stop_movement() {
        let m = router();
        let mut p = PlayerSim::spawn_at(m.spawns[0].0, m.spawns[0].1, false);
        for _ in 0..300 {
            p.step(&m, LEFT);
        }
        assert!(p.x >= TILE - 0.01, "walked into left wall: x={}", p.x);
    }

    #[test]
    fn drops_through_platform() {
        let m = router();
        let (x, y) = m.spawns[0]; // spawn 0 stands on a one-way platform
        let mut p = PlayerSim::spawn_at(x, y, true);
        settle(&m, &mut p);
        p.step(&m, DOWN | JUMP);
        for _ in 0..20 {
            p.step(&m, DOWN);
        }
        assert!(p.y > y + TILE, "did not drop through: y={} start={y}", p.y);
    }

    #[test]
    fn deterministic() {
        let m = router();
        let script = |p: &mut PlayerSim| {
            for i in 0..240u32 {
                let b = match i % 50 {
                    0..=20 => RIGHT,
                    21 => RIGHT | JUMP,
                    22..=30 => JUMP,
                    _ => LEFT,
                };
                p.step(&m, b);
            }
        };
        let mut a = PlayerSim::spawn_at(m.spawns[2].0, m.spawns[2].1, true);
        let mut b = a;
        script(&mut a);
        script(&mut b);
        assert_eq!(a, b);
    }
}
