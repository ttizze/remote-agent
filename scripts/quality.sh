#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
case "${1:-apple}" in apple|rust|kotlin|swift) language=${1:-apple} ;; *) echo 'quality expects apple, rust, kotlin, or swift' >&2; exit 2 ;; esac
[[ $# -le 1 ]] || { echo 'Run native builds and E2E through their explicit just recipes.' >&2; exit 2; }
export CARGO_INCREMENTAL=0
# Native acceptance is owned by CI or an explicitly requested E2E command.
if [[ $language == apple || $language == swift ]]; then
    swiftformat --lint apps/mobile/iosApp/Bex apps/mobile/iosApp/UITests apps/mobile/iosApp/TaskActivity
    swiftlint lint --strict
fi
if [[ $language == apple || $language == rust ]]; then
    if [[ -n ${AGENT_REMOTE_BUILDER-} ]]; then
        exec scripts/remote-run.sh --native "$AGENT_REMOTE_BUILDER" just quality rust
    fi
    actionlint
    cargo fmt --all --check
    nix build .#agent-peer --no-link
    cargo clippy --locked --workspace --all-targets -- --no-deps -D warnings
    integration_targets=(
        --test errors --test iroh --test iroh_host --test browser_bridge
        --test management --test codex_accounts --test claude --test adapter_conformance
        --test crate_boundaries --test diagnostics
    )
    just unit-tests "${integration_targets[@]}"
fi
if [[ $language == kotlin ]]; then
    ./gradlew :apps:mobile:ktfmtCheck :apps:mobile:detekt --continue --console=plain
fi
