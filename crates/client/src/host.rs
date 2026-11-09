//! "HOST GAME": runs the full 0xGG server inside this client process.
//!
//! It is the exact same server as the `gg-server` binary (`gg_server::run`),
//! just on a background thread, listening on 0.0.0.0:7777 (TCP + UDP) so other
//! machines on the LAN can join. The host's own client then connects to it
//! over localhost like any other client. Closing the window stops the server.

use std::net::{IpAddr, TcpListener, UdpSocket};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use gg_shared::constants::DEFAULT_PORT;

pub struct HostedServer {
    /// The address friends type: this machine's LAN IP (None if offline).
    pub lan_ip: Option<IpAddr>,
    /// False when a server was already running on this computer and we reuse it.
    pub embedded: bool,
    /// Receives a message if the server thread dies.
    died: Option<Receiver<String>>,
}

impl HostedServer {
    /// Start the server, or reuse one already listening on this machine.
    pub fn start() -> Result<Self, String> {
        let lan_ip = lan_ip();
        let listener = match TcpListener::bind(("0.0.0.0", DEFAULT_PORT)) {
            Ok(l) => l,
            // Probably `gg-server` / another 0xGG window is already hosting here.
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                return Ok(HostedServer {
                    lan_ip,
                    embedded: false,
                    died: None,
                })
            }
            Err(e) => return Err(format!("cannot host on port {DEFAULT_PORT}: {e}")),
        };
        let (tx, rx) = mpsc::channel();
        let config = gg_server::Config {
            server_name: "0xGG".into(),
            users_path: PathBuf::from("users.json"),
            udp_port: DEFAULT_PORT,
        };
        thread::Builder::new()
            .name("embedded-server".into())
            .spawn(move || {
                let why = match gg_server::run(listener, config) {
                    Ok(()) => "server stopped".to_string(),
                    Err(e) => format!("server error: {e}"),
                };
                let _ = tx.send(why);
            })
            .map_err(|e| format!("cannot start server thread: {e}"))?;
        Ok(HostedServer {
            lan_ip,
            embedded: true,
            died: Some(rx),
        })
    }

    /// Some(reason) once, if the embedded server thread exited.
    pub fn poll_died(&mut self) -> Option<String> {
        let rx = self.died.as_ref()?;
        match rx.try_recv() {
            Ok(why) => {
                self.died = None;
                Some(why)
            }
            Err(TryRecvError::Disconnected) => {
                self.died = None;
                Some("server thread exited".into())
            }
            Err(TryRecvError::Empty) => None,
        }
    }
}

/// This machine's LAN address: the source IP the OS would use to reach the
/// outside. `connect` on UDP sends nothing; it only picks a route.
pub fn lan_ip() -> Option<IpAddr> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    let ip = sock.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

/// "192.168.1.20:7777" -> "192.168.1.20" (the default port is implied).
pub fn short_addr(addr: &str) -> String {
    let addr = addr.trim();
    addr.strip_suffix(&format!(":{DEFAULT_PORT}"))
        .unwrap_or(addr)
        .to_string()
}

/// Remember the last server so the IP is prefilled next launch.
const LAST_SERVER_FILE: &str = "last_server.txt";

pub fn load_last_server() -> Option<String> {
    let s = std::fs::read_to_string(LAST_SERVER_FILE).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

pub fn save_last_server(addr: &str) {
    let _ = std::fs::write(LAST_SERVER_FILE, short_addr(addr));
}
