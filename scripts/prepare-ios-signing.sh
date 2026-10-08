#!/usr/bin/env bash
# Import iOS distribution material into ephemeral CI locations.
set -euo pipefail

: "${IOS_DISTRIBUTION_CERTIFICATE_BASE64:?IOS_DISTRIBUTION_CERTIFICATE_BASE64 is required}"
: "${IOS_DISTRIBUTION_CERTIFICATE_PASSWORD:?IOS_DISTRIBUTION_CERTIFICATE_PASSWORD is required}"
: "${IOS_SIGNING_KEYCHAIN_PASSWORD:?IOS_SIGNING_KEYCHAIN_PASSWORD is required}"
: "${IOS_PROVISIONING_PROFILE_BASE64:?IOS_PROVISIONING_PROFILE_BASE64 is required}"
: "${RUNNER_TEMP:?RUNNER_TEMP is required on CI}"

keychain="$RUNNER_TEMP/ios-distribution.keychain-db"
certificate="$RUNNER_TEMP/ios-distribution.p12"
profile="$RUNNER_TEMP/ios-distribution.mobileprovision"
if base64 --decode </dev/null >/dev/null 2>&1; then
    printf '%s' "$IOS_DISTRIBUTION_CERTIFICATE_BASE64" | base64 --decode > "$certificate"
    printf '%s' "$IOS_PROVISIONING_PROFILE_BASE64" | base64 --decode > "$profile"
else
    printf '%s' "$IOS_DISTRIBUTION_CERTIFICATE_BASE64" | base64 -D > "$certificate"
    printf '%s' "$IOS_PROVISIONING_PROFILE_BASE64" | base64 -D > "$profile"
fi
security create-keychain -p "$IOS_SIGNING_KEYCHAIN_PASSWORD" "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$IOS_SIGNING_KEYCHAIN_PASSWORD" "$keychain"
security import "$certificate" -k "$keychain" -P "$IOS_DISTRIBUTION_CERTIFICATE_PASSWORD" \
    -T /usr/bin/codesign -T /usr/bin/security
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k \
    "$IOS_SIGNING_KEYCHAIN_PASSWORD" "$keychain" >/dev/null
security list-keychains -d user -s "$keychain"
profile_uuid=$(security cms -D -i "$profile" | /usr/libexec/PlistBuddy -c 'Print:UUID' /dev/stdin)
profile_dir="$HOME/Library/MobileDevice/Provisioning Profiles"
mkdir -p "$profile_dir"
cp "$profile" "$profile_dir/$profile_uuid.mobileprovision"
identities=$(security find-identity -v -p codesigning "$keychain" 2>/dev/null || true)
if ! grep -Eq '[1-9][0-9]* valid identities found' <<<"$identities"; then
    echo 'no iOS distribution identity found in certificate' >&2
    exit 1
fi
printf 'IOS_SIGNING_KEYCHAIN=%s\n' "$keychain" >> "$GITHUB_ENV"
echo 'iOS distribution signing keychain and provisioning profile prepared.'
