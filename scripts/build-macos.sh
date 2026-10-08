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
    node scripts/install-claude-sdk.mjs "$target/release"
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
cp crates/host-daemon/src/claude/sdk/SDK-LICENSE.md "$resources/Claude-Agent-SDK-LICENSE.md"
cp "$target/release/bex-desktop" "$executables/Bex"
cp "$target/release/host-daemon" "$executables/host-daemon"
cp crates/host-daemon/src/claude/sdk/bridge.bundle.mjs "$resources/bex-claude-sdk.mjs"
cp "$target/release/bex-provider-supervisor" "$executables/bex-provider-supervisor"
cp apps/desktop/macos/Info.plist "$bundle/Contents/Info.plist"
# Keep Nix-linked libraries inside the bundle so it also runs on other Macs.
code=("$executables/Bex" "$executables/host-daemon" "$executables/bex-provider-supervisor")
libraries=()
frameworks="$bundle/Contents/Frameworks"
for ((index=0; index<${#code[@]}; index++)); do
    while IFS= read -r library; do
        [[ $library == /nix/store/* ]] || continue
        name=${library##*/}
        mkdir -p "$frameworks"
        if [[ -f $frameworks/$name ]]; then
            for source in "${libraries[@]}"; do
                if [[ ${source##*/} == "$name" ]] && ! cmp -s "$library" "$source"; then
                    echo "Conflicting bundled library: $name" >&2; exit 1
                fi
            done
        else
            cp "$library" "$frameworks/$name"
            libraries+=("$library")
            chmod u+w "$frameworks/$name"
            /usr/bin/install_name_tool -id "@rpath/$name" "$frameworks/$name"
            code+=("$frameworks/$name")
        fi
        /usr/bin/install_name_tool -change "$library" "@executable_path/../Frameworks/$name" "${code[index]}"
    done < <(/usr/bin/otool -L "${code[index]}" | awk 'NR > 1 {print $1}')
done
for library in "${libraries[@]}"; do
    sign "$frameworks/${library##*/}"
done
sign "$executables/Bex"
sign --identifier app.bex.provider-supervisor "$executables/bex-provider-supervisor"
sign --identifier app.bex.host "$executables/host-daemon"
sign "$bundle"
verify "$bundle"
if [[ -e $destination ]]; then mv "$destination" "$staging/previous.app"; fi
mv "$bundle" "$destination"
echo "$destination"
