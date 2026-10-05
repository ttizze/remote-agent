#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
case "${1:-apple}" in apple|rust|kotlin|swift) language=${1:-apple} ;; *) echo 'quality expects apple, rust, kotlin, or swift' >&2; exit 2 ;; esac
export CARGO_INCREMENTAL=0
if [[ $language == apple || $language == rust ]]; then
    actionlint
    cargo fmt --all --check
    cargo clippy --locked --workspace --all-targets --features agent-core/bindings -- -D warnings
    just unit-tests
    cargo nextest run --locked -p xtask --test crate_boundaries --test build_cleanup
fi
if [[ $language == kotlin ]]; then
    ./gradlew :apps:mobile:ktfmtCheck :apps:mobile:assembleDebug --console=plain
fi
if [[ $language == apple || $language == swift ]]; then
    scripts/build-agent-ios.sh simulator
    xcodebuild -project apps/mobile/iosApp/Bex.xcodeproj -scheme Bex \
        -configuration Debug -destination 'generic/platform=iOS Simulator' \
        -derivedDataPath target/qa/ios-derived-data -skipPackagePluginValidation \
        CODE_SIGNING_ALLOWED=NO build
fi
