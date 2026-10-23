#!/bin/sh
# Build Gantry-<version>-x86_64.AppImage from a built binary.
#   cargo build --release && packaging/appimage/build.sh [path/to/rig] [output folder]
# The AppImage carries only Gantry itself: it still uses the system's curl, bsdtar/7z and a Chromium based browser.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
bin=${1:-$root/target/release/rig}
out=${2:-$PWD}
ver=$("$bin" --version | awk '{print $2}')
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
app="$work/AppDir"
mkdir -p "$app/usr/bin" "$app/usr/share/applications" "$app/usr/share/icons/hicolor/256x256/apps"
cp "$bin" "$app/usr/bin/rig"
cp "$root/src/icon.png" "$app/gantry.png"
cp "$root/src/icon.png" "$app/usr/share/icons/hicolor/256x256/apps/gantry.png"
sed 's|^Exec=.*|Exec=rig|' "$root/packaging/gantry.desktop" > "$app/gantry.desktop"
cp "$app/gantry.desktop" "$app/usr/share/applications/gantry.desktop"
printf '#!/bin/sh\nHERE=$(dirname "$(readlink -f "$0")")\nexec "$HERE/usr/bin/rig" "$@"\n' > "$app/AppRun"
chmod +x "$app/AppRun"
tool=$(command -v appimagetool || true)
if [ -z "$tool" ]; then
  tool="$work/appimagetool"
  curl -fsSL -o "$tool" https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
  chmod +x "$tool"
fi
mkdir -p "$out"
ARCH=x86_64 "$tool" --appimage-extract-and-run "$app" "$out/Gantry-$ver-x86_64.AppImage"
echo "built $out/Gantry-$ver-x86_64.AppImage"
