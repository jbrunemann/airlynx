//! UDP discovery: devices ask "AIRLYNX_DISCOVER <id>" and every other running device answers
//! "AIRLYNX_HERE <tcp_port> <id> <mutual> <kind> <name>" to the asker's address.
//! `mutual` is 1 when the answering device has the asker as a favorite.
//!
//! The question reaches devices three ways, because no single one works on every network:
//! broadcast on every interface (the subnet's real broadcast address, not a guessed /24, plus the
//! limited broadcast on the default route), unicast to the last known address of each favorite, and
//! unicast to every address mDNS has found (see `mdns`). Replies are the same whichever way
//! the question arrived.

use std::collections::HashSet;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::identity::{DeviceId, DeviceKind, Favorites, Identity, Route, clean_name};
use crate::mdns;
use crate::protocol::{ANNOUNCE_PREFIX, DISCOVER_PREFIX, DISCOVERY_PORT};
use crate::stats;

/// How often a running scan looks for addresses mDNS has found since it started.
const MDNS_POLL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone)]
pub struct Peer {
    pub addr: SocketAddr,
    pub id: DeviceId,
    pub name: String,
    /// The peer has us as a favorite.
    pub mutual: bool,
    pub kind: DeviceKind,
}

/// Binds the discovery port. Done separately from `spawn_responder` so bind
/// errors surface to the caller instead of inside a background thread.
pub(crate) fn bind_responder(poll_interval: Duration) -> io::Result<UdpSocket> {
    bind_responder_on(Ipv4Addr::UNSPECIFIED, DISCOVERY_PORT, poll_interval)
}

fn bind_responder_on(ip: Ipv4Addr, port: u16, poll_interval: Duration) -> io::Result<UdpSocket> {
    let socket = UdpSocket::bind((ip, port))?;
    socket.set_read_timeout(Some(poll_interval))?;
    Ok(socket)
}

/// Answers discovery broadcasts until `stop` is set.
pub(crate) fn spawn_responder(
    socket: UdpSocket,
    tcp_port: u16,
    me: Identity,
    favorites: Favorites,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut buf = [0u8; 256];
        while !stop.load(Ordering::Relaxed) {
            stats::discovery_poll();
            match socket.recv_from(&mut buf) {
                Ok((n, from)) => {
                    let Some(asker) = parse_request(&buf[..n]) else { continue };
                    let mutual = u8::from(favorites.contains(asker));
                    let reply = format!("{ANNOUNCE_PREFIX} {tcp_port} {} {mutual} {} {}", me.id, me.kind.code(), me.name);
                    let _ = socket.send_to(reply.as_bytes(), from);
                    stats::discovery_reply();
                }
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
                Err(_) => thread::sleep(Duration::from_millis(200)),
            }
        }
    })
}

/// Asks the network who is there and collects answers until `wait` elapses, using broadcast and mDNS.
/// `me` is left out of the result, so a device never lists itself.
pub fn discover(me: &Identity, wait: Duration) -> io::Result<Vec<Peer>> {
    discover_with(me, wait, &[])
}

/// Like `discover`, and also asks each of `hints` directly, for devices that broadcast and mDNS miss.
pub fn discover_with(me: &Identity, wait: Duration, hints: &[IpAddr]) -> io::Result<Vec<Peer>> {
    discover_on(DISCOVERY_PORT, me, wait, hints)
}

