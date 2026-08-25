#!/bin/zsh
set -euo pipefail

script_dir=${0:A:h}
repository_root=${script_dir:h}
cd "$repository_root"

identity=${HOST_CODE_SIGN_IDENTITY:-}
if [[ -z "$identity" ]]; then
  identity_count=$(
    security find-identity -v -p codesigning \
      | awk '/"Apple Development:/{count += 1} END {print count + 0}'
  )
  if [[ "$identity_count" != 1 ]]; then
    print -u2 "Set HOST_CODE_SIGN_IDENTITY because exactly one Apple Development identity was not found."
    exit 1
  fi
  identity=$(
    security find-identity -v -p codesigning \
      | awk '/"Apple Development:/{print $2; exit}'
  )
fi

cargo build --package host-daemon --release
/usr/bin/codesign \
  --force \
  --sign "$identity" \
  --identifier dev.remoteagent.host-daemon \
  --timestamp=none \
  target/release/host-daemon
/usr/bin/codesign --verify --strict --verbose=2 target/release/host-daemon
