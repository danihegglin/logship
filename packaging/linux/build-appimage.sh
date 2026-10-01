#!/usr/bin/env bash
# Builds an AppImage from an already-built release binary.
# Usage: packaging/linux/build-appimage.sh <binary> <version> <out-dir>
set -euo pipefail

bin=$1
version=$2
out=$3
arch=$(uname -m)
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
appdir="$work/logship.AppDir"

mkdir -p "$appdir/usr/bin" "$appdir/usr/lib" \
  "$appdir/usr/share/applications" "$appdir/usr/share/icons/hicolor/scalable/apps"
install -m755 "$bin" "$appdir/usr/bin/logship"
install -m644 "$root/packaging/linux/logship.desktop" "$appdir/usr/share/applications/"
install -m644 "$root/packaging/linux/logship.svg" "$appdir/usr/share/icons/hicolor/scalable/apps/"
cp "$root/packaging/linux/logship.desktop" "$root/packaging/linux/logship.svg" "$appdir/"
ln -s logship.svg "$appdir/.DirIcon"

# Bundle the xkbcommon libraries, which are not installed everywhere.
# X11, Wayland, Vulkan and fontconfig come from the host system.
ldd "$bin" | awk '/libxkbcommon|libxcb-xkb/ { print $3 }' | while read -r lib; do
  cp -L "$lib" "$appdir/usr/lib/"
done
for lib in "$appdir"/usr/lib/*.so*; do
  patchelf --set-rpath '$ORIGIN' "$lib"
done
patchelf --set-rpath '$ORIGIN/../lib' "$appdir/usr/bin/logship"

cat > "$appdir/AppRun" <<'APPRUN'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
exec "$HERE/usr/bin/logship" "$@"
APPRUN
chmod +x "$appdir/AppRun"

tool="$work/appimagetool"
curl -fsSL -o "$tool" \
  "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$arch.AppImage"
chmod +x "$tool"

mkdir -p "$out"
APPIMAGE_EXTRACT_AND_RUN=1 ARCH="$arch" "$tool" --no-appstream "$appdir" \
  "$out/logship-$version-$arch.AppImage"
rm -rf "$work"
