//! Reverse connections, for a device that can reach us but that we cannot reach back, because it sits behind NAT
//! (a phone on a VPN whose server masquerades its traffic, say). Discovery, broadcast and a cached address all fail
//! in that direction, but the device can still open connections to us.
//!
//! The device that cannot be reached (the dialer, usually a phone) keeps one encrypted control connection open to
//! each favorite it knows the address of. That is the standby connection, and it also tells the other side the device
//! is online. When the other side (usually a computer) wants a session with it, it sends a one-time token over the
//! standby connection. The dialer opens a second connection and presents the token. The computer plays the asking
//! side of the session over that connection, and the dialer plays the answering side, so the file protocol, browsing
//! and encryption are the same as for a connection opened the usual way. Only who opened the TCP connection differs.
//!
//! A standby connection shows up as a `Peer` in `scan` whose address is where the connection came from. Callers hand
//! that address to `send_files`, `browse::list` and so on, and `send::open_session` recognizes it here.
//!
//! This follows the trust model of the rest of the protocol: the device id is an identifier, not a secret, and a
//! favorite is whoever says it is one.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::identity::{DeviceId, Favorites, Identity};
use crate::protocol::*;
use crate::receive::{ReceiveEvent, serve_session};
use crate::secure::SecureStream;
use crate::send::{begin_session, connect};

/// Control bytes on a standby connection. The computer sends them, and the dialer answers a ping with a pong.
const MSG_PING: u8 = 0;
const MSG_PONG: u8 = 0;
/// Followed by the token of the connection the computer is waiting for.
const MSG_WANT: u8 = 1;

/// How often the computer checks that a standby connection is alive while nobody is asking for it.
const PING_EVERY: Duration = Duration::from_secs(15);
/// How long it waits for the pong before it counts the device as gone.
const PONG_WAIT: Duration = Duration::from_secs(10);
/// How long the dialer waits for anything before it counts the computer as gone (three missed pings).
const SILENCE_LIMIT: Duration = Duration::from_secs(45);
/// How long a session waits for the dialer to open its connection.
const ATTACH_WAIT: Duration = Duration::from_secs(10);

/// How often the dialer looks for a favorite that has no standby connection.
const DIAL_CHECK: Duration = Duration::from_secs(5);
/// How long it waits before dialing a favorite again after losing the connection, and after failing to make one.
const RETRY_AFTER_LOSS: Duration = Duration::from_secs(10);
const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(60);

type Token = [u8; 16];

struct Standby {
    id: DeviceId,
    name: String,
    /// Tokens to send to the dialer. Dropping this ends the connection's keeper thread.
    wants: mpsc::Sender<Token>,
}

/// Devices with a standby connection, by the address the connection came from.
static STANDBY: LazyLock<Mutex<HashMap<SocketAddr, Standby>>> = LazyLock::new(Default::default);
/// Sessions waiting for the dialer to open the connection that carries this token.
static WAITING: LazyLock<Mutex<HashMap<Token, mpsc::Sender<SecureStream<TcpStream>>>>> = LazyLock::new(Default::default);

/// A device that is on standby with us.
pub(crate) struct StandbyPeer {
    /// Where its standby connection came from. Not a way to reach it, but what `open_session` looks it up by.
    pub addr: SocketAddr,
    pub id: DeviceId,
    pub name: String,
}

pub(crate) fn standby_peers() -> Vec<StandbyPeer> {
    let map = STANDBY.lock().unwrap();
    map.iter().map(|(addr, s)| StandbyPeer { addr: *addr, id: s.id, name: s.name.clone() }).collect()
}

pub(crate) fn is_standby(addr: SocketAddr) -> bool {
    STANDBY.lock().unwrap().contains_key(&addr)
}

/// Asks the device on standby at `addr` to open a connection for us and waits for it. The stream that comes back is
/// past the key exchange and the dialer's own introduction, and ready for us to introduce ourselves.
pub(crate) fn request(addr: SocketAddr) -> io::Result<SecureStream<TcpStream>> {
    let gone = || io::Error::new(io::ErrorKind::NotConnected, "the device is no longer connected");
    let wants = STANDBY.lock().unwrap().get(&addr).map(|s| s.wants.clone()).ok_or_else(gone)?;

    let mut token = [0u8; 16];
    getrandom::fill(&mut token).map_err(|e| io::Error::other(format!("no random source: {e}")))?;
    let (tx, rx) = mpsc::channel();
    WAITING.lock().unwrap().insert(token, tx);
    if wants.send(token).is_err() {
        WAITING.lock().unwrap().remove(&token);
        return Err(gone());
    }
    rx.recv_timeout(ATTACH_WAIT).map_err(|_| {
        WAITING.lock().unwrap().remove(&token);
        io::Error::new(io::ErrorKind::TimedOut, "the device did not connect back")
    })
}

