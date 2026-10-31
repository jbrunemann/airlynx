# Why LocalSend could not discover devices here, and AirLynx could

Date of the tests: 2026-10-28.

## Summary

On this network LocalSend on the Mac and LocalSend on the phone did not see each other, while AirLynx did.
The Mac and the phone are on the same /16 but in different /24 ranges. LocalSend's multicast announcements
did not reach the Mac, and its fallback scan only probes the /24 of the device's own address, so it never
looks in the other device's range. AirLynx sends one UDP broadcast to `255.255.255.255`, which crosses
the /24 boundary inside the /16, and the phone answers.

## Setup

| | Mac | Phone |
|---|---|---|
| Address | 10.11.19.114 | 10.11.46.85 |
| Netmask | 255.255.0.0 (/16) | not read, same /16 |
| /24 range | 10.11.19.0 - 10.11.19.255 | 10.11.46.0 - 10.11.46.255 |
| Device | macOS, interface `en0`, gateway 10.11.1.1 | Samsung SM-S911B |

Both devices ran LocalSend (TCP and UDP 53317) and AirLynx (TCP 47800, UDP 47801).

## Evidence

### 1. The Mac side of LocalSend is healthy

- `lsof` showed LocalSend (pid 12636) listening on `*:53317` for TCP and UDP, IPv4 and IPv6.
- The macOS application firewall was disabled (`socketfilterfw --getglobalstate`: State = 0).
- The multicast route `224.0.0/4` points at `en0`.

### 2. The phone is reachable, and LocalSend is running on it

- `ping 10.11.46.85`: 0% loss, 80 to 200 ms (high for a LAN, probably Wi-Fi power saving on the phone).
- `nc -z 10.11.46.85 53317`: open (LocalSend).
- `nc -z 10.11.46.85 47800`: open (AirLynx).

So nothing blocks unicast between the two devices. Only discovery fails.

### 3. LocalSend's fallback scan only covers the local /24

The protocol spec (localsend/protocol, section 3.2) says the HTTP fallback is used "when multicast was
unsuccessful" and sends the register request "to all local IP addresses". It does not define the range.

The implementation does (`packages/core/src/discovery/mod.rs`, `scan_subnet`): it takes the octets of the
local interface address and probes hosts 0 to 255 of that `/24` only.

- The Mac (10.11.19.114) scans 10.11.19.0 - 10.11.19.255 and never tries 10.11.46.85.
- The phone (10.11.46.85) scans 10.11.46.0 - 10.11.46.255 and never tries 10.11.19.114.

### 4. LocalSend's multicast does not reach the Mac

LocalSend announces on `224.0.0.167`, UDP port 53317.

- A listener on the Mac joined that group on `en0` (script: `mcast_listen.py`, SO_REUSEPORT, so it ran
  next to LocalSend).
- Window: 90 seconds, while LocalSend was opened and refreshed several times on the phone.
- Result: `done, 0 packets`.
- Positive control (`mcast_selftest.py`): a packet sent from the Mac to `224.0.0.167:53317` via `en0` was
  received by the same kind of listener. So the listener works and the Mac can join the group.
- The phone's LocalSend also did not list the Mac.

### 5. AirLynx finds the phone across the /24 boundary

AirLynx discovery (`core/src/discovery.rs`) sends `AIRLYNX_DISCOVER <id>` to `255.255.255.255:47801` and
every device answers with `AIRLYNX_HERE ...`.

- `airlynx discover` from the Mac listed `10.11.46.85:47800 "SM-S911B"`, a mutual favorite.
- A direct unicast probe to `10.11.46.85:47801` got `AIRLYNX_HERE 47800 <id> 0 m SM-S911B`.
- An earlier `discover` run found nothing because the phone's AirLynx was not open yet.

## Conclusion

Two things combine:

1. Multicast from the phone does not reach the Mac on this network (evidence 4).
2. LocalSend's fallback cannot make up for it, because the devices are in different /24 ranges
   (evidence 3).

AirLynx works because limited broadcast to `255.255.255.255` reaches the whole layer-2 segment, whatever
the /24 split (evidence 5).

## What this does not prove

- It does not show that the phone actually transmitted multicast during the test. It shows that none
  arrived. A phone setting (for example Wi-Fi multicast filtering) or LocalSend not announcing are not
  ruled out.
- Whether the network blocks multicast or something on the phone does was not told apart. A capture
  on the phone or on the access point would settle it.
- The phone to Mac direction of LocalSend's TCP traffic was not tested. Only Mac to phone was.
- The macOS "Local Network" permission for LocalSend was not checked.
- The LocalSend behavior comes from the source on GitHub at the time of the test, not from running a
  debug build.

## Workarounds

- LocalSend: add the other device by address in the Send tab (phone `10.11.46.85`, port 53317). It breaks
  when the address changes, and a new lease can land in another /24.
- Network: put both devices in the same /24, give the phone a DHCP reservation, or allow multicast
  between clients (needs router or access point access).
- Possible upstream improvement for LocalSend: a UDP broadcast fallback, or a scan that uses the real
  subnet mask.

## Sources

- LocalSend protocol: https://github.com/localsend/protocol
- LocalSend discovery source: https://github.com/localsend/localsend/blob/main/packages/core/src/discovery/mod.rs
