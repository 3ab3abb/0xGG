# 0xGG

> *Last packet standing.*

A competitive multiplayer 2D arena brawler where you play as a data packet fighting inside a network. Only one packet gets delivered.

Built in Rust with raw `std::net` TCP + UDP sockets for the CS411 Computer Networks project (1 server + 3 clients).

- **Protocol spec:** [`docs/PROTOCOL.md`](docs/PROTOCOL.md)

## Status

| Milestone | State |
|---|---|
| 1. TCP: register/login, lobby, rooms, ready, chat, match start/end, disconnect handling | ✅ done |
| 2. UDP game loop: inputs, snapshots, physics, stomps/pits, rounds, reliable kill events | ✅ done |
| 3. Weapons (Ping Pistol, Broadcast Shotgun, Fork Bomb, Firewall, Checksum Blade), pickup/throw/stun, spawners | ✅ done |
| 4. Prediction, interpolation, lag compensation | ⏳ next |
| 5. Cross-network deploy (bonus) | |

## Requirements

- Rust 1.80+ (`rustup` from https://rustup.rs)

## Run

```bash
# Server (machine 1). Listens on TCP and UDP port 7777.
cargo run --release -p gg-server -- --bind 0.0.0.0:7777

# Clients (machines 2-4)
cargo run --release -p gg-client -- --server <SERVER_IP>
```

Find the server's LAN IP with `ipconfig getifaddr en0` (macOS), `hostname -I` (Linux) or `ipconfig` (Windows). Allow port **7777 TCP and UDP** through the server machine's firewall.

Then: register → create a port (room) → others join → everyone presses **READY** → match starts when the port is full (3/3), or the host presses **START** with 2.

**Controls**

| Action | WASD layout | Arrow layout |
|---|---|---|
| Move | `A` / `D` | `←` / `→` |
| Jump (hold for higher) | `Space` | `Z` |
| Aim up | `W` | `↑` |
| Drop through platform | `S` + jump | `↓` + jump |
| Shoot / use | `J` | `X` |
| Pick up / throw | `K` | `C` |
| Forfeit | `Esc` | `Esc` |

One hit drops a packet. Grab weapons from the blue spawner pads: **Ping Pistol** (hitscan), **Broadcast Shotgun** (spread, recoil), **Fork Bomb** (bounces, explodes, forks into two), **Firewall** (blocks shots for 5 s), **Checksum Blade** (melee, reflects shots mid-swing). Throw an empty gun to stun. Landing on a header or falling into a pit also drops you. Last packet standing wins the round; first to 5 rounds wins.

The footer shows TCP and UDP round-trip times; in a match the bottom-left shows snapshot rate, measured loss and how many inputs are still unacknowledged.

### Demo / test flags (client)

| Flag | Effect |
|---|---|
| `--loss 20` | Drop 20% of UDP packets in each direction (shows the game survives loss) |
| `--login name:pass` | Register or log in automatically |
| `--autojoin` | Join the first open port (or create one) and ready up |
| `--bot` | Sparring bot: grabs weapons, chases the nearest packet, lines up and fires |

Quick local 3-player session (one terminal each, a few seconds apart):

```bash
cargo run --release -p gg-server
cargo run --release -p gg-client -- --login alice:pw123 --autojoin
cargo run --release -p gg-client -- --login bob:pw123 --autojoin --bot
cargo run --release -p gg-client -- --login carol:pw123 --autojoin --bot --loss 20
```

Server options: `--users FILE` (account database, default `users.json`), `--name NAME`. Set `RUST_LOG=debug` for verbose logs.

## Tests

```bash
cargo test --workspace
```

- **Integration tests** start a real server on localhost and drive it with real sockets: auth errors, full 3-player room flow, crash and voluntary-leave mid-match, malformed frames, silent-drop detection via heartbeat timeout, UDP Hello binding, unauthenticated UDP ignored, inputs moving the packet in snapshots.
- **Unit tests** cover framing, the UDP codec (snapshot size budget), deterministic physics, stomps, a full first-to-5 match, duplicate/reordered input handling and event retransmission until ACK.

## Layout

```
crates/shared   protocol messages, TCP framing, UDP packets, physics simulation, maps
crates/server   accept loop, per-connection reader/writer threads, lobby thread, UDP router, 60 Hz game loop per match, account store
crates/client   macroquad app: login, lobby, room, game view, results (0xGG / 0xFF)
assets/maps     ASCII maps (compiled into both binaries)
```
