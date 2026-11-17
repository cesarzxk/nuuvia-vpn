#!/usr/bin/env bash
# Builds the Linux .deb and .AppImage packages.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
: "${VERSION:=1.2.0}"
ARCH="amd64"
OUT="$ROOT/dist"
WORK="$OUT/linux"
APPIMAGE_TOOL="${APPIMAGE_TOOL:-/tmp/nuvia-build/appimagetool-x86_64.AppImage}"

echo "==> building release binaries"
cargo build --release -p nuvia-gui -p nuvia-cli -p nuvia-probe

rm -rf "$WORK"
mkdir -p "$WORK" "$OUT"

# ---------------------------------------------------------------------------
# .deb
# ---------------------------------------------------------------------------
DEB="$WORK/deb/nuvia_${VERSION}_${ARCH}"
mkdir -p \
    "$DEB/DEBIAN" \
    "$DEB/usr/bin" \
    "$DEB/usr/share/applications" \
    "$DEB/usr/share/icons/hicolor/256x256/apps" \
    "$DEB/usr/share/icons/hicolor/128x128/apps" \
    "$DEB/usr/share/icons/hicolor/64x64/apps" \
    "$DEB/usr/share/doc/nuvia"

install -m 0755 target/release/nuvia-gui "$DEB/usr/bin/nuvia-gui"
install -m 0755 target/release/nuvia "$DEB/usr/bin/nuvia"
install -m 0755 target/release/lanprobe "$DEB/usr/bin/lanprobe"
install -m 0755 deploy/linux/nuvia-gui-launch "$DEB/usr/bin/nuvia-gui-launch"
install -m 0644 deploy/linux/nuvia.desktop "$DEB/usr/share/applications/nuvia.desktop"
install -m 0644 deploy/linux/nuvia-256.png "$DEB/usr/share/icons/hicolor/256x256/apps/nuvia.png"
install -m 0644 deploy/linux/nuvia-128.png "$DEB/usr/share/icons/hicolor/128x128/apps/nuvia.png"
install -m 0644 deploy/linux/nuvia-64.png "$DEB/usr/share/icons/hicolor/64x64/apps/nuvia.png"
install -m 0644 README.md "$DEB/usr/share/doc/nuvia/README.md"
install -m 0644 README.pt-BR.md "$DEB/usr/share/doc/nuvia/README.pt-BR.md"

SIZE="$(du -sk "$DEB/usr" | cut -f1)"
cat > "$DEB/DEBIAN/control" <<EOF
Package: nuvia
Version: $VERSION
Section: net
Priority: optional
Architecture: $ARCH
Installed-Size: $SIZE
Depends: libc6 (>= 2.31), libx11-6, libxkbcommon0, libgl1, libegl1, policykit-1 | pkexec
Recommends: libwayland-client0
Maintainer: nuvia <nuvia@localhost>
Homepage: https://github.com/
Description: LAN virtual criptografada para jogos
 O Nuuvia cria uma Ethernet virtual Layer 2 sobre UDP criptografado,
 permitindo descoberta de LAN em jogos (Minecraft, etc.) entre Linux e
 Windows. A GUI usa TAP, rotas de descoberta e relay na internet.
EOF

cat > "$DEB/usr/share/doc/nuvia/copyright" <<'EOF'
nuvia is distributed under the MIT license.
The bundled Windows driver (not part of this package) is GPLv2.
EOF

dpkg-deb --root-owner-group --build "$DEB" "$WORK/nuvia_${VERSION}_${ARCH}.deb"
mv -f "$WORK/nuvia_${VERSION}_${ARCH}.deb" "$OUT/nuvia_${VERSION}_${ARCH}.deb"
echo "==> $OUT/nuvia_${VERSION}_${ARCH}.deb"

