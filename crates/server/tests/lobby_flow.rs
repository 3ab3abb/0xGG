//! End-to-end lobby tests over real TCP sockets on localhost.

mod common;

use std::io::BufReader;
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use gg_server::Config;
use gg_shared::framing::{read_frame, write_frame};
use gg_shared::protocol::*;

fn start_server() -> SocketAddr {
    let listener = common::listener_with_free_udp();
    let addr = listener.local_addr().unwrap();
    let users_path: PathBuf = std::env::temp_dir().join(format!(
        "gg-test-users-{}-{}.json",
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

struct TestClient {
    stream: TcpStream,
    reader: BufReader<TcpStream>,
}

impl TestClient {
    fn connect(addr: SocketAddr) -> Self {
        let stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let reader = BufReader::new(stream.try_clone().unwrap());
        let mut c = TestClient { stream, reader };
        assert!(matches!(c.recv(), ServerMsg::Welcome { protocol, .. } if protocol == gg_shared::constants::PROTOCOL_VERSION));
        c
    }

    fn register(addr: SocketAddr, name: &str) -> Self {
        let mut c = Self::connect(addr);
        c.send(ClientMsg::Register {
            username: name.into(),
            password: "hunter2".into(),
        });
        c.expect(|m| matches!(m, ServerMsg::AuthOk { .. }));
        c
    }

    fn send(&mut self, msg: ClientMsg) {
        write_frame(&mut self.stream, &msg).unwrap();
    }

    fn recv(&mut self) -> ServerMsg {
        read_frame(&mut self.reader).expect("expected a message from server")
    }

    /// Read until a message matches; pushed updates in between are skipped.
    fn expect(&mut self, pred: impl Fn(&ServerMsg) -> bool) -> ServerMsg {
        loop {
            let m = self.recv();
            if pred(&m) {
                return m;
            }
        }
    }
}

fn room_id_of(m: &ServerMsg) -> u32 {
    match m {
        ServerMsg::RoomState { room_id, .. } => *room_id,
        _ => unreachable!(),
    }
}

#[test]
fn register_login_and_auth_errors() {
    let addr = start_server();
    let alice = TestClient::register(addr, "alice");

    // Duplicate username.
    let mut c = TestClient::connect(addr);
    c.send(ClientMsg::Register {
        username: "ALICE".into(),
        password: "xxx".into(),
    });
    assert_eq!(
        c.recv(),
        ServerMsg::AuthError {
            reason: AuthErrorReason::UserExists
        }
    );

    // Wrong password.
    c.send(ClientMsg::Login {
        username: "alice".into(),
        password: "nope".into(),
    });
    assert_eq!(
        c.recv(),
        ServerMsg::AuthError {
            reason: AuthErrorReason::BadCredentials
        }
    );

    // Same account twice.
    c.send(ClientMsg::Login {
        username: "alice".into(),
        password: "hunter2".into(),
    });
    assert_eq!(
        c.recv(),
        ServerMsg::AuthError {
            reason: AuthErrorReason::AlreadyLoggedIn
        }
    );

    // After alice disconnects, the account can log in again.
    // The server learns about the disconnect asynchronously (reader thread
    // → lobby), so retry instead of assuming it happens within N ms.
    drop(alice);
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        c.send(ClientMsg::Login {
            username: "alice".into(),
            password: "hunter2".into(),
        });
        match c.expect(|m| matches!(m, ServerMsg::AuthOk { .. } | ServerMsg::AuthError { .. })) {
            ServerMsg::AuthOk { username, .. } => {
                assert_eq!(username, "alice");
                break;
            }
            ServerMsg::AuthError {
                reason: AuthErrorReason::AlreadyLoggedIn,
            } if std::time::Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(50));
            }
            other => panic!("re-login failed: {other:?}"),
        }
    }

    // Commands that need a login are rejected before it.
    let mut anon = TestClient::connect(addr);
    anon.send(ClientMsg::CreateRoom { name: "x".into() });
    assert!(matches!(anon.recv(), ServerMsg::Error { ref code, .. } if code == "bad_state"));
}

