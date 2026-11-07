# 0xGG — Network Protocol Specification

> *Last packet standing.*
> Version: **v1** (protocol version byte = `1`) · Status: draft · 2026-11-07

This document is the contract between `server` and `client`. If code and spec disagree, fix one of them — never leave them diverged.

---

## 1. Architecture overview

```
                 ┌──────────────────────── SERVER (authoritative) ────────────────────────┐
                 │                                                                        │
  Client 0x01 ───┼── TCP :7777 ──▶ conn thread ──┐                                        │
  Client 0x02 ───┼── TCP :7777 ──▶ conn thread ──┼──▶ Lobby thread (owns users/rooms)     │
  Client 0x03 ───┼── TCP :7777 ──▶ conn thread ──┘         │ spawns                       │
                 │                                         ▼                              │
  all clients ◀──┼── UDP :7777 ◀─▶ UDP thread ◀──────▶ Room game-loop thread (60 Hz)     │
                 └────────────────────────────────────────────────────────────────────────┘
```

- **One server, N clients** (rooms hold 2–3 players; the demo uses 3).
- **Server is authoritative**: clients send *inputs only*; the server runs physics, hit detection, scoring. Clients cannot cheat by sending positions.
- **Same port number for TCP and UDP (7777).** TCP and UDP port spaces are independent, so this is legal and keeps firewall setup to "open 7777/tcp and 7777/udp".
- **Concurrency model: threads + message passing, no shared mutable state.** Each thread owns its data; they communicate through channels (`crossbeam-channel`). No `tokio` — we want the raw `std::net` sockets visible in the code we present.

---

## 2. Protocol choice — which interaction uses what, and why

| Interaction | Transport | Why |
|---|---|---|
| Register / login | **TCP** | Must arrive exactly once, in order. One-shot, latency-insensitive. |
| Lobby, room list, create / join / leave room, ready | **TCP** | Every message changes shared state; a lost "join" would desync the lobby. |
| Chat | **TCP** | Text must be complete and ordered. |
| Match start / match end / final rankings | **TCP** | Critical lifecycle events; losing "match ended" leaves a client stuck. |
| Player disconnected notice | **TCP** | Must reach everyone reliably. |
| Player input (60 Hz) | **UDP** | Each packet is superseded 16 ms later. Retransmitting an old input is useless; TCP's head-of-line blocking would *delay the new input behind the old one*. Loss is covered by **redundancy** (each packet repeats the last 3 inputs). |
| World snapshots (30 Hz) | **UDP** | Full state each time — a lost snapshot is fixed by the next one 33 ms later. Same head-of-line argument. |
| In-match events (kills, pickups, firewall spawn, explosions) | **Reliable-over-UDP** | Must not be lost, *but* must not stall movement either. We piggyback them on snapshots and resend until ACKed (§5.4). This is the "best of both" channel. |

**Key slide sentence:** TCP gives reliability *and ordering*, and ordering is exactly what hurts real-time data: one lost segment blocks every later one until retransmission (~1 RTT+). For state that is replaced 30–60 times per second, freshness beats completeness.

Extra TCP tuning: **`TCP_NODELAY` is set on every TCP socket** (disables Nagle, which would otherwise delay small messages by up to ~40 ms waiting to coalesce).

---

## 3. Connection lifecycle

```
Client                                   Server
  │──── TCP connect :7777 ───────────────▶│
  │◀─── Welcome {protocol, server_name} ──│
  │──── Login {username, password} ──────▶│   (or Register)
  │◀─── AuthOk {player_id, token, udp_port}│
  │                                        │
  │──── UDP Hello {token} ───────────────▶│   repeated every 200 ms
  │◀─── UDP HelloAck ─────────────────────│   until acked (max 25 tries = 5 s)
  │        (server now maps UDP addr → player via token)
  │                                        │
  │──── ListRooms / CreateRoom / JoinRoom ▶│
  │◀─── RoomList / RoomState (pushed) ────│
  │──── SetReady {true} ─────────────────▶│
  │◀─── MatchStarting {map, slots, ...} ──│   room full (3) & all ready,
  │                                        │   or host sends StartMatch (≥2, all ready)
  │                                        │
  │════ UDP Input (60 Hz) ═══════════════▶│
  │◀═══ UDP Snapshot (30 Hz) ═════════════│   rounds play out...
  │                                        │
  │◀─── MatchEnded {rankings, winner} ────│   (TCP)
  │        back to room                    │
```

