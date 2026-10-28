# AirLynx

Prototype for sending files between devices on a local network.
Discovery uses UDP broadcast, and transfers use TCP with a SHA-256 check per file.

## Layout

| Crate  | What                                                              |
|--------|-------------------------------------------------------------------|
| `core` | Protocol, discovery, sender and receiver (`airlynx-core`)         |
| `cli`  | Command-line tool (`airlynx`)                                     |
| `app`  | egui app; native binary on macOS, `cdylib` + NativeActivity APK on Android |

## macOS app

```sh
./scripts/bundle-macos.sh            # -> target/release/AirLynx.app
open target/release/AirLynx.app
```

Or run it directly with `cargo run --release -p airlynx-app`.
The app starts receiving on launch and saves to `~/Downloads/AirLynx`.
Drop files onto the window to send them.

## Android app

```sh
./scripts/build-apk.sh               # -> target/release/apk/AirLynx.apk
./scripts/build-apk.sh --install     # build and install over adb
```

Uses `~/Library/Android/sdk` and its newest NDK unless `ANDROID_HOME` / `ANDROID_NDK_ROOT` are set.
The APK targets arm64 devices running Android 11+ and is signed with a local debug key
(`android-debug.keystore`, generated on first build).

On first use, tap **Grant file access** and enable "All files access" for AirLynx.
That lets the built-in browser read your files and saves received files to `Download/AirLynx`.
Without it, received files go to `Android/data/dev.airlynx.app/files/received`.

## CLI

```sh
airlynx receive --dir ~/Downloads/airlynx
airlynx discover
airlynx send photo.jpg notes.pdf             # auto-discovers a single receiver
airlynx send photo.jpg --to 192.168.1.20     # skip discovery
```

## Network

Ports: TCP 47800 (transfer) and UDP 47801 (discovery).

- macOS asks to allow incoming connections (and local network access for the `.app`) the first time. Allow it.
- Some Wi-Fi networks (guest/hotel) block broadcast or client-to-client traffic.
  If scanning finds nothing, enter the receiver's IP manually.
- The Android emulator sits behind NAT: it reaches the Mac at `10.0.2.2`,
  and the Mac reaches it with `adb forward tcp:47900 tcp:47800` plus `--to 127.0.0.1:47900`.

## Protocol

Big-endian integers over one TCP connection:

```
"ALNX" | version u8                         -> receiver: status u8
file count u32
per file: name_len u16 | name | size u64 | data | sha256[32]
                                            -> receiver: status u8 (0 ok, 1 hash mismatch)
```
