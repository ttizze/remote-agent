set shell := ["bash", "-euo", "pipefail", "-c"]
set positional-arguments

# List supported development commands.
default:
    @just --list

# Run unit and retained integration tests; retired conversation/E2E runners are absent.
unit-tests *targets:
    #!/usr/bin/env bash
    set -uo pipefail
    failed=0
    npm --prefix crates/host-daemon/src/claude/sdk test || failed=1
    cargo build --locked -p bex-process --bin bex-provider-supervisor || failed=1
    cargo nextest run --locked --no-fail-fast --workspace --features agent-core/bindings "$@" &
    rust_pid=$!
    CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target}" cargo test --locked --no-fail-fast --manifest-path tools/agent-peer/Cargo.toml || failed=1
    wait "$rust_pid" || failed=1
    exit "$failed"

# Prune inactive Cargo outputs older than 3 days or over the 32 GiB idle budget.
clean-builds *args:
    cargo xtask clean-builds {{args}}

# Build and verify the certificate-signed Host executable.
build-host-macos:
    scripts/build-macos.sh host

# Build and verify the certificate-signed target/Bex.app.
build-desktop-macos:
    scripts/build-macos.sh desktop

# Launch the normal build with a separate Host and shared provider accounts.
dev: build-desktop-macos
    #!/usr/bin/env bash
    set -euo pipefail
    accounts="$HOME/Library/Application Support/app.bex.BEX"
    if [[ -f "$accounts/host-instance.json" ]]; then
        accounts=$(/usr/bin/plutil -extract directory raw -o - "$accounts/host-instance.json")
    fi
    target=$(cargo metadata --no-deps --format-version 1 | jq -er .target_directory)
    open -n "$target/Bex.app" --env BEX_ISOLATED_HOST=1 \
        --env "BEX_STATE_DIR=$HOME/Library/Application Support/app.bex.BEX-Dev" \
        --env "BEX_ACCOUNT_STATE_DIR=$accounts"

# Archive iOS with the pinned package plugins trusted from the first build.
ios-archive archive-path derived-data-path *args:
    scripts/archive-ios.sh "$@"

# Exercise the production Mac Browser view and WebKit persistence in fresh processes.
macos-e2e:
    cargo test --locked --features agent-core/bindings -p bex-desktop --test chrome_cookie_webview

# Check the local Mac Host/desktop and iPhone client, or one selected language.
quality language="apple":
    scripts/quality.sh "$1"

# Audit diff presentation tests in an isolated copy; extra arguments go to cargo-mutants.
mutants-diff *args:
    mkdir -p target/mutation-diff
    PROPTEST_RNG_SEED=20260925 cargo mutants --package agent-core --file crates/agent-core/src/presentation/diff.rs --output target/mutation-diff --jobs 2 --build-timeout 900 --timeout 60 --cargo-arg=--locked --cargo-arg=--lib "$@" -- presentation::diff::tests

# Verify bounded Git path decoding with the dedicated `nix develop .#kani` environment.
kani-diff *args:
    mkdir -p target/kani-diff
    cargo kani --package agent-core --lib --harness presentation::diff::proofs --output-format terse -Z unstable-options --harness-timeout 120 --export-json target/kani-diff/results.json "$@"
