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
| 4. Client-side prediction + reconciliation, entity interpolation | ✅ done |
| 4b. Server lag compensation (rewind hitscan/melee to `view_tick`), burst-tolerant input queue | ✅ done |
| 5. Cross-network: dedicated server on a public Linux VM | ✅ done |

## Download (no Rust needed)

Grab the zip for your OS from [Releases](https://github.com/3ab3abb/0xGG/releases/latest), unzip it and read its `README.txt`.
Everyone double-clicks `PLAY.bat` (`play.command` on macOS). One player presses **HOST GAME** and sends the IP it shows; the others type it in the *host's IP* box.

To publish a new build: `git tag v0.1.1 && git push origin v0.1.1` (GitHub Actions tests, builds and attaches the zips).

## Build from source

### Requirements

- Rust 1.80+ (`rustup` from https://rustup.rs)
- Optional: the character sprites (their license forbids sharing the files, so they are not in git). Copy the free Seeker_Lite sheets into `assets/seeker/` and they are compiled into the client, see [`assets/CREDITS.md`](assets/CREDITS.md). Without them players are drawn as plain packets. Release zips already include them inside the game.

### Run

**Easiest: host from the game.** Everyone runs the client. One player presses **HOST GAME** on the login screen: the
server starts inside their window (same code as `gg-server`, on `0.0.0.0:7777`) and their LAN IP is shown on the
login screen, in the lobby and in the port, with a **COPY** button. The others type that IP in the *host's IP* box
(remembered in `last_server.txt`). Closing the host's window stops the server.

```bash
cargo run --release -p gg-client              # then HOST GAME, or type the host's IP
cargo run --release -p gg-client -- --host    # press HOST GAME at startup
```

**Dedicated server** (a machine that doesn't play, the "1 server + 3 clients" setup):

```bash
cargo run --release -p gg-server -- --bind 0.0.0.0:7777
cargo run --release -p gg-client -- --server <SERVER_IP>   # on each player machine
```

Allow port **7777 TCP and UDP** through the host's firewall. Everyone must be on the same Wi-Fi / hotspot.

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
| Fullscreen | `F11` or `Alt+Enter` | |

The window can be resized freely and `--fullscreen` starts in fullscreen: the 1280x720 layout scales to fit (letterboxed), with text rendered at native resolution.

One hit drops a packet. Grab weapons from the blue spawner pads: **Ping Pistol** (hitscan), **Broadcast Shotgun** (spread, recoil), **Fork Bomb** (bounces, explodes, forks into two), **Firewall** (blocks shots for 5 s), **Checksum Blade** (melee, reflects shots mid-swing). Throw an empty gun to stun. Landing on a header or falling into a pit also drops you. Last packet standing wins the round; first to 5 rounds wins.

The footer shows TCP and UDP round-trip times; in a match the bottom-left shows snapshot rate, measured loss, how many inputs are still unacknowledged and the last prediction correction. **F1** toggles client-side prediction, **F2** toggles interpolation (try both with `--lag 75`).

### Bots

Alone? In a port, the host can press **+ ADD BOT** (up to 2) and **− REMOVE BOT**. Bots are real network clients the server spawns on localhost: they log in, do the UDP handshake and play through the same protocol as you. They rematch after each match and leave when the last human does.

One-command solo practice (hosts its own server): `cargo run --release -p gg-client -- --login you:pw123 --add-bots 2`

### Demo / test flags (client)

| Flag | Effect |
|---|---|
| `--host` | Start hosting at launch (same as the HOST GAME button) |
| `--loss 20` | Drop 20% of UDP packets in each direction (shows the game survives loss) |
| `--lag 75` | Delay UDP packets 75 ms each way (150 ms RTT): shows prediction/interpolation on a LAN |
| `--login name:pass` | Register or log in automatically |
| `--autojoin` | Join the first open port (or create one) and ready up |
| `--bot` | This client plays itself with the bot brain |
| `--add-bots N` | Create a port, add N (1-2) server bots and start |

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
crates/client   macroquad app: login (+ HOST GAME, embeds the server), lobby, room, game view, results (0xGG / 0xFF)
assets/maps     ASCII maps (compiled into both binaries)
assets/sprites  CC0 weapon/effect sprites (compiled into the client)
assets/seeker   character sheets, loaded at runtime if present (gitignored, see assets/CREDITS.md)
```
