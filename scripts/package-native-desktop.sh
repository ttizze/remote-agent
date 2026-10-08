#!/usr/bin/env bash
# Package the native desktop executable produced for Linux or Windows.
set -euo pipefail

[[ $# -ge 3 ]] || {
    echo "usage: package-native-desktop.sh platform arch output-dir [target-dir]" >&2
    exit 2
}
platform=$1
arch=$2
output_dir=$3
target_dir=${4:-target}
case "$platform" in
    linux|windows) ;;
    *) echo "native desktop packaging supports Linux and Windows" >&2; exit 2 ;;
esac
suffix=
[[ $platform == windows ]] && suffix=.exe
source="$target_dir/release/bex-desktop$suffix"
[[ -f $source ]] || { echo "missing desktop executable: $source" >&2; exit 1; }

stage=$(mktemp -d)
cleanup() { rm -rf "$stage"; }
trap cleanup EXIT
cp "$source" "$stage/desktop$suffix"
printf '%s\n' "$platform" > "$stage/platform"
printf '%s\n' "$arch" > "$stage/architecture"
mkdir -p "$output_dir"
archive="$output_dir/desktop-$platform-$arch.tar.gz"
if tar --sort=name --mtime='UTC 1970-01-01' --owner=0 --group=0 --numeric-owner \
    -cf /dev/null -C "$stage" . >/dev/null 2>&1; then
    tar --sort=name --mtime='UTC 1970-01-01' --owner=0 --group=0 --numeric-owner \
        -czf "$archive" -C "$stage" .
else
    tar -czf "$archive" -C "$stage" .
fi
if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$archive" > "$archive.sha256"
else
    shasum -a 256 "$archive" > "$archive.sha256"
fi
printf '%s\n' "$archive"
