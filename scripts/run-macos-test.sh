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
test_link=$test_directory/$test_name
if ! ln "$test_binary" "$test_link" 2>/dev/null; then
    [[ $test_binary -ef $test_link ]]
fi
exec "$test_link" "$@"
