#!/usr/bin/env bash
# Remove ephemeral iOS release credentials even when a previous step failed.
set -euo pipefail

runner_temp=${RUNNER_TEMP:-}
[[ -n $runner_temp ]] || exit 0
private_dir="$runner_temp/ios-distribution"
registration="$runner_temp/ios-distribution-keychain"
security_bin=${SECURITY_BIN:-security}
cleanup_status=0
profile_path=${IOS_SIGNING_PROFILE_PATH:-}
if [[ -z $profile_path && -f "$private_dir/profile-path" ]]; then
    profile_path=$(<"$private_dir/profile-path")
fi
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
profile_dir="$HOME/Library/MobileDevice/Provisioning Profiles/"
if [[ -n $profile_path && $profile_path == "$profile_dir"* ]]; then
    rm -f -- "$profile_path"
fi
rm -f "$runner_temp/testflight-upload.log"
if [[ ${ASC_KEY_ID:-} =~ ^[A-Za-z0-9._-]+$ ]]; then
    rm -f "$HOME/.appstoreconnect/private_keys/AuthKey_${ASC_KEY_ID}.p8"
fi
exit "$cleanup_status"
