#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_INCREMENTAL=0
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- --no-deps -D warnings
cargo deny --locked check --warn unmaintained --hide-inclusion-graph
cargo deny --locked --manifest-path tools/agent-peer/Cargo.toml check --warn unmaintained --hide-inclusion-graph
cargo machete --skip-target-dir crates apps tools/agent-peer
