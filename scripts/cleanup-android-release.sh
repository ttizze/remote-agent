#!/usr/bin/env bash
# Remove ephemeral Android release credentials even when a previous step failed.
set -euo pipefail

runner_temp=${RUNNER_TEMP:-}
[[ -n $runner_temp ]] || exit 0
rm -f "$runner_temp/android-release.keystore" "$runner_temp/play-service-account.json"
