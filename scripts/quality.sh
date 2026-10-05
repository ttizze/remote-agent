#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
case "${1:-apple}" in apple|rust|kotlin|swift) language=${1:-apple} ;; *) echo 'quality expects apple, rust, kotlin, or swift' >&2; exit 2 ;; esac
export CARGO_INCREMENTAL=0
failed=0
if [[ $language == apple || $language == rust ]]; then
    actionlint || failed=1
    nix build .#agent-peer --no-link || failed=1
    cargo fmt --all --check || failed=1
    cargo clippy --locked --workspace --all-targets -- --no-deps -D warnings || failed=1
    integration_targets=(
        --test errors --test iroh --test iroh_host --test browser_bridge
        --test management --test codex_accounts --test claude --test adapter_conformance
        --test crate_boundaries --test build_cleanup --test diagnostics
    )
    if [[ $(uname -s) == Darwin ]]; then
        integration_targets+=(--test chrome_cookie_webview)
    fi
    just unit-tests "${integration_targets[@]}" || failed=1
    if [[ $(uname -s) == Darwin ]]; then
        # The Host-owned Chrome: navigation, clicks, concurrent phone and agent input, popups and persistence.
        cargo nextest run --locked --workspace --lib --features agent-core/bindings \
            --run-ignored only -E 'test(=browser::tests::shared_browser_live)' || failed=1
    fi
fi
if [[ $language == kotlin ]]; then
    ./gradlew :apps:mobile:ktfmtCheck :apps:mobile:detekt --continue --console=plain || failed=1
    just android-e2e || failed=1
fi
# Every shard runs acceptance; shared Swift checks run on the first shard only.
if [[ ( $language == apple || $language == swift ) && ${BEX_IOS_TEST_SHARD:-0} == 0 ]]; then
    swiftformat --lint apps/mobile/iosApp/Bex apps/mobile/iosApp/BexUITests || failed=1
    swiftlint lint --strict || failed=1
    if [[ $language == swift ]]; then
        just ios-markdown || failed=1
    fi
fi
if [[ $language == apple || $language == swift ]]; then
    just conversation-ui || failed=1
fi
cargo xtask clean-builds || failed=1
exit "$failed"
