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
# Preserve timestamps for unchanged bindings, but replace the complete directory
# after a schema change so retired generated files cannot survive.
mkdir -p target
generated=$(mktemp -d target/agent-bindings.XXXXXX)
trap 'rm -rf "$generated"' EXIT
"$target/debug/agent-bindgen" generate "$library" --language swift --language kotlin --out-dir "$generated" --no-format
if [[ ! -d target/agent-bindings ]] || ! diff -qr target/agent-bindings "$generated" >/dev/null; then
    rm -rf target/agent-bindings
    mv "$generated" target/agent-bindings
fi