/// `discover_with` against devices listening on `port`; tests use one other than the real port.
fn discover_on(port: u16, me: &Identity, wait: Duration, hints: &[IpAddr]) -> io::Result<Vec<Peer>> {
    let request = format!("{DISCOVER_PREFIX} {}", me.id);
    let request = request.as_bytes();

    // The wildcard socket takes the limited broadcast (default route) and all unicast probes.
    let wildcard = Arc::new(UdpSocket::bind(("0.0.0.0", 0))?);
    wildcard.set_broadcast(true)?;
    let mut sockets = vec![wildcard.clone()];
    let mut targets: Vec<(usize, SocketAddr)> = vec![(0, (Ipv4Addr::BROADCAST, port).into())];

    // One socket per interface, bound to its address so the question and the answers use that
    // interface, aimed at that subnet's broadcast address. A computer on Wi-Fi plus a VPN or a hotspot
    // would otherwise only ask on the default route.
    for (ip, broadcast) in interface_broadcasts() {
        let Ok(socket) = UdpSocket::bind((ip, 0)) else { continue };
        if socket.set_broadcast(true).is_err() {
            continue;
        }
        targets.push((sockets.len(), (broadcast, port).into()));
        sockets.push(Arc::new(socket));
    }

    let mut last_error = None;
    let mut sent_any = false;
    let mut note = |result: io::Result<usize>| match result {
        Ok(_) => sent_any = true,
        Err(e) => last_error = Some(e),
    };
    // UDP may drop packets, so ask a couple of times.
    for _ in 0..2 {
        for &(i, to) in &targets {
            note(sockets[i].send_to(request, to));
        }
    }
    let mut probed: HashSet<IpAddr> = HashSet::new();
    for &ip in hints {
        if probed.insert(ip) {
            note(wildcard.send_to(request, (ip, port)));
        }
    }
    if !sent_any {
        return Err(last_error.unwrap_or_else(|| io::Error::other("no network interface to ask on")));
    }

    let deadline = Instant::now() + wait;
    let (tx, rx) = mpsc::channel();
    let mut seen = HashSet::new();
    let mut peers = Vec::new();

    thread::scope(|scope| {
        for socket in &sockets {
            let tx = tx.clone();
            scope.spawn(move || {
                let mut buf = [0u8; 512];
                loop {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() || socket.set_read_timeout(Some(remaining)).is_err() {
                        break;
                    }
                    match socket.recv_from(&mut buf) {
                        Ok((n, from)) => {
                            if let Some(peer) = parse_announce(&buf[..n], from)
                                && tx.send(peer).is_err()
                            {
                                break;
                            }
                        }
                        Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => break,
                        Err(_) => thread::sleep(Duration::from_millis(50)),
                    }
                }
            });
        }
        drop(tx);

        loop {
            // mDNS answers trickle in while we wait, so keep probing what it finds.
            for ip in mdns::candidates(me.id) {
                if probed.insert(ip) {
                    let _ = wildcard.send_to(request, (ip, port));
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match rx.recv_timeout(remaining.min(MDNS_POLL)) {
                // A device with several interfaces answers once per interface.
                Ok(peer) => {
                    if peer.id != me.id && seen.insert(peer.id) {
                        peers.push(peer);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });
    Ok(peers)
}

/// Like `discover_with`, using the last known address of every favorite, and remembering what it
/// learns about the favorites it finds (address, port, route) for next time.
pub fn scan(me: &Identity, favorites: &Favorites, wait: Duration) -> io::Result<Vec<Peer>> {
    let peers = discover_with(me, wait, &favorites.known_addresses())?;
    for peer in &peers {
        if favorites.contains(peer.id) {
            favorites.remember_peer(peer.id, peer.addr.ip(), peer.addr.port(), peer.mutual, route_to(peer.addr.ip()));
        }
    }
    Ok(peers)
}

/// The interface that shares a subnet with `ip` (the most specific one if several do), or `None` when
/// `ip` is not on a directly connected network and traffic goes through the default gateway.
pub fn route_to(ip: IpAddr) -> Option<Route> {
    let IpAddr::V4(ip) = ip else { return None };
    let interfaces = if_addrs::get_if_addrs().ok()?;
    interfaces
        .iter()
        .filter(|interface| !interface.is_loopback())
        .filter_map(|interface| match &interface.addr {
            if_addrs::IfAddr::V4(v4) if same_subnet(ip, v4.ip, v4.netmask) => Some((v4.prefixlen, interface.name.clone(), v4.ip)),
            _ => None,
        })
        .max_by_key(|(prefix, ..)| *prefix)
        .map(|(_, interface, local_ip)| Route { interface, local_ip })
}

fn same_subnet(a: Ipv4Addr, b: Ipv4Addr, netmask: Ipv4Addr) -> bool {
    u32::from(a) & u32::from(netmask) == u32::from(b) & u32::from(netmask)
}

/// The broadcast address of the subnet `ip` is in. On a /16 this reaches the other /24 ranges too,
/// which a broadcast address guessed as `x.y.z.255` would not.
fn directed_broadcast(ip: Ipv4Addr, netmask: Ipv4Addr) -> Ipv4Addr {
    Ipv4Addr::from(u32::from(ip) | !u32::from(netmask))
}

/// The local address and subnet broadcast address of every usable IPv4 interface.
fn interface_broadcasts() -> Vec<(Ipv4Addr, Ipv4Addr)> {
    let Ok(interfaces) = if_addrs::get_if_addrs() else { return Vec::new() };
    let mut found: Vec<(Ipv4Addr, Ipv4Addr)> = Vec::new();
    for interface in interfaces {
        let if_addrs::IfAddr::V4(v4) = &interface.addr else { continue };
        if interface.is_loopback() || interface.is_p2p || v4.ip.is_link_local() || v4.prefixlen >= 31 {
            continue;
        }
        let broadcast = directed_broadcast(v4.ip, v4.netmask);
        if !found.contains(&(v4.ip, broadcast)) {
            found.push((v4.ip, broadcast));
        }
    }
    found
}

fn parse_request(msg: &[u8]) -> Option<DeviceId> {
    let text = std::str::from_utf8(msg).ok()?;
    let (prefix, id) = text.split_once(' ')?;
    if prefix != DISCOVER_PREFIX {
        return None;
    }
    id.parse().ok()
}

fn parse_announce(msg: &[u8], from: SocketAddr) -> Option<Peer> {
    let text = std::str::from_utf8(msg).ok()?;
    let mut parts = text.splitn(6, ' ');
    if parts.next()? != ANNOUNCE_PREFIX {
        return None;
    }
    let port: u16 = parts.next()?.parse().ok()?;
    let id: DeviceId = parts.next()?.parse().ok()?;
    let mutual = parts.next()? == "1";
    let kind = DeviceKind::from_code(parts.next()?);
    let name = clean_name(parts.next().unwrap_or(""));
    Some(Peer {
        addr: SocketAddr::new(from.ip(), port),
        id,
        name,
        mutual,
        kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announce_round_trip() {
        let id = DeviceId::random();
        let from: SocketAddr = "192.168.1.5:5555".parse().unwrap();
        let peer = parse_announce(format!("{ANNOUNCE_PREFIX} 47800 {id} 1 d Living room Mac").as_bytes(), from).unwrap();
        assert_eq!(peer.addr, "192.168.1.5:47800".parse().unwrap());
        assert_eq!((peer.id, peer.mutual, peer.name.as_str()), (id, true, "Living room Mac"));
        assert_eq!(peer.kind, DeviceKind::Desktop);
        let peer = parse_announce(format!("{ANNOUNCE_PREFIX} 47800 {id} 0 x Mac").as_bytes(), from).unwrap();
        assert_eq!(peer.kind, DeviceKind::Macsplorer);
        // Older formats lack the id or the kind and must not parse.
        assert!(parse_announce(b"AIRLYNX_HERE 47800 Old Mac", from).is_none());
        assert!(parse_announce(format!("{ANNOUNCE_PREFIX} 47800 {id} 1").as_bytes(), from).is_none());
    }

    #[test]
    fn broadcast_follows_the_netmask() {
        let ip = |s: &str| s.parse::<Ipv4Addr>().unwrap();
        assert_eq!(directed_broadcast(ip("10.11.19.114"), ip("255.255.0.0")), ip("10.11.255.255"));
        assert_eq!(directed_broadcast(ip("192.168.1.20"), ip("255.255.255.0")), ip("192.168.1.255"));
        assert_eq!(directed_broadcast(ip("172.16.5.9"), ip("255.255.254.0")), ip("172.16.5.255"));
    }

    #[test]
    fn subnets_follow_the_netmask() {
        let ip = |s: &str| s.parse::<Ipv4Addr>().unwrap();
        assert!(same_subnet(ip("10.11.19.114"), ip("10.11.46.85"), ip("255.255.0.0")));
        assert!(!same_subnet(ip("10.11.19.114"), ip("10.11.46.85"), ip("255.255.255.0")));
    }

    #[test]
    fn loopback_and_remote_addresses_have_no_route() {
        assert_eq!(route_to(Ipv4Addr::LOCALHOST.into()), None);
        assert_eq!(route_to("::1".parse().unwrap()), None);
    }

    #[test]
    fn interfaces_have_usable_broadcasts() {
        for (ip, broadcast) in interface_broadcasts() {
            assert!(!ip.is_loopback() && !ip.is_link_local());
            assert_ne!(ip, broadcast);
        }
    }

    /// Starts a responder for `id` on an ephemeral loopback port, as another device would run it. Being
    /// bound to loopback, it can't hear broadcasts, only a direct question.
    fn responder(id: DeviceId, favorites: Favorites, stop: &Arc<AtomicBool>) -> (u16, JoinHandle<()>) {
        let socket = bind_responder_on(Ipv4Addr::LOCALHOST, 0, Duration::from_millis(50)).unwrap();
        let port = socket.local_addr().unwrap().port();
        let me = Identity::new(id, "Test phone").with_kind(DeviceKind::Mobile);
        (port, spawn_responder(socket, 47800, me, favorites, stop.clone()))
    }

    #[test]
    fn a_hinted_address_is_asked_directly() {
        let (asker, other) = (Identity::new(DeviceId::random(), "Mac"), DeviceId::random());
        let favorites = Favorites::in_memory();
        favorites.add(asker.id, "Mac", DeviceKind::Desktop).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (port, thread) = responder(other, favorites, &stop);

        let loopback: IpAddr = Ipv4Addr::LOCALHOST.into();
        let found = discover_on(port, &asker, Duration::from_millis(600), &[loopback, loopback]).unwrap();
        stop.store(true, Ordering::Relaxed);
        thread.join().unwrap();

        assert_eq!(found.len(), 1);
        assert_eq!((found[0].id, found[0].mutual, found[0].kind), (other, true, DeviceKind::Mobile));
        assert_eq!(found[0].addr, SocketAddr::new(loopback, 47800));
    }

    #[test]
    fn without_a_hint_nothing_is_found() {
        let asker = Identity::new(DeviceId::random(), "Mac");
        let stop = Arc::new(AtomicBool::new(false));
        let (port, thread) = responder(DeviceId::random(), Favorites::in_memory(), &stop);
        let found = discover_on(port, &asker, Duration::from_millis(300), &[]).unwrap();
        stop.store(true, Ordering::Relaxed);
        thread.join().unwrap();
        assert!(found.is_empty());
    }

    #[test]
    fn request_needs_an_id() {
        let id = DeviceId::random();
        assert_eq!(parse_request(format!("{DISCOVER_PREFIX} {id}").as_bytes()), Some(id));
        assert_eq!(parse_request(DISCOVER_PREFIX.as_bytes()), None);
    }
}
