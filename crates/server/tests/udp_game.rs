//! End-to-end UDP tests: Hello/Ack binding, inputs → authoritative movement
//! in snapshots, unknown tokens ignored.

mod common;

use std::io::BufReader;
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::thread;
use std::time::{Duration, Instant};

use gg_server::Config;
use gg_shared::framing::{read_frame, write_frame};
use gg_shared::game::buttons;
use gg_shared::protocol::*;
use gg_shared::udp::{self, ClientPacket, InputFrame, RoundPhase, ServerPacket, Snapshot};

fn start_server() -> SocketAddr {
    let listener = common::listener_with_free_udp();
    let addr = listener.local_addr().unwrap();
    let users_path = std::env::temp_dir().join(format!(
        "gg-udp-test-{}-{}.json",
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

struct Player {
    tcp: TcpStream,
    reader: BufReader<TcpStream>,
    udp: UdpSocket,
    token: u64,
}

impl Player {
    fn login(server: SocketAddr, name: &str) -> Self {
        let tcp = TcpStream::connect(server).unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let reader = BufReader::new(tcp.try_clone().unwrap());
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        udp.connect(server).unwrap();
        udp.set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let mut p = Player {
            tcp,
            reader,
            udp,
            token: 0,
        };
        p.send(ClientMsg::Register {
            username: name.into(),
            password: "pw123".into(),
        });
        if let ServerMsg::AuthOk { token, .. } = p.expect(|m| matches!(m, ServerMsg::AuthOk { .. }))
        {
            p.token = token;
        }
        p
    }
    fn send(&mut self, m: ClientMsg) {
        write_frame(&mut self.tcp, &m).unwrap();
    }
    fn expect(&mut self, pred: impl Fn(&ServerMsg) -> bool) -> ServerMsg {
        loop {
            let m: ServerMsg = read_frame(&mut self.reader).unwrap();
            if pred(&m) {
                return m;
            }
        }
    }
    fn udp_send(&self, p: &ClientPacket) {
        self.udp.send(&udp::encode(p).unwrap()).unwrap();
    }
    fn udp_recv(&self) -> Option<ServerPacket> {
        let mut buf = [0u8; 2048];
        let n = self.udp.recv(&mut buf).ok()?;
        udp::decode(&buf[..n])
    }
    fn hello(&self) {
        for _ in 0..10 {
            self.udp_send(&ClientPacket::Hello { token: self.token });
            if let Some(ServerPacket::HelloAck) = self.udp_recv() {
                return;
            }
        }
        panic!("no HelloAck");
    }
}

#[test]
fn unknown_token_gets_no_reply() {
    let server = start_server();
    let _p = Player::login(server, "real");
    let rogue = UdpSocket::bind("127.0.0.1:0").unwrap();
    rogue.connect(server).unwrap();
    rogue
        .set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    rogue
        .send(&udp::encode(&ClientPacket::Hello { token: 42 }).unwrap())
        .unwrap();
    rogue.send(b"garbage").unwrap();
    let mut buf = [0u8; 64];
    assert!(
        rogue.recv(&mut buf).is_err(),
        "server answered an unauthenticated packet"
    );
}

#[test]
fn inputs_move_the_packet_in_snapshots() {
    let server = start_server();
    let mut a = Player::login(server, "mover");
    let mut b = Player::login(server, "idler");
    a.hello();
    b.hello();

    a.send(ClientMsg::CreateRoom { name: "udp".into() });
    let room_id = match a.expect(|m| matches!(m, ServerMsg::RoomState { .. })) {
        ServerMsg::RoomState { room_id, .. } => room_id,
        _ => unreachable!(),
    };
    b.send(ClientMsg::JoinRoom { room_id });
    a.send(ClientMsg::SetReady { ready: true });
    b.send(ClientMsg::SetReady { ready: true });
    b.expect(|m| matches!(m, ServerMsg::RoomState { players, .. } if players.len() == 2 && players.iter().all(|p| p.ready)));
    a.send(ClientMsg::StartMatch); // 2/3: host must start explicitly
    let my_slot = match a.expect(|m| matches!(m, ServerMsg::MatchStarting { .. })) {
        ServerMsg::MatchStarting { your_slot, .. } => your_slot,
        _ => unreachable!(),
    };
    b.expect(|m| matches!(m, ServerMsg::MatchStarting { .. }));

    // Play ~5 s: a holds RIGHT, b idles. Inputs at ~60 Hz with redundancy.
    let started = Instant::now();
    let mut seq = 0u32;
    let mut history: Vec<InputFrame> = Vec::new();
    let mut first_playing_x: Option<f32> = None;
    let mut last: Option<Snapshot> = None;
    let mut snapshots = 0;
    a.udp
        .set_read_timeout(Some(Duration::from_millis(5)))
        .unwrap();
    while started.elapsed() < Duration::from_secs(5) {
        // Pace inputs by wall clock (60 per second) like the real client, not by
        // loop iterations: socket timeouts round up to ~16 ms ticks on Windows.
        let due = (started.elapsed().as_secs_f64() * 60.0) as u32 + 1;
        while seq < due {
            seq += 1;
            history.push(InputFrame {
                seq,
                buttons: buttons::RIGHT,
                view_tick: 0,
            });
            let frames = history[history.len().saturating_sub(3)..].to_vec();
            a.udp_send(&ClientPacket::Input {
                token: a.token,
                frames,
                last_snapshot_tick: 0,
                events_ack: 0,
            });
            b.udp_send(&ClientPacket::Input {
                token: b.token,
                frames: vec![InputFrame {
                    seq,
                    buttons: 0,
                    view_tick: 0,
                }],
                last_snapshot_tick: 0,
                events_ack: 0,
            });
        }
        while let Some(ServerPacket::Snapshot(s)) = a.udp_recv() {
            snapshots += 1;
            if let Some(prev) = &last {
                assert!(s.tick >= prev.tick, "ticks went backwards on localhost");
            }
            let me = s.players.iter().find(|p| p.slot == my_slot).unwrap();
            if matches!(s.phase, RoundPhase::Playing { .. }) && first_playing_x.is_none() {
                first_playing_x = Some(me.x);
            }
            last = Some(s);
        }
    }

    let last = last.expect("received no snapshots");
    assert!(
        snapshots > 60,
        "only {snapshots} snapshots in 5 s (expected ~150 at 30 Hz)"
    );
    assert!(
        matches!(last.phase, RoundPhase::Playing { .. }),
        "phase never reached Playing: {:?}",
        last.phase
    );
    assert!(
        last.ack_input_seq > 100,
        "server acked only up to seq {}",
        last.ack_input_seq
    );
    let x0 = first_playing_x.unwrap();
    let x1 = last.players.iter().find(|p| p.slot == my_slot).unwrap().x;
    assert!(x1 > x0 + 30.0, "packet did not move right: {x0} -> {x1}");
    assert_eq!(last.scores.len(), 2);
}
