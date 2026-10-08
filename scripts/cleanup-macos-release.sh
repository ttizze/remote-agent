#!/usr/bin/env bash
# Remove ephemeral macOS release credentials even when a previous step failed.
set -euo pipefail

runner_temp=${RUNNER_TEMP:-}
[[ -n $runner_temp ]] || exit 0
private_dir="$runner_temp/macos-distribution"
registration="$runner_temp/macos-distribution-keychain"
security_bin=${SECURITY_BIN:-security}
cleanup_status=0
if [[ -f $registration ]]; then
    registered_keychain=$(<"$registration")
    if [[ $registered_keychain == "$private_dir/"* ]]; then
        "$security_bin" delete-keychain "$registered_keychain" >/dev/null 2>&1 || cleanup_status=1
    else
        cleanup_status=1
    fi
fi
rm -f "$registration"
rm -rf "$private_dir"
rm -f "$runner_temp/testflight-upload.log"
if [[ ${APPLE_API_KEY_ID:-} =~ ^[A-Za-z0-9._-]+$ ]]; then
    rm -f "$runner_temp/AuthKey_${APPLE_API_KEY_ID}.p8"
fi
exit "$cleanup_status"
