//! AirLynx core: UDP and mDNS discovery, favorite devices, and a small TCP file-transfer protocol.

pub mod browse;
pub mod discovery;
pub mod identity;
pub(crate) mod mdns;
pub mod protocol;
pub mod receive;
pub(crate) mod reverse;
pub mod secure;
pub mod send;
pub mod stats;

pub use discovery::{Peer, discover, discover_with, route_to, scan};
pub use identity::{DeviceId, DeviceKind, Favorite, Favorites, Identity, Profile, Route, Sighting};
pub use protocol::{DEFAULT_TCP_PORT, DISCOVERY_PORT, human_age, human_size};
