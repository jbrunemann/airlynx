use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::browse;
use crate::busy::Busy;
use crate::discovery;
use crate::identity::{DeviceId, Favorites, Identity};
use crate::mdns::{self, Advertisement};
use crate::protocol::*;
use crate::reverse;
use crate::secure::{Ephemeral, SecureStream};
use crate::stats;
use crate::stop::Stop;

pub(crate) static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Emit a progress event roughly every this many bytes.
const PROGRESS_STEP: u64 = 512 * 1024;

#[derive(Debug, Clone)]
pub struct ReceiverConfig {
    pub dir: PathBuf,
    pub port: u16,
    pub identity: Identity,
    /// Only these devices may send. Shared, so changes apply to the running receiver.
    pub favorites: Favorites,
    /// Keep a connection open to every favorite we know the address of, so that it can start sessions with us even
    /// when we cannot be reached from its side (see `reverse`). Meant for a phone behind NAT or a VPN.
    pub reverse: bool,
}

/// Everything a receiver reports. `peer` is the sender's "ip:port".
#[derive(Debug, Clone)]
pub enum ReceiveEvent {
    /// A device that is not a favorite tried to send. Nothing was accepted.
    Refused { peer: String, id: DeviceId, name: String },
    /// A favorite tried to send, but the save folder cannot be written to. Nothing was accepted.
    NotWritable { peer: String, error: String },
    Connected { peer: String, files: u32 },
    FileStarted { peer: String, name: String, size: u64 },
    FileProgress { peer: String, name: String, received: u64, size: u64 },
    FileSaved { peer: String, path: PathBuf, size: u64 },
    FileRejected { peer: String, name: String, reason: String },
    /// A favorite looked into the save folder. `path` is relative to it, "" for the folder itself.
    Listed { peer: String, path: String },
    /// A favorite downloaded `path` from the save folder.
    Served { peer: String, path: String, size: u64 },
    SessionEnded { peer: String, error: Option<String> },
}

/// A running receiver. Stops listening when `stop` is called or it is dropped.
/// Transfers already in progress run to completion.
pub struct ReceiverHandle {
    port: u16,
    /// The discovery port, which the responder thread is blocked on.
    discovery_port: u16,
    stop: Stop,
    threads: Vec<JoinHandle<()>>,
    /// Keeps the mDNS registration alive (none if mDNS is unavailable).
    _advertisement: Option<Advertisement>,
}