### Client state machine
`Disconnected → Connected → Authenticated (in lobby) → InRoom → InMatch → InRoom → …`

The server rejects any message that is invalid for the client's current state with `Error { code: "bad_state" }` — it never panics.

---

## 4. TCP channel

### 4.1 Framing
TCP is a byte stream, not a message stream, so every message is framed:

```
┌───────────────────────┬───────────────────────────────┐
│ length: u32 big-endian│ payload: `length` bytes (JSON)│
└───────────────────────┴───────────────────────────────┘
```

- Max payload: **64 KiB**. Larger ⇒ protocol error ⇒ connection closed.
- Reader loops until it has 4 bytes, then until it has `length` bytes (handles partial reads / coalesced messages).
- **Payload is JSON** (`serde_json`, externally tagged enum: `{"type": "Login", ...}`).
  *Why JSON on TCP:* low frequency, so size doesn't matter; human-readable in Wireshark, which is great for debugging and for the demo.

### 4.2 Client → Server messages

| Message | Fields | Valid in state |
|---|---|---|
| `Register` | `username: String (3–16, [A-Za-z0-9_])`, `password: String` | Connected |
| `Login` | `username`, `password` | Connected |
| `Logout` | — (server cleans up and closes the connection) | any authenticated |
| `ListRooms` | — | Lobby |
| `CreateRoom` | `name: String (1–24)` | Lobby |
| `JoinRoom` | `room_id: u32` | Lobby |
| `LeaveRoom` | — | InRoom |
| `SetReady` | `ready: bool` | InRoom |
| `StartMatch` | — (host only; ≥2 players, all ready) | InRoom |
| `Chat` | `text: String (1–200)` | Lobby / InRoom / InMatch |
| `Ping` | `nonce: u64` | any |

### 4.3 Server → Client messages

| Message | Fields | Notes |
|---|---|---|
| `Welcome` | `protocol: u8`, `server_name` | First message on connect. Client disconnects if `protocol` mismatches. |
| `AuthOk` | `player_id: u32`, `username`, `token: u64`, `udp_port: u16` | `token` is random, used to authenticate UDP. |
| `AuthError` | `reason` | `bad_credentials`, `user_exists`, `already_logged_in`, `invalid_username` |
| `RoomList` | `rooms: [{id, name, players, max, in_match}]` | **Pushed** to all lobby clients whenever it changes. |
| `RoomState` | `room_id, name, host_id, players: [{player_id, username, ready}]` | **Pushed** to room members on any change. |
| `LeftRoom` | — | Confirms `LeaveRoom`; client is back in the lobby (a `RoomList` follows). |
| `Chat` | `from: String`, `text`, `scope: "lobby" \| "room"` | |
| `MatchStarting` | `map: String`, `slots: [{slot: u8, player_id, username}]`, `your_slot: u8`, `countdown_ms: u32` | `slot` is the in-match hex id `0x01..0x03`; color derives from slot (orange / blue / white). |
| `PlayerDisconnected` | `slot: u8`, `username`, `reason: "closed" \| "timeout" \| "error" \| "left"` | Broadcast to the room. |
| `MatchEnded` | `reason: "normal" \| "not_enough_players"`, `winner_slot: Option<u8>`, `rankings: [{slot, username, round_wins, kills, deaths, disconnected}]` | Sorted by rank, winner first. |
| `Error` | `code`, `message` | Non-fatal. |
| `Pong` | `nonce: u64` | |

### 4.4 Accounts
- Stored server-side in `users.json`: `{username, salt, password_hash}` (SHA-256 over salt‖password).
- **Known limitation (say it on the slides):** the password crosses the wire in plaintext. Production would wrap TCP in TLS. Out of scope for this project.
- A username can be logged in **once**; a second login gets `already_logged_in`.

---

## 5. UDP channel

### 5.1 Packet layout
All UDP packets are **binary** (`postcard` serialization of a Rust enum), prefixed by a fixed header:

```
┌────────────┬──────────┬──────────┬───────────────────────┐
│ magic "GG" │ version  │ kind: u8 │ body (postcard)        │
│  2 bytes   │  u8 = 1  │          │                        │
└────────────┴──────────┴──────────┴───────────────────────┘
```

