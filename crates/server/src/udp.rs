//! The UDP thread: one socket for all clients, on the same port as TCP.
//!
//! It authenticates every datagram by session token (handed out over TCP at
//! login) and routes inputs to the right room's game loop. It never trusts an
//! address the client claims: it uses the source address it observes, and
//! updates it if a valid token shows up from a new address (NAT rebinding,
//! Wi-Fi switch).

use std::collections::HashMap;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use gg_shared::udp::{self, ClientPacket, ServerPacket};
use log::{debug, info, warn};

use crate::game::RoomEvent;

/// Control messages from the lobby thread.
pub enum UdpCtl {
    /// A player logged in: accept UDP packets carrying this token.
    Register {
        token: u64,
        label: String,
    },
    Unregister {
        token: u64,
    },
    /// Start (Some) or stop (None) forwarding this player's inputs to a room.
    Route {
        token: u64,
        route: Option<(Sender<RoomEvent>, u8)>,
    },
}

struct Entry {
    label: String,
    addr: Option<SocketAddr>,
    route: Option<(Sender<RoomEvent>, u8)>,
}

pub fn spawn(socket: UdpSocket, ctl: Receiver<UdpCtl>) -> io::Result<()> {
    // Short timeout so control messages are applied within ~5 ms even when
    // no datagrams are arriving.
    socket.set_read_timeout(Some(Duration::from_millis(5)))?;
    thread::Builder::new()
        .name("udp".into())
        .spawn(move || run(socket, ctl))?;
    Ok(())
}

fn reply(socket: &UdpSocket, addr: SocketAddr, packet: &ServerPacket) {
    match udp::encode(packet) {
        Ok(bytes) => {
            if let Err(e) = socket.send_to(&bytes, addr) {
                debug!("udp send to {addr} failed: {e}");
            }
        }
        Err(e) => warn!("udp encode failed: {e}"),
    }
}

fn run(socket: UdpSocket, ctl: Receiver<UdpCtl>) {
    let mut entries: HashMap<u64, Entry> = HashMap::new();
    let mut buf = [0u8; 2048];
    let mut rejected: u64 = 0;

    loop {
        loop {
            match ctl.try_recv() {
                Ok(UdpCtl::Register { token, label }) => {
                    entries.insert(
                        token,
                        Entry {
                            label,
                            addr: None,
                            route: None,
                        },
                    );
                }
                Ok(UdpCtl::Unregister { token }) => {
                    entries.remove(&token);
                }
                Ok(UdpCtl::Route { token, route }) => {
                    if let Some(e) = entries.get_mut(&token) {
                        e.route = route;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        let (n, addr) = match socket.recv_from(&mut buf) {
            Ok(v) => v,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue
            }
            // e.g. Windows reports ICMP port-unreachable as ConnectionReset on
            // the *next* recv. Harmless for a connectionless socket: keep going.
            Err(e) => {
                debug!("udp recv error: {e}");
                continue;
            }
        };

        let Some(packet) = udp::decode::<ClientPacket>(&buf[..n]) else {
            rejected += 1;
            if rejected.is_power_of_two() {
                warn!("udp: dropped {rejected} malformed datagram(s), latest from {addr}");
            }
            continue;
        };

        let token = match &packet {
            ClientPacket::Hello { token }
            | ClientPacket::Input { token, .. }
            | ClientPacket::Ping { token, .. } => *token,
        };
        let Some(entry) = entries.get_mut(&token) else {
            // Unknown token: ignore silently (never reply, so we can't be used
            // to reflect traffic at a spoofed address).
            rejected += 1;
            continue;
        };
        if entry.addr != Some(addr) {
            match entry.addr {
                None => info!("udp: {} bound to {addr}", entry.label),
                Some(old) => info!("udp: {} rebound {old} -> {addr}", entry.label),
            }
            entry.addr = Some(addr);
        }

        match packet {
            ClientPacket::Hello { .. } => reply(&socket, addr, &ServerPacket::HelloAck),
            ClientPacket::Ping { client_time_ms, .. } => {
                reply(&socket, addr, &ServerPacket::Pong { client_time_ms })
            }
            ClientPacket::Input {
                frames, events_ack, ..
            } => {
                if let Some((room, slot)) = &entry.route {
                    let _ = room.send(RoomEvent::Input {
                        slot: *slot,
                        addr,
                        frames,
                        events_ack,
                    });
                }
            }
        }
    }
}
