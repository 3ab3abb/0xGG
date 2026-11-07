//! 0xGG server library. `main.rs` is a thin CLI around [`run`]; integration
//! tests start the server in-process with the same function.

pub mod conn;
pub mod lobby;
pub mod users;

use std::io;
use std::net::TcpListener;
use std::path::PathBuf;
use std::thread;

use log::{error, info};

use crate::conn::ConnId;
use crate::lobby::Lobby;
use crate::users::UserStore;

#[derive(Debug, Clone)]
pub struct Config {
    pub server_name: String,
    /// Where accounts are persisted.
    pub users_path: PathBuf,
    /// UDP port announced to clients in `AuthOk`.
    pub udp_port: u16,
}

/// Accept TCP connections forever.
///
/// Thread layout:
/// - this thread: `accept()` loop;
/// - `lobby`: owns all lobby/room state, fed by one channel;
/// - per connection: one reader + one writer thread (see [`conn`]).
pub fn run(listener: TcpListener, config: Config) -> io::Result<()> {
    let users = UserStore::load(&config.users_path)?;
    info!("loaded {} account(s) from {}", users.len(), config.users_path.display());

    let (lobby_tx, lobby_rx) = crossbeam_channel::unbounded();
    let lobby = Lobby::new(config, users);
    thread::Builder::new().name("lobby".into()).spawn(move || lobby.run(lobby_rx))?;

    info!("listening on tcp://{}", listener.local_addr()?);
    let mut next_id = 1u64;
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let id = ConnId(next_id);
                next_id += 1;
                if let Err(e) = conn::spawn(id, stream, lobby_tx.clone()) {
                    error!("{id}: failed to set up connection: {e}");
                }
            }
            // A failed accept (e.g. client reset during handshake) must not kill the server.
            Err(e) => error!("accept failed: {e}"),
        }
    }
    Ok(())
}
