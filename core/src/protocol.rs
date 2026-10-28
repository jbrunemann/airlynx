//! Wire protocol shared by sender and receiver.
//!
//! Session (sender -> receiver, all integers big-endian):
//!   "ALNX" | version: u8                      -> receiver replies one status byte
//!   file count: u32
//!   per file:
//!     name len: u16 | name: UTF-8 | size: u64 | data | sha256: [u8; 32]
//!                                             -> receiver replies one status byte

use std::io::{self, Read, Write};

pub const MAGIC: &[u8; 4] = b"ALNX";
pub const VERSION: u8 = 1;

pub const DEFAULT_TCP_PORT: u16 = 47800;
pub const DISCOVERY_PORT: u16 = 47801;
pub const DISCOVER_MSG: &[u8] = b"AIRLYNX_DISCOVER";
pub const ANNOUNCE_PREFIX: &str = "AIRLYNX_HERE";

pub const MAX_NAME_LEN: usize = 1024;
pub const CHUNK_SIZE: usize = 64 * 1024;

pub const STATUS_OK: u8 = 0;
pub const STATUS_HASH_MISMATCH: u8 = 1;
pub const STATUS_REJECTED: u8 = 2;

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
