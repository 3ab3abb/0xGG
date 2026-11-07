//! Client side of the TCP control channel.
//!
//! macroquad runs everything on one thread at 60 FPS, so a blocking `read()`
//! there would freeze the game. A background thread does the blocking reads
//! and hands complete messages to the main loop through a channel; the main
//! loop polls it once per frame without ever blocking.

use std::io::{self, BufReader};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use gg_shared::constants::{DEFAULT_PORT, TCP_HEARTBEAT_TIMEOUT};
use gg_shared::framing::{read_frame, write_frame};
use gg_shared::protocol::{ClientMsg, ServerMsg};

pub enum NetEvent {
    Msg(ServerMsg),
    Closed(String),
}

pub struct Connection {
    stream: TcpStream,
    rx: Receiver<NetEvent>,
}

/// Accepts `host`, `host:port`, `1.2.3.4` or `1.2.3.4:port`.
pub fn with_default_port(addr: &str) -> String {
    let addr = addr.trim();
    if addr.rsplit_once(':').is_some_and(|(_, p)| p.parse::<u16>().is_ok()) {
        addr.to_string()
    } else {
        format!("{addr}:{DEFAULT_PORT}")
    }
}

impl Connection {
    pub fn connect(addr: &str) -> io::Result<Self> {
        let addr = with_default_port(addr);
        let target = addr
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "address did not resolve"))?;
        let stream = TcpStream::connect_timeout(&target, Duration::from_secs(3))?;
        stream.set_nodelay(true)?;
        // The server answers our 2 s pings, so 6 s of silence = server gone.
        stream.set_read_timeout(Some(TCP_HEARTBEAT_TIMEOUT))?;

        let mut reader = BufReader::new(stream.try_clone()?);
        let (tx, rx) = mpsc::channel();
        thread::Builder::new().name("tcp-reader".into()).spawn(move || loop {
            match read_frame::<_, ServerMsg>(&mut reader) {
                Ok(msg) => {
                    if tx.send(NetEvent::Msg(msg)).is_err() {
                        break; // Connection dropped by the game
                    }
                }
                Err(e) => {
                    let why = match e.kind() {
                        io::ErrorKind::UnexpectedEof => "server closed the connection".to_string(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => "server timed out".to_string(),
                        _ => e.to_string(),
                    };
                    let _ = tx.send(NetEvent::Closed(why));
                    break;
                }
            }
        })?;
        Ok(Connection { stream, rx })
    }

    pub fn send(&mut self, msg: &ClientMsg) -> io::Result<()> {
        write_frame(&mut self.stream, msg)
    }

    /// Non-blocking: everything that arrived since the last frame.
    pub fn poll(&self) -> Vec<NetEvent> {
        self.rx.try_iter().collect()
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}
