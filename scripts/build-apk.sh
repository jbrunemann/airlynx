#!/usr/bin/env bash
# Builds and signs target/release/apk/AirLynx.apk (arm64) and AirLynx-armv7.apk. Pass --install to install via adb.
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

# cargo-apk can neither ship Java code nor declare a <service> or <provider>, and the receive service and
# the received-files provider (app/java) need all of that, and so does the Android TV banner. So: compile it to classes.dex, patch the generated manifest, and
# reassemble the APK from the pieces cargo-apk left behind.
BUILD_TOOLS="$(ls -d "$ANDROID_HOME"/build-tools/* | sort -V | tail -1)"
ANDROID_JAR="$ANDROID_HOME/platforms/android-34/android.jar"
OUT=target/release/apk
WORK="$OUT/java-build"
rm -rf "$WORK"
mkdir -p "$WORK/classes" "$WORK/dex"
javac --release 11 -Xlint:-options -cp "$ANDROID_JAR" -d "$WORK/classes" app/java/dev/airlynx/app/*.java
"$BUILD_TOOLS/d8" --min-api 30 --lib "$ANDROID_JAR" --output "$WORK/dex" $(find "$WORK/classes" -name '*.class')

sed -e 's|android:hasCode="false"|android:hasCode="true"|' \
    -e 's|<application |<application android:banner="@drawable/banner" |' \
    -e 's|</application>|<service android:name="dev.airlynx.app.ReceiveService" android:exported="false" android:foregroundServiceType="dataSync"/><provider android:name="dev.airlynx.app.ReceivedFilesProvider" android:authorities="dev.airlynx.app.files" android:exported="false" android:grantUriPermissions="true"/></application>|' \
    "$OUT/AndroidManifest.xml" > "$WORK/AndroidManifest.xml"
grep -q 'android:banner' "$WORK/AndroidManifest.xml" || { echo "manifest patch failed" >&2; exit 1; }
grep -q ReceiveService "$WORK/AndroidManifest.xml" || { echo "manifest patch failed" >&2; exit 1; }
grep -q ReceivedFilesProvider "$WORK/AndroidManifest.xml" || { echo "manifest patch failed" >&2; exit 1; }

"$BUILD_TOOLS/aapt" package -f -F "$WORK/unaligned.apk" -M "$WORK/AndroidManifest.xml" -S app/res -I "$ANDROID_JAR"
cp "$WORK/dex/classes.dex" "$WORK/classes.dex"
(cd "$WORK" && "$BUILD_TOOLS/aapt" add unaligned.apk classes.dex >/dev/null)
# One APK per ABI, from the same pieces: AirLynx.apk (arm64, phones and most TVs) and AirLynx-armv7.apk (TVs with a 32-bit userland).
for pair in "AirLynx.apk:arm64-v8a" "AirLynx-armv7.apk:armeabi-v7a"; do
    apk="${pair%%:*}"
    abi="${pair##*:}"
    cp "$WORK/unaligned.apk" "$WORK/$abi.apk"
    (cd "$OUT" && "$BUILD_TOOLS/aapt" add "java-build/$abi.apk" "lib/$abi/libairlynx_app.so" >/dev/null)
    rm -f "$OUT/$apk" "$OUT/$apk.idsig"
    "$BUILD_TOOLS/zipalign" -f 4 "$WORK/$abi.apk" "$OUT/$apk"
    "$BUILD_TOOLS/apksigner" sign --ks android-debug.keystore --ks-pass pass:android "$OUT/$apk"
done
rm -rf "$WORK"
echo "APK: $(pwd)/target/release/apk/AirLynx.apk (arm64)"
echo "APK: $(pwd)/target/release/apk/AirLynx-armv7.apk (armv7)"

if [[ "${1:-}" == "--install" ]]; then
    "$ANDROID_HOME/platform-tools/adb" install -r target/release/apk/AirLynx.apk
fi
