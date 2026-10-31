#!/bin/sh
# Installs AirLynx for the current user in Termux: the airlynx program, plus the folder that received files go to.
# Shipped inside the Termux tarball (see scripts/build-termux.sh). Run "./install.sh --uninstall" to remove it again.
set -eu
here=$(cd "$(dirname "$0")" && pwd)

if [ -t 1 ]; then
    bold=$(printf '\033[1m') dim=$(printf '\033[2m') green=$(printf '\033[32m') red=$(printf '\033[31m') off=$(printf '\033[0m')
else
    bold= dim= green= red= off=
fi
say() { printf '%s\n' "$*"; }
fail() { printf '%serror:%s %s\n' "$red" "$off" "$*" >&2; exit 1; }

# Termux sets PREFIX to its own /usr; elsewhere (a plain Android shell, say) fall back to ~/.local.
if [ -n "${PREFIX:-}" ]; then
    bin=$PREFIX/bin
else
    bin=$HOME/.local/bin
fi

if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$bin/airlynx"
    say "Removed $bin/airlynx. Your device id and favorites are kept in ~/.config/AirLynx."
    exit 0
fi

# A binary built for the other CPU width fails with a vague "not found" or "exec format error", so say what is wrong.
if ! version=$("$here/airlynx" --version 2>&1); then
    say "$version" >&2
    fail "this build does not run on this device (CPU: $(uname -m)). Use the airlynx-termux-aarch64 bundle on 64-bit Android and airlynx-termux-armv7 on 32-bit."
fi

mkdir -p "$bin"
install -m755 "$here/airlynx" "$bin/airlynx"

say "${green}${bold}Installed${off} $version in $bin"
case ":$PATH:" in
    *":$bin:"*) ;;
    *) say "${dim}Note: $bin is not on your PATH. Add it with: export PATH=\"$bin:\$PATH\"${off}" ;;
esac

# Termux can only reach shared storage after termux-setup-storage, which links it under ~/storage.
if [ -d "$HOME/storage/downloads" ]; then
    saves=$HOME/storage/downloads/AirLynx
    mkdir -p "$saves"
    say "Received files will go to ${bold}$saves${off} (the Downloads folder of this phone)."
else
    saves=\$HOME/storage/downloads/AirLynx
    say "${dim}Shared storage is not set up yet. Run termux-setup-storage once, then received files can go to your Downloads folder.${off}"
fi

cat <<TXT

${bold}Get started${off}
  airlynx discover                 find AirLynx devices on this network
  airlynx favorite add <name>      add one as a favorite (add this device on the other side too)
  airlynx send photo.jpg -d <name> send a file
  airlynx receive --name "My phone" -d $saves

Termux is stopped by Android when it sleeps. To keep receiving with the screen off, run
  termux-wake-lock
first (and allow Termux to ignore battery optimization in the Android settings).
TXT
