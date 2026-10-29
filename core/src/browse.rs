//! Browsing another device's save folder, read only.
//!
//! The device that owns the folder answers `COMMAND_LIST` and `COMMAND_GET` (see `receive`), and a favorite
//! that the owner has added can ask. Paths are "/" separated and relative to the save folder. Nothing outside it
//! can be named: `..`, hidden names (which covers the `.part` files of transfers in progress) and symlinks that
//! lead out of the folder are all answered with `STATUS_NOT_FOUND`.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::UNIX_EPOCH;

use sha2::{Digest, Sha256};

use crate::identity::Identity;
use crate::protocol::*;
use crate::receive::{TMP_COUNTER, receive_file, sanitize_name, unique_path};
use crate::send::open_session;

/// Longest path a request may carry.
pub const MAX_PATH_LEN: usize = 4096;
/// Most entries one listing carries. A bigger folder is cut off (the client shows what arrived).
pub const MAX_LIST_ENTRIES: usize = 20_000;

/// One item in a remote folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    /// Unix seconds, 0 when unknown.
    pub modified: u64,
}

/// Lists the folder `path` ("" for the save folder itself) on the device at `addr`. Folders come first, then files,
/// each sorted by name ignoring case. Fails with `ErrorKind::NotFound` if the path is not a folder there.
pub fn list(addr: SocketAddr, me: &Identity, path: &str) -> io::Result<Vec<DirEntry>> {
    let mut stream = open_session(addr, me, COMMAND_LIST)?;
    write_path(&mut stream, path)?;
    read_status(&mut stream)?;
    let count = read_u32(&mut stream)? as usize;
    if count > MAX_LIST_ENTRIES {
        return Err(invalid("folder listing too long"));
    }
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let kind = read_u8(&mut stream)?;
        let name_len = read_u16(&mut stream)? as usize;
        if name_len == 0 || name_len > MAX_NAME_LEN {
            return Err(invalid("file name length out of range"));
        }
        let mut name = vec![0u8; name_len];
        stream.read_exact(&mut name)?;
        let name = String::from_utf8(name).map_err(|_| invalid("file name is not UTF-8"))?;
        let size = read_u64(&mut stream)?;
        let modified = read_u64(&mut stream)?;
        entries.push(DirEntry { name, is_dir: kind == ENTRY_DIR, size, modified });
    }
    Ok(entries)
}

/// Downloads the file `path` from the device at `addr` into `dest_dir`, as `name (1).ext` if the name is taken, and
/// returns where it was saved. The file is checked against the owner's SHA-256 before it gets its final name.
/// `on_progress` gets the bytes received so far and the file size, about every half megabyte.
pub fn download(
    addr: SocketAddr,
    me: &Identity,
    path: &str,
    dest_dir: &Path,
    mut on_progress: impl FnMut(u64, u64),
) -> io::Result<PathBuf> {
    let name = sanitize_name(path)?;
    fs::create_dir_all(dest_dir)?;
    let mut stream = open_session(addr, me, COMMAND_GET)?;
    write_path(&mut stream, path)?;
    read_status(&mut stream)?;
    let size = read_u64(&mut stream)?;

    let tmp_path = dest_dir.join(format!(
        ".{name}.{}.{}.part",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let (actual, expected) = match receive_file(&mut stream, &tmp_path, size, |received| on_progress(received, size)) {
        Ok(hashes) => hashes,
        Err(e) => {
            let _ = fs::remove_file(&tmp_path);
            return Err(e);
        }
    };
    if actual != expected {
        let _ = fs::remove_file(&tmp_path);
        return Err(invalid(format!("{name} failed verification, the checksum does not match")));
    }
    let final_path = unique_path(dest_dir, &name);
    fs::rename(&tmp_path, &final_path)?;
    Ok(final_path)
}

fn write_path(w: &mut impl Write, path: &str) -> io::Result<()> {
    if path.len() > MAX_PATH_LEN {
        return Err(invalid("path too long"));
    }
    w.write_all(&(path.len() as u16).to_be_bytes())?;
    w.write_all(path.as_bytes())
}

fn read_path(r: &mut impl Read) -> io::Result<String> {
    let len = read_u16(r)? as usize;
    if len > MAX_PATH_LEN {
        return Err(invalid("path too long"));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    String::from_utf8(buf).map_err(|_| invalid("path is not UTF-8"))
}

/// Turns the owner's status byte into an error unless it is OK.
fn read_status(r: &mut impl Read) -> io::Result<()> {
    match read_u8(r)? {
        STATUS_OK => Ok(()),
        STATUS_NOT_FOUND => Err(io::Error::new(io::ErrorKind::NotFound, "not found on the other device")),
        STATUS_READ_ERROR => Err(io::Error::other("the other device could not read it")),
        other => Err(invalid(format!("unexpected status {other}"))),
    }
}

// ---- the side that owns the folder --------------------------------------------------------------

/// Resolves `rel` against `root`. Fails with `NotFound` for anything that is not plainly inside it.
fn resolve(root: &Path, rel: &str) -> io::Result<PathBuf> {
    let not_found = || io::Error::new(io::ErrorKind::NotFound, format!("no such path {rel:?}"));
    let mut path = root.to_path_buf();
    for part in rel.split('/').filter(|p| !p.is_empty()) {
        if part.starts_with('.') || part.contains(['\\', '\0']) || part.chars().any(char::is_control) {
            return Err(not_found());
        }
        path.push(part);
    }
    // Symlinks inside the folder may point anywhere, so compare the real locations.
    let real = path.canonicalize().map_err(|_| not_found())?;
    let real_root = root.canonicalize().map_err(|_| not_found())?;
    if real.starts_with(&real_root) { Ok(real) } else { Err(not_found()) }
}

fn status_for(e: &io::Error) -> u8 {
    if e.kind() == io::ErrorKind::NotFound { STATUS_NOT_FOUND } else { STATUS_READ_ERROR }
}

/// Answers a `COMMAND_LIST` request. Returns the path that was listed.
pub(crate) fn serve_list(stream: &mut (impl Read + Write), root: &Path) -> io::Result<String> {
    let rel = read_path(stream)?;
    let entries = resolve(root, &rel).and_then(|dir| read_entries(&dir));
    match entries {
        Ok(entries) => {
            stream.write_all(&[STATUS_OK])?;
            stream.write_all(&(entries.len() as u32).to_be_bytes())?;
            for e in &entries {
                stream.write_all(&[if e.is_dir { ENTRY_DIR } else { ENTRY_FILE }])?;
                stream.write_all(&(e.name.len() as u16).to_be_bytes())?;
                stream.write_all(e.name.as_bytes())?;
                stream.write_all(&e.size.to_be_bytes())?;
                stream.write_all(&e.modified.to_be_bytes())?;
            }
            stream.flush()?;
            Ok(rel)
        }
        Err(e) => {
            stream.write_all(&[status_for(&e)])?;
            stream.flush()?;
            Err(e)
        }
    }
}

fn read_entries(dir: &Path) -> io::Result<Vec<DirEntry>> {
    if !dir.is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "not a folder"));
    }
    let mut entries = Vec::new();
    for item in fs::read_dir(dir)? {
        let Ok(item) = item else { continue };
        let Ok(name) = item.file_name().into_string() else { continue };
        let Ok(kind) = item.file_type() else { continue };
        if name.starts_with('.') || name.len() > MAX_NAME_LEN || kind.is_symlink() || !(kind.is_dir() || kind.is_file()) {
            continue;
        }
        let Ok(meta) = item.metadata() else { continue };
        let modified = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
        entries.push(DirEntry { name, is_dir: kind.is_dir(), size: if kind.is_dir() { 0 } else { meta.len() }, modified });
        if entries.len() == MAX_LIST_ENTRIES {
            break;
        }
    }
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(entries)
}

