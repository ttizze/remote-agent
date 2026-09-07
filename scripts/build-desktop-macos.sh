#!/bin/sh
set -eu

# Run with: nix develop --command scripts/build-desktop-macos.sh
repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repository_root"
[ "$(uname -sm)" = "Darwin arm64" ] || { echo 'The GPUI Mac bundle currently targets Apple Silicon.' >&2; exit 1; }

. "$repository_root/scripts/macos-signing.sh"
bex_resolve_signing_identity

npm --prefix apps/desktop/web ci --ignore-scripts --no-audit --no-fund
cargo build --locked --package host-daemon --package bex-desktop --release

# Build a fresh bundle so the previous JS runtime and native addon cannot survive a cutover.
mkdir -p "$repository_root/target"
staging=$(mktemp -d "$repository_root/target/.Bex-build.XXXXXX")
trap 'rm -rf "$staging"' EXIT HUP INT TERM
bundle="$staging/Bex.app"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
cp apps/desktop/assets/icon.icns "$bundle/Contents/Resources/Bex.icns"
cp target/release/bex-desktop "$bundle/Contents/MacOS/Bex"
cp target/release/host-daemon "$bundle/Contents/MacOS/host-daemon"
mkdir -p "$bundle/Contents/Resources/terminal"
cp apps/desktop/web/node_modules/@xterm/xterm/lib/xterm.js "$bundle/Contents/Resources/terminal/"
cp apps/desktop/web/node_modules/@xterm/xterm/css/xterm.css "$bundle/Contents/Resources/terminal/"
cp apps/desktop/web/node_modules/@xterm/addon-fit/lib/addon-fit.js "$bundle/Contents/Resources/terminal/"
cp apps/desktop/web/node_modules/@xterm/xterm/LICENSE "$bundle/Contents/Resources/terminal/LICENSE-xterm"
cp apps/desktop/web/node_modules/@xterm/addon-fit/LICENSE "$bundle/Contents/Resources/terminal/LICENSE-addon-fit"
picker="$bundle/Contents/Resources/Bex File Picker.app"
mkdir -p "$picker/Contents/MacOS"
xcrun swiftc -O apps/desktop/macos/FilePicker.swift -o "$picker/Contents/MacOS/FilePicker"
cat > "$picker/Contents/Info.plist" <<'PICKER'
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>app.bex.filepicker</string>
<key>CFBundleName</key><string>Bex File Picker</string>
<key>CFBundleExecutable</key><string>FilePicker</string>
<key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>
PICKER
cat > "$bundle/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>app.bex.desktop</string>
<key>CFBundleName</key><string>Bex</string>
<key>CFBundleDisplayName</key><string>Bex</string>
<key>CFBundleExecutable</key><string>Bex</string>
<key>CFBundleIconFile</key><string>Bex.icns</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
<key>CFBundleVersion</key><string>1</string>
<key>LSMinimumSystemVersion</key><string>13.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
# Certificate signing preserves the Host's Keychain identity across updates.
sign_identity=$BEX_CODE_SIGN_IDENTITY
/usr/bin/codesign --force --sign "$sign_identity" --timestamp=none "$picker"
/usr/bin/codesign --force --sign "$sign_identity" --timestamp=none "$bundle/Contents/MacOS/Bex"
/usr/bin/codesign --force --sign "$sign_identity" --timestamp=none --identifier app.bex.host "$bundle/Contents/MacOS/host-daemon"
/usr/bin/codesign --force --sign "$sign_identity" --timestamp=none "$bundle"
/usr/bin/codesign --verify --deep --strict "$bundle"
destination="$repository_root/target/Bex.app"
if [ -d "$destination" ]; then mv "$destination" "$staging/previous.app"; fi
if ! mv "$bundle" "$destination"; then
    if [ -d "$staging/previous.app" ]; then mv "$staging/previous.app" "$destination"; fi
    exit 1
fi
echo "$destination"
