# Showing AirLynx favorites in Finder's Network view (SMB gateway)

Date of the spike: 2026-10-31. Nothing from it was kept in the repo; this file is the record.

## Goal

Make each online favorite appear under **Network** in the macOS Finder sidebar, and let a click open that device's
save folder (what `browse::list` and `browse::download` already read) as an ordinary Finder window.

## Result in one paragraph

It works, but only through SMB. Finder's Network view cannot be given custom entries: it lists hosts that advertise a
file-sharing service over Bonjour, and a click makes Finder's own client connect to them. So the plan is a read-only SMB2
server inside AirLynx that translates to the AirLynx LIST and GET commands, plus one `_smb._tcp` advertisement per
online favorite. The spike showed that Finder mounts a hand-rolled SMB server through the Network entry, on a custom
port, with Guest login, with no root needed. The server itself is not written yet.

## What was tested

Test machine: macOS (Darwin 27.0.0). Test services were registered with `dns-sd -R "<name>" _<type>._tcp . <port>` and
Finder's Network view was checked by eye.

| Bonjour type | Shown in Network? | Notes |
|---|---|---|
| `_smb._tcp` | yes | Clicking it connected to the **advertised port** (8445), not to 445. |
| `_afpovertcp._tcp` | yes | Dead end: a click never opened a connection to port 548. The AFP client is gone or disabled. |
| `_adisk._tcp` | yes | Time Machine disk discovery only, not file browsing. |
| `_webdav._tcp` | no | |
| `_webdavs._tcp` | no | |
| `_ftp._tcp` | no | |
| `_sftp-ssh._tcp` | no | |
| `_nfs._tcp` | no | |

Only the SMB and AFP entries were clicked. The others were only checked for appearing.

### What Finder sent to a bare listener on 8445

A plain TCP listener that logged the first packet saw, per click: two connections that sent nothing (probes), then one
connection with a 73-byte **SMB1 multi-protocol negotiate** (`ff 53 4d 42 72 ...`, dialects "NT LM 0.12", "SMB 2.002" and
"SMB 2.???"). So the server answers with an SMB2 NEGOTIATE response and the session continues as SMB2. A binding below
1024 worked without root too (ports 445 and 548 bound fine from a normal process), though a custom port per favorite
makes that unnecessary.

### Full mount with a reference server

impacket's `SimpleSMBServer` (pip package `impacket`, run from a throwaway venv) served a folder as share `PIXEL` on
port 8445 with SMB2 on and no credentials, while the Bonjour entry `AirLynx Test SMB` pointed at that port:

```python
from impacket import smbserver
s = smbserver.SimpleSMBServer(listenAddress="0.0.0.0", listenPort=8445)
s.addShare("PIXEL", "/path/to/folder", "AirLynx spike")
s.setSMB2Support(True)
s.setSMBChallenge("")
s.start()
```

Result: clicking the entry in Network mounted it, the share was browsable, folders opened and files were listed.
The one thing that did not open was `Photos/a.jpg`, but that was a test file containing the text "pic", not an image,
so it says nothing about the server. Not checked: whether Finder showed a login dialog (Guest was the fallback if it
did), whether a real file opens and previews, and copying a file out.

## Design

- **Where:** `core/src/smb.rs` (the protocol and the server), so the app can reuse it, plus a thin `airlynx smb` command
  in the CLI first. Later a Settings toggle in the app. No new heavy dependencies: hand-rolled SMB2 over `std::net`,
  one thread per connection like the rest of the code.
- **One service per favorite:** each online favorite gets its own TCP port and its own `_smb._tcp` advertisement named
  after the favorite, with a single share. A click then lands straight in that favorite's save folder. Advertise with
  the `mdns-sd` crate already in `core`. Add and remove advertisements as the scan reports devices going online and
  offline (the app rescans every 6 s while focused, every 60 s otherwise). Only favorites that have added this Mac can
  be browsed, so only those should be advertised.
- **SMB2 commands needed (read only):** NEGOTIATE (answering the SMB1 multi-protocol request with an SMB2 reply),
  SESSION_SETUP (NTLMSSP in SPNEGO, accepting Guest), TREE_CONNECT, CREATE, CLOSE, QUERY_DIRECTORY, QUERY_INFO,
  READ, IOCTL (validate negotiate), ECHO, TREE_DISCONNECT, LOGOFF. Finder asks for the share list first, so `IPC$`
  also needs a small `srvsvc` answer (DCE/RPC bind and NetShareEnumAll over the named pipe) that returns the one share.
  Estimate: about 1,000 lines.
- **Directory listing:** `browse::list` per QUERY_DIRECTORY, with a cache of a few seconds so Finder's repeated
  queries do not each open an encrypted session.
- **File reads:** AirLynx GET is the whole file followed by a SHA-256, with no ranges, but Finder and Quick Look read at
  arbitrary offsets. Start `browse::download` into a temp file in the background on the first READ and let READs
  wait until the bytes they need have arrived. Bytes served before the final hash check are unverified; if the hash
  fails, the temp file is dropped and later reads error. Document that in the README. Adding range support to GET would be
  the cleaner fix, but it changes the wire format, so `VERSION` in `core/src/protocol.rs` must be bumped and the
  README protocol section updated.
- **Writes:** out of scope at first. Drag and drop onto the volume could map to SEND later.

## Open questions and risks

- Does Finder show a Guest login dialog, or does it go straight in? Test with the real server.
- Does a Bonjour address that points at this same Mac (a gateway on loopback or the LAN address) show up in Network? The
  spike advertised the Mac's own LAN address and it did show, so a LAN-address advertisement is fine. Loopback only is
  untested.
- Large files: opening a big video waits for the transfer to get far enough ahead. Quick Look and thumbnails will read
  a lot of files just to render a folder, so consider reading nothing until a file is really opened, and returning small
  or empty data for thumbnail probes.
- macOS Local Network permission: the app already needs it for mDNS and discovery, so no new prompt is expected.
- Port 445: avoided on purpose. If the Mac has File Sharing turned on, `smbd` owns 445.
- Name clashes: two favorites with the same name need distinct advertised instance names.
- Trust model: the gateway is an unauthenticated listener on the LAN that exposes a favorite's folder to anyone who can
  reach the port. It should listen on loopback if loopback advertisements turn out to work, otherwise only answer
  while the app window is open, and be off by default behind a Settings switch.

## Not worth pursuing

- WebDAV, FTP, SFTP and NFS: not listed by Finder's Network view (see the table above).
- AFP: the client no longer connects.
- A File Provider extension would give a proper sidebar entry with lazy downloads, but it needs a Swift app extension,
  real signing and entitlements, and it appears under Locations rather than Network.
- Samba plus a FUSE layer: macFUSE needs a kernel extension.
