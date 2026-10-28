//! AirLynx core: UDP discovery plus a small TCP file-transfer protocol.

pub mod discovery;
pub mod protocol;
pub mod receive;
pub mod send;

pub use discovery::{Peer, discover};
pub use protocol::{DEFAULT_TCP_PORT, DISCOVERY_PORT, human_size};
