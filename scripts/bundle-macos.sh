#!/usr/bin/env bash
# Builds target/release/AirLynx.app (ad-hoc signed) from the egui app.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release -p airlynx-app
VERSION="$(cargo pkgid -p airlynx-app | sed 's/.*[#@]//')"
APP=target/release/AirLynx.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp assets/icon/AirLynx.icns "$APP/Contents/Resources/AirLynx.icns"
cp target/release/airlynx-app "$APP/Contents/MacOS/AirLynx"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>AirLynx</string>
    <key>CFBundleDisplayName</key><string>AirLynx</string>
    <key>CFBundleIdentifier</key><string>dev.airlynx.app</string>
    <key>CFBundleExecutable</key><string>AirLynx</string>
    <key>CFBundleIconFile</key><string>AirLynx</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleVersion</key><string>${VERSION}</string>
    <key>CFBundleShortVersionString</key><string>${VERSION}</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSLocalNetworkUsageDescription</key>
    <string>AirLynx finds and exchanges files with devices on your local network.</string>
</dict>
</plist>
PLIST

codesign --force --sign - "$APP"
echo "App: $(pwd)/$APP"
