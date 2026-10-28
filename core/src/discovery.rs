//! UDP broadcast discovery: senders broadcast DISCOVER_MSG, receivers answer
//! with "AIRLYNX_HERE <tcp_port> <name>".

use std::collections::HashSet;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::protocol::{ANNOUNCE_PREFIX, DISCOVER_MSG, DISCOVERY_PORT};

#[derive(Debug, Clone)]
pub struct Peer {
    pub addr: SocketAddr,
    pub name: String,
}

/// Binds the discovery port. Done separately from `spawn_responder` so bind
/// errors surface to the caller instead of inside a background thread.
pub(crate) fn bind_responder() -> io::Result<UdpSocket> {
    let socket = UdpSocket::bind(("0.0.0.0", DISCOVERY_PORT))?;
    socket.set_read_timeout(Some(Duration::from_millis(200)))?;
    Ok(socket)
}

/// Answers discovery broadcasts until `stop` is set.
pub(crate) fn spawn_responder(
    socket: UdpSocket,
    tcp_port: u16,
    name: String,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let reply = format!("{ANNOUNCE_PREFIX} {tcp_port} {name}");
        let mut buf = [0u8; 256];
        while !stop.load(Ordering::Relaxed) {
            match socket.recv_from(&mut buf) {
                Ok((n, from)) if &buf[..n] == DISCOVER_MSG => {
                    let _ = socket.send_to(reply.as_bytes(), from);
                }
                Ok(_) => {}
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
                Err(_) => thread::sleep(Duration::from_millis(200)),
            }
        }
    })
}

/// Broadcasts a discovery request and collects answers until `wait` elapses.
pub fn discover(wait: Duration) -> io::Result<Vec<Peer>> {
    let socket = UdpSocket::bind(("0.0.0.0", 0))?;
    socket.set_broadcast(true)?;

    // UDP may drop packets, so ask a couple of times.
    for _ in 0..2 {
        socket.send_to(DISCOVER_MSG, ("255.255.255.255", DISCOVERY_PORT))?;
    }

    let deadline = Instant::now() + wait;
    let mut seen = HashSet::new();
    let mut peers = Vec::new();
    let mut buf = [0u8; 512];

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        socket.set_read_timeout(Some(remaining))?;
        let (n, from) = match socket.recv_from(&mut buf) {
            Ok(r) => r,
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => break,
            Err(e) => return Err(e),
        };
        let Some(peer) = parse_announce(&buf[..n], from) else {
            continue;
        };
        if seen.insert(peer.addr) {
            peers.push(peer);
        }
    }
    Ok(peers)
}

fn parse_announce(msg: &[u8], from: SocketAddr) -> Option<Peer> {
    let text = std::str::from_utf8(msg).ok()?;
    let mut parts = text.splitn(3, ' ');
    if parts.next()? != ANNOUNCE_PREFIX {
        return None;
    }
    let port: u16 = parts.next()?.parse().ok()?;
    let name = parts.next().unwrap_or("unknown").to_string();
    Some(Peer {
        addr: SocketAddr::new(from.ip(), port),
        name,
    })
}
