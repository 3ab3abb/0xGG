//! One game-loop thread per running match: the authoritative simulation.
//!
//! Every tick (60 Hz):
//! 1. drain events (inputs routed by the UDP thread, leaves from the lobby);
//! 2. apply one buffered input per player and step the shared physics;
//! 3. resolve combat (stomps, pits) and round flow;
//! 4. every 2nd tick, send each client a full snapshot over UDP.
//!
//! When the match is decided (or fewer than 2 players remain) the thread
//! reports final stats to the lobby, which announces results over TCP.

use std::collections::VecDeque;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use gg_shared::constants::{MATCH_COUNTDOWN_MS, MIN_PLAYERS_TO_START, ROUNDS_TO_WIN};
use gg_shared::game::{
    overlap, Map, PlayerSim, PLAYER_H, PLAYER_W, SNAPSHOT_EVERY, STOMP_BOUNCE, TICK_RATE,
};
use gg_shared::protocol::MatchEndReason;
use gg_shared::udp::{
    self, flags, Event, EventKind, InputFrame, KillCause, PlayerState, RoundPhase, ServerPacket,
    Snapshot, MAX_EVENTS_PER_SNAPSHOT,
};
use log::{debug, info, warn};

use crate::conn::LobbyEvent;

const fn secs(s: f32) -> u32 {
    (s * TICK_RATE as f32) as u32
}
const FIRST_COUNTDOWN_TICKS: u32 = MATCH_COUNTDOWN_MS * TICK_RATE / 1000;
const ROUND_COUNTDOWN_TICKS: u32 = secs(1.5);
const ROUND_OVER_TICKS: u32 = secs(2.5);
const ROUND_TIME_TICKS: u32 = secs(90.0);
/// If a client's input queue grows past this (burst after a lag spike),
/// skip ahead instead of playing stale inputs late.
const MAX_INPUT_BACKLOG: usize = 4;
/// A falling packet whose feet were at most this far below the victim's top
/// last tick counts as landing on its header.
const STOMP_TOLERANCE: f32 = 4.0;

pub enum RoomEvent {
    Input {
        slot: u8,
        addr: SocketAddr,
        frames: Vec<InputFrame>,
        events_ack: u32,
    },
    PlayerLeft {
        slot: u8,
    },
}

#[derive(Debug, Clone)]
pub struct SlotStats {
    pub slot: u8,
    pub round_wins: u8,
    pub kills: u32,
    pub deaths: u32,
}

#[derive(Clone, Copy)]
enum Phase {
    Countdown(u32),
    Playing(u32),
    RoundOver { winner: Option<u8>, left: u32 },
}

struct Player {
    slot: u8,
    username: String,
    sim: PlayerSim,
    alive: bool,
    connected: bool,
    addr: Option<SocketAddr>,
    inputs: VecDeque<InputFrame>,
    last_queued_seq: u32,
    last_applied_seq: u32,
    buttons: u16,
    outbox: VecDeque<Event>,
    next_event_id: u32,
    stats: SlotStats,
}

struct Game {
    room_id: u32,
    map: Map,
    players: Vec<Player>,
    socket: UdpSocket,
    tick: u32,
    round: u8,
    phase: Phase,
}

pub fn spawn(
    room_id: u32,
    map: Map,
    slots: Vec<(u8, String)>,
    socket: UdpSocket,
    rx: Receiver<RoomEvent>,
    lobby: Sender<LobbyEvent>,
) -> io::Result<()> {
    let game = Game::new(room_id, map, slots, socket);
    thread::Builder::new()
        .name(format!("room-{room_id}"))
        .spawn(move || game.run(rx, lobby))?;
    Ok(())
}

fn ticks_to_ms(t: u32) -> u32 {
    t * 1000 / TICK_RATE
}