impl ReceiverHandle {
    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if self.threads.is_empty() {
            return;
        }
        self.stop.set();
        // The accept and discovery threads block until something arrives, rather than waking up to check
        // for a stop, which costs battery. Send each of them something so they see it.
        let _ = TcpStream::connect_timeout(&(Ipv4Addr::LOCALHOST, self.port).into(), Duration::from_secs(1));
        if let Ok(udp) = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)) {
            let _ = udp.send_to(b"", (Ipv4Addr::LOCALHOST, self.discovery_port));
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

impl Drop for ReceiverHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Binds the TCP and discovery ports and starts accepting transfers in the background.
pub fn start(
    config: ReceiverConfig,
    on_event: impl Fn(ReceiveEvent) + Send + Sync + 'static,
) -> io::Result<ReceiverHandle> {
    fs::create_dir_all(&config.dir)?;
    let listener = TcpListener::bind(("0.0.0.0", config.port))?;
    let port = listener.local_addr()?.port();
    let udp = discovery::bind_responder()?;
    let discovery_port = udp.local_addr()?.port();

    let stop = Stop::default();
    let responder = discovery::spawn_responder(
        udp,
        port,
        config.identity.clone(),
        config.favorites.clone(),
        stop.clone(),
    );

    let advertisement = mdns::advertise(&config.identity, port);

    let on_event: Arc<dyn Fn(ReceiveEvent) + Send + Sync> = Arc::new(on_event);
    let accept_stop = stop.clone();
    let dir = config.dir;
    let favorites = config.favorites;
    let dialer = config.reverse.then(|| {
        reverse::spawn_dialer(config.identity.clone(), favorites.clone(), dir.clone(), on_event.clone(), stop.clone())
    });
    let acceptor = thread::spawn(move || {
        loop {
            let accepted = listener.accept();
            stats::accept_poll();
            if accept_stop.is_set() {
                break;
            }
            match accepted {
                Ok((stream, addr)) => {
                    stats::session();
                    let dir = dir.clone();
                    let on_event = on_event.clone();
                    let favorites = favorites.clone();
                    thread::spawn(move || {
                        let peer = addr.to_string();
                        let result = handle_session(stream, &dir, &peer, &favorites, &*on_event);
                        // A standby or attach connection is not a transfer, and says nothing when it ends.
                        if !matches!(result, Ok(Handled::Reverse)) {
                            on_event(ReceiveEvent::SessionEnded {
                                peer,
                                error: result.err().map(|e| e.to_string()),
                            });
                        }
                    });
                }
                // Out of file descriptors and the like: don't spin.
                Err(_) => thread::sleep(Duration::from_millis(200)),
            }
        }
    });

    Ok(ReceiverHandle {
        port,
        discovery_port,
        stop,
        threads: [Some(responder), Some(acceptor), dialer].into_iter().flatten().collect(),
        _advertisement: advertisement,
    })
}

/// What a connection turned out to be, for the caller deciding whether to report it.
pub(crate) enum Handled {
    /// A session that ran (or was refused), as far as it got.
    Session,
    /// A device asking to be kept on standby, or a connection it opened for a session we asked for (see `reverse`).
    Reverse,
}

pub(crate) fn handle_session(
    mut stream: TcpStream,
    dir: &Path,
    peer: &str,
    favorites: &Favorites,
    on_event: &(dyn Fn(ReceiveEvent) + Send + Sync),
) -> io::Result<Handled> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let peer_addr = stream.peer_addr().ok();

    let mut magic = [0u8; 4];
    stream.read_exact(&mut magic)?;
    let version = read_u8(&mut stream)?;
    if &magic != MAGIC || version != VERSION {
        stream.write_all(&[STATUS_REJECTED])?;
        return Err(invalid(format!("bad handshake (version {version})")));
    }
    let mut peer_public = [0u8; 32];
    stream.read_exact(&mut peer_public)?;
    let ephemeral = Ephemeral::new()?;
    stream.write_all(&[STATUS_OK])?;
    stream.write_all(&ephemeral.public())?;
    let stream = ephemeral.into_stream(stream, peer_public, false)?;
    serve_session(stream, peer_addr, true, dir, peer, favorites, on_event)
}

