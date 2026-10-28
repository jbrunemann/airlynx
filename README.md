# AirLynx

Local-network file transfer between Android and macOS (and anything else that runs Rust).
Devices find each other with UDP broadcast. Files go over TCP with a SHA-256 check per file.

**Status:** working prototype (v0.2.0). Both directions were tested end to end on a
Pixel 3a API 34 emulator and on macOS. It has not been tested on a physical phone yet.

---

## Quick start

```sh
# macOS app (starts receiving on launch, saves to ~/Downloads/AirLynx)
./scripts/bundle-macos.sh && open target/release/AirLynx.app
cargo run --release -p airlynx-app          # or run without bundling

# Android APK (arm64, Android 11+)
./scripts/build-apk.sh                      # -> target/release/apk/AirLynx.apk
./scripts/build-apk.sh --install            # build + adb install

# CLI
cargo run --release -p airlynx -- receive --dir ./received
cargo run --release -p airlynx -- discover
cargo run --release -p airlynx -- send a.jpg b.pdf [--to 192.168.1.20[:port]]
```

On Android, tap **Grant file access** and enable "All files access". The built-in
file browser needs it, and received files then go to `Download/AirLynx`.
Without it, received files go to `Android/data/dev.airlynx.app/files/received`.

---

## Repository layout

Cargo workspace (edition 2024, resolver 3):

| Path | Crate | Purpose |
|------|-------|---------|
| `core/src/protocol.rs` | `airlynx-core` | Constants (ports, magic, status codes), big-endian read helpers, `human_size` |
| `core/src/discovery.rs` | | `discover()` broadcasts and collects `Peer`s; responder thread used by the receiver |
| `core/src/send.rs` | | `prepare()` validates paths, `send_files()` streams them with a progress callback (`ControlFlow::Break` cancels), `resolve()` parses `host[:port]` |
| `core/src/receive.rs` | | `start()` returns a `ReceiverHandle` (stops on `stop()`/drop) and emits `ReceiveEvent`s; filename sanitizing and `name (1).ext` dedup |
| `cli/src/main.rs` | `airlynx` | clap CLI: `send`, `receive`, `discover` |
| `app/src/app.rs` | `airlynx-app` | The egui UI shared by desktop and Android (Send/Receive tabs) |
| `app/src/platform.rs` | | Platform facade: desktop impl inline (rfd dialogs, `dirs`, `open`), Android re-exports `android.rs` |
| `app/src/android.rs` | | JNI glue: device model, all-files-access check/request, multicast lock, status bar height |
| `app/src/browser.rs` | | In-app file browser (Android only) |
| `app/src/lib.rs` | | Module wiring and `android_main` entry point |
| `app/src/main.rs` | | Desktop entry point |
| `scripts/build-apk.sh` | | Sets SDK/NDK env, creates the debug keystore if missing, runs `cargo apk build --release --lib` |
| `scripts/bundle-macos.sh` | | Builds `AirLynx.app` with Info.plist (incl. `NSLocalNetworkUsageDescription`), ad-hoc signs it |

### Threading model (app)

All networking runs on background threads. They write into `Arc<Mutex<Shared>>` and call
`ctx.request_repaint()`. The UI only reads `Shared` each frame. One thread runs per scan, per
send, and per incoming connection. The receiver also has an accept thread and a discovery
responder thread, and both poll a stop flag.

---

## Protocol (version 1)

**Discovery** on UDP 47801. The sender broadcasts `AIRLYNX_DISCOVER` to `255.255.255.255` twice.
Receivers reply `AIRLYNX_HERE <tcp_port> <name>` to the sender's address.

**Transfer** on TCP 47800 (default). There is one connection per send session, and all integers are big-endian:

```
sender                                       receiver
"ALNX" | version u8                    ->
                                       <-    status u8   (0 ok, 2 rejected)
file count u32                         ->
per file:
  name_len u16 | name (UTF-8) | size u64 | data | sha256[32]  ->
                                       <-    status u8   (0 ok, 1 hash mismatch)
```

The receiver writes each file to a hidden `.<name>.<pid>.<n>.part` file and renames it only
after the hash matches. Names are reduced to their last path component, so they can't escape
the target directory.

Changing the wire format means bumping `VERSION` in `core/src/protocol.rs`.

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
$ADB logcat | grep -i airlynx
```

- The emulator sits behind NAT, so broadcast discovery can't reach the Mac. Use the address field.
- **Emulator to Mac:** send to `10.0.2.2`.
- **Mac to emulator:** run `adb forward tcp:47900 tcp:47800`, then `airlynx send f --to 127.0.0.1:47900`.

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
  Keep targetSdk at 34, because 35 forces edge-to-edge and the bottom inset would need handling too.
- **egui's default fonts lack many symbols** (📁 ✖ ⬆ ✔ render as empty boxes). Stick to ASCII in UI strings (`…` is fine).
- **Accepted sockets inherit non-blocking mode** from the listener on macOS. `handle_session` resets it.
- **The Android soft keyboard** appears through winit's `set_ime_allowed`, and typing works. The window does not
  resize, though, so the keyboard covers anything below the focused field.
- **JNI calls** go through `android::with_env`, which attaches the thread and logs and clears Java exceptions.
  The `AndroidApp` is stored in a `OnceLock` by `android::init`.
- **Edition 2024** requires `#[unsafe(no_mangle)]` on `android_main`.

---

## Known limitations / next steps

Rough priority order:

1. **Test on a real phone and Mac on the same Wi-Fi**, especially discovery. Sending to `255.255.255.255`
   may pick the wrong interface on multi-homed Macs. A fallback would be to broadcast per interface
   (subnet-directed), or to switch to mDNS/Bonjour.
2. **The soft keyboard covers the Send button.** Scroll the focused field into view, or move Send above the address field.
3. **Android share target:** "Share to AirLynx" from other apps needs a Java/Kotlin activity
   (intent filter for `ACTION_SEND`/`ACTION_SEND_MULTIPLE`), which means moving from cargo-apk to Gradle,
   or to GameActivity with a small Java shim. This would also allow the Storage Access Framework
   instead of all-files access, which Play Store review usually rejects.
4. **Security:** there is no encryption and no authentication, and any device on the LAN can push files.
   Ideas: a pairing code or accept prompt on the receiver, and TLS with pinned self-signed certs.
5. **Receiver UX:** an accept/decline prompt, a cancel button for incoming transfers, persisted history,
   and a notification on Android.
6. **Packaging:** app icons (both platforms), a release keystore, x86_64/armv7 Android targets, and a universal macOS binary.
7. **Robustness:** resume interrupted transfers, show 0-byte files in send progress (no progress callback fires for them),
   and add integration tests for `core` (loopback send/receive, hash mismatch, name sanitizing; only the last exists).
