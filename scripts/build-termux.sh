#!/usr/bin/env bash
# Builds the CLI for Termux (a plain Android executable, not an APK) as one tarball per CPU, each with install.sh and
# README.txt: target/release/termux/airlynx-termux-aarch64.tar.gz and airlynx-termux-armv7.tar.gz. Pass --push to copy
# the aarch64 one to the first adb device (/sdcard/Download).
set -euo pipefail
cd "$(dirname "$0")/.."

export ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
if [[ -z "${ANDROID_NDK_ROOT:-}" ]]; then
    ANDROID_NDK_ROOT="$(ls -d "$ANDROID_HOME"/ndk/* | sort -V | tail -1)"
fi
TOOLCHAIN="$(ls -d "$ANDROID_NDK_ROOT"/toolchains/llvm/prebuilt/* | head -1)/bin"

# Termux supports Android 7 (API 24) and newer, so link against that level.
API=24
OUT=target/release/termux
mkdir -p "$OUT"

# rust target : NDK clang prefix : output name
for spec in "aarch64-linux-android:aarch64-linux-android:aarch64" "armv7-linux-androideabi:armv7a-linux-androideabi:armv7"; do
    IFS=: read -r target clang name <<<"$spec"
    upper="$(echo "$target" | tr 'a-z-' 'A-Z_')"
    export "CARGO_TARGET_${upper}_LINKER=$TOOLCHAIN/${clang}${API}-clang"
    cargo build --release --target "$target" -p airlynx
    dir="$OUT/airlynx-termux-$name"
    rm -rf "$dir"
    mkdir -p "$dir"
    cp "target/$target/release/airlynx" "$dir/airlynx"
    cp scripts/termux-install.sh "$dir/install.sh"
    cat > "$dir/README.txt" <<TXT
AirLynx for Termux ($name)

Install it for your user (run termux-setup-storage first if you have not):

  ./install.sh

Then start with "airlynx discover" or "airlynx receive --name 'My phone'".
Remove it with "./install.sh --uninstall".

This is the command line version. It needs no root and no APK.
TXT
    tar -C "$OUT" -czf "$dir.tar.gz" "airlynx-termux-$name"
    echo "Termux: $(pwd)/$dir.tar.gz"
done

if [[ "${1:-}" == "--push" ]]; then
    "$ANDROID_HOME/platform-tools/adb" push "$OUT/airlynx-termux-aarch64.tar.gz" /sdcard/Download/
    echo "In Termux (run termux-setup-storage once):"
    echo "  tar xzf /sdcard/Download/airlynx-termux-aarch64.tar.gz -C ~ && ~/airlynx-termux-aarch64/install.sh"
fi
