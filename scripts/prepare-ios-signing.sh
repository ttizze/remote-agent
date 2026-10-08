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
certificate_password_file="$RUNNER_TEMP/ios-distribution-password"
decrypted_certificate="$RUNNER_TEMP/ios-distribution.pem"
cleanup() {
    rm -f "$certificate" "$profile" "$certificate_password_file" "$decrypted_certificate"
}
trap cleanup EXIT
umask 077
security_with_stdin() {
    # security prompts for omitted passwords; keep the secret out of argv.
    local password=$1
    shift
    printf '%s\n' "$password" | security "$@"
}
if base64 --decode </dev/null >/dev/null 2>&1; then
    printf '%s' "$IOS_DISTRIBUTION_CERTIFICATE_BASE64" | base64 --decode > "$certificate"
    printf '%s' "$IOS_PROVISIONING_PROFILE_BASE64" | base64 --decode > "$profile"
else
    printf '%s' "$IOS_DISTRIBUTION_CERTIFICATE_BASE64" | base64 -D > "$certificate"
    printf '%s' "$IOS_PROVISIONING_PROFILE_BASE64" | base64 -D > "$profile"
fi
printf '%s\n' "$IOS_DISTRIBUTION_CERTIFICATE_PASSWORD" > "$certificate_password_file"
openssl pkcs12 -in "$certificate" -passin "file:$certificate_password_file" -nodes -out "$decrypted_certificate"
security_with_stdin "$IOS_SIGNING_KEYCHAIN_PASSWORD" create-keychain "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security_with_stdin "$IOS_SIGNING_KEYCHAIN_PASSWORD" unlock-keychain "$keychain"
security import "$decrypted_certificate" -f pemseq -t agg -k "$keychain" \
    -T /usr/bin/codesign -T /usr/bin/security
security_with_stdin "$IOS_SIGNING_KEYCHAIN_PASSWORD" set-key-partition-list \
    -S apple-tool:,apple:,codesign: -s "$keychain" >/dev/null
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
