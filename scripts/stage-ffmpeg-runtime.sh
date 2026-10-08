#!/usr/bin/env bash
# Copy the pinned FFmpeg/libvpx runtime beside a release executable.
#
# The source executable is supplied by AGENT_FFMPEG_EXECUTABLE when a release
# job has an explicit runtime artifact.  Otherwise it is resolved from PATH;
# the release Nix shells put the lockfile-pinned FFmpeg there.  The resulting
# directory is self-contained: no Nix store, Homebrew prefix, or PATH entry is
# needed by a shipped Host or desktop executable.
set -euo pipefail

[[ $# -eq 2 ]] || {
    echo 'usage: stage-ffmpeg-runtime.sh platform destination-dir' >&2
    exit 2
}
platform=$1
destination=$2
case "$platform" in
    linux|windows|macos) ;;
    *) echo "unsupported FFmpeg platform: $platform" >&2; exit 2 ;;
esac

source=${AGENT_FFMPEG_EXECUTABLE:-}
if [[ -z $source ]]; then
    source=$(command -v ffmpeg || true)
fi
if [[ $source =~ ^[A-Za-z]:[\\/].* ]]; then
    command -v cygpath >/dev/null 2>&1 || {
        echo 'Windows FFmpeg paths require cygpath when staged from a POSIX shell' >&2
        exit 1
    }
    source=$(cygpath -u "$source")
fi
[[ -n $source && -f $source ]] || {
    echo 'a pinned FFmpeg executable is required (set AGENT_FFMPEG_EXECUTABLE or provide ffmpeg on PATH)' >&2
    exit 1
}
source=$(cd "$(dirname "$source")" && pwd -P)/$(basename "$source")
runtime_dir=${AGENT_FFMPEG_RUNTIME_DIR:-$(cd "$(dirname "$source")/.." && pwd -P)}
if [[ $runtime_dir =~ ^[A-Za-z]:[\\/].* ]]; then
    command -v cygpath >/dev/null 2>&1 || {
        echo 'Windows FFmpeg paths require cygpath when staged from a POSIX shell' >&2
        exit 1
    }
    runtime_dir=$(cygpath -u "$runtime_dir")
fi
[[ -d $runtime_dir ]] || {
    echo "FFmpeg runtime directory does not exist: $runtime_dir" >&2
    exit 1
}
project_root=$(cd "$(dirname "$0")/.." && pwd -P)
license_dir=${AGENT_FFMPEG_LICENSE_DIR:-$project_root/third_party/ffmpeg}
[[ -d $license_dir ]] || {
    echo "FFmpeg license inventory directory does not exist: $license_dir" >&2
    exit 1
}
license_inventory_file=${AGENT_FFMPEG_LICENSE_INVENTORY:-$project_root/third_party/ffmpeg/components.tsv}
[[ -f $license_inventory_file ]] || {
    echo "FFmpeg component license inventory does not exist: $license_inventory_file" >&2
    exit 1
}