impl Game {
    fn new(room_id: u32, map: Map, slots: Vec<(u8, String)>, socket: UdpSocket) -> Game {
        let players = slots
            .into_iter()
            .map(|(slot, username)| Player {
                slot,
                username,
                sim: PlayerSim::default(),
                alive: false,
                connected: true,
                addr: None,
                inputs: VecDeque::new(),
                last_queued_seq: 0,
                last_applied_seq: 0,
                buttons: 0,
                outbox: VecDeque::new(),
                next_event_id: 1,
                stats: SlotStats {
                    slot,
                    round_wins: 0,
                    kills: 0,
                    deaths: 0,
                },
            })
            .collect();
        let mut game = Game {
            room_id,
            map,
            players,
            socket,
            tick: 0,
            round: 0,
            phase: Phase::Countdown(0),
        };
        game.start_round(FIRST_COUNTDOWN_TICKS);
        game
    }

    fn run(mut self, rx: Receiver<RoomEvent>, lobby: Sender<LobbyEvent>) {
        info!(
            "room {}: game loop started on map '{}'",
            self.room_id, self.map.name
        );
        let tick_len = Duration::from_secs_f64(1.0 / TICK_RATE as f64);
        let mut next = Instant::now();

        let reason = loop {
            loop {
                match rx.try_recv() {
                    Ok(ev) => self.on_event(ev),
                    Err(TryRecvError::Empty) => break,
                    // Lobby dropped the room entirely: nobody to report to.
                    Err(TryRecvError::Disconnected) => return,
                }
            }
            if self.players.iter().filter(|p| p.connected).count() < MIN_PLAYERS_TO_START {
                break MatchEndReason::NotEnoughPlayers;
            }
            if self.step() {
                break MatchEndReason::Normal;
            }
            if self.tick.is_multiple_of(SNAPSHOT_EVERY) {
                self.send_snapshots();
            }

            // Fixed-rate scheduling against an absolute deadline, so small
            // sleep inaccuracies don't accumulate into drift.
            next += tick_len;
            let now = Instant::now();
            if next > now {
                thread::sleep(next - now);
            } else if now - next > Duration::from_millis(250) {
                warn!(
                    "room {}: game loop fell {:?} behind, resyncing",
                    self.room_id,
                    now - next
                );
                next = now;
            }
        };

        self.send_snapshots();
        info!(
            "room {}: game loop finished ({reason:?}) after {} ticks",
            self.room_id, self.tick
        );
        let stats = self.players.iter().map(|p| p.stats.clone()).collect();
        let _ = lobby.send(LobbyEvent::MatchFinished {
            room_id: self.room_id,
            reason,
            stats,
        });
    }

    fn on_event(&mut self, ev: RoomEvent) {
        match ev {
            RoomEvent::Input {
                slot,
                addr,
                frames,
                events_ack,
            } => {
                let Some(p) = self
                    .players
                    .iter_mut()
                    .find(|p| p.slot == slot && p.connected)
                else {
                    return;
                };
                p.addr = Some(addr);
                // Redundant frames: keep only ones we haven't seen. Old,
                // duplicated or reordered packets are naturally ignored.
                for f in frames {
                    if f.seq > p.last_queued_seq {
                        p.inputs.push_back(f);
                        p.last_queued_seq = f.seq;
                    }
                }
                while p.outbox.front().is_some_and(|e| e.id <= events_ack) {
                    p.outbox.pop_front();
                }
            }
            RoomEvent::PlayerLeft { slot } => {
                if let Some(p) = self.players.iter_mut().find(|p| p.slot == slot) {
                    info!(
                        "room {}: 0x{slot:02X} ({}) left the game loop",
                        self.room_id, p.username
                    );
                    p.connected = false;
                    p.alive = false;
                }
            }
        }
    }

    fn start_round(&mut self, countdown: u32) {
        self.round += 1;
        let spawns = &self.map.spawns;
        let center = self.map.pixel_width() / 2.0;
        // Rotate spawn assignment every round so nobody keeps the best spot.
        let offset = self.round as usize * 2;
        for (i, p) in self.players.iter_mut().enumerate() {
            p.alive = p.connected;
            let (x, y) = spawns[(i * spawns.len() / 3 + offset) % spawns.len()];
            p.sim = PlayerSim::spawn_at(x, y, x < center);
            p.sim.prev_buttons = p.buttons; // a held jump must not fire on spawn
        }
        self.phase = Phase::Countdown(countdown);
    }

