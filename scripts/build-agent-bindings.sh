#!/usr/bin/env bash
# Run inside nix develop. Generated sources stay outside the source tree.
set -euo pipefail
cd "$(dirname "$0")/.."
target=$(cargo metadata --no-deps --format-version 1 | jq -er .target_directory)
case "$(uname -s)" in
    Darwin) library="$target/debug/libagent_ffi.dylib" ;;
    Linux) library="$target/debug/libagent_ffi.so" ;;
    *) library="$target/debug/agent_ffi.dll" ;;
esac
cargo build --locked -p agent-ffi --features bindgen --lib --bin agent-bindgen
# A namespace change must not leave obsolete generated Kotlin beside its replacement.
rm -rf target/agent-bindings
"$target/debug/agent-bindgen" generate "$library" --language swift --language kotlin --out-dir target/agent-bindings --no-format
