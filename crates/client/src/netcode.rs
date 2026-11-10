//! Client-side latency hiding (docs/PROTOCOL.md §9):
//!
//! - [`Predictor`]: **client-side prediction + server reconciliation** for
//!   your own packet. Every 60 Hz input is simulated locally right away with
//!   the same `PlayerSim::step` the server runs, so movement has no input lag.
//!   When a snapshot says "I applied your input #N and you are here", we
//!   restart from that authoritative state and replay inputs N+1.. on top.
//!   Any visible difference is smoothed out over ~100 ms instead of snapping.
//!
//! - [`Interpolator`]: **entity interpolation** for everything else (other
//!   packets, projectiles, items). Snapshots arrive at 30 Hz with jitter, so
//!   drawing the newest one makes others stutter. Instead we draw the world as
//!   it was [`INTERP_DELAY_TICKS`] ago, blending between the two buffered
//!   snapshots around that moment. That buffer also rides out lost snapshots.

use std::collections::VecDeque;

use gg_shared::game::{Map, PlayerSim, TICK_RATE};
use gg_shared::udp::{RoundPhase, Snapshot};

/// Render remote entities this far in the past: 6 ticks = 100 ms = 3
/// snapshots, so up to 2 lost snapshots in a row still interpolate.
pub const INTERP_DELAY_TICKS: f64 = 6.0;
/// Moves bigger than this between two snapshots are teleports (respawn,
/// new round): jump, don't slide across the map.
const TELEPORT_DIST: f32 = 48.0;
/// Reconciliation errors bigger than this are snapped, not smoothed.
const SNAP_DIST: f32 = 48.0;
/// Visual correction decay rate (1/s): ~95% gone after 100 ms.
const SMOOTH_RATE: f32 = 30.0;
/// Unacknowledged inputs we keep for replay (2 s at 60 Hz).
const MAX_PENDING: usize = 120;

// ------------------------------------------------------------ prediction

pub struct Predictor {
    /// Predicted state of our packet; `None` while not predicting
    /// (countdown, dead, disabled): the server's state is drawn instead.
    pub sim: Option<PlayerSim>,
    /// Inputs sent but not yet applied by the server: `(seq, buttons)`.
    pending: VecDeque<(u32, u16)>,
    /// Visual offset (world px) left over from the last correction, decays to 0.
    pub smooth: (f32, f32),
    /// Size of the last correction in world px, for the debug HUD.
    pub last_error: f32,
}

impl Default for Predictor {
    fn default() -> Self {
        Predictor {
            sim: None,
            pending: VecDeque::new(),
            smooth: (0.0, 0.0),
            last_error: 0.0,
        }
    }
}

/// Does the server move players in this phase? (Frozen during countdown.)
pub fn phase_moves(phase: RoundPhase) -> bool {
    !matches!(phase, RoundPhase::Countdown { .. })
}

impl Predictor {
    /// A new local input tick: remember it for replay and simulate it now.
    pub fn on_input(&mut self, seq: u32, buttons: u16, map: &Map) {
        self.pending.push_back((seq, buttons));
        while self.pending.len() > MAX_PENDING {
            self.pending.pop_front();
        }
        if let Some(sim) = &mut self.sim {
            sim.step(map, buttons);
        }
    }

    /// Reconcile with an authoritative snapshot.
    pub fn on_snapshot(&mut self, s: &Snapshot, map: &Map, enabled: bool) {
        // The server has applied everything up to ack_input_seq.
        while self
            .pending
            .front()
            .is_some_and(|&(seq, _)| seq <= s.ack_input_seq)
        {
            self.pending.pop_front();
        }
        let you = match s.you {
            Some(you) if enabled && phase_moves(s.phase) => you,
            _ => {
                self.sim = None;
                self.smooth = (0.0, 0.0);
                return;
            }
        };
        // Server state after input #ack, then replay what it hasn't seen yet.
        let mut sim = you;
        for &(_, b) in &self.pending {
            sim.step(map, b);
        }
        if let Some(old) = self.sim {
            let (ex, ey) = (old.x - sim.x, old.y - sim.y);
            self.last_error = (ex * ex + ey * ey).sqrt();
            if self.last_error > SNAP_DIST {
                self.smooth = (0.0, 0.0); // teleport: no point sliding there
            } else {
                // Keep drawing where we were, then glide to the corrected spot.
                self.smooth.0 += ex;
                self.smooth.1 += ey;
            }
        }
        self.sim = Some(sim);
    }

    /// Fade the visual correction. Call once per frame.
    pub fn decay(&mut self, dt: f32) {
        let k = (-SMOOTH_RATE * dt).exp();
        self.smooth.0 *= k;
        self.smooth.1 *= k;
        if self.smooth.0.abs() < 0.01 && self.smooth.1.abs() < 0.01 {
            self.smooth = (0.0, 0.0);
        }
    }

