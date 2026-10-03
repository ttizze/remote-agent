#!/usr/bin/env bash
# Signing uses the installed Apple toolchain.
set -euo pipefail
cd "$(dirname "$0")/.."
[[ $(uname -s) == Darwin ]] || { echo 'Mac builds require macOS' >&2; exit 2; }
case "${1:-desktop}" in
    host|desktop) product=${1:-desktop} ;;
    *) echo "usage: $0 [host|desktop]" >&2; exit 2 ;;
esac
identity=${BEX_CODE_SIGN_IDENTITY:-}
if [[ -z $identity ]]; then
    identities=$(/usr/bin/security find-identity -v -p codesigning | awk '/"Apple Development:|"Developer ID Application:/ {print $2}')
    [[ -n $identities && $identities != *$'\n'* ]] || {
        echo 'Set BEX_CODE_SIGN_IDENTITY to one Apple Development or Developer ID Application certificate.' >&2
        exit 1
    }
    identity=$identities
fi
[[ $identity != - ]] || { echo 'Certificate signing is required for Mac builds.' >&2; exit 1; }
target=$(cargo metadata --no-deps --format-version 1 | jq -er .target_directory)
export BEX_BUILD_REVISION
BEX_BUILD_REVISION=$(git rev-parse HEAD)
if ! git diff --quiet HEAD --; then BEX_BUILD_REVISION+=-dirty; fi
sign() { /usr/bin/codesign --force --sign "$identity" --timestamp=none "$@"; }
verify() { /usr/bin/codesign --verify --deep --strict "$1"; }
if [[ $product == host ]]; then
    cargo build --locked --package host-daemon --package codex-app-server --package bex-process --release
    sign --identifier app.bex.provider-supervisor "$target/release/bex-provider-supervisor"
    verify "$target/release/bex-provider-supervisor"
    sign --identifier app.bex.host "$target/release/host-daemon"
    verify "$target/release/host-daemon"
    echo "$target/release/host-daemon"
    exit
fi
[[ $(uname -m) == arm64 ]] || { echo 'The GPUI Mac bundle requires Apple Silicon.' >&2; exit 2; }
cargo build --locked --package host-daemon --package codex-app-server --package bex-process --package bex-desktop --release
staging=$(mktemp -d "$target/.Bex-build.XXXXXX")
destination="$target/Bex.app"
cleanup() {
    result=$?
    if [[ -d $staging/previous.app && ! -e $destination ]]; then
        if ! mv "$staging/previous.app" "$destination"; then
            echo "Previous bundle retained in $staging/previous.app" >&2
            exit 1
        fi
    fi
    rm -rf "$staging"
    exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
bundle="$staging/Bex.app"
executables="$bundle/Contents/MacOS"
resources="$bundle/Contents/Resources"
mkdir -p "$executables" "$resources"
cp apps/desktop/assets/icon.icns "$resources/Bex.icns"
cp "$target/release/bex-desktop" "$executables/Bex"
cp "$target/release/host-daemon" "$executables/host-daemon"
cp "$target/release/bex-provider-supervisor" "$executables/bex-provider-supervisor"
cp apps/desktop/macos/Info.plist "$bundle/Contents/Info.plist"
sign "$executables/Bex"
sign --identifier app.bex.provider-supervisor "$executables/bex-provider-supervisor"
sign --identifier app.bex.host "$executables/host-daemon"
sign "$bundle"
verify "$bundle"
if [[ -e $destination ]]; then mv "$destination" "$staging/previous.app"; fi
mv "$bundle" "$destination"
echo "$destination"
