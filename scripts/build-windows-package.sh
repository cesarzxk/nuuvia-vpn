#!/usr/bin/env bash
# Builds the portable Windows zip and the NSIS installer.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
: "${VERSION:=1.0.0}"
TARGET="x86_64-pc-windows-gnu"
OUT="$ROOT/dist/windows"
DRIVER_SRC="${DRIVER_SRC:-/tmp/nuvia-build/driver/amd64}"

if ! command -v makensis >/dev/null 2>&1; then
    echo "makensis (NSIS) is required: sudo apt install nsis" >&2
    exit 1
fi

echo "==> building Windows binaries"
cargo build --release -p nuvia-gui -p nuvia-cli -p nuvia-probe --target "$TARGET"

rm -rf "$OUT/driver"
mkdir -p "$OUT" "$OUT/driver"

if [ ! -d "$DRIVER_SRC" ]; then
    echo "==> fetching tap-windows6 9.27.0"
    FETCH=/tmp/nuvia-build/tap-windows6
    mkdir -p "$FETCH"
    if [ ! -f "$FETCH/dist.win10.zip" ]; then
        curl -L --fail -o "$FETCH/dist.win10.zip" \
            https://github.com/OpenVPN/tap-windows6/releases/download/9.27.0/dist.win10.zip
    fi
    rm -rf "$FETCH/dist.win10"
    (cd "$FETCH" && unzip -q -o dist.win10.zip 'dist.win10/amd64/*')
    DRIVER_SRC="$FETCH/dist.win10/amd64"
    curl -L --fail -o "$DRIVER_SRC/COPYING" \
        https://raw.githubusercontent.com/OpenVPN/tap-windows6/master/COPYING 2>/dev/null || true
fi

install -m 0755 "target/$TARGET/release/nuvia-gui.exe" "$OUT/nuvia-gui.exe"
install -m 0755 "target/$TARGET/release/nuvia.exe" "$OUT/nuvia.exe"
install -m 0755 "target/$TARGET/release/lanprobe.exe" "$OUT/lanprobe.exe"
install -m 0644 deploy/windows/README.md "$OUT/README.md"
install -m 0644 deploy/windows/install-tap.ps1 "$OUT/install-tap.ps1"
install -m 0644 deploy/windows/reset-tap.ps1 "$OUT/reset-tap.ps1"

if [ -d "$DRIVER_SRC" ]; then
    cp -f "$DRIVER_SRC"/* "$OUT/driver/"
else
    echo "warning: driver not found at $DRIVER_SRC; portable zip will download it" >&2
fi

echo "==> portable zip"
rm -f "$ROOT/dist/nuvia-windows-x86_64.zip"
(cd "$OUT" && zip -q -r "$ROOT/dist/nuvia-windows-x86_64.zip" .)

echo "==> NSIS installer"
ICON="$(mktemp /tmp/nuvia-icon-XXXXXX.ico)"
cp deploy/windows/nuvia-nsis.ico "$ICON"
makensis -DVERSION="$VERSION" -DROOT="$ROOT" -DICON="$ICON" \
    deploy/windows/nuvia.nsi
rm -f "$ICON"

echo "==> $ROOT/dist/nuvia-setup-x86_64.exe"
echo "==> $ROOT/dist/nuvia-windows-x86_64.zip"
