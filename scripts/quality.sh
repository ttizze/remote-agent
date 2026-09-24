#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
case "${1:-all}" in all|rust|kotlin|swift) language=${1:-all} ;; *) echo 'quality expects rust, kotlin, or swift' >&2; exit 2 ;; esac
export CARGO_INCREMENTAL=0
failed=0
if [[ $language == all || $language == rust ]]; then
    nix build .#agent-peer --no-link || failed=1
    python3 -B -m unittest discover -s scripts/tests || failed=1
    cargo fmt --all --check || failed=1
    cargo clippy --locked --workspace --all-targets -- --no-deps -D warnings || failed=1
    cargo test --locked --features agent-core/bindings -p agent-protocol -p agent-transport -p agent-core -p bex-desktop --lib --bins || failed=1
    cargo test --locked -p agent-cli || failed=1
    cargo build --locked -p bex-process --bin bex-provider-supervisor || failed=1
    cargo test --locked -p host-daemon -p host-fixture --lib --test iroh_host --test browser_bridge --test management --test codex_accounts || failed=1
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
python3 scripts/clean-builds.py || failed=1
exit "$failed"
