#!/usr/bin/env bash
# Installe WinCtrl Config comme application de bureau pour l'utilisateur courant :
# binaire release, icônes hicolor, fichier .desktop (Exec en chemin absolu),
# catalogues de traduction, puis rafraîchit les caches. Idempotent. Aucun sudo
# (installe dans ~/.local).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" # packaging/
REPO="$(cd "$HERE/.." && pwd)"                        # racine du dépôt
APPID="io.github.f3nr1l.WinctrlConfig"
BIN="$REPO/rust/target/release/winctrl-gui"

# 1. Binaire release (compilé si absent).
if [ ! -x "$BIN" ]; then
    echo "Compilation release (cargo build --release)…"
    (cd "$REPO/rust" && cargo build --release --bin winctrl-gui)
fi

# 2. Icônes hicolor.
for size in 512 256 128 64 48 32; do
    dest="$HOME/.local/share/icons/hicolor/${size}x${size}/apps"
    mkdir -p "$dest"
    cp -f "$HERE/icons/hicolor/${size}x${size}/apps/$APPID.png" "$dest/$APPID.png"
done

# 3. Fichier .desktop (Exec = chemin absolu du binaire).
appdir="$HOME/.local/share/applications"
mkdir -p "$appdir"
sed "s|@BINARY@|$BIN|g" "$HERE/$APPID.desktop" > "$appdir/$APPID.desktop"

# 4. Catalogues de traduction (.po -> .mo), domaine gettext « winctrl ».
if command -v msgfmt >/dev/null 2>&1; then
    while read -r lang; do
        [ -n "$lang" ] || continue
        po="$REPO/rust/po/$lang.po"
        [ -f "$po" ] || continue
        modir="$HOME/.local/share/locale/$lang/LC_MESSAGES"
        mkdir -p "$modir"
        msgfmt -o "$modir/winctrl.mo" "$po"
    done < "$REPO/rust/po/LINGUAS"
else
    echo "  (msgfmt absent : traductions non installées ; l'anglais reste disponible)"
fi

# 5. Rafraîchit les caches (best-effort).
gtk-update-icon-cache -f -t "$HOME/.local/share/icons/hicolor" >/dev/null 2>&1 || true
update-desktop-database "$appdir" >/dev/null 2>&1 || true

echo "Installé."
echo "  Binaire : $BIN"
echo "  Lanceur : $appdir/$APPID.desktop"
echo "  « WinCtrl Config » doit apparaître dans la grille d'applications."
