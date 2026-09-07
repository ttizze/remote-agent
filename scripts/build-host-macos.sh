#!/bin/sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repository_root"

. "$repository_root/scripts/macos-signing.sh"
bex_resolve_signing_identity

cargo build --package host-daemon --release
/usr/bin/codesign --force --sign "$BEX_CODE_SIGN_IDENTITY" \
  --identifier app.bex.host --timestamp=none target/release/host-daemon
/usr/bin/codesign --verify --strict target/release/host-daemon