    /// Advance one tick. Returns true when the match is won.
    fn step(&mut self) -> bool {
        self.tick += 1;
        for p in &mut self.players {
            if p.inputs.len() > MAX_INPUT_BACKLOG {
                let skip = p.inputs.len() - 2;
                p.inputs.drain(..skip);
            }
            // No new input this tick (late or lost packet): repeat the last
            // known buttons, the best guess for a held key.
            if let Some(f) = p.inputs.pop_front() {
                p.buttons = f.buttons;
                p.last_applied_seq = f.seq;
            }
        }

        match self.phase {
            Phase::Countdown(left) => {
                for p in &mut self.players {
                    p.sim.prev_buttons = p.buttons;
                }
                self.phase = if left <= 1 {
                    Phase::Playing(ROUND_TIME_TICKS)
                } else {
                    Phase::Countdown(left - 1)
                };
            }
            Phase::Playing(left) => {
                self.simulate(true);
                let alive: Vec<u8> = self
                    .players
                    .iter()
                    .filter(|p| p.alive)
                    .map(|p| p.slot)
                    .collect();
                if alive.len() <= 1 || left <= 1 {
                    let winner = if alive.len() == 1 {
                        Some(alive[0])
                    } else {
                        None
                    };
                    if let Some(p) = self.players.iter_mut().find(|p| Some(p.slot) == winner) {
                        p.stats.round_wins += 1;
                    }
                    let score: Vec<String> = self
                        .players
                        .iter()
                        .map(|p| format!("0x{:02X}:{}", p.slot, p.stats.round_wins))
                        .collect();
                    info!(
                        "room {}: round {} won by {winner:?} [{}]",
                        self.room_id,
                        self.round,
                        score.join(" ")
                    );
                    self.phase = Phase::RoundOver {
                        winner,
                        left: ROUND_OVER_TICKS,
                    };
                } else {
                    self.phase = Phase::Playing(left - 1);
                }
            }
            Phase::RoundOver { winner, left } => {
                self.simulate(false); // victory lap: movement only, no kills
                if left <= 1 {
                    let won = self
                        .players
                        .iter()
                        .any(|p| Some(p.slot) == winner && p.stats.round_wins >= ROUNDS_TO_WIN);
                    if won {
                        return true;
                    }
                    self.start_round(ROUND_COUNTDOWN_TICKS);
                } else {
                    self.phase = Phase::RoundOver {
                        winner,
                        left: left - 1,
                    };
                }
            }
        }
        false
    }

    fn simulate(&mut self, combat: bool) {
        let prev: Vec<(f32, f32)> = self.players.iter().map(|p| (p.sim.x, p.sim.y)).collect();
        for p in self.players.iter_mut().filter(|p| p.alive) {
            p.sim.step(&self.map, p.buttons);
        }
        if !combat {
            return;
        }

        let mut kills: Vec<(Option<usize>, usize, KillCause)> = Vec::new();
        for (i, a) in self.players.iter().enumerate() {
            for (j, b) in self.players.iter().enumerate() {
                if i == j || !a.alive || !b.alive || a.sim.vy <= 0.0 {
                    continue;
                }
                let touching = overlap(
                    a.sim.x, a.sim.y, PLAYER_W, PLAYER_H, b.sim.x, b.sim.y, PLAYER_W, PLAYER_H,
                );
                let came_from_above = prev[i].1 + PLAYER_H <= prev[j].1 + STOMP_TOLERANCE;
                if touching && came_from_above {
                    kills.push((Some(i), j, KillCause::Stomp));
                }
            }
        }
        for (j, p) in self.players.iter().enumerate() {
            if p.alive && p.sim.fell_out(&self.map) {
                kills.push((None, j, KillCause::Fall));
            }
        }

        for (killer, victim, cause) in kills {
            if !self.players[victim].alive {
                continue; // already dropped this tick
            }
            self.players[victim].alive = false;
            self.players[victim].stats.deaths += 1;
            let killer_slot = killer.map(|k| {
                let k = &mut self.players[k];
                k.stats.kills += 1;
                k.sim.vy = STOMP_BOUNCE;
                k.slot
            });
            let victim_slot = self.players[victim].slot;
            debug!(
                "room {}: 0x{victim_slot:02X} dropped by {killer_slot:?} ({cause:?})",
                self.room_id
            );
            self.push_event(EventKind::Kill {
                killer: killer_slot,
                victim: victim_slot,
                cause,
            });
        }
    }

