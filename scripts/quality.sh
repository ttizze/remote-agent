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
    if [[ $(uname -s) == Darwin ]]; then
        # The Host-owned Chrome: navigation, clicks, concurrent phone and agent input, popups and persistence.
        cargo nextest run --locked --workspace --lib --features agent-core/bindings \
            --run-ignored only -E 'test(=browser::tests::shared_browser_live)'
    fi
    cargo xtask clean-builds --dry-run
fi
if [[ $language == kotlin ]]; then
    ./gradlew :apps:mobile:ktfmtCheck :apps:mobile:detekt :apps:mobile:testDebugUnitTest :apps:mobile:assembleDebug --console=plain
fi
if [[ $language == apple || $language == swift ]]; then
    swiftformat --lint apps/mobile/iosApp/Bex apps/mobile/iosApp/Shared \
        apps/mobile/iosApp/ActivityExtension apps/mobile/iosApp/ShareExtension
    swiftlint lint --strict
    /usr/bin/xcrun swift test --package-path apps/mobile/iosApp --scratch-path target/qa/ios-unit
    scripts/build-agent-ios.sh simulator
    xcodebuild -project apps/mobile/iosApp/Bex.xcodeproj -scheme Bex \
        -configuration Debug -destination 'generic/platform=iOS Simulator' \
        -derivedDataPath target/qa/ios-derived-data -skipPackagePluginValidation \
        CODE_SIGNING_ALLOWED=NO build
fi
