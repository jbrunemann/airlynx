#!/usr/bin/env bash
# Builds and signs target/release/apk/AirLynx.apk. Pass --install to install via adb.
set -euo pipefail
cd "$(dirname "$0")/.."

export ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
if [[ -z "${ANDROID_NDK_ROOT:-}" ]]; then
    ANDROID_NDK_ROOT="$(ls -d "$ANDROID_HOME"/ndk/* | sort -V | tail -1)"
fi
export ANDROID_NDK_ROOT

if [[ ! -f android-debug.keystore ]]; then
    keytool -genkeypair -keystore android-debug.keystore -storepass android -keypass android \
        -alias androiddebugkey -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=AirLynx Debug"
fi

(cd app && cargo apk build --release --lib)
echo "APK: $(pwd)/target/release/apk/AirLynx.apk"

if [[ "${1:-}" == "--install" ]]; then
    "$ANDROID_HOME/platform-tools/adb" install -r target/release/apk/AirLynx.apk
fi
