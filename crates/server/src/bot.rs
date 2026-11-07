//! Server-spawned bots: headless clients running in a thread of the server
//! process. Each bot talks to the server over **real TCP and UDP sockets on
//! localhost**, through exactly the same protocol as a human client:
//! login → UDP Hello → JoinRoom → SetReady → 60 Hz Input / Snapshot.
//! The server's networking code has no special path for bots.
//!
//! A bot re-readies after every match (rematches) and quits as soon as no
//! human is left in its room, or when the host removes it (connection closed).

use std::collections::VecDeque;
use std::io::{self, BufReader};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::sync::mpsc::{self, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use gg_shared::bot::BotBrain;
use gg_shared::constants::TCP_HEARTBEAT_INTERVAL;
use gg_shared::framing::{read_frame, write_frame};
use gg_shared::game::TICK_RATE;
use gg_shared::protocol::{ClientMsg, ServerMsg};
use gg_shared::udp::{self, ClientPacket, InputFrame, ServerPacket, Snapshot, INPUT_REDUNDANCY};
use log::{debug, info};

pub fn spawn(
    server: SocketAddr,
    username: String,
    password: String,
    room_id: u32,
) -> io::Result<()> {
    thread::Builder::new()
        .name(username.clone())
        .spawn(move || match run(server, &username, &password, room_id) {
            Ok(()) => info!("{username}: left"),
            Err(e) => debug!("{username}: stopped: {e}"),
        })?;
    Ok(())
}

fn err(msg: &str) -> io::Error {
    io::Error::other(msg.to_string())
}

/// Shuts the socket down when the bot exits for any reason. Dropping our
/// handle alone isn't enough: the reader thread holds a clone, which would
/// keep the connection open until the server's heartbeat timeout.
struct CloseOnDrop(TcpStream);

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

fn run(server: SocketAddr, username: &str, password: &str, room_id: u32) -> io::Result<()> {
    // --- TCP: connect + login -----------------------------------------
    let mut tcp = TcpStream::connect(server)?;
    tcp.set_nodelay(true)?;
    let _close = CloseOnDrop(tcp.try_clone()?);
    let mut reader = BufReader::new(tcp.try_clone()?);
    let (tx, rx) = mpsc::channel::<ServerMsg>();
    thread::Builder::new()
        .name(format!("{username}-tcp"))
        .spawn(move || {
            while let Ok(m) = read_frame::<_, ServerMsg>(&mut reader) {
                if tx.send(m).is_err() {
                    break;
                }
            }
        })?;
    write_frame(
        &mut tcp,
        &ClientMsg::Login {
            username: username.into(),
            password: password.into(),
        },
    )?;
    let (token, udp_port) = loop {
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(ServerMsg::AuthOk {
                token, udp_port, ..
            }) => break (token, udp_port),
            Ok(ServerMsg::AuthError { .. }) => return Err(err("login refused")),
            Ok(_) => continue,
            Err(_) => return Err(err("no AuthOk")),
        }
    };

    // --- UDP: bind + Hello handshake ------------------------------------
    let bind = if server.is_ipv4() {
        "127.0.0.1:0"
    } else {
        "[::1]:0"
    };
    let udp = UdpSocket::bind(bind)?;
    udp.connect(SocketAddr::new(server.ip(), udp_port))?;
    udp.set_read_timeout(Some(Duration::from_millis(100)))?;
    let hello = udp::encode(&ClientPacket::Hello { token }).map_err(|e| err(&e))?;
    let mut buf = [0u8; 2048];
    let mut acked = false;
    for _ in 0..20 {
        udp.send(&hello)?;
        if let Ok(n) = udp.recv(&mut buf) {
            if let Some(ServerPacket::HelloAck) = udp::decode(&buf[..n]) {
                acked = true;
                break;
            }
        }
    }
    if !acked {
        return Err(err("no HelloAck"));
    }
    udp.set_nonblocking(true)?;

    write_frame(&mut tcp, &ClientMsg::JoinRoom { room_id })?;

    // --- main loop at 60 Hz ----------------------------------------------
    let seed = token ^ Instant::now().elapsed().as_nanos() as u64;
    let mut brain = BotBrain::new(seed);
    let start = Instant::now();
    let tick = Duration::from_secs_f64(1.0 / TICK_RATE as f64);
    let mut next = Instant::now();
    let mut last_ping = Instant::now();
    let mut my_slot: Option<u8> = None;
    let mut latest: Option<Snapshot> = None;
    let mut events_ack = 0u32;
    let mut seq = 0u32;
    let mut history: VecDeque<InputFrame> = VecDeque::new();

    loop {
        // TCP control messages.
        loop {
            let msg = match rx.try_recv() {
                Ok(m) => m,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()), // removed / server gone
            };
            match msg {
                ServerMsg::RoomState { players, .. } => {
                    if !players.iter().any(|p| !p.is_bot) {
                        return Ok(()); // no humans left: dropping the socket logs us out
                    }
                    let me_ready = players.iter().any(|p| p.username == username && p.ready);
                    if my_slot.is_none() && !me_ready {
                        write_frame(&mut tcp, &ClientMsg::SetReady { ready: true })?;
                    }
                }
                ServerMsg::MatchStarting { your_slot, .. } => {
                    my_slot = Some(your_slot);
                    latest = None;
                    events_ack = 0;
                    seq = 0;
                    history.clear();
                }
                // RoomState follows with ready=false, and we re-ready: rematch.
                ServerMsg::MatchEnded { .. } => my_slot = None,
                ServerMsg::LeftRoom => return Ok(()),
                ServerMsg::Error { code, .. }
                    if my_slot.is_none()
                        && matches!(
                            code.as_str(),
                            "room_full" | "room_not_found" | "room_in_match"
                        ) =>
                {
                    return Err(err("could not join room"));
                }
                _ => {}
            }
        }

        // UDP snapshots.
        while let Ok(n) = udp.recv(&mut buf) {
            if let Some(ServerPacket::Snapshot(s)) = udp::decode(&buf[..n]) {
                if latest.as_ref().is_some_and(|l| s.tick <= l.tick) {
                    continue; // stale / reordered
                }
                let mut ids: Vec<u32> = s.events.iter().map(|e| e.id).collect();
                ids.sort_unstable();
                for id in ids {
                    if id == events_ack + 1 {
                        events_ack = id; // bots don't draw effects, they just ack
                    }
                }
                latest = Some(s);
            }
        }

        // Input.
        if let Some(slot) = my_slot {
            let buttons = latest
                .as_ref()
                .map_or(0, |s| brain.think(s, slot, start.elapsed().as_secs_f64()));
            let view_tick = latest.as_ref().map_or(0, |s| s.tick);
            seq += 1;
            history.push_back(InputFrame {
                seq,
                buttons,
                view_tick,
            });
            while history.len() > INPUT_REDUNDANCY {
                history.pop_front();
            }
            let packet = ClientPacket::Input {
                token,
                frames: history.iter().copied().collect(),
                last_snapshot_tick: view_tick,
                events_ack,
            };
            if let Ok(bytes) = udp::encode(&packet) {
                let _ = udp.send(&bytes);
            }
        }

        if last_ping.elapsed() >= TCP_HEARTBEAT_INTERVAL {
            last_ping = Instant::now();
            write_frame(&mut tcp, &ClientMsg::Ping { nonce: 0 })?;
        }

        // TCP messages wait in the channel until the top of the next tick.
        next += tick;
        match next.checked_duration_since(Instant::now()) {
            Some(d) => thread::sleep(d),
            None => next = Instant::now(),
        }
    }
}