#[test]
fn full_room_flow_with_disconnects() {
    let addr = start_server();
    let mut a = TestClient::register(addr, "p_one");
    let mut b = TestClient::register(addr, "p_two");
    let mut c = TestClient::register(addr, "p_three");
    let mut late = TestClient::register(addr, "p_late");

    // Create + join.
    a.send(ClientMsg::CreateRoom {
        name: "port 80".into(),
    });
    let room_id = room_id_of(&a.expect(|m| matches!(m, ServerMsg::RoomState { .. })));
    // Lobby members get the pushed room list.
    late.expect(
        |m| matches!(m, ServerMsg::RoomList { rooms } if rooms.len() == 1 && rooms[0].players == 1),
    );

    b.send(ClientMsg::JoinRoom { room_id });
    c.send(ClientMsg::JoinRoom { room_id });
    c.expect(|m| matches!(m, ServerMsg::RoomState { players, .. } if players.len() == 3));

    // Fourth player: room full.
    late.send(ClientMsg::JoinRoom { room_id });
    late.expect(|m| matches!(m, ServerMsg::Error { code, .. } if code == "room_full"));

    // Room chat reaches members only.
    b.send(ClientMsg::Chat {
        text: "gl hf".into(),
    });
    a.expect(
        |m| matches!(m, ServerMsg::Chat { from, scope: ChatScope::Room, .. } if from == "p_two"),
    );

    // All ready → match starts, everybody gets a distinct slot.
    for cl in [&mut a, &mut b, &mut c] {
        cl.send(ClientMsg::SetReady { ready: true });
    }
    let mut slots = Vec::new();
    for cl in [&mut a, &mut b, &mut c] {
        match cl.expect(|m| matches!(m, ServerMsg::MatchStarting { .. })) {
            ServerMsg::MatchStarting {
                your_slot,
                slots: all,
                ..
            } => {
                assert_eq!(all.len(), 3);
                slots.push(your_slot);
            }
            _ => unreachable!(),
        }
    }
    slots.sort();
    assert_eq!(slots, vec![1, 2, 3]);
    late.expect(|m| matches!(m, ServerMsg::RoomList { rooms } if rooms[0].in_match));

    // Player 3 crashes (abrupt close): others are told, match continues with 2.
    c.stream.shutdown(Shutdown::Both).unwrap();
    drop(c);
    a.expect(|m| {
        matches!(
            m,
            ServerMsg::PlayerDisconnected {
                slot: 3,
                reason: DisconnectReason::Closed,
                ..
            }
        )
    });

    // Player 2 leaves voluntarily: only 1 left → match ends, player 1 wins.
    b.send(ClientMsg::LeaveRoom);
    b.expect(|m| matches!(m, ServerMsg::LeftRoom));
    let ended = a.expect(|m| matches!(m, ServerMsg::MatchEnded { .. }));
    match ended {
        ServerMsg::MatchEnded {
            reason,
            winner_slot,
            rankings,
        } => {
            assert_eq!(reason, MatchEndReason::NotEnoughPlayers);
            assert_eq!(winner_slot, Some(1));
            assert_eq!(rankings[0].slot, 1);
            assert_eq!(rankings.iter().filter(|r| r.disconnected).count(), 2);
        }
        _ => unreachable!(),
    }
    // Room survives with its last member, back to not-ready.
    a.expect(|m| matches!(m, ServerMsg::RoomState { players, .. } if players.len() == 1 && !players[0].ready));

    // Last member leaves → room deleted from the lobby list.
    a.send(ClientMsg::LeaveRoom);
    late.expect(|m| matches!(m, ServerMsg::RoomList { rooms } if rooms.is_empty()));
}

#[test]
fn garbage_input_does_not_crash_server() {
    use std::io::Write;
    let addr = start_server();

    // Oversized length prefix, then raw junk.
    let mut s = TcpStream::connect(addr).unwrap();
    s.write_all(&u32::MAX.to_be_bytes()).unwrap();
    let mut s2 = TcpStream::connect(addr).unwrap();
    s2.write_all(&[0, 0, 0, 5, b'h', b'e', b'l', b'l', b'o'])
        .unwrap();
    thread::sleep(Duration::from_millis(200));

    // Server still serves new clients normally.
    let _ok = TestClient::register(addr, "survivor");
}

#[test]
fn silent_client_is_dropped_by_heartbeat_timeout() {
    let addr = start_server();
    let mut host = TestClient::register(addr, "pinger");
    let mut ghost = TestClient::register(addr, "ghost");

    host.send(ClientMsg::CreateRoom { name: "hb".into() });
    let room_id = room_id_of(&host.expect(|m| matches!(m, ServerMsg::RoomState { .. })));
    ghost.send(ClientMsg::JoinRoom { room_id });
    host.expect(|m| matches!(m, ServerMsg::RoomState { players, .. } if players.len() == 2));

    // Host keeps pinging like a real client; ghost goes silent but keeps the
    // socket open (simulates Wi-Fi dropping: no FIN, no RST).
    let started = std::time::Instant::now();
    loop {
        host.send(ClientMsg::Ping { nonce: 0 });
        let m = host.expect(|m| matches!(m, ServerMsg::Pong { .. } | ServerMsg::RoomState { .. }));
        if matches!(m, ServerMsg::RoomState { ref players, .. } if players.len() == 1) {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "ghost was never timed out"
        );
        thread::sleep(Duration::from_millis(500));
    }
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_secs(5),
        "dropped too early: {waited:?}"
    );
    drop(ghost);
}
