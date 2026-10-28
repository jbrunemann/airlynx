# AirLynx

Local-network file transfer between Android and macOS (and anything else that runs Rust).
Devices find each other with UDP broadcast on every interface, mDNS, and the last known address of each favorite. Files go over TCP, encrypted, with a SHA-256 check per file.

**Favorites:** each install has a random device id. A device only accepts files from devices it has
added as favorites. When two devices have added each other (a mutual favorite), either one can send to
the other at any time, with no prompt. The receiver is always on while the app is open, and a device
never lists itself under Add a device.

**Status:** working prototype (v0.3.0). The favorites flow was tested end to end between two CLI
profiles on one Mac. The v0.2 send/receive flows were tested on a Pixel 3a API 34 emulator and on macOS.
The redesigned UI (see "UI design") was checked on the emulator in light and dark mode. Its desktop
layout (sidebar, two columns) has only been compiled, not looked at, because screen capture was unavailable.
It has not been tested on a physical phone yet.

---

## Quick start

```sh
# macOS app (starts receiving on launch, saves to ~/Downloads/AirLynx)
./scripts/bundle-macos.sh && open target/release/AirLynx.app
cargo run --release -p airlynx-app          # or run without bundling

# Android APK (arm64, Android 11+)
./scripts/build-apk.sh                      # -> target/release/apk/AirLynx.apk
./scripts/build-apk.sh --install            # build + adb install

# CLI (same profile as the desktop app unless --config-dir is given)
cargo run --release -p airlynx -- receive --dir ./received
cargo run --release -p airlynx -- discover                  # lists id, address, favorite / has-you-as-favorite
cargo run --release -p airlynx -- favorite add "Pixel"      # by name or id prefix; a full id works offline
cargo run --release -p airlynx -- favorite list | remove <name>
cargo run --release -p airlynx -- send a.jpg b.pdf --device Pixel   # or --to 192.168.1.20[:port]
```

**Pairing in the app:** open AirLynx on both devices. On the **Send** tab, under **Add a device**, tap **Add** next to the other
device, and do the same on the other device. The favorite then shows **Online**, and **Send** is enabled once
files are chosen. Files the receiver confirms (hash verified) are removed from
the list after a send. Files that failed verification, or a cancelled or failed send, stay in the list. If a device that is not a favorite tries to send, the receiver shows a request on the
**Received** tab with an **Add favorite** button, which also works when discovery cannot find the device.

On Android, tap **Grant file access** and enable "All files access". The built-in
file browser needs it, and received files then go to `Download/AirLynx`.
Without it, received files go to `Android/data/dev.airlynx.app/files/received`.

---

## Repository layout

Cargo workspace (edition 2024, resolver 3):

