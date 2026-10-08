#!/usr/bin/env bash
# Submit and staple a macOS desktop disk image when Apple credentials exist.
set -euo pipefail

[[ $# -eq 1 ]] || { echo "usage: notarize-macos.sh desktop.dmg" >&2; exit 2; }
dmg=$1
[[ -f $dmg ]] || { echo "disk image does not exist: $dmg" >&2; exit 1; }
: "${APPLE_API_KEY_P8:?APPLE_API_KEY_P8 is required}"
: "${APPLE_API_KEY_ID:?APPLE_API_KEY_ID is required}"
: "${APPLE_API_ISSUER_ID:?APPLE_API_ISSUER_ID is required}"
: "${RUNNER_TEMP:?RUNNER_TEMP is required on CI}"

key_path="$RUNNER_TEMP/AuthKey_${APPLE_API_KEY_ID}.p8"
printf '%s' "$APPLE_API_KEY_P8" > "$key_path"
chmod 600 "$key_path"
xcrun notarytool submit "$dmg" \
    --key "$key_path" \
    --key-id "$APPLE_API_KEY_ID" \
    --issuer "$APPLE_API_ISSUER_ID" \
    --wait
xcrun stapler staple "$dmg"
xcrun stapler validate "$dmg"
if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$dmg" > "$dmg.sha256"
else
    sha256sum "$dmg" > "$dmg.sha256"
fi
echo 'macOS desktop disk image notarized and stapled.'
