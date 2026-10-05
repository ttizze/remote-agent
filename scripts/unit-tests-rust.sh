#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
build_args=(--locked --workspace --lib --bins --features agent-core/bindings)
if [[ $(uname -s) != Darwin ]]; then
    exec cargo nextest run --no-fail-fast "${build_args[@]}" "$@"
fi

# CoreFoundation scans the executable's directory while discovering system
# proxies. Keep unchanged test binaries out of Cargo's large dependency cache.
# Nextest's build-reuse metadata preserves working directories and test selection.
target_dir=$(cargo metadata --locked --no-deps --format-version 1 | jq -er .target_directory)
run_dir=$(mktemp -d "$target_dir/nextest-units.XXXXXX")
trap 'rm -rf -- "$run_dir"' EXIT
runtime="$run_dir/runtime"
cargo nextest list "${build_args[@]}" --list-type binaries-only --message-format json > "$run_dir/binaries.json"
cargo metadata --locked --format-version 1 --features agent-core/bindings > "$run_dir/cargo.json"
while IFS= read -r binary; do
    relative=${binary#"$target_dir/"}
    [[ $relative != "$binary" ]] || { echo 'Test binary is outside the Cargo target directory.' >&2; exit 1; }
    mkdir -p "$runtime/$(dirname "$relative")"
    ln "$binary" "$runtime/$relative"
done < <(jq -r '
    .["rust-build-meta"]["target-directory"] as $root |
    [.["rust-binaries"][]["binary-path"],
     (.["rust-build-meta"]["non-test-binaries"][][] | $root + "/" + .path)] |
    unique[]' "$run_dir/binaries.json")
# The process owner locates its companion beside the executable. Native link
# paths and build-script outputs retain the original, unchanged build artifacts.
ln "$target_dir/debug/bex-provider-supervisor" "$runtime/debug/bex-provider-supervisor"
ln -s "$target_dir/debug/build" "$runtime/debug/build"
cargo nextest run --no-fail-fast --cargo-metadata "$run_dir/cargo.json" \
    --binaries-metadata "$run_dir/binaries.json" --target-dir-remap "$runtime" "$@"