/// Hands out the session waiting for `token`, once. Used for a connection that presents a token.
pub(crate) fn take_waiting(token: Token) -> Option<mpsc::Sender<SecureStream<TcpStream>>> {
    WAITING.lock().unwrap().remove(&token)
}

/// Keeps `stream` as the standby connection of `who` until it dies, which is when this returns. `addr` is where it
/// came from.
pub(crate) fn serve_standby(mut stream: SecureStream<TcpStream>, addr: SocketAddr, who: Identity) {
    let (wants, requests) = mpsc::channel();
    {
        let mut map = STANDBY.lock().unwrap();
        // A device that connects again replaces its earlier connection, which may have died without us noticing.
        // Dropping the old entry makes its thread end.
        map.retain(|_, s| s.id != who.id);
        map.insert(addr, Standby { id: who.id, name: who.name, wants });
    }
    let _ = stream.get_ref().set_read_timeout(Some(PONG_WAIT));
    let _ = keep(&mut stream, &requests);
    STANDBY.lock().unwrap().remove(&addr);
}

fn keep(stream: &mut SecureStream<TcpStream>, requests: &mpsc::Receiver<Token>) -> io::Result<()> {
    loop {
        match requests.recv_timeout(PING_EVERY) {
            Ok(token) => {
                stream.write_all(&[MSG_WANT])?;
                stream.write_all(&token)?;
                stream.flush()?;
            }
            Err(RecvTimeoutError::Timeout) => {
                stream.write_all(&[MSG_PING])?;
                stream.flush()?;
                if read_u8(stream)? != MSG_PONG {
                    return Err(invalid("the device did not answer a ping"));
                }
            }
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

/// What the dialing side needs to answer the sessions it is asked for.
struct Dial {
    me: Identity,
    favorites: Favorites,
    dir: PathBuf,
    on_event: Arc<dyn Fn(ReceiveEvent) + Send + Sync>,
    stop: Arc<AtomicBool>,
}

enum Slot {
    Running,
    /// Not before this time.
    Wait(Instant),
}

/// Keeps a standby connection to every favorite whose address is known, until `stop` is set. Connections that are
/// already open end on their own within a ping or two of that.
pub(crate) fn spawn_dialer(
    me: Identity,
    favorites: Favorites,
    dir: PathBuf,
    on_event: Arc<dyn Fn(ReceiveEvent) + Send + Sync>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    let dial = Arc::new(Dial { me, favorites, dir, on_event, stop });
    let slots: Arc<Mutex<HashMap<DeviceId, Slot>>> = Arc::default();
    thread::spawn(move || {
        while !dial.stop.load(Ordering::Relaxed) {
            for favorite in dial.favorites.list() {
                let Some(sighting) = dial.favorites.sighting(favorite.id) else { continue };
                let addr = SocketAddr::new(sighting.ip, sighting.port.unwrap_or(DEFAULT_TCP_PORT));
                {
                    let mut slots = slots.lock().unwrap();
                    match slots.get(&favorite.id) {
                        Some(Slot::Running) => continue,
                        Some(Slot::Wait(until)) if *until > Instant::now() => continue,
                        _ => {}
                    }
                    slots.insert(favorite.id, Slot::Running);
                }
                let (dial, slots) = (dial.clone(), slots.clone());
                thread::spawn(move || {
                    let (connected, _) = stand_by(addr, &dial);
                    let wait = if connected { RETRY_AFTER_LOSS } else { RETRY_AFTER_FAILURE };
                    slots.lock().unwrap().insert(favorite.id, Slot::Wait(Instant::now() + wait));
                });
            }
            // Short sleeps, so that stopping is noticed quickly.
            let until = Instant::now() + DIAL_CHECK;
            while Instant::now() < until && !dial.stop.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(250));
            }
        }
    })
}

/// Opens a standby connection to `addr` and answers it until it ends. Returns whether the connection was
/// established, and how it ended.
fn stand_by(addr: SocketAddr, dial: &Arc<Dial>) -> (bool, io::Result<()>) {
    let mut stream = match connect(addr) {
        Ok(stream) => stream,
        Err(e) => return (false, Err(e)),
    };
    if let Err(e) = begin_session(&mut stream, &dial.me, COMMAND_STANDBY, &[]) {
        return (false, Err(e));
    }
    let _ = stream.get_ref().set_read_timeout(Some(SILENCE_LIMIT));
    (true, listen(&mut stream, addr, dial))
}

fn listen(stream: &mut SecureStream<TcpStream>, addr: SocketAddr, dial: &Arc<Dial>) -> io::Result<()> {
    while !dial.stop.load(Ordering::Relaxed) {
        match read_u8(stream)? {
            MSG_PING => {
                stream.write_all(&[MSG_PONG])?;
                stream.flush()?;
            }
            MSG_WANT => {
                let mut token = [0u8; 16];
                stream.read_exact(&mut token)?;
                let dial = dial.clone();
                thread::spawn(move || dial_back(addr, token, &dial));
            }
            other => return Err(invalid(format!("unexpected control byte {other}"))),
        }
    }
    Ok(())
}

/// Opens the connection a session on the other side is waiting for, and answers that session over it.
fn dial_back(addr: SocketAddr, token: Token, dial: &Dial) {
    let peer = addr.to_string();
    let result = (|| {
        let mut stream = connect(addr)?;
        begin_session(&mut stream, &dial.me, COMMAND_ATTACH, &token)?;
        serve_session(stream, Some(addr), false, &dial.dir, &peer, &dial.favorites, &*dial.on_event)
    })();
    (dial.on_event)(ReceiveEvent::SessionEnded { peer, error: result.err().map(|e| e.to_string()) });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browse;
    use crate::identity::DeviceKind;
    use crate::receive::handle_session;
    use crate::send::{self, Entry};
    use std::fs;
    use std::net::TcpListener;
    use std::ops::ControlFlow;

    /// Waits up to five seconds for `found` to return something.
    fn wait_for<T>(mut found: impl FnMut() -> Option<T>) -> T {
        for _ in 0..100 {
            if let Some(value) = found() {
                return value;
            }
            thread::sleep(Duration::from_millis(50));
        }
        panic!("timed out");
    }

    #[test]
    fn a_device_on_standby_is_asked_over_the_connection_it_opened() {
        let base = std::env::temp_dir().join(format!("airlynx-reverse-{}", DeviceId::random()));
        let (mac_dir, phone_dir) = (base.join("mac"), base.join("phone"));
        fs::create_dir_all(&mac_dir).unwrap();
        fs::create_dir_all(&phone_dir).unwrap();
        fs::write(phone_dir.join("photo.txt"), b"from the phone").unwrap();

        let mac = Identity::new(DeviceId::random(), "Mac");
        let phone = Identity::new(DeviceId::random(), "Phone").with_kind(DeviceKind::Mobile);
        let mac_favorites = Favorites::in_memory();
        mac_favorites.add(phone.id, "Phone", DeviceKind::Mobile).unwrap();
        let phone_favorites = Favorites::in_memory();
        phone_favorites.add(mac.id, "Mac", DeviceKind::Desktop).unwrap();

        // The computer accepts connections the way the receiver does.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mac_addr = listener.local_addr().unwrap();
        {
            let (dir, favorites) = (mac_dir.clone(), mac_favorites.clone());
            thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let (dir, favorites) = (dir.clone(), favorites.clone());
                    thread::spawn(move || {
                        let _ = handle_session(stream, &dir, "test", &favorites, &|_| {});
                    });
                }
            });
        }

        // The phone knows where the computer is, and keeps a connection open to it.
        phone_favorites.remember_peer(mac.id, mac_addr.ip(), mac_addr.port(), true, None);
        let stop = Arc::new(AtomicBool::new(false));
        let dialer = spawn_dialer(phone.clone(), phone_favorites, phone_dir.clone(), Arc::new(|_| {}), stop.clone());
        let standby = wait_for(|| standby_peers().into_iter().find(|p| p.id == phone.id));
        assert_eq!(standby.name, "Phone");

        // Listing, downloading and sending all run over connections the phone opened on request.
        let entries = browse::list(standby.addr, &mac, "").unwrap();
        assert_eq!(entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["photo.txt"]);
        let saved = browse::download(standby.addr, &mac, "photo.txt", &mac_dir, |_, _| {}).unwrap();
        assert_eq!(fs::read(saved).unwrap(), b"from the phone");

        let src = mac_dir.join("note.txt");
        fs::write(&src, b"from the mac").unwrap();
        let entry = Entry { path: src, name: "note.txt".into(), size: 12 };
        let report = send::send_files(standby.addr, &mac, &[entry], |_| ControlFlow::Continue(())).unwrap();
        assert_eq!(report.sent, ["note.txt"]);
        assert_eq!(fs::read(phone_dir.join("note.txt")).unwrap(), b"from the mac");

        stop.store(true, Ordering::Relaxed);
        dialer.join().unwrap();
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_device_that_is_not_on_standby_cannot_be_asked() {
        let addr: SocketAddr = "127.0.0.1:9".parse().unwrap();
        assert!(!is_standby(addr));
        assert_eq!(request(addr).err().map(|e| e.kind()), Some(io::ErrorKind::NotConnected));
    }

    #[test]
    fn an_unknown_token_is_not_waited_for() {
        assert!(take_waiting([7; 16]).is_none());
    }
}