    /// Where to draw our packet (prediction + leftover correction).
    pub fn draw_pos(&self) -> Option<(f32, f32)> {
        self.sim.map(|s| (s.x + self.smooth.0, s.y + self.smooth.1))
    }

    #[cfg(test)]
    pub fn unacked(&self) -> usize {
        self.pending.len()
    }
}

// --------------------------------------------------------- interpolation

#[derive(Default)]
pub struct Interpolator {
    /// Received snapshots, oldest first, strictly increasing ticks.
    buffer: VecDeque<Snapshot>,
    /// Estimated `server_tick - local_seconds * TICK_RATE`.
    clock_offset: Option<f64>,
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Blend `(ax, ay)` toward `(bx, by)`, unless that's a teleport.
fn lerp_pos(ax: f32, ay: f32, bx: f32, by: f32, t: f32) -> (f32, f32) {
    let (dx, dy) = (bx - ax, by - ay);
    if dx * dx + dy * dy > TELEPORT_DIST * TELEPORT_DIST {
        (ax, ay)
    } else {
        (lerp(ax, bx, t), lerp(ay, by, t))
    }
}

impl Interpolator {
    /// Add a snapshot (caller already dropped stale/duplicate ones).
    /// `now` is the local clock in seconds.
    pub fn push(&mut self, s: &Snapshot, now: f64) {
        // Track the server clock. Each arrival gives a sample; jitter only
        // ever delays packets, so follow fast when a sample is ahead (fresher)
        // and slowly when behind, and resync on big jumps.
        let sample = s.tick as f64 - now * TICK_RATE as f64;
        self.clock_offset = Some(match self.clock_offset {
            None => sample,
            Some(o) if (sample - o).abs() > 30.0 => sample,
            Some(o) if sample > o => o + (sample - o) * 0.2,
            Some(o) => o + (sample - o) * 0.02,
        });
        self.buffer.push_back(s.clone());
        // Keep ~1 s, plenty for a 100 ms delay.
        while self.buffer.len() > 2
            && self
                .buffer
                .front()
                .is_some_and(|f| f.tick + TICK_RATE < s.tick)
        {
            self.buffer.pop_front();
        }
    }

    /// Server tick being rendered right now (fractional).
    pub fn render_tick(&self, now: f64) -> Option<f64> {
        Some(now * TICK_RATE as f64 + self.clock_offset? - INTERP_DELAY_TICKS)
    }