/// Answers one session on a connection that is past the key exchange. `accepted` is true if the other device
/// opened the connection, which is the only case where it may ask for standby or attach. When the device on the other
/// end got our attach (see `reverse`), it is the one who dialed and we are the one who asks, but the questions that
/// get answered are the same, so this runs on that connection too.
pub(crate) fn serve_session(
    mut stream: SecureStream<TcpStream>,
    peer_addr: Option<SocketAddr>,
    accepted: bool,
    dir: &Path,
    peer: &str,
    favorites: &Favorites,
    on_event: &(dyn Fn(ReceiveEvent) + Send + Sync),
) -> io::Result<Handled> {
    let peer_ip: Option<IpAddr> = peer_addr.map(|a| a.ip());

    let sender = read_hello(&mut stream)?;
    let command = read_u8(&mut stream)?;
    // An attach carries its token right after the command, before anything is answered.
    let token = if accepted && command == COMMAND_ATTACH {
        let mut token = [0u8; 16];
        stream.read_exact(&mut token)?;
        Some(token)
    } else {
        None
    };
    if !favorites.contains(sender.id) {
        stream.write_all(&[STATUS_NOT_FAVORITE])?;
        stream.flush()?;
        on_event(ReceiveEvent::Refused { peer: peer.into(), id: sender.id, name: sender.name });
        return Ok(Handled::Session);
    }
    let known = match command {
        COMMAND_SEND | COMMAND_LIST | COMMAND_GET => true,
        COMMAND_STANDBY | COMMAND_ATTACH => accepted,
        _ => false,
    };
    if !known {
        stream.write_all(&[STATUS_REJECTED])?;
        stream.flush()?;
        return Err(invalid(format!("unknown command {command}")));
    }
    if let Some(token) = token {
        // Not one we are waiting for (it timed out, or was never ours): turn it away.
        let Some(waiting) = reverse::take_waiting(token) else {
            stream.write_all(&[STATUS_REJECTED])?;
            stream.flush()?;
            return Ok(Handled::Reverse);
        };
        stream.write_all(&[STATUS_OK])?;
        stream.flush()?;
        // The session is now ours to ask for, by whoever is waiting for this connection.
        let _ = waiting.send(stream);
        return Ok(Handled::Reverse);
    }
    // Say so before the sender streams any data, rather than failing halfway through the first file.
    // Only favorites get here, so a stranger learns nothing about this device's storage.
    if command == COMMAND_SEND && let Err(e) = check_writable(dir) {
        stream.write_all(&[STATUS_NO_WRITE_ACCESS])?;
        stream.flush()?;
        on_event(ReceiveEvent::NotWritable { peer: peer.into(), error: e.to_string() });
        return Ok(Handled::Session);
    }
    stream.write_all(&[STATUS_OK])?;
    stream.flush()?;
    if command == COMMAND_STANDBY {
        // Its address is where it dialed from, which says nothing about how to reach it, so it is not remembered.
        if let Some(addr) = peer_addr {
            reverse::serve_standby(stream, addr, sender);
        }
        return Ok(Handled::Reverse);
    }
    let _busy = Busy::new();
    if let Some(ip) = peer_ip {
        favorites.remember_address(sender.id, ip);
    }

    match command {
        COMMAND_LIST => {
            let path = browse::serve_list(&mut stream, dir)?;
            on_event(ReceiveEvent::Listed { peer: peer.into(), path });
            return Ok(Handled::Session);
        }
        COMMAND_GET => {
            let (path, size) = browse::serve_get(&mut stream, dir)?;
            on_event(ReceiveEvent::Served { peer: peer.into(), path, size });
            return Ok(Handled::Session);
        }
        _ => {}
    }

    let count = read_u32(&mut stream)?;
    on_event(ReceiveEvent::Connected { peer: peer.into(), files: count });

    for _ in 0..count {
        let name_len = read_u16(&mut stream)? as usize;
        if name_len == 0 || name_len > MAX_NAME_LEN {
            return Err(invalid("file name length out of range"));
        }
        let mut name_buf = vec![0u8; name_len];
        stream.read_exact(&mut name_buf)?;
        let raw_name = String::from_utf8(name_buf).map_err(|_| invalid("file name is not UTF-8"))?;
        let size = read_u64(&mut stream)?;
        let name = sanitize_name(&raw_name)?;
        on_event(ReceiveEvent::FileStarted { peer: peer.into(), name: name.clone(), size });

        let tmp_path = dir.join(format!(
            ".{name}.{}.{}.part",
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let progress = |received| {
            on_event(ReceiveEvent::FileProgress { peer: peer.into(), name: name.clone(), received, size });
        };
        let (actual, expected) = match receive_file(&mut stream, &tmp_path, size, progress) {
            Ok(hashes) => hashes,
            Err(e) => {
                let _ = fs::remove_file(&tmp_path);
                return Err(e);
            }
        };

        if actual == expected {
            let final_path = unique_path(dir, &name);
            fs::rename(&tmp_path, &final_path)?;
            stream.write_all(&[STATUS_OK])?;
            stream.flush()?;
            on_event(ReceiveEvent::FileSaved { peer: peer.into(), path: final_path, size });
        } else {
            let _ = fs::remove_file(&tmp_path);
            stream.write_all(&[STATUS_HASH_MISMATCH])?;
            stream.flush()?;
            on_event(ReceiveEvent::FileRejected {
                peer: peer.into(),
                name,
                reason: "checksum mismatch".into(),
            });
        }
    }
    Ok(Handled::Session)
}

/// Makes sure `dir` exists and a file can be created in it. Permissions can change while the receiver
/// runs (all-files access revoked, a read-only volume, a deleted folder), so this is asked per session.
fn check_writable(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let probe = dir.join(format!(
        ".airlynx-write-test.{}.{}",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    File::create(&probe)?;
    fs::remove_file(&probe)
}

/// Streams `size` bytes into `path`, then reads the sender's hash.
/// Returns (computed hash, sender hash).
pub(crate) fn receive_file(
    stream: &mut impl Read,
    path: &Path,
    size: u64,
    mut progress: impl FnMut(u64),
) -> io::Result<([u8; 32], [u8; 32])> {
    let mut file = File::create(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK_SIZE];
    let mut received = 0u64;
    let mut next_report = PROGRESS_STEP;

    while received < size {
        let want = (size - received).min(buf.len() as u64) as usize;
        let n = stream.read(&mut buf[..want])?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed mid-file"));
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])?;
        received += n as u64;
        if received >= next_report || received == size {
            progress(received);
            next_report = received + PROGRESS_STEP;
        }
    }
    file.sync_all()?;

    let mut expected = [0u8; 32];
    stream.read_exact(&mut expected)?;
    Ok((hasher.finalize().into(), expected))
}

/// Reduces a sender-supplied name to a plain file name so it cannot escape `dir`.
pub(crate) fn sanitize_name(raw: &str) -> io::Result<String> {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base.chars().filter(|c| !c.is_control()).collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        return Err(invalid(format!("unusable file name {raw:?}")));
    }
    Ok(cleaned.to_string())
}

/// Returns `dir/name`, or `dir/stem (n).ext` if that already exists.
pub(crate) fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let path = Path::new(name);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    let ext = path.extension().and_then(|e| e.to_str());
    (1..)
        .map(|i| match ext {
            Some(ext) => dir.join(format!("{stem} ({i}).{ext}")),
            None => dir.join(format!("{stem} ({i})")),
        })
        .find(|p| !p.exists())
        .expect("unbounded range")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_paths() {
        assert_eq!(sanitize_name("../../etc/passwd").unwrap(), "passwd");
        assert_eq!(sanitize_name("C:\\evil\\a.txt").unwrap(), "a.txt");
        assert_eq!(sanitize_name("photo.jpg").unwrap(), "photo.jpg");
        assert!(sanitize_name("..").is_err());
        assert!(sanitize_name("dir/").is_err());
    }

    use crate::identity::{DeviceId, DeviceKind};
    use crate::send::{self, Entry};
    use std::net::{SocketAddr, TcpListener};
    use std::ops::ControlFlow;
    use std::sync::Mutex;

    /// Runs one receiver session on loopback and returns the sender's result
    /// and the receiver's events.
    fn loopback(favorites: &Favorites, sender: &Identity) -> (io::Result<send::Report>, Vec<ReceiveEvent>) {
        loopback_to(favorites, sender, |base| base.join("in"))
    }

    /// Like `loopback`, with the receiver saving to the folder `save_dir` picks inside the scratch folder.
    fn loopback_to(
        favorites: &Favorites,
        sender: &Identity,
        save_dir: impl FnOnce(&Path) -> PathBuf,
    ) -> (io::Result<send::Report>, Vec<ReceiveEvent>) {
        let dir = std::env::temp_dir().join(format!("airlynx-recv-{}", DeviceId::random()));
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("hello.txt");
        fs::write(&src, b"hello favorites").unwrap();
        let entries = vec![Entry { path: src, name: "hello.txt".into(), size: 15 }];

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let server = {
            let (favorites, events, dir) = (favorites.clone(), events.clone(), save_dir(&dir));
            let _ = fs::create_dir_all(&dir);
            thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                let sink = |e| events.lock().unwrap().push(e);
                let _ = handle_session(stream, &dir, "test", &favorites, &sink);
            })
        };
        let result = send::send_files(addr, sender, &entries, |_| ControlFlow::Continue(()));
        server.join().unwrap();
        let _ = fs::remove_dir_all(&dir);
        let events = std::mem::take(&mut *events.lock().unwrap());
        (result, events)
    }

    #[test]
    fn favorite_can_send() {
        let me = Identity::new(DeviceId::random(), "Phone");
        let favorites = Favorites::in_memory();
        favorites.add(me.id, "Phone", DeviceKind::Unknown).unwrap();
        let (result, events) = loopback(&favorites, &me);
        assert_eq!(result.unwrap().sent, ["hello.txt"]);
        assert!(events.iter().any(|e| matches!(e, ReceiveEvent::FileSaved { .. })));
    }

    #[test]
    fn unwritable_folder_is_refused_before_any_data() {
        let me = Identity::new(DeviceId::random(), "Phone");
        let favorites = Favorites::in_memory();
        favorites.add(me.id, "Phone", DeviceKind::Unknown).unwrap();
        // A folder below a regular file can be neither found nor created, whoever runs the test.
        let (result, events) = loopback_to(&favorites, &me, |base| {
            let blocker = base.join("blocker");
            fs::write(&blocker, b"not a folder").unwrap();
            blocker.join("in")
        });
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(err.to_string().contains("cannot write"), "{err}");
        assert!(matches!(&events[..], [ReceiveEvent::NotWritable { .. }]), "{events:?}");
    }

    #[test]
    fn stranger_is_refused() {
        let me = Identity::new(DeviceId::random(), "Phone");
        let (result, events) = loopback(&Favorites::in_memory(), &me);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert!(matches!(&events[..], [ReceiveEvent::Refused { id, name, .. }] if *id == me.id && name == "Phone"));
    }

    /// Serves one session from a folder holding `a.txt` and `sub/b.txt`, and runs `ask` against it.
    fn browsing<T>(favorites: &Favorites, ask: impl FnOnce(SocketAddr, &Path) -> T) -> (T, Vec<ReceiveEvent>) {
        let base = std::env::temp_dir().join(format!("airlynx-browse-{}", DeviceId::random()));
        let root = base.join("shared");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("a.txt"), b"alpha").unwrap();
        fs::write(root.join("sub/b.txt"), b"beta beta").unwrap();
        fs::write(base.join("outside.txt"), b"secret").unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let server = {
            let (favorites, events, root) = (favorites.clone(), events.clone(), root.clone());
            thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                let sink = |e| events.lock().unwrap().push(e);
                let _ = handle_session(stream, &root, "test", &favorites, &sink);
            })
        };
        let result = ask(addr, &base);
        server.join().unwrap();
        let _ = fs::remove_dir_all(&base);
        let events = std::mem::take(&mut *events.lock().unwrap());
        (result, events)
    }

    fn favorite_me() -> (Identity, Favorites) {
        let me = Identity::new(DeviceId::random(), "Phone");
        let favorites = Favorites::in_memory();
        favorites.add(me.id, "Phone", DeviceKind::Unknown).unwrap();
        (me, favorites)
    }

    #[test]
    fn favorite_can_list_and_download() {
        let (me, favorites) = favorite_me();
        let (result, events) = browsing(&favorites, |addr, base| {
            let root = crate::browse::list(addr, &me, "").unwrap();
            (root, base.to_path_buf())
        });
        let names: Vec<_> = result.0.iter().map(|e| (e.name.as_str(), e.is_dir, e.size)).collect();
        assert_eq!(names, [("sub", true, 0), ("a.txt", false, 5)]);
        assert!(matches!(&events[..], [ReceiveEvent::Listed { path, .. }] if path.is_empty()), "{events:?}");

        let (result, events) = browsing(&favorites, |addr, base| {
            let dest = base.join("downloads");
            let saved = crate::browse::download(addr, &me, "sub/b.txt", &dest, |_, _| {}).unwrap();
            (fs::read(&saved).unwrap(), saved.file_name().unwrap().to_owned())
        });
        assert_eq!(result.0, b"beta beta");
        assert_eq!(result.1, "b.txt");
        assert!(matches!(&events[0], ReceiveEvent::Served { path, size: 9, .. } if path == "sub/b.txt"), "{events:?}");
    }

    #[test]
    fn browsing_cannot_leave_the_save_folder() {
        let (me, favorites) = favorite_me();
        for bad in ["..", "../outside.txt", "sub/../../outside.txt", "missing"] {
            let (err, _) = browsing(&favorites, |addr, _| crate::browse::list(addr, &me, bad).unwrap_err());
            assert_eq!(err.kind(), io::ErrorKind::NotFound, "list {bad}");
            // ".." is refused by the client before it connects, which would leave the server waiting.
            if bad != ".." {
                let (err, _) = browsing(&favorites, |addr, base| {
                    crate::browse::download(addr, &me, bad, &base.join("d"), |_, _| {}).unwrap_err()
                });
                assert_eq!(err.kind(), io::ErrorKind::NotFound, "get {bad}");
            }
        }
        // A folder is not a file, and a file is not a folder.
        let (err, _) = browsing(&favorites, |addr, base| {
            crate::browse::download(addr, &me, "sub", &base.join("d"), |_, _| {}).unwrap_err()
        });
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        let (err, _) = browsing(&favorites, |addr, _| crate::browse::list(addr, &me, "a.txt").unwrap_err());
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn stranger_cannot_browse() {
        let me = Identity::new(DeviceId::random(), "Phone");
        let (err, events) = browsing(&Favorites::in_memory(), |addr, _| crate::browse::list(addr, &me, "").unwrap_err());
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(matches!(&events[..], [ReceiveEvent::Refused { .. }]), "{events:?}");
    }

    #[test]
    fn browsing_works_when_the_folder_is_not_writable() {
        // Listing and downloading only read, so they do not need the write check that sending does.
        let (me, favorites) = favorite_me();
        let (result, _) = browsing(&favorites, |addr, base| {
            let root = base.join("shared");
            let mut perms = fs::metadata(&root).unwrap().permissions();
            perms.set_readonly(true);
            fs::set_permissions(&root, perms.clone()).unwrap();
            let listed = crate::browse::list(addr, &me, "").is_ok();
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            fs::set_permissions(&root, perms).unwrap();
            listed
        });
        assert!(result);
    }
}
