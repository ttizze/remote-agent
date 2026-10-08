#!/usr/bin/env bash
# Package the signed desktop bundle produced by build-macos.sh.
set -euo pipefail

[[ $# -ge 2 ]] || {
    echo "usage: package-macos-desktop.sh version output-dir [app-path]" >&2
    exit 2
}
version=$1
output_dir=$2
app_path=${3:-target/Bex.app}
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]] || {
    echo "invalid release version: $version" >&2
    exit 1
}
[[ -d $app_path ]] || { echo "desktop bundle does not exist: $app_path" >&2; exit 1; }
[[ $(uname -s) == Darwin ]] || { echo 'macOS desktop packaging requires macOS' >&2; exit 2; }
mkdir -p "$output_dir"

zip_path="$output_dir/desktop-macos-arm64.zip"
dmg_path="$output_dir/desktop-macos-arm64.dmg"
ditto -c -k --sequesterRsrc --keepParent "$app_path" "$zip_path"
hdiutil create -quiet -volname 'Desktop' -srcfolder "$app_path" -ov -format UDZO "$dmg_path"
if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$zip_path" > "$zip_path.sha256"
    shasum -a 256 "$dmg_path" > "$dmg_path.sha256"
else
    sha256sum "$zip_path" > "$zip_path.sha256"
    sha256sum "$dmg_path" > "$dmg_path.sha256"
fi
printf '%s\n' "$zip_path" "$dmg_path"
