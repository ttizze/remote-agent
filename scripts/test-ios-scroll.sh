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
    local fixture=$1 name
    name=${fixture##*/}
    name=${name%.swift}
    shift
    local swift_flags=()
    if [[ ${1-} == --dev ]]; then
        swift_flags=(-D AGENT_DEV_DIAGNOSTICS)
        shift
    fi
    local key app executable pending_dir pending
    key=$({
        printf '%s\n' "$sdk" "$sdk_build" "$compiler" "$swift_target"
        printf '%s\n' "${swift_flags[*]-}"
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
            ${swift_flags[@]+"${swift_flags[@]}"} "$@" "$fixture" -o "$pending/Contents/MacOS/$name" \
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
    # A main-thread layout loop never reaches the fixture's own failure checks.
    timeout 120 "$executable"
    if [[ $name == text-view-tests ]]; then
        if [[ ${#swift_flags[@]} -gt 0 ]]; then
            nm "$executable" | grep -E 'ConversationTextDiagnosticLog' >/dev/null
        elif nm "$executable" | grep -E 'ConversationText(Diagnostic|Container)' >/dev/null; then
            echo 'Non-Dev text rendering must contain no diagnostic code.' >&2
            return 1
        fi
    fi
}
run_test apps/mobile/iosApp/UITests/Fixtures/navigation-tests.swift apps/mobile/iosApp/Bex/NavigationAppearance.swift
run_test apps/mobile/iosApp/UITests/Fixtures/scroll-tests.swift apps/mobile/iosApp/Bex/ConversationList.swift apps/mobile/iosApp/Bex/ConversationListLayout.swift
run_test apps/mobile/iosApp/UITests/Fixtures/text-view-tests.swift apps/mobile/iosApp/Bex/ConversationTextLayout.swift apps/mobile/iosApp/Bex/ConversationTextDiagnostics.swift
run_test apps/mobile/iosApp/UITests/Fixtures/text-view-tests.swift --dev apps/mobile/iosApp/Bex/ConversationTextLayout.swift apps/mobile/iosApp/Bex/ConversationTextDiagnostics.swift
run_test apps/nsh/apple/Tests/Fixtures/message-layout-tests.swift apps/nsh/apple/Sources/BexNshNative/MessageAttributes.swift apps/nsh/apple/Sources/BexNshNative/MessageText.swift