declare -A component_license=()
declare -A component_spdx=()
declare -A component_source=()
while IFS=$'\t' read -r component license spdx source_url; do
    [[ -n $component || -n $license || -n $spdx || -n $source_url ]] || continue
    [[ $component == \#* ]] && continue
    [[ -n $component && -n $license && -n $spdx && -n $source_url ]] || {
        echo "invalid FFmpeg component license inventory row" >&2
        exit 1
    }
    [[ $source_url == https://* ]] || {
        echo "FFmpeg component source must use HTTPS: $component" >&2
        exit 1
    }
    [[ $license != /* && $license != *..* ]] || {
        echo "FFmpeg component license path escapes its inventory directory: $component" >&2
        exit 1
    }
    [[ ${component_license[$component]+yes} ]] && {
        echo "duplicate FFmpeg component license inventory row: $component" >&2
        exit 1
    }
    component_license[$component]=$license
    component_spdx[$component]=$spdx
    component_source[$component]=$source_url
done < "$license_inventory_file"

declare -a license_roots=()
declare -a license_root_components=()
declare -A license_root_seen=()
add_license_root() {
    local candidate=$1 component=$2 canonical key
    [[ -d $candidate ]] || return 0
    canonical=$(cd "$candidate" && pwd -P)
    key="$canonical|$component"
    [[ ${license_root_seen[$key]+yes} ]] && return 0
    license_root_seen[$key]=yes
    license_roots+=("$canonical")
    license_root_components+=("$component")
}
component_name_for() {
    local path=$1 rest root_name
    case "$path" in
        */nix/store/*)
            rest=${path#*/nix/store/}
            root_name=${rest%%/*}
            if [[ $root_name =~ ^[a-z0-9]{20,32}-(.+)$ ]]; then
                root_name=${BASH_REMATCH[1]}
            fi
            printf '%s\n' "$root_name"
            ;;
        *)
            printf 'runtime-root\n'
            ;;
    esac
}
package_root_for() {
    local dependency=$1 rest prefix
    case "$dependency" in
        */nix/store/*/*)
            rest=${dependency#*/nix/store/}
            prefix=${dependency%%/nix/store/*}
            if [[ -n $prefix ]]; then
                printf '%s/nix/store/%s\n' "$prefix" "${rest%%/*}"
            else
                printf '/nix/store/%s\n' "${rest%%/*}"
            fi
            ;;
        *)
            printf '%s\n' "$runtime_dir"
            ;;
    esac
}
is_system_linux_path() {
    case "$1" in
        /lib/*|/lib64/*|/usr/lib/*|/usr/lib64/*|/usr/libexec/*) return 0 ;;
        *) return 1 ;;
    esac
}
add_license_root "$runtime_dir" "$(component_name_for "$runtime_dir")"

version_output=$({ "$source" -hide_banner -version 2>&1 || true; } | sed -n '1p')
[[ $version_output == ffmpeg\ version\ * ]] || {
    echo "could not read FFmpeg version from $source" >&2
    exit 1
}
ffmpeg_version=${version_output#ffmpeg version }
ffmpeg_version=${ffmpeg_version%% *}
encoder_output=$({ "$source" -hide_banner -loglevel error -encoders 2>&1 || true; })
grep -Fq 'libvpx-vp9' <<< "$encoder_output" || {
    echo "FFmpeg at $source does not provide the required libvpx-vp9 encoder" >&2
    exit 1
}

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

mkdir -p "$destination"
[[ ! -e "$destination/ffmpeg" && ! -e "$destination/ffmpeg.exe" ]] || {
    echo "FFmpeg destination already contains an executable: $destination" >&2
    exit 1
}
temporary=$(mktemp -d "$destination/.ffmpeg-runtime.XXXXXX")
cleanup() { rm -rf "$temporary"; }
trap cleanup EXIT
mkdir -p "$temporary/lib"
launcher_type=direct
loader_name=

if [[ $platform == linux ]]; then
    command -v ldd >/dev/null 2>&1 || { echo 'Linux FFmpeg packaging requires ldd' >&2; exit 1; }
    command -v patchelf >/dev/null 2>&1 || { echo 'Linux FFmpeg packaging requires patchelf' >&2; exit 1; }
    cp -L "$source" "$temporary/ffmpeg-bin"

    declare -A copied_linux=()
    declare -A copied_linux_names=()
    queue=("$source")
    while ((${#queue[@]} > 0)); do
        current=${queue[0]}
        queue=("${queue[@]:1}")
        dependencies=$(ldd "$current" 2>&1) || {
            echo "could not inspect Linux FFmpeg dependency closure for $current" >&2
            exit 1
        }
        if grep -Fq 'not found' <<< "$dependencies"; then
            echo "Linux FFmpeg dependency closure is incomplete for $current" >&2
            exit 1
        fi
        while IFS= read -r dependency; do
            [[ -n $dependency && -e $dependency ]] || continue
            dependency_name=$(basename "$dependency")
            destination_path="$temporary/lib/$dependency_name"
            dependency_key=$(printf '%s' "$dependency" | sed 's#/$##')
            [[ ${copied_linux[$dependency_key]+yes} ]] && continue
            copied_linux[$dependency_key]=yes
            if [[ ${copied_linux_names[$dependency_name]+yes} ]]; then
                previous=${copied_linux_names[$dependency_name]}
                cmp -s "$previous" "$dependency" || {
                    echo "Linux FFmpeg dependency basename collision: $previous and $dependency" >&2
                    exit 1
                }
            else
                cp -L "$dependency" "$destination_path"
                copied_linux_names[$dependency_name]=$dependency
            fi
            if ! is_system_linux_path "$dependency"; then
                package_root=$(package_root_for "$dependency")
                add_license_root "$package_root" "$(component_name_for "$package_root")"
            fi
            queue+=("$dependency")
        done < <(awk '
            /=>[[:space:]]*\// { print $3; next }
            /^[[:space:]]*\// { print $1 }
        ' <<< "$dependencies")
    done

    if interpreter=$(patchelf --print-interpreter "$temporary/ffmpeg-bin" 2>/dev/null); then
        interpreter_name=$(basename "$interpreter")
        [[ -f "$temporary/lib/$interpreter_name" ]] || {
            echo "FFmpeg interpreter was not captured: $interpreter" >&2
            exit 1
        }
        patchelf --set-rpath "\$ORIGIN/lib" "$temporary/ffmpeg-bin"
        for library in "$temporary"/lib/*; do
            [[ -f $library ]] || continue
            [[ $(basename "$library") == "$interpreter_name" ]] && continue
            patchelf --set-rpath "\$ORIGIN" "$library"
        done
        cat > "$temporary/ffmpeg" <<EOF
#!/bin/sh
set -eu
here=\$(CDPATH= cd -- "\$(dirname -- "\$0")" && pwd -P)
exec "\$here/lib/$interpreter_name" --library-path "\$here/lib" "\$here/ffmpeg-bin" "\$@"
EOF
        chmod 755 "$temporary/ffmpeg"
        launcher_type=loader
        loader_name=$interpreter_name
    else
        mv "$temporary/ffmpeg-bin" "$temporary/ffmpeg"
    fi
elif [[ $platform == windows ]]; then
    cp -L "$source" "$temporary/ffmpeg.exe"
    declare -A copied_windows_names=()
    while IFS= read -r -d '' library; do
        library_name=$(basename "$library")
        if [[ ${copied_windows_names[$library_name]+yes} ]]; then
            previous=${copied_windows_names[$library_name]}
            cmp -s "$previous" "$library" || {
                echo "Windows FFmpeg dependency basename collision: $previous and $library" >&2
                exit 1
            }
        else
            cp -L "$library" "$temporary/$library_name"
            copied_windows_names[$library_name]=$library
        fi
    done < <(find "$runtime_dir" -type f \( -iname '*.dll' \) -print0)
else
    [[ $(uname -s) == Darwin ]] || { echo 'macOS FFmpeg packaging requires macOS' >&2; exit 2; }
    command -v otool >/dev/null 2>&1 || { echo 'macOS FFmpeg packaging requires otool' >&2; exit 1; }
    command -v install_name_tool >/dev/null 2>&1 || {
        echo 'macOS FFmpeg packaging requires install_name_tool' >&2
        exit 1
    }
    cp -L "$source" "$temporary/ffmpeg"

    is_system_macos_path() {
        case "$1" in
            /System/Library/*|/System/iOSSupport/*|/usr/lib/*|/Library/Apple/System/*) return 0 ;;
            *) return 1 ;;
        esac
    }
    canonical_macos_path() {
        local value=$1
        if [[ $value == /* && -e $value ]]; then
            (cd "$(dirname "$value")" && printf '%s/%s\n' "$(pwd -P)" "$(basename "$value")")
        else
            printf '%s\n' "$value"
        fi
    }
    resolve_macos_dependency() {
        local owner=$1 dependency=$2 candidate rpath
        case "$dependency" in
            /*) printf '%s\n' "$dependency"; return 0 ;;
            @loader_path/*)
                printf '%s/%s\n' "$(dirname "$owner")" "${dependency#@loader_path/}"
                return 0
                ;;
            @executable_path/*)
                printf '%s/%s\n' "$(dirname "$source")" "${dependency#@executable_path/}"
                return 0
                ;;
            @rpath/*)
                while IFS= read -r rpath; do
                    [[ -n $rpath ]] || continue
                    rpath=${rpath//@loader_path/$(dirname "$owner")}
                    rpath=${rpath//@executable_path/$(dirname "$source")}
                    candidate="$rpath/${dependency#@rpath/}"
                    [[ -e $candidate ]] && { printf '%s\n' "$candidate"; return 0; }
                done < <(otool -l "$owner" | awk '$1 == "path" { print $2 }')
                ;;
        esac
        return 1
    }

    declare -A copied_macos=()
    declare -A copied_macos_names=()
    stage_macos_file() {
        local original=$1 owner_destination dependency resolved dependency_name destination_path
        original=$(canonical_macos_path "$original")
        [[ ${copied_macos[$original]+yes} ]] && return 0
        [[ -f $original ]] || { echo "macOS FFmpeg dependency does not exist: $original" >&2; exit 1; }
        dependency_name=$(basename "$original")
        if [[ $original == "$source" ]]; then
            owner_destination="$temporary/ffmpeg"
        else
            owner_destination="$temporary/lib/$dependency_name"
        fi
        copied_macos[$original]=$owner_destination
        if [[ ${copied_macos_names[$dependency_name]+yes} ]]; then
            previous=${copied_macos_names[$dependency_name]}
            cmp -s "$previous" "$original" || {
                echo "macOS FFmpeg dependency basename collision: $previous and $original" >&2
                exit 1
            }
        else
            [[ -e $owner_destination ]] || cp -L "$original" "$owner_destination"
            copied_macos_names[$dependency_name]=$original
        fi
        if ! is_system_macos_path "$original"; then
            package_root=$(package_root_for "$original")
            add_license_root "$package_root" "$(component_name_for "$package_root")"
        fi
        while IFS= read -r dependency; do
            [[ -n $dependency && $dependency != "$original" ]] || continue
            is_system_macos_path "$dependency" && continue
            resolved=$(resolve_macos_dependency "$original" "$dependency") || {
                echo "could not resolve macOS FFmpeg dependency $dependency of $original" >&2
                exit 1
            }
            resolved=$(canonical_macos_path "$resolved")
            is_system_macos_path "$resolved" && continue
            [[ -f $resolved ]] || { echo "macOS FFmpeg dependency does not exist: $resolved" >&2; exit 1; }
            stage_macos_file "$resolved"
            if [[ $original == "$source" ]]; then
                destination_path="@loader_path/lib/$(basename "$resolved")"
            else
                destination_path="@loader_path/$(basename "$resolved")"
            fi
            install_name_tool -change "$dependency" "$destination_path" "$owner_destination"
        done < <(otool -L "$original" | sed -n '2,$' | sed -E 's/^[[:space:]]+([^[:space:]]+).*/\1/')
        if [[ $original != "$source" ]]; then
            install_name_tool -id "@loader_path/$(basename "$original")" "$owner_destination" 2>/dev/null || true
        fi
    }
    stage_macos_file "$source"
fi

license_records="$temporary/.license-records"
: > "$license_records"
register_license() {
    local component=$1 spdx=$2 source_url=$3 license=$4 key
    [[ -s $license ]] || {
        echo "FFmpeg license file is empty or missing: $license" >&2
        exit 1
    }
    key="$component|$license"
    grep -Fqx "$key" "$temporary/.license-record-keys" 2>/dev/null && return 0
    printf '%s\n' "$key" >> "$temporary/.license-record-keys"
    printf '%s\t%s\t%s\t%s\n' "$component" "$spdx" "$source_url" "$license" >> "$license_records"
}
: > "$temporary/.license-record-keys"
license_root_index=0
for root_index in "${!license_roots[@]}"; do
    root=${license_roots[$root_index]}
    component=${license_root_components[$root_index]}
    license_root_index=$((license_root_index + 1))
    root_candidates="$temporary/.license-root-$license_root_index"
    find "$root" -maxdepth 6 -type f \( \
        -iname 'LICENSE*' -o -iname 'COPYING*' -o -iname 'COPYRIGHT*' \
    \) -print | LC_ALL=C sort > "$root_candidates"
    if [[ -s $root_candidates ]]; then
        while IFS= read -r license; do
            [[ -n $license ]] || continue
            register_license "$component" UNKNOWN local-runtime-root "$license"
        done < "$root_candidates"
        continue
    fi

    mapped_license=${component_license[$component]:-}
    [[ -n $mapped_license ]] || {
        echo "unknown unlicensed FFmpeg runtime component: $component ($root)" >&2
        exit 1
    }
    mapped_path="$license_dir/$mapped_license"
    [[ -f $mapped_path ]] || {
        echo "mapped FFmpeg license is missing for $component: $mapped_path" >&2
        exit 1
    }
    register_license "$component" "${component_spdx[$component]}" "${component_source[$component]}" "$mapped_path"
done
[[ -s $license_records ]] || {
    echo "FFmpeg runtime at $runtime_dir has no license or copying file" >&2
    exit 1
}
LC_ALL=C sort -u "$license_records" -o "$license_records"
license_count=0
license_inventory=()
license_manifest_records="$temporary/.license-manifest-records"
: > "$license_manifest_records"
while IFS=$'\t' read -r component spdx source_url license; do
    [[ -n $license ]] || continue
    license_name=$(basename "$license")
    license_count=$((license_count + 1))
    staged_name=$(printf 'FFMPEG-LICENSE-%03d-%s' "$license_count" "$license_name")
    cp -L "$license" "$temporary/$staged_name"
    license_inventory+=("$staged_name")
    printf '%s\t%s\t%s\t%s\n' "$component" "$spdx" "$source_url" "$staged_name" >> "$license_manifest_records"
done < "$license_records"
(( license_count > 0 )) || {
    echo "FFmpeg runtime at $runtime_dir has no license or copying file" >&2
    exit 1
}

source_digest=$(sha256 "$source")
executable_name=ffmpeg
[[ $platform == windows ]] && executable_name+=.exe
{
    printf 'version=%s\n' "$ffmpeg_version"
    printf 'encoder=libvpx-vp9\n'
    printf 'source_sha256=%s\n' "$source_digest"
    printf 'executable=%s\n' "$executable_name"
    printf 'launcher=%s\n' "$launcher_type"
    printf 'loader=%s\n' "${loader_name:-none}"
    printf 'license_files='
    (IFS=,; printf '%s' "${license_inventory[*]}")
    printf '\n'
    license_record_index=0
    while IFS=$'\t' read -r component spdx source_url staged_name; do
        [[ -n $staged_name ]] || continue
        license_record_index=$((license_record_index + 1))
        printf 'license.%d.component=%s\n' "$license_record_index" "$component"
        printf 'license.%d.spdx=%s\n' "$license_record_index" "$spdx"
        printf 'license.%d.source=%s\n' "$license_record_index" "$source_url"
        printf 'license.%d.file=%s\n' "$license_record_index" "$staged_name"
    done < "$license_manifest_records"
    printf 'dependencies='
    find "$temporary/lib" -maxdepth 1 -type f -print \
        | sed "s#^$temporary/##" | LC_ALL=C sort | paste -sd, -
} > "$temporary/FFMPEG-RUNTIME.txt"

for entry in "$temporary"/*; do
    [[ -e $entry ]] || continue
    mv "$entry" "$destination/$(basename "$entry")"
done
printf '%s\n' "$destination/$executable_name"