| Path | Crate | Purpose |
|------|-------|---------|
| `core/src/protocol.rs` | `airlynx-core` | Constants (ports, magic, status codes), big-endian read helpers, handshake hello, `human_size` |
| `core/src/identity.rs` | | `DeviceId`, `Identity`, `Favorites` (shared, persisted, reloads when the file changes on disk, plus the `addresses` cache: last IP per favorite), `Profile` (id + favorites in one directory) |
| `core/src/discovery.rs` | | `scan()` / `discover_with()` / `discover()` ask the network and collect `Peer`s (never including the caller): broadcast per interface, unicast to cached addresses and to mDNS finds. Responder thread used by the receiver |
| `core/src/mdns.rs` | | `_airlynx._tcp` registration while receiving, and one browser thread per process that remembers the addresses it hears about (best effort, off if the daemon cannot start) |
| `core/src/secure.rs` | | `Ephemeral` X25519 key exchange and `SecureStream`, an encrypted `Read + Write` wrapper (ChaCha20-Poly1305 records) |
| `core/src/send.rs` | | `prepare()` validates paths, `send_files()` streams them with a progress callback (`ControlFlow::Break` cancels), `resolve()` parses `host[:port]` |
| `core/src/receive.rs` | | `start()` returns a `ReceiverHandle` (stops on `stop()`/drop) and emits `ReceiveEvent`s; refuses non-favorites; filename sanitizing and `name (1).ext` dedup |
| `core/src/stats.rs` | | Wakeup counters for the accept and discovery loops, discovery replies and sessions, plus process CPU time and thread count read from `/proc` |
| `cli/src/main.rs` | `airlynx` | clap CLI: `send`, `receive`, `discover`, `favorite` |
| `app/src/app.rs` | `airlynx-app` | The egui UI shared by desktop and Android: three pages, Send (files, favorites, add a device), Received (incoming transfers, requests, history) and Settings (this device, save folder, favorites). Windows 720 px or wider get a sidebar and two columns, narrower ones a bottom tab bar |
| `app/src/theme.rs` | | Light and dark palettes (follows the system setting), cards, buttons, status pill, progress bar and the painted icons |
| `app/src/platform.rs` | | Platform facade: desktop impl inline (rfd dialogs, `dirs`, `open`), Android re-exports `android.rs` |
| `app/src/android.rs` | | JNI glue: device model, files shared from other apps, config dir, all-files-access check/request, multicast lock, status bar and navigation bar height, dark mode |
| `app/src/browser.rs` | | In-app file browser (Android only) |
| `app/src/diag.rs` | | Battery and wakeup diagnostics (Android only): the Settings > Diagnostics card and the optional once-a-minute logcat line |
| `app/src/lib.rs` | | Module wiring and `android_main` entry point |
| `app/src/main.rs` | | Desktop entry point |
| `app/java/dev/airlynx/app/MainActivity.java` | | NativeActivity subclass that is the Share target: copies shared content URIs (and shared text) into the cache, and hands the paths to Rust over JNI |
| `app/java/dev/airlynx/app/ReceiveService.java` | | Foreground service (notification) that keeps the process running while the screen is locked |
| `app/res/` | | Android launcher icon (adaptive, with monochrome layer), generated by `scripts/make-icons.py` |
| `assets/fonts/` | | Inter Regular and SemiBold (SIL OFL, license included), embedded by `theme.rs`. egui's default text font is too thin on retina screens |
| `assets/icon/` | | Icon master (`airlynx.svg`), 512 px PNG (desktop window icon), `AirLynx.icns` (macOS bundle), `airlynx-logo.png` (192 px, cropped to the icon edge, shown in the app header) |
| `scripts/make-icons.py` | | Draws the icon and renders every size (needs `rsvg-convert`, `iconutil`); outputs are committed |
| `scripts/build-apk.sh` | | Sets SDK/NDK env, creates the debug keystore if missing, runs `cargo apk build --release --lib`, then adds the Java service (see gotchas) |
| `scripts/bundle-macos.sh` | | Builds `AirLynx.app` with Info.plist (incl. `NSLocalNetworkUsageDescription`), ad-hoc signs it |

### Threading model (app)

All networking runs on background threads. They write into `Arc<Mutex<Shared>>` and call
`ctx.request_repaint()`. The UI clones `Shared` each frame and records what the user did in an `Actions`
value that is applied after drawing. One thread runs per scan, per send, and per incoming connection. The
receiver also has an accept thread and a discovery responder thread, and both poll a stop flag.
The app rescans every 6 s, and a device stays "online" for 20 s after its last answer.

Identity and favorites are stored in `<config dir>/AirLynx` on desktop (`~/Library/Application Support/AirLynx`
on macOS) and in the app's private `files/config` on Android, as `device-id` and `favorites` text files.
A `favorites` line is `<id> kind:<desktop|mobile|unknown> <name>`; older lines without `kind:` still load.

---

## Protocol (version 4)

**Discovery** on UDP 47801. A device sends `AIRLYNX_DISCOVER <id>` (its 32-hex-digit device id) twice, three ways at once, because
no single one works on every network:

1. **Broadcast per interface.** To `255.255.255.255` on the default route, and from a socket bound to each IPv4
   interface to that subnet's own broadcast address (`ip | !netmask`, so a /16 reaches all of its /24 ranges). A phone on
   Wi-Fi plus a hotspot, or a Mac with a VPN, is asked on every network.
2. **Last known address.** Unicast to the IP each favorite last answered from (or last connected from). It is stored in the
   `addresses` file next to `favorites`, and is only ever a hint: a stale address just gets no answer.
