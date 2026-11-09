//! Client side of the TCP control channel.
//!
//! macroquad runs everything on one thread at 60 FPS, so a blocking `read()`
//! there would freeze the game. A background thread does the blocking reads
//! and hands complete messages to the main loop through a channel; the main
//! loop polls it once per frame without ever blocking.

use std::collections::VecDeque;
use std::io::{self, BufReader};
use std::net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use gg_shared::constants::{DEFAULT_PORT, TCP_HEARTBEAT_TIMEOUT};
use gg_shared::framing::{read_frame, write_frame};
use gg_shared::protocol::{ClientMsg, ServerMsg};
use gg_shared::udp::{self, ClientPacket, ServerPacket};

/// Milliseconds since process start, usable from any thread. Reader threads
/// stamp arrivals with it, so RTT readouts measure the network, not how long
/// a message waited for the next frame (up to 16 ms at 60 FPS).
pub fn now_ms() -> f64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
}

pub enum NetEvent {
    /// Message plus its arrival time (`now_ms`).
    Msg(ServerMsg, f64),
    Closed(String),
}

pub struct Connection {
    stream: TcpStream,
    rx: Receiver<NetEvent>,
}

/// Accepts `host`, `host:port`, `1.2.3.4` or `1.2.3.4:port`.
pub fn with_default_port(addr: &str) -> String {
    let addr = addr.trim();
    if addr
        .rsplit_once(':')
        .is_some_and(|(_, p)| p.parse::<u16>().is_ok())
    {
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
        thread::Builder::new()
            .name("tcp-reader".into())
            .spawn(move || loop {
                match read_frame::<_, ServerMsg>(&mut reader) {
                    Ok(msg) => {
                        if tx.send(NetEvent::Msg(msg, now_ms())).is_err() {
                            break; // Connection dropped by the game
                        }
                    }
                    Err(e) => {
                        let why = match e.kind() {
                            io::ErrorKind::UnexpectedEof => {
                                "server closed the connection".to_string()
                            }
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
                                "server timed out".to_string()
                            }
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

    /// The server address as seen by our TCP connection; UDP goes to the same IP.
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.stream.peer_addr()
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

/// Client side of the UDP game channel.
///
/// The socket is `connect()`ed to the server, so the OS discards datagrams
/// from any other source. `loss` simulates packet loss (0.0-1.0) in both
/// directions, to demonstrate that the game survives an unreliable network.
pub struct UdpLink {
    socket: UdpSocket,
    rx: Receiver<(ServerPacket, f64)>,
    stop: Arc<AtomicBool>,
    loss: f32,
    /// Simulated one-way delay in ms (`--lag`), applied in both directions.
    lag_ms: f64,
    /// Outgoing datagrams held back until `due` (ms, `now_ms` clock).
    delayed_out: VecDeque<(f64, Vec<u8>)>,
    /// Incoming packets held back until `due`.
    delayed_in: VecDeque<(f64, ServerPacket)>,
    pub sent: u64,
    pub received: u64,
}

fn chance(p: f32) -> bool {
    p > 0.0 && macroquad::rand::gen_range(0.0f32, 1.0) < p
}

impl UdpLink {
    pub fn open(server: SocketAddr, loss: f32, lag_ms: f64) -> io::Result<Self> {
        let bind = if server.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        };
        let socket = UdpSocket::bind(bind)?;
        socket.connect(server)?;
        let reader = socket.try_clone()?;
        // Wake up periodically to notice `stop`.
        reader.set_read_timeout(Some(Duration::from_millis(250)))?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("udp-reader".into())
            .spawn(move || {
                let mut buf = [0u8; 2048];
                while !stop_flag.load(Ordering::Relaxed) {
                    let Ok(n) = reader.recv(&mut buf) else {
                        continue;
                    };
                    if chance(loss) {
                        continue; // simulated loss on the way in
                    }
                    if let Some(p) = udp::decode::<ServerPacket>(&buf[..n]) {
                        if tx.send((p, now_ms())).is_err() {
                            break;
                        }
                    }
                }
            })?;
        Ok(UdpLink {
            socket,
            rx,
            stop,
            loss,
            lag_ms,
            delayed_out: VecDeque::new(),
            delayed_in: VecDeque::new(),
            sent: 0,
            received: 0,
        })
    }

    pub fn send(&mut self, packet: &ClientPacket) {
        self.sent += 1;
        if chance(self.loss) {
            return; // simulated loss on the way out
        }
        if let Ok(bytes) = udp::encode(packet) {
            if self.lag_ms > 0.0 {
                self.delayed_out.push_back((now_ms() + self.lag_ms, bytes));
                self.flush_out();
            } else {
                // UDP send errors (e.g. ICMP unreachable) are not fatal: the
                // heartbeat/TCP layer decides whether we are disconnected.
                let _ = self.socket.send(&bytes);
            }
        }
    }

    fn flush_out(&mut self) {
        let now = now_ms();
        while self.delayed_out.front().is_some_and(|(due, _)| *due <= now) {
            if let Some((_, bytes)) = self.delayed_out.pop_front() {
                let _ = self.socket.send(&bytes);
            }
        }
    }

    /// Packets that have arrived, with their arrival time (`now_ms`). With
    /// `--lag`, a packet is only delivered `lag_ms` after it really arrived.
    pub fn poll(&mut self) -> Vec<(ServerPacket, f64)> {
        let mut v: Vec<(ServerPacket, f64)> = self.rx.try_iter().collect();
        if self.lag_ms > 0.0 {
            self.flush_out();
            for (p, arrived) in v.drain(..) {
                self.delayed_in.push_back((arrived + self.lag_ms, p));
            }
            let now = now_ms();
            while self.delayed_in.front().is_some_and(|(due, _)| *due <= now) {
                if let Some((due, p)) = self.delayed_in.pop_front() {
                    v.push((p, due));
                }
            }
        }
        self.received += v.len() as u64;
        v
    }
}

impl Drop for UdpLink {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
