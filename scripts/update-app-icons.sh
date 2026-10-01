#!/usr/bin/env bash
# Desktop PNG is the source for platform icon assets. Requires macOS sips/iconutil.
set -euo pipefail
cd "$(dirname "$0")/.."
source=apps/desktop/assets/icon.png
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT
iconset="$temporary/Bex.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$source" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    doubled=$((size * 2))
    sips -z "$doubled" "$doubled" "$source" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o apps/desktop/assets/icon.icns
sips -z 1024 1024 "$source" --out apps/mobile/iosApp/Bex/Assets.xcassets/AppIcon.appiconset/AppIcon.png >/dev/null
sips -z 192 192 "$source" --out apps/mobile/src/main/res/drawable/bex_icon.png >/dev/null
