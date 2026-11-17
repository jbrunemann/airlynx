Read docs/DEVELOPING.md first (README.md is the user-facing page): it covers the architecture, protocol, build/test workflow (including driving the Android emulator over adb), the gotchas found so far, and the prioritized next steps.

- Build the APK only via `./scripts/build-apk.sh` (needs `cargo apk build --lib`).
- Keep `cargo clippy --workspace` warning-free and UI strings ASCII-only.
- When changing the wire format, bump `VERSION` in `core/src/protocol.rs` and update the protocol section in docs/DEVELOPING.md.
