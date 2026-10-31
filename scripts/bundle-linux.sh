#!/bin/sh
# Builds target/linux/airlynx-linux-<arch>.tar.gz (honors CARGO_TARGET_DIR): the desktop app, the command line tool, a desktop entry, the icons,
# an install script and the font license. Unpack it and run ./install.sh to get AirLynx in the menu with its icon.
# Usage: scripts/bundle-linux.sh [rust-target]   (a target such as aarch64-unknown-linux-gnu cross-compiles; it needs
# the matching linker, see .github/workflows/linux.yml)
set -eu
cd "$(dirname "$0")/.."

tdir=${CARGO_TARGET_DIR:-target}
target=${1:-}
if [ -n "$target" ]; then
    cargo build --release --target "$target" -p airlynx -p airlynx-app
    out=$tdir/$target/release
    arch=${target%%-*}
else
    cargo build --release -p airlynx -p airlynx-app
    out=$tdir/release
    arch=$(uname -m)
fi

dir=$tdir/linux/airlynx-linux-$arch
rm -rf "$dir"
mkdir -p "$dir"
cp "$out/airlynx-app" "$out/airlynx" "$dir/"
cp assets/airlynx.desktop "$dir/airlynx.desktop"
cp assets/fonts/Inter-LICENSE.txt "$dir/LICENSE-inter.txt"
# The icon in the standard hicolor sizes (made by scripts/make-icons.py): desktops do not look in a lone 512 px png,
# and with no size they can use they show the generic executable icon.
mkdir -p "$dir/icons"
cp -R assets/hicolor "$dir/icons/hicolor"
cp scripts/linux-install.sh "$dir/install.sh"
cat > "$dir/README.txt" <<TXT
AirLynx for Linux ($arch)

Run ./airlynx-app to try the desktop app. To install it for your user, with the
icon in the panel and the application menu (Internet > AirLynx):

  ./install.sh

The desktop matches a window to its icon through airlynx.desktop, so the icon
only shows when that file and the icons are installed. Log out and in if the
panel still shows a generic icon. Remove it again with ./install.sh --uninstall.

./airlynx is the command line tool (airlynx --help). It uses the same device id
and favorites as the desktop app.

The desktop app needs an X11 or Wayland desktop with OpenGL and the libxkbcommon
library (Debian and Ubuntu: sudo apt install libxkbcommon0 libgl1). Devices are
found over the local network, so allow UDP 47801 and TCP 47800 in a firewall.

The Inter font is built into the program (license: LICENSE-inter.txt).
TXT

tar -C "$tdir/linux" -czf "$dir.tar.gz" "airlynx-linux-$arch"
echo "$dir.tar.gz"
