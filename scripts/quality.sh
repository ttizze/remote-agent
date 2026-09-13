#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
case "${1:-all}" in all|rust|kotlin|swift) language=${1:-all} ;; *) echo 'quality expects rust, kotlin, or swift' >&2; exit 2 ;; esac
failed=0
if [[ $language == all || $language == rust ]]; then
    cargo fmt --all --check || failed=1
    cargo clippy --locked --workspace --all-targets -- --no-deps -D warnings || failed=1
    cargo test --locked -p agent-core -p bex-desktop --lib --bins || failed=1
fi
if [[ $language == all || $language == kotlin ]]; then
    ./gradlew :apps:mobile:ktfmtCheck :apps:mobile:detekt --continue --console=plain || failed=1
    just android-e2e || failed=1
fi
if [[ $language == all || $language == swift ]]; then
    swiftformat --lint apps/mobile/iosApp/Bex apps/mobile/iosApp/BexUITests apps/desktop/macos || failed=1
    swiftlint lint --strict || failed=1
    just ios-markdown || failed=1
    just conversation-ui || failed=1
fi
exit "$failed"
