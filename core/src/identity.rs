//! Device identity and the persisted favorites list.
//!
//! Every install has a random `DeviceId`. A device only accepts files from
//! devices in its favorites, so two devices can exchange files in both
//! directions once each has added the other.

use std::collections::HashMap;
use std::collections::hash_map::RandomState;
use std::fmt;
use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::protocol::{MAX_DEVICE_NAME_LEN, VERSION, human_age};

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeviceId([u8; 16]);

impl DeviceId {
    pub fn random() -> Self {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let mut bytes = [0u8; 16];
        for (i, chunk) in bytes.chunks_mut(8).enumerate() {
            // RandomState is seeded from the OS, which avoids a dependency just for this.
            let mut hasher = RandomState::new().build_hasher();
            hasher.write_u128(nanos);
            hasher.write_usize(i);
            chunk.copy_from_slice(&hasher.finish().to_be_bytes());
        }
        Self(bytes)
    }

    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// First eight hex digits, enough to tell devices apart in a list.
    pub fn short(&self) -> String {
        self.to_string()[..8].to_string()
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

impl fmt::Debug for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DeviceId({self})")
    }
}

impl FromStr for DeviceId {
    type Err = io::Error;

    fn from_str(s: &str) -> io::Result<Self> {
        let bad = || io::Error::new(io::ErrorKind::InvalidData, format!("invalid device id {s:?}"));
        if s.len() != 32 || !s.is_ascii() {
            return Err(bad());
        }
        let mut bytes = [0u8; 16];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|_| bad())?;
        }
        Ok(Self(bytes))
    }
}

/// What sort of device this is, shown as an icon next to its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeviceKind {
    #[default]
    Unknown,
    Desktop,
    Mobile,
    /// A computer running Macsplorer, which speaks AirLynx too. Older builds read its code as `Unknown`.
    Macsplorer,
    /// An Android TV. Older builds read its code as `Unknown`.
    Tv,
}

impl DeviceKind {
    /// One letter on the wire (discovery replies).
    pub fn code(self) -> char {
        match self {
            DeviceKind::Unknown => '?',
            DeviceKind::Desktop => 'd',
            DeviceKind::Mobile => 'm',
            DeviceKind::Macsplorer => 'x',
            DeviceKind::Tv => 't',
        }
    }

    pub fn from_code(code: &str) -> Self {
        match code {
            "d" => DeviceKind::Desktop,
            "m" => DeviceKind::Mobile,
            "x" => DeviceKind::Macsplorer,
            "t" => DeviceKind::Tv,
            _ => DeviceKind::Unknown,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            DeviceKind::Unknown => "unknown",
            DeviceKind::Desktop => "desktop",
            DeviceKind::Mobile => "mobile",
            DeviceKind::Macsplorer => "macsplorer",
            DeviceKind::Tv => "tv",
        }
    }

    fn from_word(word: &str) -> Option<Self> {
        [DeviceKind::Unknown, DeviceKind::Desktop, DeviceKind::Mobile, DeviceKind::Macsplorer, DeviceKind::Tv].into_iter().find(|k| k.word() == word)
    }
}

/// Who we are on the network: the id plus the display name.
#[derive(Debug, Clone)]
pub struct Identity {
    pub id: DeviceId,
    pub name: String,
    pub kind: DeviceKind,
}

impl Identity {
    pub fn new(id: DeviceId, name: &str) -> Self {
        Self { id, name: clean_name(name), kind: DeviceKind::Unknown }
    }

    pub fn with_kind(mut self, kind: DeviceKind) -> Self {
        self.kind = kind;
        self
    }
}

/// Makes a peer-supplied name safe to show and store: no control characters,
/// trimmed, at most `MAX_DEVICE_NAME_LEN` bytes, never empty.
pub fn clean_name(raw: &str) -> String {
    let mut name = String::new();
    for c in raw.chars().filter(|c| !c.is_control()) {
        if name.len() + c.len_utf8() > MAX_DEVICE_NAME_LEN {
            break;
        }
        name.push(c);
    }
    let name = name.trim();
    if name.is_empty() { "unknown".into() } else { name.into() }
}