3. **mDNS.** A receiving device registers `_airlynx._tcp.local.` (instance name = device id, TXT `id`, `kind`, `name`).
   Every scan also unicasts the question to each address the mDNS browser has found. mDNS can't carry `mutual`, which differs
   per asker, so it only supplies addresses and the UDP exchange below still decides who is a peer. This helps on networks
   that pass multicast but drop broadcast.

The wire format of the question and the answer did not change (neither did the TCP protocol, so `VERSION` stays 4).
Every other running device replies
`AIRLYNX_HERE <tcp_port> <id> <mutual 0|1> <kind d|m|?> <name>` to the asker's address. `kind` is `d` for a
computer, `m` for a mobile device, `?` if unknown (shown as an icon next to favorites). `mutual` is 1 when the replier has
the asker as a favorite. The asker drops replies carrying its own id and de-duplicates by id, so a device that answers on several interfaces or
through several of the ways above is listed once.

**Transfer** on TCP 47800 (default). There is one connection per send session, and all integers are big-endian:

```
sender                                       receiver
"ALNX" | version u8 | public key [u8;32] ->
                                       <-    status u8   (0 ok, 2 rejected: version mismatch, ends the session)
                                       <-    public key [u8;32]   (only after status 0)
==== everything below is inside the encrypted stream ====
sender id [u8;16] | name_len u8 | name ->
                                       <-    status u8   (0 ok, 3 sender is not a favorite)
file count u32                         ->
per file:
  name_len u16 | name (UTF-8) | size u64 | data | sha256[32]  ->
                                       <-    status u8   (0 ok, 1 hash mismatch)
```

**Encryption:** both sides make a throwaway X25519 key pair per session and swap public keys in the handshake.
Each direction gets its own ChaCha20-Poly1305 key, `SHA-256("airlynx v4 " | label | shared secret | sender pub | receiver pub)`.
After the handshake the stream is a series of records, `len u16 | ciphertext+tag`, with up to 16 KiB of plaintext each.
The nonce is a per-direction record counter, so records can't be reordered, replayed or dropped unnoticed.
Writes are buffered and flushed when a record fills, on `flush`, and before every read.
This keeps file names, contents and device names out of packet captures and router logs. UDP discovery is **not**
encrypted (it carries the device id, kind and name).

The receiver writes each file to a hidden `.<name>.<pid>.<n>.part` file and renames it only
after the hash matches. Names are reduced to their last path component, so they can't escape
the target directory.

Changing the wire format means bumping `VERSION` in `core/src/protocol.rs`.

**Trust model:** the sender id is a plain identifier, not a secret. It is broadcast on the LAN, so a host that
has seen it can claim to be that device. Favorites keep strangers and mistakes out, but they are not
authentication. The key exchange is likewise unauthenticated: it stops anyone who only records traffic, but a host that
actively sits in the middle of a connection could still read it. See "Security" under next steps.

---

## UI design

The app is one egui UI for desktop and Android (`app/src/app.rs`, styling in `app/src/theme.rs`).

- **Pages:** **Send** (files, favorites as device cards, Add a device), **Received** (incoming transfers, requests
  from non-favorites, history), **Settings** (this device, save folder, file access, favorites with Remove).
  Window width picks the layout: 720 px or more gets a left sidebar and two columns on Send, anything narrower
  (all phones) gets a header and a bottom tab bar. The Android file browser replaces the page and hides the tab bar.
- **Flow:** choose files, then press **Send** on an online, mutual favorite. While sending, that card shows the
  progress bar, byte counts and **Cancel**, and the other Send buttons are disabled. The result is a dismissible
  banner at the top of Send. The Received tab badge counts unseen files plus pending requests.
- **Theme:** `theme::LIGHT` and `theme::DARK` palettes, picked by `theme::pal(ui)` from egui's dark mode. Colors come from
  the app icon (indigo accent, navy text) plus green, amber and red for status. All widgets are painted in
  `theme.rs` (`card`, `button`, `icon_button`, `tile`, `pill`, `progress`, `logo`) and the icons are drawn with
  shapes on a 24 by 24 grid (`theme::Icon`), so no icon font or images are needed. Controls are 44 pt high on
  Android and 38 pt on desktop.
