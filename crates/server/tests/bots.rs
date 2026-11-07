//! In-game bots: spawned by the server, connected over real localhost sockets.

use std::io::BufReader;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use gg_server::Config;
use gg_shared::framing::{read_frame, write_frame};
use gg_shared::protocol::*;

fn start_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let users_path = std::env::temp_dir().join(format!(
        "gg-bot-test-{}-{}.json",
        std::process::id(),
        addr.port()
    ));
    let _ = std::fs::remove_file(&users_path);
    let config = Config {
        server_name: "test".into(),
        users_path,
        udp_port: addr.port(),
    };
    thread::spawn(move || gg_server::run(listener, config));
    addr
}

struct C {
    s: TcpStream,
    r: BufReader<TcpStream>,
}

impl C {
    fn register(addr: SocketAddr, name: &str) -> (C, ServerMsg) {
        let s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let r = BufReader::new(s.try_clone().unwrap());
        let mut c = C { s, r };
        c.send(ClientMsg::Register {
            username: name.into(),
            password: "pw123".into(),
        });
        let m = c.expect(|m| matches!(m, ServerMsg::AuthOk { .. } | ServerMsg::AuthError { .. }));
        (c, m)
    }
    fn send(&mut self, m: ClientMsg) {
        write_frame(&mut self.s, &m).unwrap();
    }
    fn expect(&mut self, pred: impl Fn(&ServerMsg) -> bool) -> ServerMsg {
        loop {
            let m: ServerMsg = read_frame(&mut self.r).expect("server message");
            if pred(&m) {
                return m;
            }
        }
    }
}

#[test]
fn bot_prefix_is_reserved_for_humans() {
    let addr = start_server();
    let (_c, m) = C::register(addr, "bot_sneaky");
    assert_eq!(
        m,
        ServerMsg::AuthError {
            reason: AuthErrorReason::InvalidUsername
        }
    );
}

#[test]
fn host_adds_bots_plays_and_bots_leave_with_last_human() {
    let addr = start_server();
    let (mut host, _) = C::register(addr, "host");
    let (mut guest, _) = C::register(addr, "guest");
    let (mut watcher, _) = C::register(addr, "watcher");

    host.send(ClientMsg::CreateRoom {
        name: "bots".into(),
    });
    let room_id = match host.expect(|m| matches!(m, ServerMsg::RoomState { .. })) {
        ServerMsg::RoomState { room_id, .. } => room_id,
        _ => unreachable!(),
    };

    // Only the host may add bots.
    guest.send(ClientMsg::JoinRoom { room_id });
    guest.expect(|m| matches!(m, ServerMsg::RoomState { .. }));
    guest.send(ClientMsg::AddBot);
    guest.expect(|m| matches!(m, ServerMsg::Error { code, .. } if code == "not_host"));
    guest.send(ClientMsg::LeaveRoom);
    guest.expect(|m| matches!(m, ServerMsg::LeftRoom));

    // Two bots join over real sockets and ready up by themselves.
    host.send(ClientMsg::AddBot);
    host.send(ClientMsg::AddBot);
    host.expect(|m| {
        matches!(m, ServerMsg::RoomState { players, .. }
            if players.len() == 3
                && players.iter().filter(|p| p.is_bot && p.ready).count() == 2)
    });

    // Room full + host ready → match starts with 3 slots.
    host.send(ClientMsg::SetReady { ready: true });
    match host.expect(|m| matches!(m, ServerMsg::MatchStarting { .. })) {
        ServerMsg::MatchStarting { slots, .. } => {
            assert_eq!(slots.len(), 3);
            assert_eq!(
                slots
                    .iter()
                    .filter(|s| s.username.starts_with("bot_"))
                    .count(),
                2
            );
        }
        _ => unreachable!(),
    }
    // No bot changes mid-match.
    host.send(ClientMsg::RemoveBot);
    host.expect(|m| matches!(m, ServerMsg::Error { code, .. } if code == "room_in_match"));

    // Host leaves: no human left, bots quit, room disappears.
    host.send(ClientMsg::LeaveRoom);
    let t = Instant::now();
    watcher.expect(|m| matches!(m, ServerMsg::RoomList { rooms } if rooms.is_empty()));
    assert!(t.elapsed() < Duration::from_secs(5));
}

#[test]
fn remove_bot_disconnects_it() {
    let addr = start_server();
    let (mut host, _) = C::register(addr, "host2");
    host.send(ClientMsg::CreateRoom { name: "r".into() });
    host.expect(|m| matches!(m, ServerMsg::RoomState { .. }));
    host.send(ClientMsg::AddBot);
    host.expect(|m| matches!(m, ServerMsg::RoomState { players, .. } if players.len() == 2));
    host.send(ClientMsg::RemoveBot);
    host.expect(|m| matches!(m, ServerMsg::RoomState { players, .. } if players.len() == 1));
    host.send(ClientMsg::RemoveBot);
    host.expect(|m| matches!(m, ServerMsg::Error { code, .. } if code == "no_bot"));
}