    /// The world as it was at `render_tick`: the older surrounding snapshot,
    /// with every position blended toward the newer one.
    pub fn sample(&self, now: f64) -> Option<Snapshot> {
        let t = self.render_tick(now)?;
        let newest = self.buffer.back()?;
        // Ran out of snapshots (loss burst): hold the newest, don't guess.
        if t >= newest.tick as f64 {
            return Some(newest.clone());
        }
        let i = self.buffer.iter().rposition(|s| (s.tick as f64) <= t)?;
        let (a, b) = (&self.buffer[i], &self.buffer[i + 1]);
        let alpha = ((t - a.tick as f64) / (b.tick - a.tick) as f64).clamp(0.0, 1.0) as f32;

        let mut out = a.clone();
        for p in &mut out.players {
            if let Some(q) = b.players.iter().find(|q| q.slot == p.slot) {
                (p.x, p.y) = lerp_pos(p.x, p.y, q.x, q.y, alpha);
                p.vx = lerp(p.vx, q.vx, alpha);
                p.vy = lerp(p.vy, q.vy, alpha);
            }
        }
        for pr in &mut out.projectiles {
            if let Some(q) = b.projectiles.iter().find(|q| q.id == pr.id) {
                (pr.x, pr.y) = lerp_pos(pr.x, pr.y, q.x, q.y, alpha);
            }
        }
        for it in &mut out.items {
            if let Some(q) = b.items.iter().find(|q| q.id == it.id) {
                (it.x, it.y) = lerp_pos(it.x, it.y, q.x, q.y, alpha);
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gg_shared::game::{builtin_map, buttons};
    use gg_shared::udp::{PlayerState, Snapshot};

    fn snap(tick: u32, ack: u32, you: PlayerSim, x: f32) -> Snapshot {
        Snapshot {
            tick,
            ack_input_seq: ack,
            round: 1,
            phase: RoundPhase::Playing { ms_left: 60_000 },
            scores: vec![],
            players: vec![PlayerState {
                slot: 2,
                x,
                y: 100.0,
                vx: 0.0,
                vy: 0.0,
                flags: 0,
                weapon: None,
                ammo: 0,
            }],
            items: vec![],
            projectiles: vec![],
            firewalls: vec![],
            events: vec![],
            you: Some(you),
        }
    }

    /// With a perfect server (same inputs, same sim), reconciliation must be
    /// a no-op: zero correction, prediction stays where it was.
    #[test]
    fn reconcile_with_agreeing_server_has_no_error() {
        let map = builtin_map("router").unwrap();
        let (x, y) = map.spawns[0];
        let start = PlayerSim::spawn_at(x, y, true);
        let mut server = start;
        let mut pred = Predictor {
            sim: Some(start),
            ..Default::default()
        };
        let inputs: Vec<u16> = (0..40)
            .map(|i| match i {
                0..=9 => buttons::RIGHT,
                10..=19 => buttons::RIGHT | buttons::JUMP,
                _ => buttons::LEFT,
            })
            .collect();
        for (i, &b) in inputs.iter().enumerate() {
            pred.on_input(i as u32 + 1, b, &map);
        }
        // Server has applied only the first 25 (RTT): 15 still in flight.
        for &b in &inputs[..25] {
            server.step(&map, b);
        }
        let before = pred.sim.unwrap();
        pred.on_snapshot(&snap(50, 25, server, 0.0), &map, true);
        assert_eq!(pred.unacked(), 15);
        assert_eq!(pred.sim.unwrap(), before);
        assert_eq!(pred.last_error, 0.0);
        assert_eq!(pred.smooth, (0.0, 0.0));
    }

    /// If the server disagrees (e.g. a stomp bounce we didn't predict), we
    /// adopt its state but draw from the old spot and glide over.
    #[test]
    fn reconcile_corrects_and_smooths() {
        let map = builtin_map("router").unwrap();
        let (x, y) = map.spawns[0];
        let start = PlayerSim::spawn_at(x, y, true);
        let mut pred = Predictor {
            sim: Some(start),
            ..Default::default()
        };
        for seq in 1..=10 {
            pred.on_input(seq, buttons::RIGHT, &map);
        }
        let predicted = pred.sim.unwrap();
        let mut server = start;
        server.x -= 10.0; // server says we were 10 px further left
        for _ in 0..10 {
            server.step(&map, buttons::RIGHT);
        }
        pred.on_snapshot(&snap(20, 10, server, 0.0), &map, true);
        let fixed = pred.sim.unwrap();
        assert!((fixed.x - server.x).abs() < 0.001);
        let (dx, dy) = pred.draw_pos().unwrap();
        assert!((dx - predicted.x).abs() < 0.001 && (dy - predicted.y).abs() < 0.001);
        for _ in 0..30 {
            pred.decay(1.0 / 60.0);
        }
        let (dx, _) = pred.draw_pos().unwrap();
        assert!((dx - fixed.x).abs() < 0.01, "correction should fade out");
    }

    #[test]
    fn countdown_or_death_stops_prediction() {
        let map = builtin_map("router").unwrap();
        let mut pred = Predictor::default();
        let mut s = snap(10, 0, PlayerSim::default(), 0.0);
        pred.on_snapshot(&s, &map, true);
        assert!(pred.sim.is_some());
        s.tick = 12;
        s.you = None; // dead
        pred.on_snapshot(&s, &map, true);
        assert!(pred.sim.is_none());
        s.tick = 14;
        s.you = Some(PlayerSim::default());
        s.phase = RoundPhase::Countdown { ms_left: 500 };
        pred.on_snapshot(&s, &map, true);
        assert!(pred.sim.is_none());
    }

    #[test]
    fn interpolates_between_snapshots_in_the_past() {
        let mut it = Interpolator::default();
        let you = PlayerSim::default();
        // Snapshots every 2 ticks (30 Hz), arriving exactly on time.
        for k in 0..10u32 {
            let tick = 100 + 2 * k;
            let now = tick as f64 / TICK_RATE as f64;
            it.push(&snap(tick, 0, you, 10.0 * k as f32), now);
        }
        // Newest is tick 118 at now = 118/60; we render tick 112 => k = 6 => x = 60.
        let now = 118.0 / TICK_RATE as f64;
        let s = it.sample(now).unwrap();
        assert!((s.players[0].x - 60.0).abs() < 0.01, "x={}", s.players[0].x);
        // Half a tick later: halfway between k=6 and k=7 is a quarter step.
        let s = it.sample(now + 0.5 / TICK_RATE as f64).unwrap();
        assert!((s.players[0].x - 62.5).abs() < 0.01, "x={}", s.players[0].x);
    }

    #[test]
    fn teleports_are_not_interpolated() {
        let mut it = Interpolator::default();
        let you = PlayerSim::default();
        it.push(&snap(100, 0, you, 0.0), 100.0 / 60.0);
        it.push(&snap(102, 0, you, 300.0), 102.0 / 60.0);
        it.push(&snap(104, 0, you, 300.0), 104.0 / 60.0);
        it.push(&snap(106, 0, you, 300.0), 106.0 / 60.0);
        let s = it.sample(107.0 / 60.0).unwrap(); // render tick 101
        assert_eq!(s.players[0].x, 0.0);
    }
}
