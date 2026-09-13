#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
build=$(mktemp -d /tmp/bex-markdown.XXXXXX)
trap 'rm -rf "$build"' EXIT
scripts/build-agent-bindings.sh
target=$(cargo metadata --no-deps --format-version 1 | jq -er .target_directory)
bindings="$PWD/target/agent-bindings"
xcrun swiftc "$bindings/AgentCore.swift" -emit-library -emit-module -module-name AgentCore \
    -emit-module-path "$build/AgentCore.swiftmodule" -I "$bindings" \
    -Xcc "-fmodule-map-file=$bindings/AgentCoreFFI.modulemap" \
    -L "$target/debug" -lagent_ffi -o "$build/libAgentCore.dylib"
xcrun swiftc apps/mobile/iosApp/Bex/ConversationMarkdownContent.swift \
    apps/mobile/iosApp/BexUITests/Fixtures/markdown-tests.swift \
    -I "$build" -I "$bindings" -Xcc "-fmodule-map-file=$bindings/AgentCoreFFI.modulemap" \
    -L "$build" -lAgentCore -Xlinker -rpath -Xlinker "$build" \
    -Xlinker -rpath -Xlinker "$target/debug" -o "$build/markdown-tests"
"$build/markdown-tests" crates/agent-core/tests/fixtures/markdown/table.md \
    crates/agent-core/tests/fixtures/markdown/document.md
