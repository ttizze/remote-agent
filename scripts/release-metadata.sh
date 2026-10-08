#!/usr/bin/env bash
# Resolve release metadata without contacting a package or release service.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
channel=nightly
version_override=
release_date=$(date -u +%Y%m%d)
run_number=0
commit=$(git -C "$root" rev-parse HEAD)
repository=${GITHUB_REPOSITORY:-}
android_store_url=
ios_store_url=
build_time=$(date -u +%Y-%m-%dT%H:%M:%SZ)
output=
github_output=false

usage() {
    cat >&2 <<'EOF'
usage: release-metadata.sh [options]

Options:
  --channel CHANNEL       nightly, preview, or stable (default: nightly)
  --version VERSION       stable version override
  --date YYYYMMDD         prerelease date (default: current UTC date)
  --run-number NUMBER     CI run number (default: 0 for local checks)
  --sha SHA               release commit (default: HEAD)
  --repository OWNER/REPO GitHub repository used for the update URL
  --android-store-url URL configured Play surface for native update links
  --ios-store-url URL     configured TestFlight/App Store surface for native update links
  --build-time ISO8601    build timestamp (default: current UTC time)
  --output PATH            write JSON metadata to PATH
  --github-output         append scalar values to GITHUB_OUTPUT
EOF
    exit 2
}

while (($#)); do
    case "$1" in
        --channel) channel=${2:?missing channel}; shift 2 ;;
        --version) version_override=${2:?missing version}; shift 2 ;;
        --date) release_date=${2:?missing date}; shift 2 ;;
        --run-number) run_number=${2:?missing run number}; shift 2 ;;
        --sha) commit=${2:?missing sha}; shift 2 ;;
        --repository)
            (($# >= 2)) || { echo 'missing repository' >&2; exit 2; }
            repository=$2
            shift 2
            ;;
        --android-store-url) android_store_url=${2:?missing Android store URL}; shift 2 ;;
        --ios-store-url) ios_store_url=${2:?missing iOS store URL}; shift 2 ;;
        --build-time) build_time=${2:?missing build time}; shift 2 ;;
        --output) output=${2:?missing output path}; shift 2 ;;
        --github-output) github_output=true; shift ;;
        -h|--help) usage ;;
        *) echo "unknown option: $1" >&2; usage ;;
    esac
done

case "$channel" in
    nightly|preview|stable) ;;
    *) echo "unsupported release channel: $channel" >&2; exit 1 ;;
esac
[[ $release_date =~ ^[0-9]{8}$ ]] || { echo "invalid release date: $release_date" >&2; exit 1; }
[[ $run_number =~ ^[0-9]+$ ]] || { echo "invalid run number: $run_number" >&2; exit 1; }
[[ $commit =~ ^[0-9a-fA-F]{7,64}$ ]] || { echo "invalid commit SHA: $commit" >&2; exit 1; }
[[ $build_time =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[^[:space:]]+Z$ ]] || {
    echo "invalid build timestamp: $build_time" >&2
    exit 1
}

base_version=$(awk '
    $1 == "version" && $3 ~ /^"[0-9]+\.[0-9]+\.[0-9]+"$/ {
        value = $3
        gsub(/"/, "", value)
        print value
        exit
    }
' "$root/crates/host-daemon/Cargo.toml")
[[ $base_version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
    echo "could not resolve the Host base version" >&2
    exit 1
}

if [[ $channel == stable ]]; then
    version=${version_override:-$base_version}
else
    [[ -z $version_override ]] || {
        echo "--version is only valid for the stable channel" >&2
        exit 1
    }
    version="$base_version-$channel.$release_date.$run_number"
fi
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]] || {
    echo "invalid release version: $version" >&2
    exit 1
}

short_commit=${commit:0:12}
tag="v$version"
manifest="$channel.json"
release_name="Nightly $version ($short_commit)"
if [[ $channel == preview ]]; then
    release_name="Preview $version ($short_commit)"
elif [[ $channel == stable ]]; then
    release_name="Release $version"
fi
update_url=
if [[ -n $repository ]]; then
    update_url="https://github.com/$repository/releases/download/$tag/$manifest"
fi

for store_url in "$android_store_url" "$ios_store_url"; do
    [[ -z $store_url || $store_url =~ ^https://[^[:space:]]+$ ]] || {
        echo "native store URLs must be absolute HTTPS URLs" >&2
        exit 1
    }
done
native_updates='{}'
if [[ -n $android_store_url ]]; then
    native_updates=$(jq -cn --arg url "$android_store_url" '{android: {url: $url}}')
fi
if [[ -n $ios_store_url ]]; then
    native_updates=$(jq -cn --argjson updates "$native_updates" --arg url "$ios_store_url" '$updates + {ios: {url: $url}}')
fi

metadata=$(jq -cn \
    --arg base_version "$base_version" \
    --arg channel "$channel" \
    --arg version "$version" \
    --arg tag "$tag" \
    --arg commit "$commit" \
    --arg short_commit "$short_commit" \
    --arg release_name "$release_name" \
    --arg manifest "$manifest" \
    --arg update_url "$update_url" \
    --argjson native_updates "$native_updates" \
    --arg built_at "$build_time" \
    '{schema: 1, base_version: $base_version, channel: $channel, version: $version, tag: $tag,
      commit: $commit, short_commit: $short_commit, release_name: $release_name,
      manifest: $manifest, update_url: (if $update_url == "" then null else $update_url end),
      native_updates: $native_updates,
      built_at: $built_at}')

if [[ -n $output ]]; then
    mkdir -p "$(dirname "$output")"
    printf '%s\n' "$metadata" > "$output"
else
    printf '%s\n' "$metadata"
fi

if [[ $github_output == true ]]; then
    : "${GITHUB_OUTPUT:?--github-output requires GITHUB_OUTPUT}"
    {
        printf 'base_version=%s\n' "$base_version"
        printf 'channel=%s\n' "$channel"
        printf 'version=%s\n' "$version"
        printf 'tag=%s\n' "$tag"
        printf 'commit=%s\n' "$commit"
        printf 'short_commit=%s\n' "$short_commit"
        printf 'release_name=%s\n' "$release_name"
        printf 'manifest=%s\n' "$manifest"
    } >> "$GITHUB_OUTPUT"
fi
