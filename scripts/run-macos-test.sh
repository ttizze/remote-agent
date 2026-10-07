#!/usr/bin/env bash
# CFBundle scans the executable's directory when SystemConfiguration starts.
# Cargo's unpacked debug objects make that scan exceed network test deadlines.
# Run the identical inode from a directory containing only this executable.
set -euo pipefail
test_binary=$1
shift
test_name=$(basename "$test_binary")
test_directory=$(dirname "$test_binary")/../native-test-binaries/${NEXTEST_RUN_ID:?}/$test_name
mkdir -p "$test_directory"
for test_source in "$test_binary" "$(dirname "$test_binary")/../bex-provider-supervisor"; do
    [[ -f $test_source ]] || continue
    test_link=$test_directory/$(basename "$test_source")
    if ! ln "$test_source" "$test_link" 2>/dev/null; then
        [[ $test_source -ef $test_link ]]
    fi
done
exec "$test_directory/$test_name" "$@"
