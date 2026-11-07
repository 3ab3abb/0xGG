//! One TCP connection = one reader thread + one writer thread.
//!
//! - The **reader** blocks on `read_frame` and forwards each message to the
//!   lobby. A read timeout of `TCP_HEARTBEAT_TIMEOUT` doubles as the heartbeat
//!   check: clients ping every 2 s, so 6 s of silence means the peer is gone
//!   even if TCP itself never reported an error (silent drop).
//! - The **writer** drains an outgoing channel. The lobby holds the only
//!   sender; dropping it ends the writer, which shuts the socket down, which
//!   in turn wakes the reader with EOF. So "kick a client" = drop its sender.

use std::fmt;
use std::io::{self, BufReader};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::thread;

use crossbeam_channel::Sender;
use gg_shared::constants::TCP_HEARTBEAT_TIMEOUT;
use gg_shared::framing::{read_frame, write_frame};
use gg_shared::protocol::{ClientMsg, DisconnectReason, MatchEndReason, ServerMsg};
use log::debug;

use crate::game::SlotStats;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnId(pub u64);

impl fmt::Display for ConnId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "conn#{}", self.0)
    }
}

/// Everything the lobby thread hears: from connection threads and game loops.
#[derive(Debug)]
pub enum LobbyEvent {
    Connected {
        conn: ConnId,
        addr: SocketAddr,
        tx: Sender<ServerMsg>,
    },
    Message {
        conn: ConnId,
        msg: ClientMsg,
    },
    Disconnected {
        conn: ConnId,
        reason: DisconnectReason,
    },
    /// A room's game loop ended; final stats for the rankings.
    MatchFinished {
        room_id: u32,
        reason: MatchEndReason,
        stats: Vec<SlotStats>,
    },
}

pub fn spawn(conn: ConnId, stream: TcpStream, lobby: Sender<LobbyEvent>) -> io::Result<()> {
    let addr = stream.peer_addr()?;
    // Small, latency-sensitive messages: don't let Nagle hold them back.
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(TCP_HEARTBEAT_TIMEOUT))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;

    let (out_tx, out_rx) = crossbeam_channel::unbounded::<ServerMsg>();
    // Sent before the reader starts, so the lobby always sees Connected first.
    if lobby
        .send(LobbyEvent::Connected {
            conn,
            addr,
            tx: out_tx,
        })
        .is_err()
    {
        return Ok(()); // lobby is gone: server shutting down
    }

    thread::Builder::new()
        .name(format!("{conn}-w"))
        .spawn(move || {
            for msg in out_rx.iter() {
                if let Err(e) = write_frame(&mut writer, &msg) {
                    debug!("{conn}: write failed: {e}");
                    break;
                }
            }
            let _ = writer.shutdown(Shutdown::Both);
        })?;

    thread::Builder::new()
        .name(format!("{conn}-r"))
        .spawn(move || loop {
            match read_frame::<_, ClientMsg>(&mut reader) {
                Ok(msg) => {
                    if lobby.send(LobbyEvent::Message { conn, msg }).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let reason = classify(&e);
                    debug!("{conn}: read ended ({reason:?}): {e}");
                    let _ = lobby.send(LobbyEvent::Disconnected { conn, reason });
                    break;
                }
            }
        })?;
    Ok(())
}

fn classify(e: &io::Error) -> DisconnectReason {
    match e.kind() {
        // Read timeout. macOS/Linux report WouldBlock, Windows reports TimedOut.
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => DisconnectReason::Timeout,
        io::ErrorKind::InvalidData => DisconnectReason::Error,
        _ => DisconnectReason::Closed,
    }
}