/// Answers a `COMMAND_GET` request. Returns the requested path and the file size.
pub(crate) fn serve_get(stream: &mut (impl Read + Write), root: &Path) -> io::Result<(String, u64)> {
    let rel = read_path(stream)?;
    let opened = resolve(root, &rel).and_then(|path| {
        let file = File::open(path)?;
        let meta = file.metadata()?;
        if meta.is_file() { Ok((file, meta.len())) } else { Err(io::Error::new(io::ErrorKind::NotFound, "not a file")) }
    });
    let (mut file, size) = match opened {
        Ok(opened) => opened,
        Err(e) => {
            stream.write_all(&[status_for(&e)])?;
            stream.flush()?;
            return Err(e);
        }
    };
    stream.write_all(&[STATUS_OK])?;
    stream.write_all(&size.to_be_bytes())?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK_SIZE];
    let mut remaining = size;
    while remaining > 0 {
        let want = remaining.min(buf.len() as u64) as usize;
        let n = file.read(&mut buf[..want])?;
        if n == 0 {
            return Err(invalid(format!("{rel} shrank while sending")));
        }
        hasher.update(&buf[..n]);
        stream.write_all(&buf[..n])?;
        remaining -= n as u64;
    }
    let hash: [u8; 32] = hasher.finalize().into();
    stream.write_all(&hash)?;
    stream.flush()?;
    Ok((rel, size))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("airlynx-browse-{}", crate::identity::DeviceId::random()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn resolve_stays_inside_the_folder() {
        let root = scratch();
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join("a/b/f.txt"), b"x").unwrap();
        fs::write(root.join(".hidden"), b"x").unwrap();
        let outside = scratch();
        fs::write(outside.join("secret"), b"x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();

        assert_eq!(resolve(&root, "").unwrap(), root.canonicalize().unwrap());
        assert!(resolve(&root, "/a//b/").unwrap().ends_with("a/b"));
        assert!(resolve(&root, "a/b/f.txt").is_ok());
        for bad in ["..", "a/../..", "../x", ".hidden", "a/.", "a\\b", "missing", "a/b/f.txt/x"] {
            assert_eq!(resolve(&root, bad).unwrap_err().kind(), io::ErrorKind::NotFound, "{bad}");
        }
        #[cfg(unix)]
        assert!(resolve(&root, "link/secret").is_err());
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn listing_hides_dotfiles_and_symlinks_and_sorts() {
        let root = scratch();
        fs::create_dir(root.join("Zeta")).unwrap();
        fs::write(root.join("b.txt"), b"bb").unwrap();
        fs::write(root.join("A.txt"), b"a").unwrap();
        fs::write(root.join(".x.1.2.part"), b"half").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/", root.join("rootlink")).unwrap();
        let names: Vec<_> = read_entries(&root).unwrap().into_iter().map(|e| (e.name, e.is_dir, e.size)).collect();
        assert_eq!(names, [("Zeta".into(), true, 0), ("A.txt".into(), false, 1), ("b.txt".into(), false, 2)]);
        let _ = fs::remove_dir_all(&root);
    }
}
