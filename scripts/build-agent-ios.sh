#!/usr/bin/env bash
# Apple SDK linking is the project exception to Nix clang; see the mobile build docs.
set -euo pipefail
cd "$(dirname "$0")/.."
case "${1:-simulator}" in
    simulator) rust_target=aarch64-apple-ios-sim; sdk=iphonesimulator; swift_target=arm64-apple-ios15.0-simulator ;;
    device) rust_target=aarch64-apple-ios; sdk=iphoneos; swift_target=arm64-apple-ios15.0 ;;
    *) echo "usage: $0 [simulator|device]" >&2; exit 2 ;;
esac
scripts/build-agent-bindings.sh
CC=/usr/bin/clang CXX=/usr/bin/clang++ \
CARGO_TARGET_AARCH64_APPLE_IOS_LINKER=/usr/bin/clang \
CARGO_TARGET_AARCH64_APPLE_IOS_SIM_LINKER=/usr/bin/clang \
IPHONEOS_DEPLOYMENT_TARGET=15.0 cargo build -p agent-ffi --release --target "$rust_target"
bindings="$PWD/target/agent-bindings"
output="$PWD/target/$rust_target/release"
xcrun --sdk "$sdk" swiftc "$bindings/AgentCore.swift" \
    -parse-as-library -O -emit-library -static -module-name AgentCore \
    -emit-module -emit-module-path "$output/AgentCore.swiftmodule" \
    -o "$output/libAgentCore.a" -I "$bindings" \
    -Xcc "-fmodule-map-file=$bindings/AgentCoreFFI.modulemap" \
    -sdk "$(xcrun --sdk "$sdk" --show-sdk-path)" -target "$swift_target"
