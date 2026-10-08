#!/usr/bin/env bash
# Add artifact facts to release metadata without changing release state.
set -euo pipefail

[[ $# -ge 2 ]] || {
    echo "usage: release-manifest.sh metadata.json output.json [artifact ...]" >&2
    exit 2
}
metadata=$1
output=$2
shift 2
[[ -f $metadata ]] || { echo "metadata file does not exist: $metadata" >&2; exit 1; }
(( $# > 0 )) || { echo "at least one artifact is required" >&2; exit 1; }

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

assets_file=$(mktemp)
result_file=$(mktemp)
cleanup() { rm -f "$assets_file" "$result_file"; }
trap cleanup EXIT

for artifact in "$@"; do
    [[ -f $artifact ]] || { echo "artifact does not exist: $artifact" >&2; exit 1; }
    size=$(wc -c < "$artifact" | tr -d '[:space:]')
    digest=$(sha256 "$artifact")
    jq -cn --arg name "$(basename "$artifact")" --arg sha256 "$digest" --argjson size "$size" \
        '{name: $name, sha256: $sha256, size: $size}' >> "$assets_file"
done

jq --slurpfile assets <(jq -s . "$assets_file") '. + {assets: $assets[0]}' "$metadata" > "$result_file"
mkdir -p "$(dirname "$output")"
mv "$result_file" "$output"
