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

valid_version() {
    local value=$1
    local pattern='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-([0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*))?$'
    [[ $value =~ $pattern ]] || return 1
    local prerelease=${BASH_REMATCH[5]:-}
    local part
    local -a parts
    IFS='.' read -r -a parts <<< "$prerelease"
    for part in "${parts[@]}"; do
        [[ $part =~ ^0[0-9]+$ ]] || continue
        return 1
    done
}

valid_version "$version" || {
    echo "invalid release version: $version" >&2
    exit 1
}
[[ -d $app_path ]] || { echo "desktop bundle does not exist: $app_path" >&2; exit 1; }
[[ $(uname -s) == Darwin ]] || { echo 'macOS desktop packaging requires macOS' >&2; exit 2; }
[[ -x $app_path/Contents/MacOS/ffmpeg ]] || {
    echo 'macOS desktop bundle is missing its sibling FFmpeg executable' >&2
    exit 1
}
[[ -f $app_path/Contents/MacOS/FFMPEG-RUNTIME.txt ]] || {
    echo 'macOS desktop bundle is missing its FFmpeg runtime manifest' >&2
    exit 1
}
grep -Fq 'encoder=mjpeg,libvpx-vp9' "$app_path/Contents/MacOS/FFMPEG-RUNTIME.txt" || {
    echo 'macOS desktop bundle FFmpeg does not declare the required encoders' >&2
    exit 1
}
grep -Fq 'decoder=h264,mjpeg' "$app_path/Contents/MacOS/FFMPEG-RUNTIME.txt" || {
    echo 'macOS desktop bundle FFmpeg does not declare the required decoders' >&2
    exit 1
}
grep -Fq 'filter=scale' "$app_path/Contents/MacOS/FFMPEG-RUNTIME.txt" || {
    echo 'macOS desktop bundle FFmpeg does not declare the scale filter' >&2
    exit 1
}
grep -Fq 'muxer=image2pipe,mpjpeg,matroska,webm,null' "$app_path/Contents/MacOS/FFMPEG-RUNTIME.txt" || {
    echo 'macOS desktop bundle FFmpeg does not declare the required muxers' >&2
    exit 1
}
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