#[derive(Debug, Clone)]
pub struct Favorite {
    pub id: DeviceId,
    pub name: String,
    pub kind: DeviceKind,
}

impl Favorite {
    /// Label and value pairs for a details view, as of `now` (Unix seconds). `sighting` is what
    /// `Favorites::sighting` knows. Whether the device is online right now is for the caller to add.
    pub fn details(&self, sighting: Option<&Sighting>, now: u64) -> Vec<(&'static str, String)> {
        let age = |t: Option<u64>| t.map_or("unknown".into(), |t| human_age(now.saturating_sub(t)));
        let mut rows = vec![
            ("Name", self.name.clone()),
            ("Short id", self.id.short()),
            ("Device id", self.id.to_string()),
            ("Kind", self.kind.word().into()),
        ];
        let Some(s) = sighting else {
            rows.push(("Address", "never seen".into()));
            return rows;
        };
        rows.push(("Address", s.ip.to_string()));
        rows.push(("Port", s.port.map_or("unknown".into(), |p| p.to_string())));
        rows.push((
            "Route",
            match (&s.route, s.mutual) {
                (Some(r), _) => format!("{} (from {})", r.interface, r.local_ip),
                (None, Some(_)) => "not on a local network, through the gateway".into(),
                (None, None) => "unknown".into(),
            },
        ));
        rows.push(("Protocol", format!("AirLynx v{VERSION}, encrypted TCP")));
        rows.push((
            "Faved you",
            match s.mutual {
                Some(true) => "yes",
                Some(false) => "no",
                None => "unknown",
            }
            .into(),
        ));
        rows.push(("First seen", age(s.first_seen)));
        rows.push(("Last seen", age(s.last_seen)));
        rows
    }
}

/// How this device reaches another one: the local interface and the address on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub interface: String,
    pub local_ip: Ipv4Addr,
}

/// What is known about where a favorite lives on the network, for showing in its details.
/// Times are Unix seconds. They are `None` for a favorite seen before times were recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sighting {
    pub ip: IpAddr,
    /// The TCP port it listens on; unknown if it has only ever connected to us.
    pub port: Option<u16>,
    pub first_seen: Option<u64>,
    pub last_seen: Option<u64>,
    /// Whether it had us as a favorite when it last answered discovery.
    pub mutual: Option<bool>,
    pub route: Option<Route>,
}

/// The persisted part of a `Sighting` (the address lives in the address cache).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    first: u64,
    last: u64,
    port: Option<u16>,
    mutual: Option<bool>,
    route: Option<Route>,
}

/// Disk writes of a sighting that only moved `last` forward are held back this long.
const SEEN_SAVE_INTERVAL_SECS: u64 = 60;

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The set of trusted devices. Cloning shares the same list, so the receiver
/// sees changes the UI makes immediately. Changes are written to disk.
///
/// Next to the list sits an address cache, the last IP each favorite was seen at. Discovery asks
/// those addresses directly, so a favorite is still found when broadcast and mDNS fail. It is kept
/// in its own file (`addresses`), because it changes on every network move and the list should not.
/// A third file (`seen`) keeps the rest of what is shown in a favorite's details: first and last
/// seen, port, route and whether it has us as a favorite.
#[derive(Debug, Clone)]
pub struct Favorites {
    inner: Arc<Mutex<FavoritesInner>>,
}

#[derive(Debug)]
struct FavoritesInner {
    path: Option<PathBuf>,
    list: Vec<Favorite>,
    /// Modification time of the file as last read or written, to notice edits by other processes.
    mtime: Option<SystemTime>,
    /// Last known address per favorite. Loaded once; not shared between processes.
    addresses: HashMap<DeviceId, IpAddr>,
    seen: HashMap<DeviceId, Seen>,
    /// When `seen` was last written, so a scan every few seconds does not write every few seconds.
    seen_saved: u64,
}

