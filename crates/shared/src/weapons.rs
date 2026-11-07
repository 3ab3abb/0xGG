//! Weapon catalogue. Numbers here are gameplay tuning; behavior lives in the
//! server's combat module (the client only draws what snapshots say).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Weapon {
    /// Hitscan, 6 shots.
    PingPistol,
    /// 5-pellet spread, short range, strong recoil, 2 shots.
    BroadcastShotgun,
    /// Thrown grenade that explodes and forks into 2 smaller blasts.
    ForkBomb,
    /// Deployable wall that blocks shots for a few seconds.
    Firewall,
    /// Melee swing; reflects shots during the swing. Unlimited.
    ChecksumBlade,
}

pub const ALL_WEAPONS: [Weapon; 5] = [
    Weapon::PingPistol,
    Weapon::BroadcastShotgun,
    Weapon::ForkBomb,
    Weapon::Firewall,
    Weapon::ChecksumBlade,
];

/// Pickup box size (pixels).
pub const ITEM_W: f32 = 10.0;
pub const ITEM_H: f32 = 8.0;
/// Deployed firewall size (pixels).
pub const FIREWALL_W: f32 = 6.0;
pub const FIREWALL_H: f32 = 40.0;

impl Weapon {
    pub fn name(self) -> &'static str {
        match self {
            Weapon::PingPistol => "PING PISTOL",
            Weapon::BroadcastShotgun => "BROADCAST SHOTGUN",
            Weapon::ForkBomb => "FORK BOMB",
            Weapon::Firewall => "FIREWALL",
            Weapon::ChecksumBlade => "CHECKSUM BLADE",
        }
    }

    /// Short tag for the kill feed.
    pub fn tag(self) -> &'static str {
        match self {
            Weapon::PingPistol => "PING",
            Weapon::BroadcastShotgun => "BROADCAST",
            Weapon::ForkBomb => "FORK BOMB",
            Weapon::Firewall => "FIREWALL",
            Weapon::ChecksumBlade => "CHECKSUM",
        }
    }

    /// Ammo on pickup; 0 = unlimited.
    pub fn max_ammo(self) -> u8 {
        match self {
            Weapon::PingPistol => 6,
            Weapon::BroadcastShotgun => 2,
            Weapon::ForkBomb | Weapon::Firewall => 1,
            Weapon::ChecksumBlade => 0,
        }
    }

    /// Used up (removed from hands) when the last ammo is spent.
    pub fn consumable(self) -> bool {
        matches!(self, Weapon::ForkBomb | Weapon::Firewall)
    }

    /// Ticks between two uses.
    pub fn cooldown_ticks(self) -> u8 {
        match self {
            Weapon::PingPistol => 12,
            Weapon::BroadcastShotgun => 30,
            Weapon::ForkBomb | Weapon::Firewall => 20,
            Weapon::ChecksumBlade => 24,
        }
    }
}
