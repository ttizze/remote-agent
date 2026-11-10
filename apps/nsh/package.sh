#!/usr/bin/env bash
# Package the iPhone preview after building agent-nsh and its native views.
# The Mac app is packaged by scripts/build-macos.sh desktop.
set -euo pipefail
cd "$(dirname "$0")/../.."
target=${CARGO_TARGET_DIR:-$PWD/target}
platform=${1:-ios-simulator}
case "$platform" in
    ios-simulator) rust_target=aarch64-apple-ios-sim; sdk=iphonesimulator; bundle_platform=iPhoneSimulator ;;
    ios) rust_target=aarch64-apple-ios; sdk=iphoneos; bundle_platform=iPhoneOS
        : "${AGENT_IOS_PROFILE:?Set AGENT_IOS_PROFILE to the preview provisioning profile}"
        : "${AGENT_IOS_SIGN_IDENTITY:?Set AGENT_IOS_SIGN_IDENTITY to its signing identity}" ;;
    *) printf 'Unsupported platform: %s\n' "$platform" >&2; exit 1 ;;
esac
bundle="$target/nsh-preview/$platform/Bex NSH Preview.app"
python3 - "$bundle" "$target/$rust_target/debug/agent-nsh" \
    "$target/nsh-native/debug/$platform/BexNshNative.framework" "$bundle_platform" <<'PY'
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

destination, executable, native, platform = sys.argv[1:]
bundle = pathlib.Path(destination)
info = {
    "CFBundleIdentifier": "com.bex.nsh-preview",
    "CFBundleExecutable": "agent-nsh",
    "CFBundleName": "Bex NSH Preview",
    "CFBundleDisplayName": "Bex NSH",
    "CFBundlePackageType": "APPL",
    "CFBundleShortVersionString": "0.1.0",
    "CFBundleVersion": "1",
    "NSMicrophoneUsageDescription": "録音した音声を接続先PCへ送り、文字起こししてメッセージに入力します。",
    "NSLocalNetworkUsageDescription": "同じネットワークにあるPCのBex Hostへ接続します。",
    "CFBundleSupportedPlatforms": [platform],
    "MinimumOSVersion": "26.0",
    "UIDeviceFamily": [1, 2],
    "UILaunchScreen": {},
    # NSH creates its window in the scene delegate, not the legacy app delegate.
    "UIApplicationSceneManifest": {"UIApplicationSupportsMultipleScenes": False},
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
icon_info="$target/nsh-preview/$platform-icon-info.plist"
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
if [[ $platform == ios ]]; then
    profile="$target/nsh-preview/ios-profile.plist"
    entitlements="$target/nsh-preview/ios-entitlements.plist"
    security cms -D -i "$AGENT_IOS_PROFILE" > "$profile"
    python3 - "$profile" "$entitlements" <<'PY'
import datetime
import plistlib
import sys

with open(sys.argv[1], "rb") as source:
    profile = plistlib.load(source)
entitlements = profile["Entitlements"]
if entitlements["application-identifier"].split(".", 1)[1] != "com.bex.nsh-preview":
    raise SystemExit("Use a profile dedicated to com.bex.nsh-preview.")
if profile["ExpirationDate"] <= datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None):
    raise SystemExit("The preview provisioning profile expired.")
if not profile.get("ProvisionedDevices"):
    raise SystemExit("The preview profile must allow direct device installation.")
with open(sys.argv[2], "wb") as output:
    plistlib.dump(entitlements, output)
PY
    cp "$AGENT_IOS_PROFILE" "$bundle/embedded.mobileprovision"
    signing=(--force --sign "$AGENT_IOS_SIGN_IDENTITY" --timestamp=none)
    if [[ -n ${AGENT_CODE_SIGN_KEYCHAIN:-} ]]; then signing+=(--keychain "$AGENT_CODE_SIGN_KEYCHAIN"); fi
    codesign "${signing[@]}" "$bundle/Frameworks/BexNshNative.framework"
    codesign "${signing[@]}" --entitlements "$entitlements" "$bundle"
else
    codesign --force --sign - "$bundle"
fi
codesign --verify --deep --strict "$bundle"
printf '%s\n' "$bundle"
