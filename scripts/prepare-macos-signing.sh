#!/usr/bin/env bash
# Import a distribution certificate into an ephemeral CI keychain.
set -euo pipefail

: "${MACOS_SIGNING_CERTIFICATE_BASE64:?MACOS_SIGNING_CERTIFICATE_BASE64 is required}"
: "${MACOS_SIGNING_CERTIFICATE_PASSWORD:?MACOS_SIGNING_CERTIFICATE_PASSWORD is required}"
: "${MACOS_SIGNING_KEYCHAIN_PASSWORD:?MACOS_SIGNING_KEYCHAIN_PASSWORD is required}"
: "${RUNNER_TEMP:?RUNNER_TEMP is required on CI}"

keychain="$RUNNER_TEMP/macos-distribution.keychain-db"
certificate="$RUNNER_TEMP/macos-distribution.p12"
if base64 --decode </dev/null >/dev/null 2>&1; then
    printf '%s' "$MACOS_SIGNING_CERTIFICATE_BASE64" | base64 --decode > "$certificate"
else
    printf '%s' "$MACOS_SIGNING_CERTIFICATE_BASE64" | base64 -D > "$certificate"
fi
security create-keychain -p "$MACOS_SIGNING_KEYCHAIN_PASSWORD" "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$MACOS_SIGNING_KEYCHAIN_PASSWORD" "$keychain"
security import "$certificate" -k "$keychain" -P "$MACOS_SIGNING_CERTIFICATE_PASSWORD" -T /usr/bin/codesign
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k \
    "$MACOS_SIGNING_KEYCHAIN_PASSWORD" "$keychain" >/dev/null
security list-keychains -d user -s "$keychain"
identity=$(security find-identity -v -p codesigning "$keychain" |
    sed -n 's/.*"\(Developer ID Application: [^"]*\)".*/\1/p' | head -n 1)
[[ -n $identity ]] || { echo "no Developer ID Application identity found in certificate" >&2; exit 1; }
{
    printf 'MACOS_SIGNING_KEYCHAIN=%s\n' "$keychain"
    printf 'APP_CODE_SIGN_IDENTITY=%s\n' "$identity"
} >> "$GITHUB_ENV"
echo 'macOS distribution signing keychain prepared.'
