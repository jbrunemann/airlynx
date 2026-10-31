#!/bin/sh
# Installs AirLynx for the current user: the desktop app and the command line tool, the icons, and a menu entry, which
# is what gives the panel, the menu and the window list the AirLynx icon. Shipped inside the Linux tarball (see
# scripts/bundle-linux.sh). Run "./install.sh --uninstall" to remove it again.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
bin=$HOME/.local/bin
data=${XDG_DATA_HOME:-$HOME/.local/share}

if [ -t 1 ]; then
    bold=$(printf '\033[1m') dim=$(printf '\033[2m') green=$(printf '\033[32m') red=$(printf '\033[31m') off=$(printf '\033[0m')
else
    bold= dim= green= red= off=
fi
say() { printf '%s\n' "$*"; }
fail() { printf '%serror:%s %s\n' "$red" "$off" "$*" >&2; exit 1; }

refresh_caches() {
    update-desktop-database "$data/applications" 2>/dev/null || true
    gtk-update-icon-cache -f -t "$data/icons/hicolor" 2>/dev/null || true
}

if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$bin/airlynx-app" "$bin/airlynx" "$data/applications/airlynx.desktop"
    rm -f "$data"/icons/hicolor/*/apps/airlynx.png "$data/icons/hicolor/scalable/apps/airlynx.svg"
    refresh_caches
    say "Removed AirLynx. Your device id and favorites are kept in ${XDG_CONFIG_HOME:-~/.config}/AirLynx."
    exit 0
fi

# A binary built for the other CPU width fails with a vague "not found" or "exec format error", so say what is wrong.
if ! version=$("$here/airlynx" --version 2>&1); then
    say "$version" >&2
    fail "the airlynx program does not run here (CPU: $(uname -m)). Use the bundle that matches your CPU (x86_64 or aarch64)."
fi
# The desktop app loads libxkbcommon and OpenGL at run time, so ldd cannot tell whether they are there: ask the linker cache.
# Only a warning, since a cache that cannot be read does not mean the library is missing.
libs=$( (ldconfig -p || /sbin/ldconfig -p) 2>/dev/null || true)
need=
for lib in libxkbcommon.so.0 libGL.so.1; do
    [ -z "$libs" ] || printf '%s\n' "$libs" | grep -q "$lib" || need="$need $lib"
done

mkdir -p "$bin" "$data/applications"
install -m755 "$here/airlynx-app" "$bin/airlynx-app"
install -m755 "$here/airlynx" "$bin/airlynx"
for icon in "$here"/icons/hicolor/*/apps/airlynx.*; do
    dir=$(dirname "$icon")
    size=$(basename "$(dirname "$dir")")
    mkdir -p "$data/icons/hicolor/$size/apps"
    install -m644 "$icon" "$data/icons/hicolor/$size/apps/$(basename "$icon")"
done
# The menu entry names the program by its full path: a launcher does not always have ~/.local/bin on its PATH.
sed "s|^Exec=.*|Exec=$bin/airlynx-app|" "$here/airlynx.desktop" > "$here/.airlynx.desktop.tmp"
install -m644 "$here/.airlynx.desktop.tmp" "$data/applications/airlynx.desktop"
rm -f "$here/.airlynx.desktop.tmp"
refresh_caches

say "${green}${bold}Installed${off} AirLynx $version"
say "  desktop app   $bin/airlynx-app   (menu: Internet > AirLynx)"
say "  command line  $bin/airlynx"
case ":$PATH:" in
    *":$bin:"*) ;;
    *) say "${dim}Note: $bin is not on your PATH, so type the full path in a terminal, or add it with: export PATH=\"$bin:\$PATH\"${off}" ;;
esac
if [ -n "$need" ]; then
    say "${red}The desktop app will not start until these libraries are installed:${off}$need"
    say "  Debian, Ubuntu and Xfce spins: sudo apt install libxkbcommon0 libgl1"
fi
say "${dim}If the panel still shows a generic icon, close and reopen the window, or log out and in. Received files go to ~/Downloads/AirLynx.${off}"
