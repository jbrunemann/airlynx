//! mDNS (Bonjour) discovery, for networks that pass multicast but drop UDP broadcast.
//!
//! A receiving device registers `_airlynx._tcp` with its id in a TXT record, and one browser thread
//! per process remembers the addresses of every instance it hears about. `discover` then probes
//! those addresses over the normal unicast UDP protocol, which also tells it whether the peer has us
//! as a favorite (mDNS can't carry that, because the answer differs per asker).
//!
//! Everything here is best effort: if the daemon can't start (no multicast, no permission), discovery
//! carries on with broadcast and the address cache.

use std::net::IpAddr;
use std::sync::{Mutex, OnceLock};
use std::thread;

use mdns_sd::{IfKind, ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::identity::{DeviceId, Identity};

const SERVICE_TYPE: &str = "_airlynx._tcp.local.";

struct Seen {
    fullname: String,
    id: DeviceId,
    ips: Vec<IpAddr>,
}

static SEEN: Mutex<Vec<Seen>> = Mutex::new(Vec::new());
static DAEMON: OnceLock<Option<ServiceDaemon>> = OnceLock::new();

/// The shared daemon, started (together with the browser) on first use.
fn daemon() -> Option<&'static ServiceDaemon> {
    DAEMON
        .get_or_init(|| {
            let daemon = ServiceDaemon::new().ok()?;
            // Everything else in AirLynx is IPv4.
            let _ = daemon.disable_interface(IfKind::IPv6);
            let events = daemon.browse(SERVICE_TYPE).ok()?;
            thread::Builder::new().name("airlynx-mdns".into()).spawn(move || collect(events)).ok()?;
            Some(daemon)
        })
        .as_ref()
}

/// Keeps `SEEN` in step with the browser's events until the daemon goes away.
fn collect(events: mdns_sd::Receiver<ServiceEvent>) {
    while let Ok(event) = events.recv() {
        match event {
            ServiceEvent::ServiceResolved(info) => {
                let Some(id) = info.get_property_val_str("id").and_then(|id| id.parse().ok()) else { continue };
                let ips: Vec<IpAddr> = info.get_addresses_v4().into_iter().map(IpAddr::V4).collect();
                let fullname = info.get_fullname().to_string();
                let mut seen = SEEN.lock().unwrap();
                seen.retain(|s| s.fullname != fullname);
                if !ips.is_empty() {
                    seen.push(Seen { fullname, id, ips });
                }
            }
            ServiceEvent::ServiceRemoved(_, fullname) => SEEN.lock().unwrap().retain(|s| s.fullname != fullname),
            _ => {}
        }
    }
}

/// Addresses of the other AirLynx devices mDNS has found so far, starting the browser if needed.
/// Empty at first on a fresh process; answers arrive a few hundred milliseconds after the first call.
pub(crate) fn candidates(me: DeviceId) -> Vec<IpAddr> {
    if daemon().is_none() {
        return Vec::new();
    }
    SEEN.lock().unwrap().iter().filter(|s| s.id != me).flat_map(|s| s.ips.iter().copied()).collect()
}

/// Keeps this device's `_airlynx._tcp` registration alive; unregisters when dropped.
pub(crate) struct Advertisement {
    fullname: String,
}

impl Drop for Advertisement {
    fn drop(&mut self) {
        if let Some(daemon) = daemon() {
            let _ = daemon.unregister(&self.fullname);
        }
    }
}

/// Announces `me` on the network. The instance name is the device id, so it can't collide with another
/// device and name probing is skipped.
pub(crate) fn advertise(me: &Identity, tcp_port: u16) -> Option<Advertisement> {
    let daemon = daemon()?;
    let host = format!("airlynx-{}.local.", me.id.short());
    let properties = [
        ("id", me.id.to_string()),
        ("kind", me.kind.code().to_string()),
        ("name", me.name.clone()),
    ];
    let mut info = ServiceInfo::new(SERVICE_TYPE, &me.id.to_string(), &host, (), tcp_port, &properties[..])
        .ok()?
        .enable_addr_auto();
    info.set_requires_probe(false);
    let fullname = info.get_fullname().to_string();
    daemon.register(info).ok()?;
    Some(Advertisement { fullname })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Registers a device and expects the browser (same process, same daemon) to resolve it to an
    /// address. Needs working multicast, so it is not part of the default run.
    #[test]
    #[ignore = "needs multicast; run with --ignored"]
    fn an_advertised_device_is_found() {
        let other = Identity::new(DeviceId::random(), "Test phone");
        let _advertisement = advertise(&other, 47811).expect("mDNS daemon starts");
        let me = DeviceId::random();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let found = SEEN.lock().unwrap().iter().any(|s| s.id == other.id && !s.ips.is_empty());
            if found {
                // The device is a candidate for everyone else, but never for itself.
                assert!(!candidates(me).is_empty());
                let own: Vec<IpAddr> = SEEN.lock().unwrap().iter().filter(|s| s.id == other.id).flat_map(|s| s.ips.clone()).collect();
                assert!(own.iter().all(|ip| candidates(me).contains(ip)));
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("no mDNS candidate within 10 s");
    }
}
