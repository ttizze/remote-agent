#!/usr/bin/env bash
# Exercise the portable FFmpeg staging contract without building or launching
# a real release binary. The Linux fixture models a dynamic ELF closure so the
# relative loader wrapper and license inventory are tested in isolation.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
temporary=$(mktemp -d)
cleanup() { rm -rf "$temporary"; }
trap cleanup EXIT

fixture="$temporary/fixture"
tools="$temporary/tools"
mkdir -p "$fixture/bin" "$fixture/lib" "$fixture/collision-a" "$fixture/collision-b" "$tools" "$temporary/linux-out"
cat > "$fixture/bin/ffmpeg" <<'EOF'
#!/usr/bin/env bash
case " $* " in
    *' -version '*) printf '%s\n' 'ffmpeg version 9.0.2-fixture' ;;
    *' -encoders '*) printf '%s\n' ' V..... libvpx-vp9           libvpx VP9' ;;
    *) printf '%s\n' "fixture ffmpeg $*" ;;
esac
EOF
cat > "$fixture/lib/libvpx.so.1" <<'EOF'
fixture libvpx dependency
EOF
cat > "$fixture/lib/ld-linux-fixture" <<'EOF'
#!/usr/bin/env bash
[[ $1 == --library-path ]] || exit 1
shift 2
exec "$@"
EOF
chmod 755 "$fixture/bin/ffmpeg" "$fixture/lib/ld-linux-fixture"
printf '%s\n' 'fixture FFmpeg GPLv3 license' > "$fixture/LICENSE.txt"
printf '%s\n' 'fixture libvpx BSD license' > "$fixture/COPYING.libvpx"

cat > "$tools/ldd" <<'EOF'
#!/usr/bin/env bash
if [[ ${UNRESOLVED:-0} == 1 ]]; then
    printf '%s\n' 'libmissing.so => not found'
    exit 0
fi
if [[ ${COLLISION:-0} == 1 ]]; then
    printf '%s\n' "$FIXTURE_LOADER" "$FIXTURE_COLLISION_A => $FIXTURE_COLLISION_A (0x0000)" "$FIXTURE_COLLISION_B => $FIXTURE_COLLISION_B (0x0000)"
    exit 0
fi
printf '%s\n' "$FIXTURE_LOADER" "$FIXTURE_LIBVPX => $FIXTURE_LIBVPX (0x0000)"
EOF
cat > "$tools/patchelf" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$PATCHELF_LOG"
case "$1" in
    --print-interpreter) printf '%s\n' "$FIXTURE_LOADER" ;;
esac
EOF
chmod 755 "$tools/ldd" "$tools/patchelf"
printf '%s\n' 'collision A' > "$fixture/collision-a/libcollision.so"
printf '%s\n' 'collision B' > "$fixture/collision-b/libcollision.so"

FIXTURE_LOADER="$fixture/lib/ld-linux-fixture" \
FIXTURE_LIBVPX="$fixture/lib/libvpx.so.1" \
PATCHELF_LOG="$temporary/patchelf.log" \
AGENT_FFMPEG_EXECUTABLE="$fixture/bin/ffmpeg" \
AGENT_FFMPEG_RUNTIME_DIR="$fixture" \
AGENT_FFMPEG_LICENSE_DIR="$fixture" \
PATH="$tools:$PATH" \
    "$root/scripts/stage-ffmpeg-runtime.sh" linux "$temporary/linux-out" >/dev/null

[[ -x $temporary/linux-out/ffmpeg && -x $temporary/linux-out/ffmpeg-bin ]]
grep -Fx 'launcher=loader' "$temporary/linux-out/FFMPEG-RUNTIME.txt" >/dev/null
grep -Fx 'loader=ld-linux-fixture' "$temporary/linux-out/FFMPEG-RUNTIME.txt" >/dev/null
grep -F 'license_files=FFMPEG-LICENSE-' "$temporary/linux-out/FFMPEG-RUNTIME.txt" >/dev/null
if grep -F -- '--set-interpreter' "$temporary/patchelf.log" >/dev/null; then
    echo "portable FFmpeg staging attempted to embed an unusable \$ORIGIN interpreter" >&2
    exit 1
fi
[[ $(cd "$temporary/linux-out" && ./ffmpeg -version) == 'ffmpeg version 9.0.2-fixture' ]]
[[ $(find "$temporary/linux-out" -maxdepth 1 -name 'FFMPEG-LICENSE-*' | wc -l) -ge 2 ]]

collision_out="$temporary/collision-out"
mkdir -p "$collision_out"
if COLLISION=1 \
    FIXTURE_LOADER="$fixture/lib/ld-linux-fixture" \
    FIXTURE_COLLISION_A="$fixture/collision-a/libcollision.so" \
    FIXTURE_COLLISION_B="$fixture/collision-b/libcollision.so" \
    PATCHELF_LOG="$temporary/collision-patchelf.log" \
    AGENT_FFMPEG_EXECUTABLE="$fixture/bin/ffmpeg" \
    AGENT_FFMPEG_RUNTIME_DIR="$fixture" \
    AGENT_FFMPEG_LICENSE_DIR="$fixture" \
    PATH="$tools:$PATH" \
    "$root/scripts/stage-ffmpeg-runtime.sh" linux "$collision_out" >/dev/null 2>&1; then
    echo 'FFmpeg staging accepted a conflicting dependency basename' >&2
    exit 1
fi

unresolved_out="$temporary/unresolved-out"
mkdir -p "$unresolved_out"
if UNRESOLVED=1 \
    FIXTURE_LOADER="$fixture/lib/ld-linux-fixture" \
    PATCHELF_LOG="$temporary/unresolved-patchelf.log" \
    AGENT_FFMPEG_EXECUTABLE="$fixture/bin/ffmpeg" \
    AGENT_FFMPEG_RUNTIME_DIR="$fixture" \
    AGENT_FFMPEG_LICENSE_DIR="$fixture" \
    PATH="$tools:$PATH" \
    "$root/scripts/stage-ffmpeg-runtime.sh" linux "$unresolved_out" >/dev/null 2>&1; then
    echo 'FFmpeg staging accepted an unresolved dependency' >&2
    exit 1
fi

empty="$temporary/empty"
mkdir -p "$empty/bin" "$temporary/empty-out"
cp "$fixture/bin/ffmpeg" "$empty/bin/ffmpeg.exe"
chmod 755 "$empty/bin/ffmpeg.exe"
if AGENT_FFMPEG_EXECUTABLE="$empty/bin/ffmpeg.exe" \
    AGENT_FFMPEG_RUNTIME_DIR="$empty" \
    AGENT_FFMPEG_LICENSE_DIR="$empty" \
    "$root/scripts/stage-ffmpeg-runtime.sh" windows "$temporary/empty-out" >/dev/null 2>&1; then
    echo 'FFmpeg staging accepted a runtime without license files' >&2
    exit 1
fi

echo 'FFmpeg runtime checks passed'
