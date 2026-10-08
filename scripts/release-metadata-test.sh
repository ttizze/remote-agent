#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

temporary=$(mktemp -d)
cleanup() { rm -rf "$temporary"; }
trap cleanup EXIT

scripts/release-metadata.sh \
    --channel nightly \
    --date 20261008 \
    --run-number 42 \
    --sha abcdef1234567890 \
    --repository example/remote-agent \
    --android-store-url 'https://play.google.com/store/apps/details?id=dev.remoteagent.mobile' \
    --ios-store-url https://testflight.apple.com/join/example \
    --build-time 2026-10-08T00:00:00Z \
    --output "$temporary/nightly.json"
jq -e '
    .schema == 1 and
    .base_version == "0.1.0" and
    .channel == "nightly" and
    .version == "0.1.0-nightly.20261008.42" and
    .tag == "v0.1.0-nightly.20261008.42" and
    .short_commit == "abcdef123456" and
    .manifest == "nightly.json" and
    .update_url == "https://github.com/example/remote-agent/releases/download/v0.1.0-nightly.20261008.42/nightly.json" and
    .native_updates.android.url == "https://play.google.com/store/apps/details?id=dev.remoteagent.mobile" and
    .native_updates.ios.url == "https://testflight.apple.com/join/example"
' "$temporary/nightly.json" >/dev/null

scripts/release-metadata.sh \
    --channel stable \
    --version 1.2.3 \
    --sha abcdef1234567890 \
    --output "$temporary/stable.json"
jq -e '.channel == "stable" and .version == "1.2.3" and .tag == "v1.2.3"' "$temporary/stable.json" >/dev/null

GITHUB_REPOSITORY=example/remote-agent scripts/release-metadata.sh \
    --channel nightly \
    --date 20261008 \
    --run-number 43 \
    --sha abcdef1234567890 \
    --repository '' \
    --output "$temporary/unpublished.json"
jq -e '.update_url == null and .native_updates == {}' "$temporary/unpublished.json" >/dev/null

GITHUB_OUTPUT="$temporary/github-output" scripts/release-metadata.sh \
    --channel preview \
    --date 20261008 \
    --run-number 7 \
    --sha abcdef1234567890 \
    --github-output >/dev/null
jq -R -s 'split("\n") | map(select(length > 0) | split("=") | {key: .[0], value: .[1]}) | from_entries | .channel == "preview" and .version == "0.1.0-preview.20261008.7"' \
    "$temporary/github-output" >/dev/null

printf 'one\n' > "$temporary/one.bin"
printf 'two\n' > "$temporary/two.bin"
scripts/release-manifest.sh "$temporary/nightly.json" "$temporary/manifest.json" \
    "$temporary/one.bin" "$temporary/two.bin"
jq -e '.assets | length == 2 and all(.[]; .sha256 | test("^[0-9a-f]{64}$"))' "$temporary/manifest.json" >/dev/null

mkdir -p "$temporary/target/release"
for binary in host-daemon bex-provider-supervisor Claude-Agent-SDK-LICENSE.md; do
    printf '%s\n' "$binary" > "$temporary/target/release/$binary"
done
scripts/package-host.sh linux x86_64 "$temporary/host" "$temporary/target" >/dev/null
tar -tzf "$temporary/host/host-linux-x86_64.tar.gz" | grep -Fx './host-daemon' >/dev/null
tar -tzf "$temporary/host/host-linux-x86_64.tar.gz" | grep -Fx './Claude-Agent-SDK-LICENSE.md' >/dev/null
tar -tzf "$temporary/host/host-linux-x86_64.tar.gz" | grep -Fx './NODE-RUNTIME-REQUIREMENT.txt' >/dev/null

if scripts/release-metadata.sh --channel invalid >/dev/null 2>&1; then
    echo 'invalid channel unexpectedly succeeded' >&2
    exit 1
fi
if scripts/release-metadata.sh --channel stable --version invalid >/dev/null 2>&1; then
    echo 'invalid stable version unexpectedly succeeded' >&2
    exit 1
fi
for version in 01.2.3 1.2.3-nightly.01 1.2.3-a..b; do
    if scripts/release-metadata.sh --channel stable --version "$version" >/dev/null 2>&1; then
        echo "invalid version unexpectedly succeeded: $version" >&2
        exit 1
    fi
done
for store_url in 'https://' 'http://play.google.com/store/apps/details?id=dev.remoteagent.mobile' 'https://user:password@play.google.com/store/apps/details?id=dev.remoteagent.mobile'; do
    if scripts/release-metadata.sh \
        --channel nightly \
        --date 20261008 \
        --run-number 44 \
        --sha abcdef1234567890 \
        --repository example/remote-agent \
        --android-store-url "$store_url" >/dev/null 2>&1; then
        echo "invalid native store URL unexpectedly succeeded: $store_url" >&2
        exit 1
    fi
done
scripts/signing-preflight-test.sh
echo 'release metadata checks passed'
