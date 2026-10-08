#!/usr/bin/env bash
# Import a distribution certificate into an ephemeral CI keychain.
set -euo pipefail

: "${MACOS_SIGNING_CERTIFICATE_BASE64:?MACOS_SIGNING_CERTIFICATE_BASE64 is required}"
: "${MACOS_SIGNING_CERTIFICATE_PASSWORD:?MACOS_SIGNING_CERTIFICATE_PASSWORD is required}"
: "${RUNNER_TEMP:?RUNNER_TEMP is required on CI}"

umask 077
private_dir="$RUNNER_TEMP/macos-distribution"
keychain="$private_dir/macos-distribution.keychain-db"
certificate="$private_dir/macos-distribution.p12"
certificate_password_file="$private_dir/macos-distribution-password"
decrypted_certificate="$private_dir/macos-distribution.pem"
cleanup() {
    local exit_code=$?
    rm -f "$certificate" "$certificate_password_file" "$decrypted_certificate"
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
if base64 --decode </dev/null >/dev/null 2>&1; then
    printf '%s' "$MACOS_SIGNING_CERTIFICATE_BASE64" | base64 --decode > "$certificate"
else
    printf '%s' "$MACOS_SIGNING_CERTIFICATE_BASE64" | base64 -D > "$certificate"
fi
printf '%s\n' "$MACOS_SIGNING_CERTIFICATE_PASSWORD" > "$certificate_password_file"
"$openssl_bin" pkcs12 -in "$certificate" -passin "file:$certificate_password_file" -nodes -out "$decrypted_certificate"
# This keychain is private to the job and deliberately has an empty password.
# The explicit flags avoid undocumented stdin or GUI password prompting.
"$security_bin" create-keychain -p '' "$keychain"
printf '%s\n' "$keychain" > "$RUNNER_TEMP/macos-distribution-keychain"
"$security_bin" set-keychain-settings -lut 21600 "$keychain"
"$security_bin" unlock-keychain -p '' "$keychain"
"$security_bin" import "$decrypted_certificate" -f pemseq -t agg -k "$keychain" -T /usr/bin/codesign
"$security_bin" set-key-partition-list -S apple-tool:,apple:,codesign: -s -k '' "$keychain" >/dev/null
"$security_bin" list-keychains -d user -s "$keychain"
identity=$("$security_bin" find-identity -v -p codesigning "$keychain" |
    sed -n 's/.*"\(Developer ID Application: [^"]*\)".*/\1/p' | head -n 1)
[[ -n $identity ]] || { echo "no Developer ID Application identity found in certificate" >&2; exit 1; }
{
    printf 'MACOS_SIGNING_KEYCHAIN=%s\n' "$keychain"
    printf 'APP_CODE_SIGN_IDENTITY=%s\n' "$identity"
} >> "$GITHUB_ENV"
echo 'macOS distribution signing keychain prepared.'
