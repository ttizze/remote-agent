#!/usr/bin/env bash
# Decode an Android release keystore into the ephemeral runner directory.
set -euo pipefail

: "${ANDROID_SIGNING_KEYSTORE_BASE64:?ANDROID_SIGNING_KEYSTORE_BASE64 is required}"
: "${RUNNER_TEMP:?RUNNER_TEMP is required on CI}"
keystore="$RUNNER_TEMP/android-release.keystore"
printf '%s' "$ANDROID_SIGNING_KEYSTORE_BASE64" | base64 --decode > "$keystore"
chmod 600 "$keystore"
{
    printf 'ANDROID_RELEASE_KEYSTORE=%s\n' "$keystore"
    printf 'ANDROID_RELEASE_KEY_ALIAS=%s\n' "${ANDROID_RELEASE_KEY_ALIAS:?ANDROID_RELEASE_KEY_ALIAS is required}"
    printf 'ANDROID_RELEASE_KEYSTORE_PASSWORD=%s\n' "${ANDROID_RELEASE_KEYSTORE_PASSWORD:?ANDROID_RELEASE_KEYSTORE_PASSWORD is required}"
    printf 'ANDROID_RELEASE_KEY_PASSWORD=%s\n' "${ANDROID_RELEASE_KEY_PASSWORD:?ANDROID_RELEASE_KEY_PASSWORD is required}"
} >> "$GITHUB_ENV"
echo 'Android release keystore prepared.'
