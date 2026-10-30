//! Wire protocol shared by sender and receiver.
//!
//! Session (sender -> receiver, all integers big-endian):
//!   "ALNX" | version: u8 | sender public key: [u8; 32]
//!                                             -> receiver replies status u8 (rejected on a version
//!                                               mismatch, which ends the session), then, if ok,
//!                                               its public key [u8; 32]
//!   Everything below is encrypted, see `secure`.
//!   sender id: [u8; 16] | name len: u8 | name: UTF-8 | command: u8
//!                                             -> receiver replies one status byte (ok, not a
//!                                               favorite, unknown command, or no write access
//!                                               for a send; all but ok end the session)
//!   command SEND:
//!     file count: u32
//!     per file:
//!       name len: u16 | name: UTF-8 | size: u64 | data | sha256: [u8; 32]
//!                                             -> receiver replies one status byte
//!   command LIST (browse the receiver's save folder, read only):
//!     path len: u16 | path: UTF-8, "/" separated, relative to the save folder, empty for the folder itself
//!                                             -> status byte, then if ok:
//!                                               entry count: u32, per entry:
//!                                               kind: u8 | name len: u16 | name | size: u64 | modified: u64
//!   command GET (download one file from the save folder):
//!     path len: u16 | path: UTF-8
//!                                             -> status byte, then if ok: size: u64 | data | sha256: [u8; 32]
//!   Each session carries exactly one command.
//!
//! Reverse connections (see `reverse`), for a device that can reach us but that we cannot reach, for example
//! because it sits behind NAT. The device dials us with the same handshake, hello and a command of its own:
//!   command STANDBY: no payload. Status byte, then the connection stays open and carries control bytes:
//!     we send PING (0) and it answers PONG (0), or we send WANT (1) | token: [u8; 16]
//!   command ATTACH: token: [u8; 16]. Status byte (rejected if no session is waiting for that token).
//!     After an OK the roles swap: we introduce ourselves with a hello and a command as above, and it answers as
//!     the receiver, over this same connection.

use std::io::{self, Read, Write};

use crate::identity::{DeviceId, Identity, clean_name};

pub const MAGIC: &[u8; 4] = b"ALNX";
pub const VERSION: u8 = 6;

pub const DEFAULT_TCP_PORT: u16 = 47800;
pub const DISCOVERY_PORT: u16 = 47801;
/// Followed by the asking device's id.
pub const DISCOVER_PREFIX: &str = "AIRLYNX_DISCOVER";
/// Followed by `<tcp_port> <id> <mutual 0|1> <kind d|m|x|?> <name>`.
pub const ANNOUNCE_PREFIX: &str = "AIRLYNX_HERE";

pub const MAX_NAME_LEN: usize = 1024;
pub const MAX_DEVICE_NAME_LEN: usize = 64;
pub const CHUNK_SIZE: usize = 64 * 1024;

pub const STATUS_OK: u8 = 0;
pub const STATUS_HASH_MISMATCH: u8 = 1;
pub const STATUS_REJECTED: u8 = 2;
pub const STATUS_NOT_FAVORITE: u8 = 3;
/// The receiver cannot write to its save folder. Sent in place of the OK after the sender's hello.
pub const STATUS_NO_WRITE_ACCESS: u8 = 4;
/// A browse request named something that does not exist, is hidden, or lies outside the save folder.
pub const STATUS_NOT_FOUND: u8 = 5;
/// A browse request failed while reading from the save folder.
pub const STATUS_READ_ERROR: u8 = 6;

/// What a session is for, sent right after the hello.
pub const COMMAND_SEND: u8 = 1;
pub const COMMAND_LIST: u8 = 2;
pub const COMMAND_GET: u8 = 3;
/// Sent by a device that wants to be asked for sessions over a connection it opened (see `reverse`).
pub const COMMAND_STANDBY: u8 = 4;
/// Sent by that device when asked: this connection is the one a waiting session should use.
pub const COMMAND_ATTACH: u8 = 5;

/// Entry kinds in a folder listing.
pub const ENTRY_FILE: u8 = 0;
pub const ENTRY_DIR: u8 = 1;

pub fn read_u8(r: &mut impl Read) -> io::Result<u8> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b)?;
    Ok(b[0])
}

pub fn read_u16(r: &mut impl Read) -> io::Result<u16> {
    let mut b = [0u8; 2];
    r.read_exact(&mut b)?;
    Ok(u16::from_be_bytes(b))
}

pub fn read_u32(r: &mut impl Read) -> io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b))
}

pub fn read_u64(r: &mut impl Read) -> io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_be_bytes(b))
}

pub fn write_hello(w: &mut impl Write, me: &Identity) -> io::Result<()> {
    let name = clean_name(&me.name);
    w.write_all(me.id.as_bytes())?;
    w.write_all(&[name.len() as u8])?;
    w.write_all(name.as_bytes())
}

pub fn read_hello(r: &mut impl Read) -> io::Result<Identity> {
    let mut id = [0u8; 16];
    r.read_exact(&mut id)?;
    let len = read_u8(r)? as usize;
    if len > MAX_DEVICE_NAME_LEN {
        return Err(invalid("device name too long"));
    }
    let mut name = vec![0u8; len];
    r.read_exact(&mut name)?;
    let name = String::from_utf8(name).map_err(|_| invalid("device name is not UTF-8"))?;
    Ok(Identity::new(DeviceId::from_bytes(id), &name))
}

pub fn write_file_header(w: &mut impl Write, name: &str, size: u64) -> io::Result<()> {
    let name = name.as_bytes();
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return Err(invalid("file name length out of range"));
    }
    w.write_all(&(name.len() as u16).to_be_bytes())?;
    w.write_all(name)?;
    w.write_all(&size.to_be_bytes())
}

pub fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// A time span as shown next to "seen": "just now", "5 min ago", "3 h ago", "2 days ago".
pub fn human_age(secs: u64) -> String {
    match secs {
        0..10 => "just now".into(),
        10..60 => format!("{secs}s ago"),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86400 => format!("{} h ago", secs / 3600),
        86400..172800 => "1 day ago".into(),
        _ => format!("{} days ago", secs / 86400),
    }
}