# ---------------------------------------------------------------------------
# AppImage
# ---------------------------------------------------------------------------
APPDIR="$WORK/AppDir"
mkdir -p \
    "$APPDIR/usr/bin" \
    "$APPDIR/usr/share/applications" \
    "$APPDIR/usr/share/icons/hicolor/256x256/apps" \
    "$APPDIR/usr/share/icons/hicolor/128x128/apps" \
    "$APPDIR/usr/share/icons/hicolor/64x64/apps"

install -m 0755 target/release/nuvia-gui "$APPDIR/usr/bin/nuvia-gui"
install -m 0755 target/release/nuvia "$APPDIR/usr/bin/nuvia"
install -m 0755 target/release/lanprobe "$APPDIR/usr/bin/lanprobe"
install -m 0644 deploy/linux/nuvia-256.png "$APPDIR/usr/share/icons/hicolor/256x256/apps/nuvia.png"
install -m 0644 deploy/linux/nuvia-128.png "$APPDIR/usr/share/icons/hicolor/128x128/apps/nuvia.png"
install -m 0644 deploy/linux/nuvia-64.png "$APPDIR/usr/share/icons/hicolor/64x64/apps/nuvia.png"

cat > "$APPDIR/nuvia.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=nuvia VPN
GenericName=Virtual LAN
Comment=LAN virtual criptografada para jogos
Exec=nuvia-gui
Icon=nuvia
Terminal=false
Categories=Network;Utility;
StartupWMClass=nuvia
EOF
install -m 0644 "$APPDIR/nuvia.desktop" "$APPDIR/usr/share/applications/nuvia.desktop"
install -m 0644 deploy/linux/nuvia-256.png "$APPDIR/nuvia.png"
ln -sf nuvia.png "$APPDIR/.DirIcon"

cat > "$APPDIR/AppRun" <<'EOF'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
BIN="$HERE/usr/bin/nuvia-gui"
if [ "$(id -u)" -eq 0 ]; then
    exec "$BIN" "$@"
fi
if command -v pkexec >/dev/null 2>&1; then
    # Root cannot read a user-mounted AppImage (FUSE allow_other is off), so
    # copy the standalone binary to a root-accessible temp path first.
    TMP="$(mktemp "${XDG_RUNTIME_DIR:-/tmp}/nuvia-gui.XXXXXX")" || exec "$BIN" "$@"
    cp "$BIN" "$TMP" || { rm -f "$TMP"; exec "$BIN" "$@"; }
    chmod 0755 "$TMP"
    if [ -n "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then
        pkexec env \
            DISPLAY="${DISPLAY:-:0}" \
            XAUTHORITY="${XAUTHORITY:-$HOME/.Xauthority}" \
            XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-}" \
            DBUS_SESSION_BUS_ADDRESS="$DBUS_SESSION_BUS_ADDRESS" \
            "$TMP" "$@"
    else
        pkexec env \
            DISPLAY="${DISPLAY:-:0}" \
            XAUTHORITY="${XAUTHORITY:-$HOME/.Xauthority}" \
            XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-}" \
            "$TMP" "$@"
    fi
    STATUS=$?
    rm -f "$TMP"
    exit $STATUS
fi
exec "$BIN" "$@"
EOF
chmod 0755 "$APPDIR/AppRun"

if [ ! -x "$APPIMAGE_TOOL" ]; then
    mkdir -p "$(dirname "$APPIMAGE_TOOL")"
    echo "==> downloading appimagetool"
    curl -L --fail -o "$APPIMAGE_TOOL" \
        https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
    chmod +x "$APPIMAGE_TOOL"
fi

echo "==> building AppImage"
# Build next to the final path and rename atomically: the previous AppImage may
# still be mounted/running while the user tests it.
APPIMAGE_TMP="$(mktemp "$OUT/nuvia-${VERSION}-x86_64.XXXXXX.AppImage")"
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$APPIMAGE_TOOL" \
    --no-appstream "$APPDIR" "$APPIMAGE_TMP"
mv -f "$APPIMAGE_TMP" "$OUT/nuvia-${VERSION}-x86_64.AppImage"
echo "==> $OUT/nuvia-${VERSION}-x86_64.AppImage"
