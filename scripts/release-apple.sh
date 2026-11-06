#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
: "${AGENT_BUILD_NUMBER:?Release build number is required}"
: "${AGENT_RELEASE_SHA:?Release revision is required}"
: "${AGENT_CODE_SIGN_KEYCHAIN:?Dedicated signing keychain is required}"
: "${AGENT_ASC_PRIVATE_KEY_FILE:?App Store Connect key file is required}"
: "${AGENT_RELEASE_CHANNEL:?Release channel is required}"
case "$AGENT_RELEASE_CHANNEL" in
    nightly) ios_config=(-xcconfig "$PWD/apps/mobile/iosApp/Config/Preview.xcconfig") ;;
    stable) ios_config=() ;;
    *) echo 'Release channel must be nightly or stable.' >&2; exit 1 ;;
esac
[[ $(git rev-parse HEAD) == "$AGENT_RELEASE_SHA" ]]
git diff --quiet HEAD --
output="$PWD/target/apple-release"
mkdir -p "$output"
mac_output=$(mktemp -d "$output/mac.XXXXXX")
notaries=()
cleanup() {
    local result=$?
    local notary
    for notary in "${notaries[@]}"; do
        if kill -0 "$notary" 2>/dev/null; then
            kill "$notary" 2>/dev/null || true
        fi
        wait "$notary" 2>/dev/null || true
    done
    rm -rf "$mac_output"
    exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
# Each channel builds the selected source with its own release build number.
name=bex-macOS-arm64
just build-desktop-macos
mkdir -p "$mac_output/$name"
bundle="$mac_output/$name/bex.app"
mv "$PWD/target/release/bex.app" "$bundle"
[[ $(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$bundle/Contents/Info.plist") == app.bex.desktop ]]
[[ $(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$bundle/Contents/Info.plist") == "$AGENT_BUILD_NUMBER" ]]
zip="$output/$name.zip"
ditto -c -k --sequesterRsrc --keepParent "$bundle" "$zip"
xcrun notarytool submit "$zip" --key "$AGENT_ASC_PRIVATE_KEY_FILE" \
    --key-id "$AGENT_ASC_KEY_ID" --issuer "$AGENT_ASC_ISSUER_ID" \
    --wait --output-format json > "$mac_output/$name/notarization.json" &
notaries+=("$!")
# Apple processes the Mac submission while the iPhone archive builds locally.
archive="$output/Bex.xcarchive"
auth=(-authenticationKeyPath "$AGENT_ASC_PRIVATE_KEY_FILE" -authenticationKeyID "$AGENT_ASC_KEY_ID" -authenticationKeyIssuerID "$AGENT_ASC_ISSUER_ID")
node scripts/apple-provisioning.ts
ios_bundle=$(jq -er .bundleId "$output/plan.json")
export_method=$(jq -er .method "$output/provisioning.json")
profile_specifier="AGENT_IOS_PROFILE_SPECIFIER=$(jq -er .prefix "$output/provisioning.json") \$(PRODUCT_BUNDLE_IDENTIFIER)"
# Only the app and extension consume these settings; Swift package build plugins keep their defaults.
just ios-archive "$archive" "$output/DerivedData" "${auth[@]}" \
    "${ios_config[@]}" \
    "CURRENT_PROJECT_VERSION=$AGENT_BUILD_NUMBER" "MARKETING_VERSION=${AGENT_MARKETING_VERSION:?}" AGENT_IOS_CODE_SIGN_STYLE=Manual \
    'AGENT_IOS_CODE_SIGN_IDENTITY=Apple Distribution' "$profile_specifier" \
    "OTHER_CODE_SIGN_FLAGS=--keychain $AGENT_CODE_SIGN_KEYCHAIN"
apps=("$archive/Products/Applications/"*.app)
[[ ${#apps[@]} == 1 && -d ${apps[0]} ]]
app=${apps[0]}
codesign --verify --deep --strict "$app"
[[ $(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$app/Info.plist") == "$AGENT_BUILD_NUMBER" ]]
[[ $(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$app/Info.plist") == "$ios_bundle" ]]
# Both products must validate before an iPhone upload or a Mac release can proceed.
for notary in "${notaries[@]}"; do wait "$notary"; done
notaries=()
bundle="$mac_output/$name/bex.app"
jq -e -s 'any(.[]; .status == "Accepted")' "$mac_output/$name/notarization.json" >/dev/null
xcrun stapler staple "$bundle"
codesign --verify --deep --strict "$bundle"
spctl --assess --type execute "$bundle"
ditto -c -k --sequesterRsrc --keepParent "$bundle" "$mac_output/$name/notarized.zip"
mv "$mac_output/$name/notarized.zip" "$output/$name.zip"
(cd "$output" && shasum -a 256 "$name.zip" > "$name.zip.sha256")
# Prove distribution signing locally before sending the archive to Apple.
destinations=(export)
if [[ $AGENT_RELEASE_CHANNEL == stable ]]; then destinations+=(upload); fi
for destination in "${destinations[@]}"; do
    if [[ $destination == upload ]]; then touch "$output/upload-start"; fi
    options="$output/export-$destination.plist"
    jq --arg destination "$destination" --arg method "$export_method" '{method:$method,destination:$destination,
        teamID:"K65K9J8686",signingStyle:"manual",signingCertificate:"Apple Distribution",
        provisioningProfiles:.profiles,manageAppVersionAndBuildNumber:false,uploadSymbols:true}' \
        "$output/provisioning.json" | plutil -convert xml1 -o "$options" -
    xcodebuild -exportArchive -archivePath "$archive" -exportPath "$output/$destination" \
        -exportOptionsPlist "$options" -allowProvisioningUpdates "${auth[@]}" \
        "OTHER_CODE_SIGN_FLAGS=--keychain $AGENT_CODE_SIGN_KEYCHAIN"
done
if [[ $AGENT_RELEASE_CHANNEL == stable ]]; then
    node scripts/apple-release.ts record-delivery "$(/usr/bin/getconf DARWIN_USER_TEMP_DIR)" "$output/upload-start"
else
    ipas=("$output/export/"*.ipa)
    [[ ${#ipas[@]} == 1 && -f ${ipas[0]} ]]
    mv "${ipas[0]}" "$output/bex-iOS-preview.ipa"
    (cd "$output" && shasum -a 256 bex-iOS-preview.ipa > bex-iOS-preview.ipa.sha256)
fi
