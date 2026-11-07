# 0xGG

> *Last packet standing.*

A competitive multiplayer 2D arena brawler where you play as a data packet fighting inside a network. Only one packet gets delivered.

Built in Rust with raw `std::net` TCP + UDP sockets for the CS411 Computer Networks project (1 server + 3 clients).

- **Protocol spec:** [`docs/PROTOCOL.md`](docs/PROTOCOL.md)

## Status

| Milestone | State |
|---|---|
| 1. TCP: register/login, lobby, rooms, ready, chat, match start/end, disconnect handling | ✅ done |
| 2. UDP game loop: inputs, snapshots, physics, rounds | ⏳ next |
| 3. Weapons, reliable UDP events, scoring | |
| 4. Prediction, interpolation, lag compensation | |
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

Then: register → create a port (room) → others join → everyone presses **READY** → match starts.

Server options: `--users FILE` (account database, default `users.json`), `--name NAME`. Set `RUST_LOG=debug` for verbose logs.

## Tests

```bash
cargo test --workspace
```

Integration tests start a real server on localhost and drive it with real TCP clients: auth errors, full 3-player room flow, crash and voluntary-leave mid-match, malformed frames, and silent-drop detection via heartbeat timeout.

## Layout

```
crates/shared   protocol messages, TCP framing, constants
crates/server   accept loop, per-connection reader/writer threads, lobby thread, account store
crates/client   macroquad app: login, lobby, room, match, results (0xGG / 0xFF)
```
