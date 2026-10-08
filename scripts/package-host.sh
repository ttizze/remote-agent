#!/usr/bin/env bash
# Package the already-built Host binaries for one target platform.
set -euo pipefail

[[ $# -ge 3 ]] || {
    echo "usage: package-host.sh platform arch output-dir [target-dir]" >&2
    exit 2
}
platform=$1
arch=$2
output_dir=$3
target_dir=${4:-target}
case "$platform" in
    linux|windows|macos) ;;
    *) echo "unsupported Host platform: $platform" >&2; exit 2 ;;
esac
[[ $arch =~ ^[A-Za-z0-9._-]+$ ]] || { echo "invalid architecture: $arch" >&2; exit 2; }

suffix=
[[ $platform == windows ]] && suffix=.exe
stage=$(mktemp -d)
cleanup() { rm -rf "$stage"; }
trap cleanup EXIT
for binary in host-daemon bex-provider-supervisor agent-cli; do
    source="$target_dir/release/$binary$suffix"
    [[ -f $source ]] || { echo "missing built Host binary: $source" >&2; exit 1; }
    cp "$source" "$stage/$binary$suffix"
done
for resource in bex-claude-sdk.mjs Claude-Agent-SDK-LICENSE.md; do
    source="$target_dir/release/$resource"
    [[ -f $source ]] || { echo "missing Host runtime resource: $source" >&2; exit 1; }
    cp "$source" "$stage/$resource"
done
printf '%s\n' "$platform" > "$stage/platform"
printf '%s\n' "$arch" > "$stage/architecture"

mkdir -p "$output_dir"
archive="$output_dir/host-$platform-$arch.tar.gz"
if tar --sort=name --mtime='UTC 1970-01-01' --owner=0 --group=0 --numeric-owner \
    -cf /dev/null -C "$stage" . >/dev/null 2>&1; then
    tar \
        --sort=name \
        --mtime='UTC 1970-01-01' \
        --owner=0 --group=0 --numeric-owner \
        -czf "$archive" -C "$stage" .
else
    # The pinned Nix shell supplies GNU tar on release runners. Keep a
    # portable fallback for local macOS validation where BSD tar is default.
    tar -czf "$archive" -C "$stage" .
fi
if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$archive" > "$archive.sha256"
else
    shasum -a 256 "$archive" > "$archive.sha256"
fi
printf '%s\n' "$archive"
