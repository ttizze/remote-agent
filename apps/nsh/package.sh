#!/usr/bin/env bash
# Package the iPhone preview after building agent-nsh for its platform.
# The Mac app is packaged by scripts/build-macos.sh desktop.
set -euo pipefail
cd "$(dirname "$0")/../.."
target=${CARGO_TARGET_DIR:-$PWD/target}
platform=${1:-ios-simulator}
case "$platform" in
    ios-simulator) rust_target=aarch64-apple-ios-sim; sdk=iphonesimulator; bundle_platform=iPhoneSimulator ;;
    ios-device)
        rust_target=aarch64-apple-ios; sdk=iphoneos; bundle_platform=iPhoneOS
        : "${AGENT_IOS_PROVISIONING_PROFILE:?A development provisioning profile is required}"
        : "${AGENT_CODE_SIGN_IDENTITY:?An Apple Development identity is required}"
        ;;
    *) printf 'Unsupported platform: %s\n' "$platform" >&2; exit 1 ;;
esac
if [[ $platform == ios-simulator ]]; then
    bundle="$target/nsh-preview/$platform/Bex NSH Preview.app"
else
    bundle="$target/nsh-preview/$platform/Bex NSH.app"
fi
revision=$(git describe --always --dirty --abbrev=40 --exclude '*')
build_number=${AGENT_IOS_BUILD_NUMBER:-$(git rev-list --count HEAD)}
python3 - "$bundle" "$target/$rust_target/debug/agent-nsh" \
    "$target/nsh-native/debug/$platform/BexNshNative.framework" "$bundle_platform" "$revision" "$build_number" <<'PY'
import pathlib
import plistlib
import shutil
import sys
import tempfile

def copy_file(source, destination):
    # A signed image may still be mapped by a previous launch. Give each
    # replacement its own inode so the new signature is validated.
    destination = pathlib.Path(destination)
    with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as pending:
        path = pathlib.Path(pending.name)
    try:
        shutil.copy2(source, path)
        path.replace(destination)
    finally:
        path.unlink(missing_ok=True)
    return str(destination)

destination, executable, native, platform, revision, build_number = sys.argv[1:]
bundle = pathlib.Path(destination)
info = {
    "CFBundleIdentifier": "com.bex.nsh-standalone",
    "CFBundleExecutable": "agent-nsh",
    "CFBundleName": "Bex NSH Preview",
    "CFBundleDisplayName": "Bex NSH Standalone",
    "CFBundlePackageType": "APPL",
    "CFBundleShortVersionString": "1.0" if platform == "iPhoneOS" else "0.1.0",
    "CFBundleVersion": build_number,
    "BEXBuildRevision": revision,
    "NSMicrophoneUsageDescription": "録音した音声を接続先PCへ送り、文字起こししてメッセージに入力します。",
    "CFBundleSupportedPlatforms": [platform],
    "MinimumOSVersion": "26.0",
    "UIDeviceFamily": [1, 2],
    "UILaunchScreen": {},
    "UIApplicationSupportsMultipleScenes": False,
    "CADisableMinimumFrameDurationOnPhone": True,
}
binary = bundle / "agent-nsh"
binary.parent.mkdir(parents=True, exist_ok=True)
frameworks = bundle / "Frameworks"
frameworks.mkdir(parents=True, exist_ok=True)
shutil.copytree(native, frameworks / "BexNshNative.framework", dirs_exist_ok=True, copy_function=copy_file)
copy_file(executable, binary)
with (bundle / "Info.plist").open("wb") as output:
    plistlib.dump(info, output)
PY
icon_info="$target/nsh-preview/$platform/icon-info.plist"
xcrun actool apps/mobile/iosApp/Bex/Assets.xcassets --compile "$bundle" \
    --platform "$sdk" --minimum-deployment-target 26.0 --target-device iphone \
    --target-device ipad --app-icon AppIconDev --output-partial-info-plist "$icon_info"
python3 - "$bundle/Info.plist" "$icon_info" <<'PY'
import plistlib
import sys
with open(sys.argv[1], "rb") as source:
    info = plistlib.load(source)
with open(sys.argv[2], "rb") as source:
    info.update(plistlib.load(source))
with open(sys.argv[1], "wb") as output:
    plistlib.dump(info, output)
PY
if [[ $platform == ios-device ]]; then
    cp "$AGENT_IOS_PROVISIONING_PROFILE" "$bundle/embedded.mobileprovision"
    entitlements="$target/nsh-preview/$platform/entitlements.plist"
    security cms -D -i "$AGENT_IOS_PROVISIONING_PROFILE" | python3 -c '
import plistlib, sys
profile = plistlib.loads(sys.stdin.buffer.read())
entitlements = profile["Entitlements"]
assert entitlements["application-identifier"].endswith(".com.bex.nsh-standalone")
assert entitlements.get("get-task-allow"), "A development profile is required"
with open(sys.argv[1], "wb") as output:
    plistlib.dump(entitlements, output)
' "$entitlements"
    codesign --force --sign "$AGENT_CODE_SIGN_IDENTITY" "$bundle/Frameworks/BexNshNative.framework"
    codesign --force --sign "$AGENT_CODE_SIGN_IDENTITY" --entitlements "$entitlements" "$bundle"
else
    codesign --force --sign - "$bundle"
fi
codesign --verify --deep --strict "$bundle"
printf '%s\n' "$bundle"
