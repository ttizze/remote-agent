#!/usr/bin/env bash
# Archive, export, and optionally upload the iOS app without exposing credentials.
set -euo pipefail

[[ $# -ge 3 ]] || {
    echo "usage: release-ios.sh archive-path derived-data-path output-dir [xcodebuild args...]" >&2
    exit 2
}
archive=$1
derived_data=$2
output_dir=$3
shift 3
root=$(cd "$(dirname "$0")/.." && pwd)
export_options="$root/apps/mobile/iosApp/Config/TestFlightExportOptions.plist"
: "${IOS_SIGNING_KEYCHAIN:?IOS_SIGNING_KEYCHAIN is required}"
[[ -f $IOS_SIGNING_KEYCHAIN ]] || { echo "iOS signing keychain does not exist" >&2; exit 1; }
identities=$(security find-identity -v -p codesigning "$IOS_SIGNING_KEYCHAIN" 2>/dev/null || true)
if ! grep -Eq '[1-9][0-9]* valid identities found' <<<"$identities"; then
    echo "configured iOS signing keychain has no usable identity" >&2
    exit 1
fi
[[ -f $export_options ]] || { echo "missing export options: $export_options" >&2; exit 1; }

cd "$root"
mkdir -p "$output_dir"
scripts/archive-ios.sh "$archive" "$derived_data" \
    OTHER_CODE_SIGN_FLAGS="--keychain $IOS_SIGNING_KEYCHAIN" \
    "$@"
xcodebuild -exportArchive \
    -archivePath "$archive" \
    -exportPath "$output_dir/export" \
    -exportOptionsPlist "$export_options" \
    -allowProvisioningUpdates \
    OTHER_CODE_SIGN_FLAGS="--keychain $IOS_SIGNING_KEYCHAIN"

ipa=$(find "$output_dir/export" -maxdepth 1 -type f -name '*.ipa' -print -quit)
[[ -n $ipa ]] || { echo "iOS export did not produce an IPA" >&2; exit 1; }
mv "$ipa" "$output_dir/mobile.ipa"
if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$output_dir/mobile.ipa" > "$output_dir/mobile.ipa.sha256"
else
    sha256sum "$output_dir/mobile.ipa" > "$output_dir/mobile.ipa.sha256"
fi

if [[ ${IOS_UPLOAD:-0} == 1 ]]; then
    : "${ASC_API_KEY_PATH:?ASC_API_KEY_PATH is required for TestFlight upload}"
    : "${ASC_KEY_ID:?ASC_KEY_ID is required for TestFlight upload}"
    : "${ASC_ISSUER_ID:?ASC_ISSUER_ID is required for TestFlight upload}"
    [[ -f $ASC_API_KEY_PATH ]] || { echo "App Store Connect API key file does not exist" >&2; exit 1; }
    upload_log="${RUNNER_TEMP:-$output_dir}/testflight-upload.log"
    xcrun altool --upload-app --type ios --file "$output_dir/mobile.ipa" \
        --apiKey "$ASC_KEY_ID" --apiIssuer "$ASC_ISSUER_ID" 2>&1 | tee "$upload_log"
    delivery_id=$(sed -nE \
        -e 's/.*[Dd]elivery [Uu][Uu][Ii][Dd]:[[:space:]]*([A-Za-z0-9-]+).*/\1/p' \
        -e 's/.*[Dd]elivery[[:space:]]+[Ii][Dd][[:space:]]*=[[:space:]]*([A-Za-z0-9-]+).*/\1/p' \
        "$upload_log" | head -n 1)
    if [[ -z $delivery_id ]]; then
        while IFS= read -r log; do
            delivery_id=$(sed -nE \
                -e 's/.*[Dd]elivery [Uu][Uu][Ii][Dd]:[[:space:]]*([A-Za-z0-9-]+).*/\1/p' \
                -e 's/.*[Dd]elivery[[:space:]]+[Ii][Dd][[:space:]]*=[[:space:]]*([A-Za-z0-9-]+).*/\1/p' \
                "$log" | head -n 1)
            [[ -n $delivery_id ]] && break
        done < <(find "$HOME/Library/Logs/ContentDelivery" -type f -name '*.log' -print 2>/dev/null | sort -r)
    fi
    [[ -n $delivery_id ]] || { echo 'upload completed without a delivery identifier' >&2; exit 1; }
    scripts/poll-testflight.sh "$delivery_id"
fi
printf '%s\n' "$output_dir/mobile.ipa"
