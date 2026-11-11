#!/usr/bin/env bash
# Apple SDK linking is the project exception to Nix clang; see the mobile build docs.
set -euo pipefail
cd -P "$(dirname "$0")/.."
case "${1:-simulator}" in
    simulator) rust_target=aarch64-apple-ios-sim; sdk=iphonesimulator; swift_target=arm64-apple-ios26.0-simulator; native_platform=ios-simulator ;;
    device) rust_target=aarch64-apple-ios; sdk=iphoneos; swift_target=arm64-apple-ios26.0; native_platform=ios ;;
    *) echo "usage: $0 [simulator|device]" >&2; exit 2 ;;
esac
bash apps/nsh/build-native.sh "$native_platform" release
target=$(cargo metadata --no-deps --format-version 1 | jq -er .target_directory)
CC=/usr/bin/clang CXX=/usr/bin/clang++ \
CARGO_TARGET_AARCH64_APPLE_IOS_LINKER=/usr/bin/clang \
CARGO_TARGET_AARCH64_APPLE_IOS_SIM_LINKER=/usr/bin/clang \
cargo rustc --locked -p agent-ffi --lib --crate-type staticlib --release --target "$rust_target"
bindings="$PWD/target/agent-bindings"
output="$target/$rust_target/release"
# UniFFI reads metadata from static archives without executing target code.
# Generate from the iOS artifact instead of compiling the core again for macOS.
scripts/build-agent-bindings.sh "$output/libagent_ffi.a"
swift_key=$({
    shasum -a 256 "$bindings/AgentCore.swift" "$bindings/AgentCoreFFI.h" "$bindings/AgentCoreFFI.modulemap"
    shasum -a 256 < "$0"
    xcrun --sdk "$sdk" swiftc --version
    xcrun --sdk "$sdk" --show-sdk-build-version
    printf '%s\n' "$swift_target" "$(xcrun --sdk "$sdk" --show-sdk-path)"
} | shasum -a 256 | cut -d ' ' -f 1)
if [[ -f "$output/libAgentCore.a" && -f "$output/AgentCore.swiftmodule" && -f "$output/AgentCore.swiftdoc" \
    && -f "$output/agent-core-swift.sha256" && $(cat "$output/agent-core-swift.sha256") == "$swift_key" ]]; then
    echo 'Reusing unchanged AgentCore Swift bindings'
    exit 0
fi
xcrun --sdk "$sdk" swiftc "$bindings/AgentCore.swift" \
    -parse-as-library -O -emit-library -static -module-name AgentCore \
    -emit-module -emit-module-path "$output/AgentCore.swiftmodule" \
    -o "$output/libAgentCore.a" -I "$bindings" \
    -Xcc "-fmodule-map-file=$bindings/AgentCoreFFI.modulemap" \
    -sdk "$(xcrun --sdk "$sdk" --show-sdk-path)" -target "$swift_target"
printf '%s\n' "$swift_key" > "$output/agent-core-swift.sha256"
