//! Weapons, items, projectiles and firewalls. All server-authoritative:
//! clients only send "SHOOT/INTERACT pressed" and draw what snapshots say.

use gg_shared::game::{buttons, Tile, DT, PLAYER_H, PLAYER_W, TILE};
use gg_shared::udp::{EventKind, KillCause, ProjectileKind};
use gg_shared::weapons::{Weapon, ALL_WEAPONS, FIREWALL_H, FIREWALL_W, ITEM_H, ITEM_W};

use super::{secs, Game};

const PICKUP_RANGE: f32 = 18.0;
const SPAWNER_RESPAWN_TICKS: u32 = secs(8.0);

const PISTOL_RANGE: f32 = 420.0;
const PISTOL_KNOCKBACK: f32 = 60.0;

const PELLETS: usize = 5;
const PELLET_SPEED: f32 = 520.0;
/// Total cone angle (radians) the pellets spread over.
const PELLET_SPREAD: f32 = 0.45;
const PELLET_TTL: u16 = 16;
const SHOTGUN_RECOIL: f32 = 260.0;

const BOMB_FUSE: u16 = secs(1.8) as u16;
const BOMB_RADIUS: f32 = 34.0;
const SHARD_FUSE: u16 = 24;
const SHARD_RADIUS: f32 = 22.0;
const BOMB_GRAVITY: f32 = 700.0;

const FIREWALL_TICKS: u32 = secs(5.0);

const BLADE_SWING_TICKS: u8 = 16;
/// The first N ticks of a swing hit and reflect.
const BLADE_ACTIVE_TICKS: u8 = 8;
const BLADE_REACH: f32 = 20.0;

const THROW_SPEED: f32 = 320.0;
const THROW_GRACE_TICKS: u8 = 12;
/// A thrown item stuns only while moving at least this fast.
const THROW_HIT_SPEED: f32 = 90.0;
const STUN_TICKS: u8 = 36;
const ITEM_GRAVITY: f32 = 900.0;