impl Favorites {
    /// Loads the list from `path`; a missing file is an empty list. The list reloads
    /// itself when another process (the CLI, say) changes the file.
    pub fn load(path: PathBuf) -> io::Result<Self> {
        let mut inner = FavoritesInner::new(Some(path));
        inner.reload()?;
        inner.addresses = inner.load_addresses();
        inner.seen = inner.load_seen();
        Ok(Self { inner: Arc::new(Mutex::new(inner)) })
    }

    /// A list that is never saved.
    pub fn in_memory() -> Self {
        Self { inner: Arc::new(Mutex::new(FavoritesInner::new(None))) }
    }

    pub fn contains(&self, id: DeviceId) -> bool {
        let mut inner = self.inner.lock().unwrap();
        inner.refresh();
        inner.list.iter().any(|f| f.id == id)
    }

    pub fn list(&self) -> Vec<Favorite> {
        let mut inner = self.inner.lock().unwrap();
        inner.refresh();
        inner.list.clone()
    }

    /// Adds a device, or updates its stored name and kind if it is already a favorite.
    /// An `Unknown` kind never replaces a known one. The change stays in memory even if saving fails.
    pub fn add(&self, id: DeviceId, name: &str, kind: DeviceKind) -> io::Result<()> {
        let name = clean_name(name);
        let mut inner = self.inner.lock().unwrap();
        inner.refresh();
        match inner.list.iter_mut().find(|f| f.id == id) {
            Some(existing) => {
                let kind = if kind == DeviceKind::Unknown { existing.kind } else { kind };
                if existing.name == name && existing.kind == kind {
                    return Ok(());
                }
                existing.name = name;
                existing.kind = kind;
            }
            None => inner.list.push(Favorite { id, name, kind }),
        }
        inner.save()
    }

    pub fn remove(&self, id: DeviceId) -> io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.refresh();
        inner.list.retain(|f| f.id != id);
        if inner.addresses.remove(&id).is_some() {
            inner.save_addresses();
        }
        if inner.seen.remove(&id).is_some() {
            inner.save_seen();
        }
        inner.save()
    }

    /// Where each favorite was last seen, for asking it directly.
    pub fn known_addresses(&self) -> Vec<IpAddr> {
        let mut inner = self.inner.lock().unwrap();
        inner.refresh();
        let mut ips: Vec<IpAddr> = inner.list.iter().filter_map(|f| inner.addresses.get(&f.id).copied()).collect();
        ips.sort();
        ips.dedup();
        ips
    }

    /// Notes that favorite `id` was seen at `ip` just now (it answered discovery or connected to us).
    /// Ignores devices that are not favorites. A failed save only loses the cache.
    pub fn remember_address(&self, id: DeviceId, ip: IpAddr) {
        let mut inner = self.inner.lock().unwrap();
        if !inner.list.iter().any(|f| f.id == id) {
            return;
        }
        if inner.addresses.insert(id, ip) != Some(ip) {
            inner.save_addresses();
        }
        inner.touch(id, |_| {});
    }

    /// Like `remember_address`, for a favorite that answered discovery: also notes the port it listens on,
    /// whether it has us as a favorite, and the route we reached it by.
    pub fn remember_peer(&self, id: DeviceId, ip: IpAddr, port: u16, mutual: bool, route: Option<Route>) {
        self.remember_address(id, ip);
        let mut inner = self.inner.lock().unwrap();
        if inner.list.iter().any(|f| f.id == id) {
            inner.touch(id, |seen| {
                seen.port = Some(port);
                seen.mutual = Some(mutual);
                seen.route = route;
            });
        }
    }

    /// What is known about where favorite `id` lives, or `None` if it has never been seen.
    pub fn sighting(&self, id: DeviceId) -> Option<Sighting> {
        let mut inner = self.inner.lock().unwrap();
        inner.refresh();
        let ip = *inner.addresses.get(&id)?;
        let seen = inner.seen.get(&id);
        Some(Sighting {
            ip,
            port: seen.and_then(|s| s.port),
            first_seen: seen.map(|s| s.first),
            last_seen: seen.map(|s| s.last),
            mutual: seen.and_then(|s| s.mutual),
            route: seen.and_then(|s| s.route.clone()),
        })
    }
}

