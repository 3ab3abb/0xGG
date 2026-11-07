//! Sparring-bot brain: turns a snapshot into button presses, exactly like a
//! human reading the screen. Used by in-game bots (server-spawned headless
//! clients) and by the client's `--bot` flag.
//!
//! It only reads what every client receives, so bots have no extra
//! information and the server can't tell them apart from humans.

use crate::game::{buttons::*, PLAYER_H, PLAYER_W};
use crate::udp::{flags, PlayerState, Snapshot};
use crate::weapons::{Weapon, ITEM_H, ITEM_W};

/// Usernames starting with this are reserved for bots.
pub const BOT_PREFIX: &str = "bot_";

pub fn is_bot_name(name: &str) -> bool {
    name.len() >= BOT_PREFIX.len() && name[..BOT_PREFIX.len()].eq_ignore_ascii_case(BOT_PREFIX)
}

pub struct BotBrain {
    /// Toggles every call so SHOOT/INTERACT produce fresh presses.
    flip: bool,
    jump_hold: u8,
    last_x: f32,
    stuck_frames: u32,
    wander_until: f64,
    wander_dir: u16,
    rng: u64,
}

impl BotBrain {
    pub fn new(seed: u64) -> Self {
        BotBrain {
            flip: false,
            jump_hold: 0,
            last_x: 0.0,
            stuck_frames: 0,
            wander_until: 0.0,
            wander_dir: 0,
            rng: seed | 1,
        }
    }

    fn rand(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        x
    }

    /// True with probability 1/n.
    fn one_in(&mut self, n: u64) -> bool {
        self.rand().is_multiple_of(n)
    }

    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (self.rand() % 10_000) as f64 / 10_000.0 * (hi - lo)
    }

    /// Buttons to hold this tick. `now` is any monotonic clock in seconds.
    pub fn think(&mut self, s: &Snapshot, my_slot: u8, now: f64) -> u16 {
        let alive = |p: &&PlayerState| p.flags & flags::ALIVE != 0;
        let Some(me) = s.players.iter().filter(alive).find(|p| p.slot == my_slot) else {
            return 0;
        };
        self.flip = !self.flip;
        let (mx, my) = (me.x + PLAYER_W / 2.0, me.y + PLAYER_H / 2.0);
        let grounded = me.flags & flags::GROUNDED != 0;
        let mut b = 0u16;
        let mut want_jump = false;
        let mut want_drop = false;

        let needs_weapon = match me.weapon {
            None => true,
            Some(w) => w.max_ammo() > 0 && me.ammo == 0,
        };
        let target: Option<(f32, f32)> = if needs_weapon {
            let it = s.items.iter().filter(|it| !it.flying).min_by(|a, c| {
                let da = (a.x - mx).abs() + (a.y - my).abs();
                let dc = (c.x - mx).abs() + (c.y - my).abs();
                da.total_cmp(&dc)
            });
            it.map(|it| {
                let (ix, iy) = (it.x + ITEM_W / 2.0, it.y + ITEM_H / 2.0);
                if (ix - mx).abs() < 12.0 && (iy - my).abs() < 14.0 && self.flip {
                    b |= INTERACT; // throws an empty gun first, then picks up
                }
                (ix, iy)
            })
        } else {
            let foe = s
                .players
                .iter()
                .filter(alive)
                .filter(|p| p.slot != my_slot)
                .min_by(|a, c| {
                    let da = (a.x - me.x).abs() + (a.y - me.y).abs();
                    let dc = (c.x - me.x).abs() + (c.y - me.y).abs();
                    da.total_cmp(&dc)
                });
            match (foe, me.weapon) {
                (Some(foe), Some(w)) => {
                    let (dx, dy) = (foe.x - me.x, foe.y - me.y);
                    let range = match w {
                        Weapon::PingPistol => 300.0,
                        Weapon::BroadcastShotgun => 110.0,
                        Weapon::ForkBomb => 140.0,
                        Weapon::Firewall => 90.0,
                        Weapon::ChecksumBlade => 26.0,
                    };
                    let facing_ok = (dx > 0.0) == (me.flags & flags::FACING_RIGHT != 0);
                    if dy.abs() < 10.0 && dx.abs() < range && facing_ok && self.flip {
                        b |= SHOOT;
                    } else if dx.abs() < 10.0 && dy < -20.0 && dy > -range && self.flip {
                        b |= AIM_UP | SHOOT;
                    }
                    Some((foe.x + PLAYER_W / 2.0, foe.y + PLAYER_H / 2.0))
                }
                _ => None,
            }
        };

        // Wander now and then so bots don't deadlock under a platform.
        if now < self.wander_until {
            b |= self.wander_dir;
            if grounded && self.one_in(20) {
                want_jump = true;
            }
        } else if let Some((tx, ty)) = target {
            if (tx - mx).abs() > 6.0 {
                b |= if tx > mx { RIGHT } else { LEFT };
            } else if b & SHOOT == 0 {
                // Close horizontally: turn to face the target without walking off.
                let face_right = me.flags & flags::FACING_RIGHT != 0;
                if (tx > mx) != face_right && (tx - mx).abs() > 1.0 {
                    b |= if tx > mx { RIGHT } else { LEFT };
                }
            }
            if ty < my - 20.0 {
                want_jump = true;
            } else if ty > my + 24.0 && (tx - mx).abs() < 60.0 {
                want_drop = true;
            }
        }

        // Stuck against a wall: jump, and if that fails, wander off.
        if b & (LEFT | RIGHT) != 0 && (me.x - self.last_x).abs() < 0.2 {
            self.stuck_frames += 1;
            want_jump = true;
            if self.stuck_frames > 45 {
                self.stuck_frames = 0;
                self.wander_until = now + self.range(0.6, 1.4);
                self.wander_dir = if self.one_in(2) { LEFT } else { RIGHT };
            }
        } else {
            self.stuck_frames = 0;
        }
        self.last_x = me.x;

        if self.jump_hold > 0 {
            self.jump_hold -= 1;
            b |= JUMP;
        } else if grounded && want_drop {
            b |= DOWN | JUMP;
            self.jump_hold = 2;
        } else if grounded && want_jump {
            self.jump_hold = 14; // hold for a full-height jump
            b |= JUMP;
        }
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bot_names() {
        assert!(is_bot_name("bot_1"));
        assert!(is_bot_name("BOT_x"));
        assert!(!is_bot_name("bob"));
        assert!(!is_bot_name("robot_1"));
        assert!(!is_bot_name("bo"));
    }
}
