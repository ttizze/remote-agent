#!/usr/bin/env bash
# Exercise UIKit navigation and scroll geometry on macOS without a Simulator or Host.
set -euo pipefail
cd "$(dirname "$0")/.."
sdk=$(xcrun --sdk macosx --show-sdk-path)
sdk_build=$(xcrun --sdk macosx --show-sdk-build-version)
compiler=$(xcrun swiftc --version)
swift_target="$(uname -m)-apple-ios26.0-macabi"
target=$(cargo metadata --no-deps --format-version 1 | jq -er .target_directory)
output="$target/qa/ios-scroll/builds"
mkdir -p "$output"
run_test() {
    local name=$1
    shift
    local fixture="apps/mobile/iosApp/UITests/Fixtures/$name.swift" key app executable pending_dir pending
    key=$({
        printf '%s\n' "$sdk" "$sdk_build" "$compiler" "$swift_target"
        shasum -a 256 "$0" "$@" "$fixture"
    } | shasum -a 256 | cut -d ' ' -f 1)
    app="$output/$name/$key.app"
    executable="$app/Contents/MacOS/$name"
    if [[ ! -x $executable || ! -s "$app/Contents/Info.plist" ]]; then
        mkdir -p "$output/$name"
        pending_dir=$(mktemp -d "$output/$name/.build.XXXXXX")
        pending="$pending_dir/$name.app"
        mkdir -p "$pending/Contents/MacOS"
        # UIApplication needs a bundle identifier even for an isolated Catalyst fixture.
        cat > "$pending/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>app.bex.tests.$name</string>
<key>CFBundleExecutable</key><string>$name</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>1</string>
</dict></plist>
PLIST
        if ! xcrun swiftc -sdk "$sdk" -target "$swift_target" \
            -F "$sdk/System/iOSSupport/System/Library/Frameworks" \
            -L "$sdk/System/iOSSupport/usr/lib" \
            "$@" "$fixture" -o "$pending/Contents/MacOS/$name" \
            || ! codesign --force --sign - "$pending"; then
            rm -rf "$pending_dir"
            return 1
        fi
        rm -rf "$app"
        mv "$pending" "$app"
        rmdir "$pending_dir"
    else
        echo "Reusing unchanged $name build"
    fi
    "$executable"
}
run_test navigation-tests apps/mobile/iosApp/Bex/NavigationAppearance.swift
run_test scroll-tests apps/mobile/iosApp/Bex/ConversationList.swift apps/mobile/iosApp/Bex/ConversationListLayout.swift
run_test text-view-tests apps/mobile/iosApp/Bex/ConversationTextLayout.swift