- Packets with wrong magic/version or that fail to decode are **silently dropped** (and counted in server stats). Never panic on network input.
- **Max datagram size: 1200 bytes** — safely below typical path MTU (1500 − IP/UDP headers − VPN/tunnel overhead) so packets are never IP-fragmented.
- *Why binary on UDP:* sent 30–60×/s per player; compactness directly reduces bandwidth and loss probability.

### 5.2 Client → Server

| Kind | Body | Rate |
|---|---|---|
| `Hello` | `token: u64` | every 200 ms until `HelloAck` |
| `Input` | `token: u64`, `inputs: [InputFrame; up to 3]` (newest last), `last_snapshot_tick: u32`, `events_ack: u32` | **60 Hz** during a match |
| `Ping` | `token`, `client_time_ms: u32` | 1 Hz (RTT display) |

```rust
struct InputFrame {
    seq: u32,        // increments every client tick (60 Hz)
    buttons: u16,    // bitfield, see below
    view_tick: u32,  // server tick the client was *rendering* (for lag compensation)
}
// buttons:
// 0 LEFT  1 RIGHT  2 JUMP  3 DOWN  4 AIM_UP  5 SHOOT  6 INTERACT (pick up, or throw what you hold)
// Presses (SHOOT/INTERACT edges) are detected on the SERVER's tick from consecutive
// applied frames, so a press is never lost even if its packet arrived late.
```

**Redundancy instead of retransmission:** each `Input` carries the last 3 frames. The server applies any frame with `seq > last_applied_seq` and ignores the rest. A single lost packet costs nothing; three in a row are needed to lose an input.

### 5.3 Server → Client

| Kind | Body | Rate |
|---|---|---|
| `HelloAck` | — | reply to `Hello` |
| `Snapshot` | see below | **30 Hz** |
| `Pong` | `client_time_ms` | reply to `Ping` |

```rust
struct Snapshot {
    tick: u32,                       // server tick (60 Hz clock)
    ack_input_seq: u32,              // last InputFrame.seq applied for THIS client (reconciliation)
    phase: RoundPhase,               // Countdown{ms_left} | Playing | RoundOver{winner_slot}
    round: u8,
    scores: [(u8 /*slot*/, u8 /*round_wins*/); ≤3],
    players: [PlayerState; ≤3],
    items: [ItemState],              // weapons on the floor or flying after a throw
    projectiles: [ProjectileState; ≤32],  // pellets, bombs, forked shards
    firewalls: [FirewallState],
    events: [Event; ≤20],            // reliable events not yet acked by this client (§5.4)
}
struct PlayerState { slot: u8, x: f32, y: f32, vx: f32, vy: f32,
                     flags: u8 /*alive, facing_right, grounded, connected, aiming_up, stunned, swinging*/,
                     weapon: Option<Weapon>, ammo: u8 }
struct ItemState { id: u16, weapon: Weapon, ammo: u8, x: f32, y: f32, flying: bool }
struct ProjectileState { id: u16, kind: Pellet | Bomb | Shard, x: f32, y: f32 }
struct FirewallState { id: u16, x: f32, y: f32, ms_left: u16 }
```

**Snapshots are full state, not deltas.** No snapshot depends on a previous one, so loss never corrupts state. Client rule: **drop any snapshot whose `tick` ≤ latest received tick** (handles reordering and duplicates).

**Size budget (asserted in `udp::tests`):** the worst possible snapshot (32 projectiles, 20 tracer events, 10 items, 6 firewalls) encodes to **1028 bytes**, under the 1200-byte cap. A first draft with 48 projectiles / 24 events was 1284 bytes and failed this test, which is why the caps exist. Typical snapshots are 150–300 bytes.

Bandwidth estimate (typical ~250 B/snapshot): 450 B × 30 Hz × 3 clients ≈ **40 KB/s** total downstream from the server. Inputs ≈ 45 B × 60 Hz ≈ 2.7 KB/s per client.

### 5.4 Reliable events over UDP

Events that must not be lost but must not wait behind TCP:

