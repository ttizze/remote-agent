#!/usr/bin/env bash
# Import iOS distribution material into ephemeral CI locations.
set -euo pipefail

: "${IOS_DISTRIBUTION_CERTIFICATE_BASE64:?IOS_DISTRIBUTION_CERTIFICATE_BASE64 is required}"
: "${IOS_DISTRIBUTION_CERTIFICATE_PASSWORD:?IOS_DISTRIBUTION_CERTIFICATE_PASSWORD is required}"
: "${IOS_PROVISIONING_PROFILE_BASE64:?IOS_PROVISIONING_PROFILE_BASE64 is required}"
: "${RUNNER_TEMP:?RUNNER_TEMP is required on CI}"

umask 077
private_dir="$RUNNER_TEMP/ios-distribution"
keychain="$private_dir/ios-distribution.keychain-db"
certificate="$private_dir/ios-distribution.p12"
profile="$private_dir/ios-distribution.mobileprovision"
certificate_password_file="$private_dir/ios-distribution-password"
decrypted_certificate="$private_dir/ios-distribution.pem"
cleanup() {
    local exit_code=$?
    rm -f "$certificate" "$profile" "$certificate_password_file" "$decrypted_certificate"
    if (( exit_code != 0 )); then
        rm -rf "$private_dir"
    fi
    return "$exit_code"
}
trap cleanup EXIT
mkdir -p "$private_dir"
chmod 700 "$private_dir"
security_bin=${SECURITY_BIN:-security}
openssl_bin=${OPENSSL_BIN:-openssl}
plist_buddy_bin=${PLIST_BUDDY_BIN:-/usr/libexec/PlistBuddy}
if base64 --decode </dev/null >/dev/null 2>&1; then
    printf '%s' "$IOS_DISTRIBUTION_CERTIFICATE_BASE64" | base64 --decode > "$certificate"
    printf '%s' "$IOS_PROVISIONING_PROFILE_BASE64" | base64 --decode > "$profile"
else
    printf '%s' "$IOS_DISTRIBUTION_CERTIFICATE_BASE64" | base64 -D > "$certificate"
    printf '%s' "$IOS_PROVISIONING_PROFILE_BASE64" | base64 -D > "$profile"
fi
printf '%s\n' "$IOS_DISTRIBUTION_CERTIFICATE_PASSWORD" > "$certificate_password_file"
"$openssl_bin" pkcs12 -in "$certificate" -passin "file:$certificate_password_file" -nodes -out "$decrypted_certificate"
# This keychain is private to the job and deliberately has an empty password.
# The explicit flags avoid undocumented stdin or GUI password prompting.
"$security_bin" create-keychain -p '' "$keychain"
printf '%s\n' "$keychain" > "$RUNNER_TEMP/ios-distribution-keychain"
"$security_bin" set-keychain-settings -lut 21600 "$keychain"
"$security_bin" unlock-keychain -p '' "$keychain"
"$security_bin" import "$decrypted_certificate" -f pemseq -t agg -k "$keychain" \
    -T /usr/bin/codesign -T /usr/bin/security
"$security_bin" set-key-partition-list -S apple-tool:,apple:,codesign: -s -k '' "$keychain" >/dev/null
"$security_bin" list-keychains -d user -s "$keychain"
profile_uuid=$("$security_bin" cms -D -i "$profile" | "$plist_buddy_bin" -c 'Print:UUID' /dev/stdin)
[[ $profile_uuid =~ ^[A-Fa-f0-9-]+$ ]] || {
    echo 'iOS provisioning profile UUID is invalid' >&2
    exit 1
}
profile_dir="$HOME/Library/MobileDevice/Provisioning Profiles"
profile_path="$profile_dir/$profile_uuid.mobileprovision"
mkdir -p "$profile_dir"
cp "$profile" "$profile_path"
printf '%s\n' "$profile_path" > "$private_dir/profile-path"
identities=$("$security_bin" find-identity -v -p codesigning "$keychain" 2>/dev/null || true)
if ! grep -Eq '[1-9][0-9]* valid identities found' <<<"$identities"; then
    echo 'no iOS distribution identity found in certificate' >&2
    exit 1
fi
{
    printf 'IOS_SIGNING_KEYCHAIN=%s\n' "$keychain"
    printf 'IOS_SIGNING_PROFILE_PATH=%s\n' "$profile_path"
} >> "$GITHUB_ENV"
echo 'iOS distribution signing keychain and provisioning profile prepared.'