    /// Queue a reliable event for every connected client.
    fn push_event(&mut self, kind: EventKind) {
        for p in self.players.iter_mut().filter(|p| p.connected) {
            p.outbox.push_back(Event {
                id: p.next_event_id,
                kind: kind.clone(),
            });
            p.next_event_id += 1;
        }
    }

    fn send_snapshots(&self) {
        let phase = match self.phase {
            Phase::Countdown(t) => RoundPhase::Countdown {
                ms_left: ticks_to_ms(t) as u16,
            },
            Phase::Playing(t) => RoundPhase::Playing {
                ms_left: ticks_to_ms(t),
            },
            Phase::RoundOver { winner, left } => RoundPhase::RoundOver {
                winner,
                ms_left: ticks_to_ms(left) as u16,
            },
        };
        let scores: Vec<(u8, u8)> = self
            .players
            .iter()
            .map(|p| (p.slot, p.stats.round_wins))
            .collect();
        let states: Vec<PlayerState> = self
            .players
            .iter()
            .map(|p| {
                let mut f = 0;
                if p.alive {
                    f |= flags::ALIVE;
                }
                if p.sim.facing_right {
                    f |= flags::FACING_RIGHT;
                }
                if p.sim.grounded {
                    f |= flags::GROUNDED;
                }
                if p.connected {
                    f |= flags::CONNECTED;
                }
                PlayerState {
                    slot: p.slot,
                    x: p.sim.x,
                    y: p.sim.y,
                    vx: p.sim.vx,
                    vy: p.sim.vy,
                    flags: f,
                }
            })
            .collect();

        for p in self.players.iter().filter(|p| p.connected) {
            let Some(addr) = p.addr else { continue }; // no input received yet
            let snapshot = Snapshot {
                tick: self.tick,
                ack_input_seq: p.last_applied_seq,
                round: self.round,
                phase,
                scores: scores.clone(),
                players: states.clone(),
                events: p
                    .outbox
                    .iter()
                    .take(MAX_EVENTS_PER_SNAPSHOT)
                    .cloned()
                    .collect(),
            };
            match udp::encode(&ServerPacket::Snapshot(snapshot)) {
                Ok(bytes) => {
                    if let Err(e) = self.socket.send_to(&bytes, addr) {
                        debug!("room {}: snapshot to {addr} failed: {e}", self.room_id);
                    }
                }
                Err(e) => warn!("room {}: {e}", self.room_id),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gg_shared::game::builtin_map;

    fn game() -> Game {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let slots = vec![
            (1, "a".to_string()),
            (2, "b".to_string()),
            (3, "c".to_string()),
        ];
        Game::new(1, builtin_map("router").unwrap(), slots, socket)
    }

    fn run_until_playing(g: &mut Game) {
        for _ in 0..FIRST_COUNTDOWN_TICKS + 10 {
            if matches!(g.phase, Phase::Playing(_)) {
                return;
            }
            g.step();
        }
        panic!("never reached Playing");
    }

    #[test]
    fn spawns_are_distinct() {
        let g = game();
        let mut pos: Vec<_> = g
            .players
            .iter()
            .map(|p| (p.sim.x as i32, p.sim.y as i32))
            .collect();
        pos.dedup();
        assert_eq!(pos.len(), 3);
    }

    #[test]
    fn players_frozen_during_countdown() {
        let mut g = game();
        g.players[0].buttons = gg_shared::game::buttons::RIGHT;
        let x0 = g.players[0].sim.x;
        g.step();
        assert_eq!(g.players[0].sim.x, x0);
    }

    #[test]
    fn stomp_kills_and_bounces() {
        let mut g = game();
        run_until_playing(&mut g);
        for _ in 0..30 {
            g.step(); // let everyone settle on the ground
        }
        let (bx, by) = (g.players[1].sim.x, g.players[1].sim.y);
        g.players[0].sim.x = bx;
        g.players[0].sim.y = by - PLAYER_H - 2.0;
        g.players[0].sim.vy = 200.0;
        g.players[0].sim.grounded = false;
        g.step();
        assert!(!g.players[1].alive, "victim survived the stomp");
        assert!(g.players[0].alive);
        assert_eq!(g.players[0].stats.kills, 1);
        assert_eq!(g.players[1].stats.deaths, 1);
        assert!(g.players[0].sim.vy < 0.0, "stomper should bounce");
        // Every connected client gets the reliable kill event.
        for p in &g.players {
            assert!(matches!(
                p.outbox.back().map(|e| &e.kind),
                Some(EventKind::Kill {
                    killer: Some(1),
                    victim: 2,
                    cause: KillCause::Stomp
                })
            ));
        }
    }

    #[test]
    fn full_match_first_to_five() {
        let mut g = game();
        let mut done = false;
        for _ in 0..TICK_RATE * 60 * 5 {
            if matches!(g.phase, Phase::Playing(_)) {
                // 0x02 and 0x03 jump into the backbone every round.
                for p in &mut g.players[1..] {
                    if p.alive {
                        p.sim.y = 100_000.0;
                    }
                }
            }
            if g.step() {
                done = true;
                break;
            }
        }
        assert!(done, "match never finished");
        assert_eq!(g.players[0].stats.round_wins, ROUNDS_TO_WIN);
        assert_eq!(g.players[1].stats.deaths, ROUNDS_TO_WIN as u32);
        assert_eq!(g.round, ROUNDS_TO_WIN);
    }

    #[test]
    fn redundant_and_reordered_inputs_applied_once_in_order() {
        let mut g = game();
        let addr: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let f = |seq| InputFrame {
            seq,
            buttons: seq as u16,
            view_tick: 0,
        };
        g.on_event(RoomEvent::Input {
            slot: 1,
            addr,
            frames: vec![f(1), f(2), f(3)],
            events_ack: 0,
        });
        g.on_event(RoomEvent::Input {
            slot: 1,
            addr,
            frames: vec![f(2), f(3), f(4)],
            events_ack: 0,
        });
        g.on_event(RoomEvent::Input {
            slot: 1,
            addr,
            frames: vec![f(1), f(2)],
            events_ack: 0,
        }); // late duplicate
        let seqs: Vec<u32> = g.players[0].inputs.iter().map(|i| i.seq).collect();
        assert_eq!(seqs, vec![1, 2, 3, 4]);
    }

    #[test]
    fn events_are_resent_until_acked() {
        let mut g = game();
        g.push_event(EventKind::Kill {
            killer: None,
            victim: 2,
            cause: KillCause::Fall,
        });
        g.push_event(EventKind::Kill {
            killer: None,
            victim: 3,
            cause: KillCause::Fall,
        });
        assert_eq!(g.players[0].outbox.len(), 2);
        let addr: SocketAddr = "127.0.0.1:9".parse().unwrap();
        g.on_event(RoomEvent::Input {
            slot: 1,
            addr,
            frames: vec![],
            events_ack: 1,
        });
        assert_eq!(
            g.players[0].outbox.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![2]
        );
        g.on_event(RoomEvent::Input {
            slot: 1,
            addr,
            frames: vec![],
            events_ack: 2,
        });
        assert!(g.players[0].outbox.is_empty());
        assert_eq!(g.players[1].outbox.len(), 2, "acks are per client");
    }
}