```rust
struct Event { id: u32, kind: EventKind }   // id: per-client, monotonically increasing
enum EventKind {
    Kill { killer: Option<u8>, victim: u8, cause: KillCause },  // Stomp | Fall | Weapon(w) | Reflect
    Shot { x0, y0, x1, y1 },          // hitscan tracer
    Explosion { x, y, radius },
    Stun { by: u8, victim: u8 },      // hit by a thrown weapon
}
```

Design rule: **anything that is persistent state goes in the snapshot, not in events.** Pickups, throws and firewalls are visible as `items` / `firewalls` / `PlayerState.weapon` in every snapshot, so losing one snapshot loses nothing. Only *instantaneous* things (a kill, a tracer, a blast, a stun) are events, because they'd be invisible if the one snapshot showing them were lost.

```text
```

Algorithm (cumulative ACK, like TCP's ACK number):
1. Server appends each event to a **per-client outbox** with the next `id`.
2. Every snapshot to that client includes **all outbox events with `id > client's events_ack`**.
3. Client processes events in `id` order, skipping ones it already handled, and sends back the highest contiguous `id` it has as `events_ack` in every `Input`.
4. Server drops outbox entries `≤ events_ack`.

Since snapshots go out every 33 ms, an event is effectively retransmitted every 33 ms until acknowledged — no timers needed. Events are mostly *cosmetic triggers* (sound, kill feed, particles); the authoritative consequence (player is dead, firewall exists) is also visible in the snapshot state itself.

### 5.5 UDP address binding & NAT
- The server **never trusts an IP the client claims**. It uses the source address it observes on `Hello`, and binds it to the player identified by `token`.
- If an `Input` with a valid token arrives from a **different** address (NAT rebinding, Wi-Fi switch), the server updates the binding.
- Because the client **sends first**, its NAT creates a mapping that lets the server's replies through. That is why clients behind home routers/hotspots work without any port forwarding — only the server needs a public, reachable port (bonus section).

---

## 6. Timing constants

| Constant | Value | Rationale |
|---|---|---|
| Server simulation tick | **60 Hz** (16.67 ms) | Smooth platformer physics, small hit windows. |
| Snapshot send rate | **30 Hz** | Halves bandwidth; interpolation hides the gap. |
| Client input send rate | **60 Hz** | One per simulation tick. |
| Interpolation delay (remote players) | **100 ms** | ≈ 3 snapshots buffered → tolerates 2 lost in a row. |
| Lag-compensation max rewind | **200 ms** | Beyond this, shooter's view is too stale; prevents abuse. |
| TCP app heartbeat | `Ping` every **2 s** | |
| TCP heartbeat timeout | **6 s** of silence | |
| UDP silence timeout (in match) | **3 s** without `Input` | |
| UDP `Hello` retry | every 200 ms, give up after 5 s | Then client shows "UDP blocked — check firewall". |

---

## 7. Gameplay rules that touch the protocol

- Match = rounds; **first to 5 round wins** takes the match.
- Round: 3 s `Countdown` → `Playing` → last packet alive ⇒ `RoundOver{winner}` (2 s) → next round. If everyone dies the same tick: draw, nobody scores.
- Scores, ranks and winner are **computed only on the server**; clients display what `Snapshot.scores` and `MatchEnded.rankings` say.
- Physics code lives in the `shared` crate so client prediction runs the **identical** simulation the server runs.

---

## 7b. End-of-match screens

- **Winner:** big `0xGG`, `DELIVERED`, `200 OK - last packet standing.`
- **Losers:** big `0xFF` with a fake fatal error, e.g. `FATAL 0xFF: PACKET_DROPPED / E_TTL_EXCEEDED: time to live reached 0 before delivery`.
- **Draw (no winner):** `0x00 NULL ROUTE`.

---

## 7c. Weapons (server rules)

| Weapon | Ammo | Behavior |
|---|---|---|
| Ping Pistol | 6 | Hitscan ray (2 px steps, 420 px); small knockback |
| Broadcast Shotgun | 2 | 5 pellets over a 0.45 rad cone, ~140 px range; strong recoil |
| Fork Bomb | 1 | Bouncing grenade, 1.8 s fuse, r=34; then `fork()`s into 2 shards (r=22). Hurts its owner too |
| Firewall | 1 | 6×40 px wall for 5 s; blocks rays, pellets and bombs (not players) |
| Checksum Blade | ∞ | 16-tick swing; first 8 ticks hit and **reflect** rays/pellets back at the shooter; blade vs blade parries |

- INTERACT throws the held weapon (stuns for 0.6 s on hit, no kill) or picks up the nearest item within 18 px.
- 7 spawners (`W` in the map) restock with a random weapon each round and 8 s after being emptied. Dying drops your weapon.
- Fairness: within one tick, blade swings resolve **before** shots, so the outcome never depends on slot order.

---

## 8. Disconnection handling

### Detection (two independent layers)
1. **TCP**: `read()` returns `0` (clean close) or an error (`ECONNRESET`, crash, killed process) ⇒ immediate.
2. **Heartbeats**: a silent drop (Wi-Fi off, laptop lid closed, cable pulled) produces *no* TCP error for minutes. So:
   - in lobby/room: no TCP traffic for 6 s ⇒ dead;
   - in match: no UDP `Input` for 3 s ⇒ dead (faster, since inputs flow at 60 Hz).

Whichever layer fires first, the server runs the **same cleanup path** once (idempotent).

### Policy
| Where | What happens |
|---|---|
| Lobby | Player removed from online list. |
| Room | Removed from room → `RoomState` pushed. If host left, host passes to the next player. Empty room is deleted → `RoomList` pushed. |
| Match | Their packet is **dropped** (dies with a `DROPPED (disconnected)` kill-feed line) and excluded from future rounds. `PlayerDisconnected` broadcast over TCP. Their stats remain in the final ranking, flagged disconnected. If **< 2 connected players remain** ⇒ match ends immediately, `MatchEnded{reason: "not_enough_players"}`, survivor wins. |

### Robustness rules (server must never crash)
- No `unwrap()` / `expect()` on anything derived from network input.
- Each connection runs in its own thread; a panic or error there kills only that connection (the thread is joined/cleaned up, channel disconnect is detected by the lobby).
- Malformed TCP frame ⇒ close that connection. Malformed UDP datagram ⇒ drop it.

**Stretch:** reconnect within 15 s using the same token to resume the match.

---

## 9. Netcode techniques (client)

| Technique | Applies to | Summary |
|---|---|---|
| **Client-side prediction** | Your own packet | Apply your input locally immediately → 0 ms perceived input lag. |
| **Server reconciliation** | Your own packet | On snapshot: reset to server state, re-apply inputs with `seq > ack_input_seq`. Smooth small corrections over a few frames. |
| **Entity interpolation** | Other players, projectiles | Render at `now − 100 ms`, lerping between the two surrounding snapshots. |
| **Lag compensation** | Ping Pistol (hitscan), Checksum Blade | Server keeps 1 s of hitbox history; on SHOOT it rewinds targets to the shooter's `view_tick` (clamped to 200 ms) and tests the hit there. |

Build order: plain snapshots first (works, a bit laggy) → prediction/reconciliation → interpolation → lag compensation. Each layer is independently demoable.

---

## 10. Project layout

```
project-2/
├── Cargo.toml                # workspace
├── docs/PROTOCOL.md          # this file
├── assets/maps/*.txt         # ASCII maps
└── crates/
    ├── shared/   (gg-shared) # protocol types, TCP framing, UDP header, physics, map loader, constants
    ├── server/   (gg-server) # main, lobby, room, game loop, tcp conn, udp io, users store
    └── client/   (gg-client) # macroquad app: screens (login, lobby, room, game, results), net threads, prediction, interpolation, render
```

Crate names start with `gg-` because Rust package names can't start with a digit.

### Map format (ASCII, 1 char = 1 tile of 16 px)
```
#  solid block          =  one-way platform (jump through from below)
S  spawn point          W  weapon spawner
L  latency zone (stretch)   .  empty
```

---

## 11. Cross-network setup (bonus)

- Server runs on a machine with a **public IP** (plan A: Azure for Students VM) with inbound **7777/tcp and 7777/udp** open in the cloud firewall (NSG) and OS firewall.
- Clients connect with `gg-client --server <public-ip>:7777` from any network (home Wi-Fi, phone hotspot, campus).
- Plan B: **Tailscale** on all 4 machines; clients use the server's `100.x.y.z` address. Honest framing for slides: an encrypted overlay (WireGuard) across different physical networks.
- Demo proof: show each client's different public IP (`curl ifconfig.me`) and the server's connection log listing those source addresses.