impl FavoritesInner {
    fn new(path: Option<PathBuf>) -> Self {
        Self { path, list: Vec::new(), mtime: None, addresses: HashMap::new(), seen: HashMap::new(), seen_saved: 0 }
    }

    /// Stamps the sighting of `id` with the current time, applies `change` to it, and saves it if
    /// something besides the time changed or the last save is old.
    fn touch(&mut self, id: DeviceId, change: impl FnOnce(&mut Seen)) {
        let now = now_secs();
        let seen = self.seen.entry(id).or_insert(Seen { first: now, last: now, port: None, mutual: None, route: None });
        let before = seen.clone();
        seen.last = now;
        change(seen);
        let only_time = Seen { last: before.last, ..seen.clone() } == before;
        if !only_time || now.saturating_sub(self.seen_saved) >= SEEN_SAVE_INTERVAL_SECS {
            self.save_seen();
        }
    }

    fn file_mtime(&self) -> Option<SystemTime> {
        fs::metadata(self.path.as_ref()?).and_then(|m| m.modified()).ok()
    }

    fn reload(&mut self) -> io::Result<()> {
        let Some(path) = &self.path else { return Ok(()) };
        self.list = match fs::read_to_string(path) {
            Ok(text) => text
                .lines()
                .filter_map(|line| {
                    let (id, rest) = line.split_once(' ')?;
                    // Lines are `<id> kind:<kind> <name>`; older files have `<id> <name>`.
                    let (kind, name) = match rest.split_once(' ') {
                        Some((word, name)) => match word.strip_prefix("kind:").and_then(DeviceKind::from_word) {
                            Some(kind) => (kind, name),
                            None => (DeviceKind::Unknown, rest),
                        },
                        None => (DeviceKind::Unknown, rest),
                    };
                    Some(Favorite { id: id.parse().ok()?, name: clean_name(name), kind })
                })
                .collect(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        self.mtime = self.file_mtime();
        Ok(())
    }

    /// Picks up changes made to the file by someone else. Keeps the old list if it cannot be read.
    fn refresh(&mut self) {
        if self.path.is_some() && self.file_mtime() != self.mtime {
            let _ = self.reload();
        }
    }

    fn addresses_path(&self) -> Option<PathBuf> {
        Some(self.path.as_ref()?.with_file_name("addresses"))
    }

    /// Lines of `<id> <ip>`; anything unreadable is skipped.
    fn load_addresses(&self) -> HashMap<DeviceId, IpAddr> {
        let Some(text) = self.addresses_path().and_then(|p| fs::read_to_string(p).ok()) else { return HashMap::new() };
        text.lines()
            .filter_map(|line| {
                let (id, ip) = line.split_once(' ')?;
                Some((id.parse().ok()?, ip.trim().parse().ok()?))
            })
            .collect()
    }

    fn save_addresses(&self) {
        let Some(path) = self.addresses_path() else { return };
        let text: String = self.addresses.iter().map(|(id, ip)| format!("{id} {ip}\n")).collect();
        let _ = write_atomic(&path, &text);
    }

    fn seen_path(&self) -> Option<PathBuf> {
        Some(self.path.as_ref()?.with_file_name("seen"))
    }

    /// Lines of `<id> <first> <last> <port|-> <mutual 0|1|-> <local ip|-> <interface|->`. The interface
    /// comes last because its name can hold spaces. Anything unreadable is skipped.
    fn load_seen(&self) -> HashMap<DeviceId, Seen> {
        let Some(text) = self.seen_path().and_then(|p| fs::read_to_string(p).ok()) else { return HashMap::new() };
        text.lines()
            .filter_map(|line| {
                let mut f = line.splitn(7, ' ');
                let id: DeviceId = f.next()?.parse().ok()?;
                let (first, last) = (f.next()?.parse().ok()?, f.next()?.parse().ok()?);
                let port = f.next()?.parse().ok();
                let mutual = match f.next()? {
                    "1" => Some(true),
                    "0" => Some(false),
                    _ => None,
                };
                let local_ip = f.next()?.parse().ok();
                let interface = f.next().filter(|name| *name != "-");
                let route = local_ip.zip(interface).map(|(local_ip, name)| Route { interface: name.into(), local_ip });
                Some((id, Seen { first, last, port, mutual, route }))
            })
            .collect()
    }

    fn save_seen(&mut self) {
        let Some(path) = self.seen_path() else { return };
        let text: String = self
            .seen
            .iter()
            .map(|(id, s)| {
                let port = s.port.map_or("-".into(), |p| p.to_string());
                let mutual = s.mutual.map_or('-', |m| if m { '1' } else { '0' });
                let (ip, name) = s.route.as_ref().map_or(("-".into(), "-"), |r| (r.local_ip.to_string(), r.interface.as_str()));
                format!("{id} {} {} {port} {mutual} {ip} {name}\n", s.first, s.last)
            })
            .collect();
        if write_atomic(&path, &text).is_ok() {
            self.seen_saved = now_secs();
        }
    }

    fn save(&mut self) -> io::Result<()> {
        let Some(path) = &self.path else { return Ok(()) };
        let text: String = self.list.iter().map(|f| format!("{} kind:{} {}\n", f.id, f.kind.word(), f.name)).collect();
        write_atomic(path, &text)?;
        self.mtime = self.file_mtime();
        Ok(())
    }
}

/// This device's id and favorites, stored in one directory.
pub struct Profile {
    pub id: DeviceId,
    /// The name the user chose for this device, if any. Without one the platform's own name is used.
    pub name: Option<String>,
    pub favorites: Favorites,
}

impl Profile {
    /// Loads the profile from `dir`, creating the directory and a new device id on first use.
    pub fn load(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let id_path = dir.join("device-id");
        let id = match fs::read_to_string(&id_path).ok().and_then(|s| s.trim().parse().ok()) {
            Some(id) => id,
            None => {
                let id = DeviceId::random();
                write_atomic(&id_path, &format!("{id}\n"))?;
                id
            }
        };
        let name = fs::read_to_string(dir.join(NAME_FILE)).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        Ok(Self { id, name: name.map(|n| clean_name(&n)), favorites: Favorites::load(dir.join("favorites"))? })
    }

    /// Stores the chosen device name in `dir` and returns it as it will be announced (cleaned up). An empty name
    /// removes the choice, so the platform's own name applies again and `None` is returned.
    pub fn save_name(dir: &Path, name: &str) -> io::Result<Option<String>> {
        let path = dir.join(NAME_FILE);
        if name.trim().is_empty() {
            return match fs::remove_file(&path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(None),
            };
        }
        let name = clean_name(name);
        write_atomic(&path, &format!("{name}\n"))?;
        Ok(Some(name))
    }
}

const NAME_FILE: &str = "device-name";

fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_hex_round_trip() {
        let id = DeviceId::random();
        assert_eq!(id.to_string().len(), 32);
        assert_eq!(id.to_string().parse::<DeviceId>().unwrap(), id);
        assert!("xyz".parse::<DeviceId>().is_err());
        assert!("g".repeat(32).parse::<DeviceId>().is_err());
        assert_ne!(DeviceId::random(), DeviceId::random());
    }

    #[test]
    fn chosen_name_is_saved_and_cleared() {
        let dir = std::env::temp_dir().join(format!("airlynx-name-{}", DeviceId::random()));
        assert_eq!(Profile::load(&dir).unwrap().name, None);
        assert_eq!(Profile::save_name(&dir, "  Living room\n Mac ").unwrap().as_deref(), Some("Living room Mac"));
        assert_eq!(Profile::load(&dir).unwrap().name.as_deref(), Some("Living room Mac"));
        assert_eq!(Profile::save_name(&dir, "  ").unwrap(), None);
        assert_eq!(Profile::load(&dir).unwrap().name, None);
        // Clearing twice is fine.
        assert_eq!(Profile::save_name(&dir, "").unwrap(), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_name("  Pixel\n3a "), "Pixel3a");
        assert_eq!(clean_name(""), "unknown");
        assert_eq!(clean_name(&"a".repeat(200)).len(), MAX_DEVICE_NAME_LEN);
        assert!(clean_name(&"\u{e9}".repeat(100)).len() <= MAX_DEVICE_NAME_LEN);
    }

    #[test]
    fn edits_from_another_handle_are_seen() {
        let dir = std::env::temp_dir().join(format!("airlynx-test-{}", DeviceId::random()));
        let a = Profile::load(&dir).unwrap().favorites;
        let b = Profile::load(&dir).unwrap().favorites;
        let friend = DeviceId::random();
        assert!(!a.contains(friend));
        b.add(friend, "Phone", DeviceKind::Mobile).unwrap();
        // Same-second writes can share a modification time on some filesystems.
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(a.contains(friend));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn profile_persists() {
        let dir = std::env::temp_dir().join(format!("airlynx-test-{}", DeviceId::random()));
        let first = Profile::load(&dir).unwrap();
        let friend = DeviceId::random();
        first.favorites.add(friend, "Mac mini", DeviceKind::Desktop).unwrap();
        first.favorites.add(friend, "Mac mini 2", DeviceKind::Unknown).unwrap();

        let second = Profile::load(&dir).unwrap();
        assert_eq!(second.id, first.id);
        let list = second.favorites.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "Mac mini 2");
        // An unknown kind does not erase a known one.
        assert_eq!(list[0].kind, DeviceKind::Desktop);

        second.favorites.remove(friend).unwrap();
        assert!(!Profile::load(&dir).unwrap().favorites.contains(friend));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn addresses_are_cached_for_favorites_only() {
        let dir = std::env::temp_dir().join(format!("airlynx-test-{}", DeviceId::random()));
        let favorites = Profile::load(&dir).unwrap().favorites;
        let (friend, stranger) = (DeviceId::random(), DeviceId::random());
        let (a, b): (IpAddr, IpAddr) = ("10.11.46.85".parse().unwrap(), "10.11.19.114".parse().unwrap());
        favorites.add(friend, "Pixel", DeviceKind::Mobile).unwrap();
        favorites.remember_address(friend, a);
        favorites.remember_address(stranger, b);
        assert_eq!(favorites.known_addresses(), vec![a]);

        // It survives a restart, follows a device that moved, and goes away with the favorite.
        let reloaded = Profile::load(&dir).unwrap().favorites;
        assert_eq!(reloaded.known_addresses(), vec![a]);
        reloaded.remember_address(friend, b);
        assert_eq!(Profile::load(&dir).unwrap().favorites.known_addresses(), vec![b]);
        reloaded.remove(friend).unwrap();
        assert!(Profile::load(&dir).unwrap().favorites.known_addresses().is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn sightings_keep_first_seen_and_survive_a_restart() {
        let dir = std::env::temp_dir().join(format!("airlynx-test-{}", DeviceId::random()));
        let favorites = Profile::load(&dir).unwrap().favorites;
        let (friend, stranger) = (DeviceId::random(), DeviceId::random());
        let ip: IpAddr = "10.11.46.85".parse().unwrap();
        let route = Route { interface: "Wi-Fi 2".into(), local_ip: "10.11.19.114".parse().unwrap() };
        favorites.add(friend, "Pixel", DeviceKind::Mobile).unwrap();
        assert_eq!(favorites.sighting(friend), None);

        favorites.remember_peer(friend, ip, 47800, true, Some(route.clone()));
        favorites.remember_peer(stranger, ip, 47800, true, None);
        assert_eq!(favorites.sighting(stranger), None);
        let first = favorites.sighting(friend).unwrap();
        assert_eq!((first.ip, first.port, first.mutual, first.route.as_ref()), (ip, Some(47800), Some(true), Some(&route)));
        assert!(first.first_seen.is_some() && first.first_seen == first.last_seen);

        let reloaded = Profile::load(&dir).unwrap().favorites;
        assert_eq!(reloaded.sighting(friend), Some(first));
        // Connecting to us counts as a sighting but keeps what discovery learned.
        reloaded.remember_address(friend, ip);
        assert_eq!(reloaded.sighting(friend).unwrap().port, Some(47800));
        reloaded.remove(friend).unwrap();
        assert_eq!(Profile::load(&dir).unwrap().favorites.sighting(friend), None);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn details_say_what_is_unknown() {
        let favorite = Favorite { id: DeviceId::random(), name: "Pixel".into(), kind: DeviceKind::Mobile };
        let value = |rows: &[(&str, String)], label: &str| rows.iter().find(|r| r.0 == label).map(|r| r.1.clone());
        let rows = favorite.details(None, 1_000_000);
        assert_eq!(value(&rows, "Address").as_deref(), Some("never seen"));
        assert_eq!(value(&rows, "Short id"), Some(favorite.id.short()));

        let sighting = Sighting {
            ip: "10.0.0.7".parse().unwrap(),
            port: None,
            first_seen: Some(1_000_000 - 3 * 86400),
            last_seen: Some(1_000_000 - 120),
            mutual: None,
            route: None,
        };
        let rows = favorite.details(Some(&sighting), 1_000_000);
        assert_eq!(value(&rows, "Port").as_deref(), Some("unknown"));
        assert_eq!(value(&rows, "Faved you").as_deref(), Some("unknown"));
        assert_eq!(value(&rows, "First seen").as_deref(), Some("3 days ago"));
        assert_eq!(value(&rows, "Last seen").as_deref(), Some("2 min ago"));
    }

    #[test]
    fn a_favorite_with_only_an_address_has_no_times() {
        let dir = std::env::temp_dir().join(format!("airlynx-test-{}", DeviceId::random()));
        fs::create_dir_all(&dir).unwrap();
        let id = DeviceId::random();
        fs::write(dir.join("favorites"), format!("{id} kind:mobile Pixel\n")).unwrap();
        fs::write(dir.join("addresses"), format!("{id} 10.0.0.7\n")).unwrap();
        let sighting = Profile::load(&dir).unwrap().favorites.sighting(id).unwrap();
        assert_eq!((sighting.first_seen, sighting.last_seen, sighting.port, sighting.route), (None, None, None, None));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn old_favorites_files_still_load() {
        let dir = std::env::temp_dir().join(format!("airlynx-test-{}", DeviceId::random()));
        fs::create_dir_all(&dir).unwrap();
        let (a, b) = (DeviceId::random(), DeviceId::random());
        fs::write(dir.join("favorites"), format!("{a} Old Mac\n{b} kind:mobile Pixel 3a\n")).unwrap();
        let list = Profile::load(&dir).unwrap().favorites.list();
        assert_eq!((list[0].name.as_str(), list[0].kind), ("Old Mac", DeviceKind::Unknown));
        assert_eq!((list[1].name.as_str(), list[1].kind), ("Pixel 3a", DeviceKind::Mobile));
        let _ = fs::remove_dir_all(dir);
    }
}
