#!/usr/bin/env bash
# Import a distribution certificate into an ephemeral CI keychain.
set -euo pipefail

: "${MACOS_SIGNING_CERTIFICATE_BASE64:?MACOS_SIGNING_CERTIFICATE_BASE64 is required}"
: "${MACOS_SIGNING_CERTIFICATE_PASSWORD:?MACOS_SIGNING_CERTIFICATE_PASSWORD is required}"
: "${MACOS_SIGNING_KEYCHAIN_PASSWORD:?MACOS_SIGNING_KEYCHAIN_PASSWORD is required}"
: "${RUNNER_TEMP:?RUNNER_TEMP is required on CI}"

keychain="$RUNNER_TEMP/macos-distribution.keychain-db"
certificate="$RUNNER_TEMP/macos-distribution.p12"
certificate_password_file="$RUNNER_TEMP/macos-distribution-password"
decrypted_certificate="$RUNNER_TEMP/macos-distribution.pem"
cleanup() {
    rm -f "$certificate" "$certificate_password_file" "$decrypted_certificate"
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
    printf '%s' "$MACOS_SIGNING_CERTIFICATE_BASE64" | base64 --decode > "$certificate"
else
    printf '%s' "$MACOS_SIGNING_CERTIFICATE_BASE64" | base64 -D > "$certificate"
fi
printf '%s\n' "$MACOS_SIGNING_CERTIFICATE_PASSWORD" > "$certificate_password_file"
openssl pkcs12 -in "$certificate" -passin "file:$certificate_password_file" -nodes -out "$decrypted_certificate"
security_with_stdin "$MACOS_SIGNING_KEYCHAIN_PASSWORD" create-keychain "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security_with_stdin "$MACOS_SIGNING_KEYCHAIN_PASSWORD" unlock-keychain "$keychain"
security import "$decrypted_certificate" -f pemseq -t agg -k "$keychain" -T /usr/bin/codesign
security_with_stdin "$MACOS_SIGNING_KEYCHAIN_PASSWORD" set-key-partition-list \
    -S apple-tool:,apple:,codesign: -s "$keychain" >/dev/null
security list-keychains -d user -s "$keychain"
identity=$(security find-identity -v -p codesigning "$keychain" |
    sed -n 's/.*"\(Developer ID Application: [^"]*\)".*/\1/p' | head -n 1)
[[ -n $identity ]] || { echo "no Developer ID Application identity found in certificate" >&2; exit 1; }
{
    printf 'MACOS_SIGNING_KEYCHAIN=%s\n' "$keychain"
    printf 'APP_CODE_SIGN_IDENTITY=%s\n' "$identity"
} >> "$GITHUB_ENV"
echo 'macOS distribution signing keychain prepared.'