- **Fonts:** Inter Regular and SemiBold from `assets/fonts/` replace egui's thin default text font. Headings, names
  and button labels use the `theme::bold()` family. The mockups the design came from were made on a Claude Design canvas
  and are not in the repo.
- **Dark mode on Android** is read over JNI (see the gotchas). On desktop egui follows the system setting.

---

## Toolchain and environment

- Rust 1.95+, target `aarch64-linux-android`
- Android SDK at `~/Library/Android/sdk` (platform 34, build-tools 36, NDK 27.3), `cargo-apk` 0.10
- The scripts default `ANDROID_HOME` to the path above and pick the newest NDK unless `ANDROID_NDK_ROOT` is set
- Key versions: `eframe`/`egui` 0.37 (glow backend), `winit` 0.30.13, `jni` 0.21, `rfd` 0.17
- Android package id `dev.airlynx.app`, minSdk 30, targetSdk 34

## Testing

```sh
cargo test --workspace
cargo clippy --workspace            # kept warning-free
```

**Android emulator** (AVD `Pixel_3a_API_34_extension_level_7_arm64-v8a`):

```sh
SDK=~/Library/Android/sdk; ADB=$SDK/platform-tools/adb
$SDK/emulator/emulator -avd Pixel_3a_API_34_extension_level_7_arm64-v8a -no-window -no-audio -gpu swiftshader_indirect &
./scripts/build-apk.sh --install
$ADB shell appops set dev.airlynx.app MANAGE_EXTERNAL_STORAGE allow   # skip the settings screen
$ADB shell am start -n dev.airlynx.app/android.app.NativeActivity
$ADB exec-out screencap -p > shot.png                                # look at the UI
$ADB shell input tap X Y                                             # drive it (physical px, 1080x2220)
$ADB shell cmd uimode night yes                                      # dark mode (no = light)
$ADB logcat | grep -i airlynx
```

**Measuring battery use while listening (physical phone):** Settings > Diagnostics shows drain, current, screen and Doze
state, whether the locks are held, process CPU time and how often the loops wake up, all counted since a baseline
("Reset baseline"). The screen being on inflates the numbers, so for a real test:

1. Unplug the phone (while charging the drain is not measured), open Settings and tick **Write a log line every minute**.
   That also resets the baseline and appends a header line to `airlynx-diag.log` in the save folder
   (`Download/AirLynx` with file access).
2. Lock the phone and leave it for an hour or more.
3. Get the file off the device without adb: on the Send tab, **Browse files**, pick `airlynx-diag.log` and send it to a
   favorite (the Mac saves it in `~/Downloads/AirLynx`). With adb it is `adb pull /sdcard/Download/AirLynx/airlynx-diag.log`.
   Each line is also logged to logcat (`adb logcat -d | grep airlynx_app::diag`).

Each line is self-contained: `ts` is the Unix time, `start_uah - uah` over `up_s` gives the drain in microampere-hours
(`start_pct` and `pct` are the coarse version), `cpu_s` is CPU time used since the baseline, and `accept_polls` /
`discovery_polls` count loop wakeups. `uah` and `current_ua` show `-` or odd values on devices that do not report them.
`current_ua` has a device-specific sign and is noisy, so trust the charge counter over it. Runs are appended, and a
`#` header line starts each app version and device.

**Finding out which part costs battery:** the card has an Experiments section with five switches: receiver on (off is
the control run: no service, no locks, no threads), CPU wake lock, Wi-Fi lock, multicast lock and slow polling (1 s
instead of 0.1 s). Each change restarts the receiver and the baseline, and every log line records the combination as
`exp=rx:1,wake:1,wifi:1,mcast:1,poll:100` along with the locks actually held. The switches are not saved, so a
restart goes back to everything on. A suggested sequence, with the phone unplugged and about 30 minutes per step
(the charge counter moves in 3.7 mAh steps, so shorter runs are too coarse):

1. Receiver off (control), then everything on, then switch off one lock at a time and leave it off: Wi-Fi lock, CPU wake
   lock, multicast lock, then slow polling. Finish with the control again to see whether the drain drifted.
