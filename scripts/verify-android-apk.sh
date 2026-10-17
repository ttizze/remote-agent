#!/usr/bin/env bash
set -euo pipefail
apk=$1
expected_min=$2
tools=$(find -L "$ANDROID_HOME/build-tools" -mindepth 1 -maxdepth 1 -type d | sort -V | tail -n1)
badging=$("$tools/aapt" dump badging "$apk")
printf '%s\n' "$badging" > "$(dirname "$apk")/apk-info.txt"
minimum=$(printf '%s\n' "$badging" | awk -F "'" '/^sdkVersion:/{print $2}')
if [[ "$minimum" != "$expected_min" ]]; then
  printf 'APK requires API %s; expected API %s\n' "$minimum" "$expected_min" >&2
  exit 1
fi
"$tools/apksigner" verify --verbose "$apk"