#[derive(Debug, Clone, Copy)]
pub(super) struct Held {
    pub weapon: Weapon,
    pub ammo: u8,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Item {
    pub id: u16,
    pub weapon: Weapon,
    pub ammo: u8,
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    /// Player index that threw it, while it can still stun.
    pub thrown_by: Option<usize>,
    /// Ticks during which the thrower can't catch it again.
    pub grace: u8,
    pub spawner: Option<usize>,
    pub grounded: bool,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Projectile {
    pub id: u16,
    pub kind: ProjectileKind,
    pub owner: usize,
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    pub ttl: u16,
    pub reflected: bool,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Firewall {
    pub id: u16,
    pub x: f32,
    pub y: f32,
    pub ticks: u32,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Spawner {
    /// Center x / floor y of the spawner cell.
    pub x: f32,
    pub y: f32,
    pub item: Option<u16>,
    pub respawn: u32,
}

enum Hit {
    Nothing,
    Wall,
    Firewall,
    Player(usize),
}

fn inside(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32) -> bool {
    px >= x && px < x + w && py >= y && py < y + h
}

impl Game {
    fn next_id(&mut self) -> u16 {
        self.next_entity_id = self.next_entity_id.wrapping_add(1);
        self.next_entity_id
    }

    /// xorshift64: deterministic, dependency-free; only picks spawner weapons.
    fn rand(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        x
    }

    fn solid_at(&self, x: f32, y: f32) -> bool {
        self.map
            .tile((x / TILE).floor() as i32, (y / TILE).floor() as i32)
            == Tile::Solid
    }

    fn firewall_at(&self, x: f32, y: f32) -> bool {
        self.firewalls
            .iter()
            .any(|f| inside(x, y, f.x, f.y, FIREWALL_W, FIREWALL_H))
    }

    /// Something a falling point lands on when moving from `y0` to `y1`.
    fn floor_between(&self, x: f32, y0: f32, y1: f32) -> Option<f32> {
        let tx = (x / TILE).floor() as i32;
        let ty = (y1 / TILE).floor() as i32;
        let top = ty as f32 * TILE;
        match self.map.tile(tx, ty) {
            Tile::Solid => Some(top),
            Tile::Platform if y0 <= top + 0.5 => Some(top),
            _ => None,
        }
    }

    fn center(&self, i: usize) -> (f32, f32) {
        let s = &self.players[i].sim;
        (s.x + PLAYER_W / 2.0, s.y + PLAYER_H / 2.0)
    }

    fn aim(&self, i: usize) -> (f32, f32) {
        let p = &self.players[i];
        if p.aim_up {
            (0.0, -1.0)
        } else if p.sim.facing_right {
            (1.0, 0.0)
        } else {
            (-1.0, 0.0)
        }
    }

    fn muzzle(&self, i: usize) -> (f32, f32) {
        let (cx, cy) = self.center(i);
        let (dx, dy) = self.aim(i);
        (
            cx + dx * (PLAYER_W / 2.0 + 2.0),
            cy - 2.0 + dy * (PLAYER_H / 2.0),
        )
    }

    /// Checksum Blade in its active window: hits and reflects.
    pub(super) fn reflecting(&self, i: usize) -> bool {
        let p = &self.players[i];
        p.alive
            && p.held.is_some_and(|h| h.weapon == Weapon::ChecksumBlade)
            && p.swing > BLADE_SWING_TICKS - BLADE_ACTIVE_TICKS
    }

    // ------------------------------------------------------------ rounds

    /// Clear the arena and restock every spawner (start of a round).
    pub(super) fn reset_arena(&mut self) {
        self.items.clear();
        self.projectiles.clear();
        self.firewalls.clear();
        for s in 0..self.spawners.len() {
            self.spawners[s].item = None;
            self.restock(s);
        }
    }

    fn restock(&mut self, s: usize) {
        let weapon = ALL_WEAPONS[(self.rand() % ALL_WEAPONS.len() as u64) as usize];
        let id = self.next_id();
        let sp = self.spawners[s];
        self.items.push(Item {
            id,
            weapon,
            ammo: weapon.max_ammo(),
            x: sp.x - ITEM_W / 2.0,
            y: sp.y - ITEM_H,
            vx: 0.0,
            vy: 0.0,
            thrown_by: None,
            grace: 0,
            spawner: Some(s),
            grounded: true,
        });
        self.spawners[s].item = Some(id);
    }

    // ------------------------------------------------------------- kills

    /// The single place a packet dies. Idempotent within a tick.
    pub(super) fn kill(&mut self, victim: usize, killer: Option<usize>, cause: KillCause) {
        if !self.players[victim].alive {
            return;
        }
        let (cx, cy) = self.center(victim);
        let p = &mut self.players[victim];
        p.alive = false;
        p.swing = 0;
        p.stats.deaths += 1;
        let dropped = p.held.take();
        let victim_slot = p.slot;
        if let Some(h) = dropped {
            let id = self.next_id();
            self.items.push(Item {
                id,
                weapon: h.weapon,
                ammo: h.ammo,
                x: cx - ITEM_W / 2.0,
                y: cy - ITEM_H / 2.0,
                vx: 0.0,
                vy: -120.0,
                thrown_by: None,
                grace: 0,
                spawner: None,
                grounded: false,
            });
        }
        let killer_slot = killer.map(|k| {
            if k != victim {
                self.players[k].stats.kills += 1;
            }
            self.players[k].slot
        });
        log::debug!(
            "room {}: 0x{victim_slot:02X} dropped by {killer_slot:?} ({cause:?})",
            self.room_id
        );
        self.push_event(EventKind::Kill {
            killer: killer_slot,
            victim: victim_slot,
            cause,
        });
    }

    // ------------------------------------------------------------ actions

    /// Weapons, items, projectiles and firewalls for one Playing tick.
    /// `pressed[i]` holds buttons that went down this tick for player i.
    // Index loops on purpose: each body needs `&mut self`.
    #[allow(clippy::needless_range_loop)]
    pub(super) fn combat_tick(&mut self, pressed: &[u16]) {
        let can_act = |g: &Game, i: usize| g.players[i].alive && g.players[i].sim.stun == 0;
        for p in &mut self.players {
            p.cooldown = p.cooldown.saturating_sub(1);
            p.swing = p.swing.saturating_sub(1);
        }
        for i in 0..self.players.len() {
            if can_act(self, i) && pressed[i] & buttons::INTERACT != 0 {
                self.interact(i);
            }
        }
        // Two passes so same-tick actions don't depend on slot order:
        // defensive swings first, then everything that fires.
        for blade_pass in [true, false] {
            for i in 0..self.players.len() {
                let is_blade = self.players[i]
                    .held
                    .is_some_and(|h| h.weapon == Weapon::ChecksumBlade);
                if is_blade == blade_pass && can_act(self, i) && pressed[i] & buttons::SHOOT != 0 {
                    self.fire(i);
                }
            }
        }
        self.update_items();
        self.update_projectiles();
        self.blade_hits();
        for f in &mut self.firewalls {
            f.ticks = f.ticks.saturating_sub(1);
        }
        self.firewalls.retain(|f| f.ticks > 0);
        for s in 0..self.spawners.len() {
            if self.spawners[s].item.is_none() {
                self.spawners[s].respawn = self.spawners[s].respawn.saturating_sub(1);
                if self.spawners[s].respawn == 0 {
                    self.restock(s);
                }
            }
        }
    }

    /// INTERACT: throw what you hold, or pick up the nearest item.
    fn interact(&mut self, i: usize) {
        let (cx, cy) = self.center(i);
        if let Some(h) = self.players[i].held.take() {
            let p = &self.players[i];
            let dir = if p.sim.facing_right { 1.0 } else { -1.0 };
            let (vx, vy) = if p.aim_up {
                (dir * 80.0, -380.0)
            } else {
                (dir * THROW_SPEED + p.sim.vx * 0.5, -140.0)
            };
            let id = self.next_id();
            self.items.push(Item {
                id,
                weapon: h.weapon,
                ammo: h.ammo,
                x: cx - ITEM_W / 2.0,
                y: cy - ITEM_H / 2.0,
                vx,
                vy,
                thrown_by: Some(i),
                grace: THROW_GRACE_TICKS,
                spawner: None,
                grounded: false,
            });
            return;
        }
        let nearest = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, it)| !(it.thrown_by == Some(i) && it.grace > 0))
            .map(|(k, it)| {
                let dx = it.x + ITEM_W / 2.0 - cx;
                let dy = it.y + ITEM_H / 2.0 - cy;
                (k, (dx * dx + dy * dy).sqrt())
            })
            .filter(|&(_, d)| d <= PICKUP_RANGE)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((k, _)) = nearest {
            let it = self.items.remove(k);
            if let Some(s) = it.spawner {
                self.spawners[s].item = None;
                self.spawners[s].respawn = SPAWNER_RESPAWN_TICKS;
            }
            let p = &mut self.players[i];
            p.held = Some(Held {
                weapon: it.weapon,
                ammo: it.ammo,
            });
            p.cooldown = 6; // no accidental shot on the pickup frame
        }
    }

    fn fire(&mut self, i: usize) {
        let Some(mut held) = self.players[i].held else {
            return;
        };
        let w = held.weapon;
        let unlimited = w.max_ammo() == 0;
        if self.players[i].cooldown > 0 || (!unlimited && held.ammo == 0) {
            return; // cooling down, or *click*: empty, throw it instead
        }
        self.players[i].cooldown = w.cooldown_ticks();
        let (dx, dy) = self.aim(i);
        let (mx, my) = self.muzzle(i);

        match w {
            Weapon::PingPistol => {
                self.hitscan(i, (mx, my), (dx, dy), KillCause::Weapon(w), false);
                self.players[i].sim.vx -= dx * PISTOL_KNOCKBACK;
            }
            Weapon::BroadcastShotgun => {
                let base = dy.atan2(dx);
                for k in 0..PELLETS {
                    let t = k as f32 / (PELLETS - 1) as f32 - 0.5;
                    let a = base + t * PELLET_SPREAD;
                    let id = self.next_id();
                    self.projectiles.push(Projectile {
                        id,
                        kind: ProjectileKind::Pellet,
                        owner: i,
                        x: mx,
                        y: my,
                        vx: a.cos() * PELLET_SPEED,
                        vy: a.sin() * PELLET_SPEED,
                        ttl: PELLET_TTL,
                        reflected: false,
                    });
                }
                let s = &mut self.players[i].sim;
                s.vx -= dx * SHOTGUN_RECOIL;
                if dy < 0.0 {
                    s.vy = s.vy.max(120.0); // firing upward pushes you down
                }
            }
            Weapon::ForkBomb => {
                let p = &self.players[i];
                let dir = if p.sim.facing_right { 1.0 } else { -1.0 };
                let (vx, vy) = if p.aim_up {
                    (dir * 60.0, -400.0)
                } else {
                    (dir * 230.0 + p.sim.vx * 0.5, -230.0)
                };
                let id = self.next_id();
                self.projectiles.push(Projectile {
                    id,
                    kind: ProjectileKind::Bomb,
                    owner: i,
                    x: mx,
                    y: my,
                    vx,
                    vy,
                    ttl: BOMB_FUSE,
                    reflected: false,
                });
            }
            Weapon::Firewall => {
                let s = &self.players[i].sim;
                let x = if s.facing_right {
                    s.x + PLAYER_W + 4.0
                } else {
                    s.x - 4.0 - FIREWALL_W
                };
                let y = s.y + PLAYER_H - FIREWALL_H;
                let id = self.next_id();
                self.firewalls.push(Firewall {
                    id,
                    x,
                    y,
                    ticks: FIREWALL_TICKS,
                });
            }
            Weapon::ChecksumBlade => {
                self.players[i].swing = BLADE_SWING_TICKS;
            }
        }

        if !unlimited {
            held.ammo -= 1;
        }
        self.players[i].held = if held.ammo == 0 && w.consumable() {
            None
        } else {
            Some(held)
        };
    }

    /// March a ray in 2 px steps until it hits a wall, a firewall or a packet.
    fn ray(&self, from: (f32, f32), dir: (f32, f32), ignore: usize) -> (Hit, (f32, f32)) {
        const STEP: f32 = 2.0;
        let steps = (PISTOL_RANGE / STEP) as i32;
        let mut pt = from;
        for k in 1..=steps {
            pt = (
                from.0 + dir.0 * STEP * k as f32,
                from.1 + dir.1 * STEP * k as f32,
            );
            if self.solid_at(pt.0, pt.1) {
                return (Hit::Wall, pt);
            }
            if self.firewall_at(pt.0, pt.1) {
                return (Hit::Firewall, pt);
            }
            for (j, p) in self.players.iter().enumerate() {
                if j != ignore
                    && p.alive
                    && inside(pt.0, pt.1, p.sim.x, p.sim.y, PLAYER_W, PLAYER_H)
                {
                    return (Hit::Player(j), pt);
                }
            }
        }
        (Hit::Nothing, pt)
    }

    fn hitscan(
        &mut self,
        owner: usize,
        from: (f32, f32),
        dir: (f32, f32),
        cause: KillCause,
        reflected: bool,
    ) {
        let (hit, end) = self.ray(from, dir, owner);
        self.push_event(EventKind::Shot {
            x0: from.0,
            y0: from.1,
            x1: end.0,
            y1: end.1,
        });
        match hit {
            // A blade swing sends the shot straight back (once).
            Hit::Player(j) if self.reflecting(j) && !reflected => {
                self.hitscan(j, end, (-dir.0, -dir.1), KillCause::Reflect, true);
            }
            Hit::Player(j) => self.kill(j, Some(owner), cause),
            Hit::Wall | Hit::Firewall | Hit::Nothing => {}
        }
    }

    fn blade_hits(&mut self) {
        for i in 0..self.players.len() {
            if !self.reflecting(i) {
                continue;
            }
            let p = &self.players[i];
            let s = &p.sim;
            let (bx, by, bw, bh) = if p.aim_up {
                (
                    s.x + PLAYER_W / 2.0 - 10.0,
                    s.y - BLADE_REACH,
                    20.0,
                    BLADE_REACH + 4.0,
                )
            } else if s.facing_right {
                (s.x + PLAYER_W, s.y - 3.0, BLADE_REACH, PLAYER_H + 6.0)
            } else {
                (s.x - BLADE_REACH, s.y - 3.0, BLADE_REACH, PLAYER_H + 6.0)
            };
            let victims: Vec<usize> = (0..self.players.len())
                .filter(|&j| {
                    let v = &self.players[j];
                    j != i
                        && v.alive
                        // Two blades swinging at each other parry.
                        && !self.reflecting(j)
                        && gg_shared::game::overlap(
                            bx, by, bw, bh, v.sim.x, v.sim.y, PLAYER_W, PLAYER_H,
                        )
                })
                .collect();
            for j in victims {
                self.kill(j, Some(i), KillCause::Weapon(Weapon::ChecksumBlade));
            }
        }
    }

    // -------------------------------------------------------- projectiles

    fn update_projectiles(&mut self) {
        let mut spawned = Vec::new();
        let mut k = 0;
        while k < self.projectiles.len() {
            let mut pr = self.projectiles[k];
            let alive = match pr.kind {
                ProjectileKind::Pellet => self.step_pellet(&mut pr),
                ProjectileKind::Bomb | ProjectileKind::Shard => {
                    self.step_bomb(&mut pr, &mut spawned)
                }
            };
            if alive {
                self.projectiles[k] = pr;
                k += 1;
            } else {
                self.projectiles.remove(k);
            }
        }
        self.projectiles.extend(spawned);
    }

    /// Returns false when the pellet is gone.
    fn step_pellet(&mut self, pr: &mut Projectile) -> bool {
        // Sub-steps so a fast pellet can't tunnel through a 12 px packet.
        const SUB: i32 = 3;
        for _ in 0..SUB {
            pr.x += pr.vx * DT / SUB as f32;
            pr.y += pr.vy * DT / SUB as f32;
            if self.solid_at(pr.x, pr.y) || self.firewall_at(pr.x, pr.y) {
                return false;
            }
            let hit = (0..self.players.len()).find(|&j| {
                let p = &self.players[j];
                j != pr.owner && p.alive && inside(pr.x, pr.y, p.sim.x, p.sim.y, PLAYER_W, PLAYER_H)
            });
            if let Some(j) = hit {
                if self.reflecting(j) {
                    pr.vx = -pr.vx;
                    pr.vy = -pr.vy;
                    pr.owner = j;
                    pr.reflected = true;
                } else {
                    let cause = if pr.reflected {
                        KillCause::Reflect
                    } else {
                        KillCause::Weapon(Weapon::BroadcastShotgun)
                    };
                    self.kill(j, Some(pr.owner), cause);
                    return false;
                }
            }
        }
        pr.ttl = pr.ttl.saturating_sub(1);
        pr.ttl > 0
    }

    /// Returns false when the bomb has exploded or fell out of the map.
    fn step_bomb(&mut self, pr: &mut Projectile, spawned: &mut Vec<Projectile>) -> bool {
        pr.vy = (pr.vy + BOMB_GRAVITY * DT).min(500.0);
        let nx = pr.x + pr.vx * DT;
        if self.solid_at(nx, pr.y) || self.firewall_at(nx, pr.y) {
            pr.vx = -pr.vx * 0.5;
        } else {
            pr.x = nx;
        }
        let ny = pr.y + pr.vy * DT;
        if pr.vy > 0.0 {
            match self.floor_between(pr.x, pr.y, ny) {
                Some(top) => {
                    pr.y = top - 0.5;
                    pr.vy = -pr.vy * 0.35;
                    pr.vx *= 0.8;
                }
                None => pr.y = ny,
            }
        } else if self.solid_at(pr.x, ny) {
            pr.vy = 0.0;
        } else {
            pr.y = ny;
        }
        if pr.y > self.map.pixel_height() + 64.0 {
            return false;
        }
        pr.ttl = pr.ttl.saturating_sub(1);
        if pr.ttl > 0 {
            return true;
        }

        let radius = if pr.kind == ProjectileKind::Bomb {
            BOMB_RADIUS
        } else {
            SHARD_RADIUS
        };
        self.push_event(EventKind::Explosion {
            x: pr.x,
            y: pr.y,
            radius,
        });
        let victims: Vec<usize> = (0..self.players.len())
            .filter(|&j| {
                let (cx, cy) = self.center(j);
                let (dx, dy) = (cx - pr.x, cy - pr.y);
                self.players[j].alive && (dx * dx + dy * dy).sqrt() <= radius + 6.0
            })
            .collect();
        for j in victims {
            self.kill(j, Some(pr.owner), KillCause::Weapon(Weapon::ForkBomb));
        }
        // fork(): the parent splits into two child blasts.
        if pr.kind == ProjectileKind::Bomb {
            for dir in [-1.0, 1.0] {
                let id = self.next_id();
                spawned.push(Projectile {
                    id,
                    kind: ProjectileKind::Shard,
                    owner: pr.owner,
                    x: pr.x,
                    y: pr.y - 4.0,
                    vx: dir * 150.0,
                    vy: -200.0,
                    ttl: SHARD_FUSE,
                    reflected: false,
                });
            }
        }
        false
    }

    // -------------------------------------------------------------- items

    fn update_items(&mut self) {
        let mut k = 0;
        while k < self.items.len() {
            let mut it = self.items[k];
            it.grace = it.grace.saturating_sub(1);

            it.vy = (it.vy + ITEM_GRAVITY * DT).min(450.0);
            let nx = it.x + it.vx * DT;
            let probe_x = if it.vx > 0.0 { nx + ITEM_W } else { nx };
            if self.solid_at(probe_x, it.y + ITEM_H / 2.0) {
                it.vx = -it.vx * 0.4;
            } else {
                it.x = nx;
            }
            let ny = it.y + it.vy * DT;
            it.grounded = false;
            if it.vy > 0.0 {
                let landing = [it.x + 1.0, it.x + ITEM_W - 1.0]
                    .iter()
                    .filter_map(|&px| self.floor_between(px, it.y + ITEM_H, ny + ITEM_H))
                    .reduce(f32::min);
                match landing {
                    Some(top) => {
                        it.y = top - ITEM_H;
                        it.vy = 0.0;
                        it.grounded = true;
                        it.vx *= 0.85;
                        if it.vx.abs() < 5.0 {
                            it.vx = 0.0;
                        }
                    }
                    None => it.y = ny,
                }
            } else if self.solid_at(it.x + ITEM_W / 2.0, ny) {
                it.vy = 0.0;
            } else {
                it.y = ny;
            }

            // A thrown weapon stuns whoever it hits.
            let speed = (it.vx * it.vx + it.vy * it.vy).sqrt();
            if let Some(t) = it.thrown_by {
                if speed < THROW_HIT_SPEED || it.grounded {
                    it.thrown_by = None;
                } else if let Some(j) = (0..self.players.len()).find(|&j| {
                    let p = &self.players[j];
                    j != t
                        && p.alive
                        && p.sim.stun == 0
                        && gg_shared::game::overlap(
                            it.x, it.y, ITEM_W, ITEM_H, p.sim.x, p.sim.y, PLAYER_W, PLAYER_H,
                        )
                }) {
                    let dir = it.vx.signum();
                    let v = &mut self.players[j];
                    v.sim.stun = STUN_TICKS;
                    v.sim.vx = dir * 150.0;
                    v.sim.vy = -160.0;
                    v.swing = 0;
                    let (by, victim) = (self.players[t].slot, self.players[j].slot);
                    self.push_event(EventKind::Stun { by, victim });
                    it.vx = -it.vx * 0.3;
                    it.thrown_by = None;
                }
            }

            if it.y > self.map.pixel_height() + 64.0 {
                if let Some(s) = it.spawner {
                    self.spawners[s].item = None;
                    self.spawners[s].respawn = SPAWNER_RESPAWN_TICKS;
                }
                self.items.remove(k);
            } else {
                self.items[k] = it;
                k += 1;
            }
        }
    }
}