2. Wake the phone only to flip a switch, then lock it again. Compare only the lines with `screen=false`.
3. Send the file back and subtract each step's drain from the one before. Slow polling delays accepting a new
   connection by up to a second, and the other switches can make discovery or the first transfer slower, so also
   check that a send from the Mac still works in each state.

For a baseline to compare against, run the same hour with the receiver stopped (close the app) and subtract. On the
emulator the battery numbers are fake (it reports 100% and 900 mA) but the CPU and wakeup counters are real.

- **Share target:** `adb shell "am start -a android.intent.action.SEND -t image/png --eu android.intent.extra.STREAM content://media/external/images/media/<id> --grant-read-uri-permission -n dev.airlynx.app/.MainActivity"`
  (find `<id>` with `adb shell content query --uri content://media/external/images/media`; quote the whole remote command, or
  `adb` splits arguments at spaces). Text: `-t text/plain --es android.intent.extra.TEXT 'hello'`. `SEND_MULTIPLE` can't be
  built with `am`, so it has only been read, not run. With `adb root`, `ls /data/data/dev.airlynx.app/cache/share` shows the copies.
- The mDNS test needs multicast, so it is skipped by default: `cargo test -p airlynx-core -- --ignored`.
- The emulator sits behind NAT, so broadcast discovery can't reach the Mac. The app has no manual address entry
  (see the icebox), so test the emulator against the CLI.
- **Emulator to Mac:** not possible from the app without an address field. The emulator can only be reached from the Mac.
- **Mac to emulator:** run `adb forward tcp:47900 tcp:47800`, then `airlynx send f --to 127.0.0.1:47900`.
  The emulator shows a request with **Add favorite**; accept it, then send again.
- Two profiles on one machine can test favorites with the CLI: `--config-dir a` and `--config-dir b`
  (only one receiver can bind the ports at a time).

---

## Gotchas learned the hard way

- **egui 0.37 API changed:** `eframe::App` now has `fn ui(&mut self, ui: &mut egui::Ui, frame)`, not `update`.
  Panels are `egui::Panel::top("id").show(ui, …)` and `egui::CentralPanel::default_margins().show(ui, …)`.
  `DroppedFile` is a trait, so use `f.path()`.
- **eframe does not re-export winit.** `winit` is a direct Android-only dependency and must stay on the
  same version eframe uses (0.30.x). `AndroidApp` comes from `winit::platform::android::activity`.
- **`jni` is pinned to 0.21.** 0.22 rewrote the API.
- **Always run `cargo apk build --lib`.** Without `--lib`, cargo-apk panics with "Bin is not compatible with Cdylib"
  because of the desktop `[[bin]]` target.
- **NativeActivity draws under the status bar.** The header adds `status_bar_height_px()` (JNI, cached).
  It draws under the gesture/navigation bar too, so the tab bar adds `navigation_bar_height_px()` (window insets via JNI) and the file
  browser adds it as bottom margin. Keep targetSdk at 34, because 35 forces edge-to-edge.
- **Android dark mode:** winit does not report it, so `android::is_dark_mode()` reads `Configuration.uiMode` and `tick` applies it
  every 2 s. Test with `adb shell cmd uimode night yes|no`.
- **egui rows:** `ui.horizontal` reads `interact_size.y` before its closure runs. To make a short row, set it on a `ui.scope`
  around the row (`theme::row`), not inside the closure.
- **egui's default fonts lack many symbols** (📁 ✖ ⬆ ✔ render as empty boxes). Stick to ASCII in UI strings (`…` is fine).
- **Accepted sockets inherit non-blocking mode** from the listener on macOS. `handle_session` resets it.
- **The Android soft keyboard** appears through winit's `set_ime_allowed`, and typing works. The window does not
  resize, though, so the keyboard covers anything below the focused field.
- **JNI calls** go through `android::with_env`, which attaches the thread and logs and clears Java exceptions.
  The `AndroidApp` is stored in a `OnceLock` by `android::init`.
