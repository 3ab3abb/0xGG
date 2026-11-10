//! Server browser probe: asks a server "are you up, who's on, how far away?"
//! with a UDP `Status` query, before any login.
//!
//! Runs on its own thread because DNS lookups (`0xgg.duckdns.org`) block.
//! UDP fits: a stateless question with a tiny answer, where a lost reply just
//! means "ask again in a moment". A TCP connect would cost a handshake, a
//! server-side session and log noise for every refresh.

use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use gg_shared::udp::{self, ClientPacket, ServerPacket};

/// Public game server shown in the login screen's CLOUD tab. A DNS name, not
/// an IP: the machine behind it can change without a new build.
const CLOUD_SERVER: &str = "0xgg.duckdns.org";
pub const CLOUD_LABEL: &str = "0xGG Cloud";

/// The CLOUD tab's server: `CLOUD_SERVER`, or `GG_CLOUD_SERVER` if set
/// (testing against another machine without a rebuild).
pub fn cloud_server() -> &'static str {
    static ADDR: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ADDR.get_or_init(|| {
        std::env::var("GG_CLOUD_SERVER")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| CLOUD_SERVER.to_string())
    })
}

/// Time between queries while the login screen is open.
const INTERVAL: Duration = Duration::from_secs(2);
/// Silence after which a server is shown offline: never answered (first
/// check) / stopped answering (two queries in a row lost).
const FIRST_TIMEOUT: Duration = Duration::from_secs(3);
const OFFLINE_AFTER: Duration = Duration::from_secs(5);
/// Re-resolve the name now and then (the record may change).
const RESOLVE_EVERY: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Checking,
    /// DNS lookup failed (no internet, or the name doesn't exist).
    NoDns,
    Offline,
    Online {
        name: String,
        rtt_ms: f64,
        online: u16,
        in_game: u16,
    },
}

pub struct Probe {
    status: Arc<Mutex<Status>>,
    active: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl Probe {
    /// Start probing `host` (default game port if none given).
    pub fn start(host: &str) -> Probe {
        let status = Arc::new(Mutex::new(Status::Checking));
        let active = Arc::new(AtomicBool::new(true));
        let stop = Arc::new(AtomicBool::new(false));
        let target = crate::net::with_default_port(host.trim());
        let (s, a, st) = (status.clone(), active.clone(), stop.clone());
        let _ = thread::Builder::new()
            .name("probe".into())
            .spawn(move || run(&target, &s, &a, &st));
        Probe {
            status,
            active,
            stop,
        }
    }

    pub fn status(&self) -> Status {
        self.status.lock().map_or(Status::Checking, |s| s.clone())
    }

    /// Only probe while someone is looking (the login screen).
    pub fn set_active(&self, on: bool) {
        self.active.store(on, Ordering::Relaxed);
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn set(status: &Mutex<Status>, s: Status) {
    if let Ok(mut g) = status.lock() {
        *g = s;
    }
}

fn run(target: &str, status: &Mutex<Status>, active: &AtomicBool, stop: &AtomicBool) {
    let Ok(socket) = UdpSocket::bind("0.0.0.0:0") else {
        return set(status, Status::Offline);
    };
    let _ = socket.set_read_timeout(Some(Duration::from_millis(100)));
    let mut addr: Option<SocketAddr> = None;
    let mut resolved_at: Option<Instant> = None;
    let mut nonce: u32 = 0;
    let mut sent_at = Instant::now();
    let mut last_reply: Option<Instant> = None;
    let mut first_sent: Option<Instant> = None;
    let mut next_send = Instant::now();
    let mut buf = [0u8; 512];

    while !stop.load(Ordering::Relaxed) {
        if !active.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(200));
            // Fresh numbers when the screen comes back (keep showing the
            // last status meanwhile, don't flash "offline").
            next_send = Instant::now();
            first_sent = None;
            last_reply = None;
            continue;
        }
        let now = Instant::now();
        if now >= next_send {
            if resolved_at.is_none_or(|t| now - t > RESOLVE_EVERY) {
                resolved_at = Some(now);
                // Prefer IPv4: the game servers listen on 0.0.0.0.
                addr = target.to_socket_addrs().ok().and_then(|mut it| {
                    let all: Vec<_> = it.by_ref().collect();
                    all.iter().find(|a| a.is_ipv4()).or(all.first()).copied()
                });
                if addr.is_none() {
                    set(status, Status::NoDns);
                }
            }
            if let Some(a) = addr {
                nonce = nonce.wrapping_add(1);
                sent_at = Instant::now();
                first_sent.get_or_insert(sent_at);
                if let Ok(bytes) = udp::encode(&ClientPacket::status_request(nonce)) {
                    let _ = socket.send_to(&bytes, a);
                }
            }
            next_send = now + INTERVAL;
        }
        if let Ok((n, from)) = socket.recv_from(&mut buf) {
            if Some(from) == addr {
                if let Some(ServerPacket::StatusReply {
                    nonce: got,
                    name,
                    online,
                    in_game,
                }) = udp::decode(&buf[..n])
                {
                    // Only the newest query's answer gives a true RTT.
                    if got == nonce {
                        last_reply = Some(Instant::now());
                        set(
                            status,
                            Status::Online {
                                name,
                                rtt_ms: sent_at.elapsed().as_secs_f64() * 1000.0,
                                online,
                                in_game,
                            },
                        );
                    }
                }
            }
        }
        let silent = match (first_sent, last_reply) {
            (_, Some(t)) => t.elapsed() > OFFLINE_AFTER,
            (Some(t), None) => t.elapsed() > FIRST_TIMEOUT,
            (None, None) => false,
        };
        if silent && addr.is_some() {
            set(status, Status::Offline);
        }
    }
}
