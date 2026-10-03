#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
case "${1:-apple}" in apple|rust|kotlin|swift) language=${1:-apple} ;; *) echo 'quality expects apple, rust, kotlin, or swift' >&2; exit 2 ;; esac
export CARGO_INCREMENTAL=0
failed=0
if [[ $language == apple || $language == rust ]]; then
    nix build .#agent-peer --no-link || failed=1
    cargo fmt --all --check || failed=1
    cargo clippy --locked --workspace --all-targets -- --no-deps -D warnings || failed=1
    cargo build --locked -p bex-process --bin bex-provider-supervisor || failed=1
    cargo test --locked --no-fail-fast --features agent-core/bindings \
        -p agent-protocol -p agent-transport -p agent-core -p bex-desktop \
        -p agent-cli -p host-daemon -p host-fixture -p xtask \
        --lib --bins --test errors --test iroh --test iroh_host --test browser_bridge \
        --test management --test codex_accounts --test claude --test adapter_conformance \
        --test quality_background --test crate_boundaries --test build_cleanup --test diagnostics || failed=1
fi
if [[ $language == kotlin ]]; then
    ./gradlew :apps:mobile:ktfmtCheck :apps:mobile:detekt --continue --console=plain || failed=1
    just android-e2e || failed=1
fi
if [[ $language == apple || $language == swift ]]; then
    swiftformat --lint apps/mobile/iosApp/Bex apps/mobile/iosApp/BexUITests || failed=1
    swiftlint lint --strict || failed=1
    just ios-markdown || failed=1
    just conversation-ui || failed=1
fi
cargo xtask clean-builds || failed=1
exit "$failed"