- **Receiving with the screen locked:** `android::set_keep_awake` starts `ReceiveService` (a `dataSync` foreground
  service with an ongoing notification) and holds a partial wake lock, a Wi-Fi lock and a multicast lock while the receiver runs
  (`android::set_keep` picks which, for the Diagnostics experiments).
  cargo-apk 0.10 can neither ship Java nor declare a `<service>`, so `build-apk.sh` compiles `app/java` with
  `javac` + `d8`, patches the generated manifest (`hasCode`, `<service>`) and reassembles the APK with `aapt`,
  `zipalign` and `apksigner`. The service is addressed by name over JNI (a native thread's class loader can't see
  app classes). Android 13+ needs the `POST_NOTIFICATIONS` permission, which the app requests at start; without it the
  service still runs but the notification is hidden. The service stops when the app exits. Verified on the API 34
  emulator with the screen asleep; not on a physical phone yet. Android 15 limits `dataSync` services to 6 h
  per day once targetSdk is 35.
- **Share target (`MainActivity`):** cargo-apk can set the activity class and launch mode (`name`, `launch_mode` in `app/Cargo.toml`)
  and add intent filters (`SEND`, `SEND_MULTIPLE`, `*/*`), so the manifest needs no patch for it. It must be `singleTask`:
  a share while the app runs then arrives in `onNewIntent`, instead of starting a second NativeActivity (and a second
  native thread). Shared content URIs are copied into `cache/share/<batch>/<name>` on a Java thread (`/proc/self/fd`
  paths are denied under scoped storage, and a share can be gigabytes), plain text becomes `<subject>.txt`. Rust polls
  `takeSharedFiles` / `sharedPreparing` / `takeShareError` from `tick` and calls the methods on the activity object directly,
  since a native thread's class loader can't find app classes. Copies are deleted when sent, removed or cleared, and the
  whole folder at startup. The shared list is in memory only. Not verified on a physical phone.
- **mDNS on Android** needs the multicast lock (already held while receiving) and runs two threads (`mDNS_daemon`,
  `airlynx-mdns`) that sleep when idle. The daemon is IPv4 only.
- **Edition 2024** requires `#[unsafe(no_mangle)]` on `android_main`.

---

## Known limitations / next steps

Rough priority order:

1. **Test on a real phone and Mac on the same Wi-Fi**, especially discovery. Per-interface broadcast, the address cache
   and mDNS are done and tested on one Mac (two CLI profiles) and the emulator, but only the CLI run exercised all three
   against a real peer. Still untested: a network that really drops broadcast, a phone on a hotspot, and macOS asking for
   the Local Network permission for mDNS.
2. **Android share target:** done (see the gotchas), including text. Open: a one-tap "send to the only online favorite"
   after a share, `SEND_MULTIPLE` on a real device, and moving from all-files access to the Storage Access Framework,
   which Play Store review prefers.
3. **Security:** transfers are encrypted (v4) but not authenticated, and favorites are matched by an unauthenticated
   device id (see the trust model above), so a host on the LAN that learns a favorite's id can impersonate it, and an
   active man in the middle can read a session. Fix: a pairing step that exchanges a secret or public key when two
   devices favorite each other (for example a short code shown on both screens), then use it to authenticate the
   key exchange. Discovery packets are still plaintext.
4. **Receiver UX:** a cancel button for incoming transfers, persisted history (it is in memory only, and ages
   show as "5 min ago" because no wall-clock time is kept), and a notification when a file arrives.
5. **UI follow-ups:** look at the desktop layout on a real screen and fix what is off, per-type file icons (every file
   uses the same tile), a gradient on the logo and progress bar (only the icon PNG has one), and an Unfavorite shortcut
   on the device card (it now lives only in Settings).
6. **Packaging:** a release keystore, x86_64/armv7 Android targets, and a universal macOS binary.
7. **Robustness:** resume interrupted transfers, show 0-byte files in send progress (no progress callback fires for them),
   and add integration tests for `core` (loopback send/receive, hash mismatch, name sanitizing; only the last exists).

### Icebox

- **Send to an address:** a collapsible section in the app with a host/IP field and its own Send button, for devices
  that discovery cannot find (for example the emulator behind NAT). It resolved the target with `send::resolve` and
  reused `start_send`. It was removed from the UI, and the CLI still has `send --to`. If it comes back, move Send above
  the field, because the soft keyboard covers it on Android.
