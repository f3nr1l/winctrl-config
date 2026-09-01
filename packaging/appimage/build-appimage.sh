#!/usr/bin/env bash
# Builds a WinCtrl Config AppImage.
#
# Requirements (downloaded automatically into ./tools if absent):
#   - linuxdeploy (x86_64 AppImage)
#   - linuxdeploy-plugin-gtk
# Also needs: a Rust toolchain (cargo), gtk4/libadwaita development files,
# msgfmt (gettext), and FUSE to run the AppImages (or set APPIMAGE_EXTRACT_AND_RUN=1).
set -euo pipefail

APPID="io.github.f3nr1l.WinctrlConfig"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # packaging/appimage/
REPO="$(cd "$HERE/../.." && pwd)"
TOOLS="$HERE/tools"
BUILD="$HERE/build"
APPDIR="$BUILD/AppDir"

mkdir -p "$TOOLS" "$BUILD"

# 1. Fetch linuxdeploy + the GTK plugin if needed.
fetch() { # url dest
    [ -x "$2" ] || { echo "Downloading $(basename "$2")…"; curl -fSL "$1" -o "$2"; chmod +x "$2"; }
}
fetch "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage" \
      "$TOOLS/linuxdeploy"
fetch "https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/master/linuxdeploy-plugin-gtk.sh" \
      "$TOOLS/linuxdeploy-plugin-gtk.sh"

# 2. Build the release binaries.
(cd "$REPO/rust" && cargo build --release --bin winctrl-gui --bin wwctrl)

# 3. Assemble the AppDir.
rm -rf "$APPDIR"
install -Dm755 "$REPO/rust/target/release/winctrl-gui" "$APPDIR/usr/bin/winctrl-gui"
install -Dm755 "$REPO/rust/target/release/wwctrl"      "$APPDIR/usr/bin/wwctrl"
mkdir -p "$APPDIR/usr/share/applications"
sed 's|@BINARY@|winctrl-gui|g' "$REPO/packaging/$APPID.desktop" \
    > "$APPDIR/usr/share/applications/$APPID.desktop"
install -Dm644 "$REPO/packaging/$APPID.metainfo.xml" \
    "$APPDIR/usr/share/metainfo/$APPID.metainfo.xml"
for s in 512 256 128 64 48 32; do
    install -Dm644 "$REPO/packaging/icons/hicolor/${s}x${s}/apps/$APPID.png" \
        "$APPDIR/usr/share/icons/hicolor/${s}x${s}/apps/$APPID.png"
done

# 4. Translations.
while read -r lang; do
    [ -n "$lang" ] || continue
    modir="$APPDIR/usr/share/locale/$lang/LC_MESSAGES"
    mkdir -p "$modir"
    msgfmt "$REPO/rust/po/$lang.po" -o "$modir/winctrl.mo"
done < "$REPO/rust/po/LINGUAS"

# 5. AppRun hook: point gettext at the bundled catalogs.
mkdir -p "$APPDIR/apprun-hooks"
cat > "$APPDIR/apprun-hooks/winctrl-locale.sh" <<'EOF'
export WINCTRL_LOCALEDIR="${APPDIR}/usr/share/locale"
EOF

# 6. Bundle GTK + dependencies and produce the AppImage.
export DEPLOY_GTK_VERSION=4
cd "$BUILD"
"$TOOLS/linuxdeploy" \
    --appdir "$APPDIR" \
    --plugin gtk \
    --desktop-file "$APPDIR/usr/share/applications/$APPID.desktop" \
    --icon-file "$REPO/packaging/icons/hicolor/256x256/apps/$APPID.png" \
    --output appimage

echo "AppImage built in $BUILD"
