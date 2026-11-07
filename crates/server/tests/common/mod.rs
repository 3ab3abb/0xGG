//! Helpers shared by the integration tests.

use std::net::{TcpListener, UdpSocket};

/// A localhost TCP listener on a port that is ALSO free for UDP.
///
/// The server binds UDP on the same port number as TCP. Asking the OS for a
/// free TCP port says nothing about UDP: another process may hold that UDP
/// port, and the test server would fail to start. Probe both, retry if not.
pub fn listener_with_free_udp() -> TcpListener {
    for _ in 0..50 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // Released right away; the server re-binds it a moment later.
        if UdpSocket::bind(("127.0.0.1", port)).is_ok() {
            return listener;
        }
    }
    panic!("no port free for both TCP and UDP");
}
