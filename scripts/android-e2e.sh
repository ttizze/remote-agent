#!/usr/bin/env bash
# Run through just android-e2e; owns the emulator, adb server, and Host.
set -euo pipefail
cd "$(dirname "$0")/.."
: "${ANDROID_HOME:?Run through nix develop .#android-test}"
mode=${1:-all}
[[ $mode == all || $mode == terminal ]] || { echo "usage: android-e2e.sh [all|terminal]" >&2; exit 2; }
target=${CARGO_TARGET_DIR:-target}
mkdir -p "$target/qa"
target=$(cd "$target" && pwd -P)
port_open() { (: <"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
server_port=$((40000 + RANDOM % 15000))
while port_open "$server_port"; do server_port=$((40000 + RANDOM % 15000)); done
export ANDROID_ADB_SERVER_PORT=$server_port
fixture=$(mktemp -d /tmp/bex-android.XXXXXX)
export ANDROID_AVD_HOME="$fixture/avd" ANDROID_USER_HOME="$fixture/user"
host='' emulator_pid=''
cleanup() {
    result=$?
    if [[ -n $host ]]; then kill -INT "$host" 2>/dev/null || true; wait "$host" || true; fi
    if [[ -n $emulator_pid ]]; then
        kill "$emulator_pid" 2>/dev/null || true
        wait "$emulator_pid" 2>/dev/null || true
    fi
    adb -P "$server_port" kill-server >/dev/null 2>&1 || true
    rm -rf "$fixture"
    exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
mkdir -p "$ANDROID_AVD_HOME" "$ANDROID_USER_HOME"
adb -P "$server_port" start-server
case $(uname -m) in arm64|aarch64) abi=arm64-v8a ;; *) abi=x86_64 ;; esac
avd="bex-os-$$"
avdmanager create avd --name "$avd" --package "system-images;android-37.0;google_apis;$abi" --device pixel_7 <<< no
log="$target/qa/android-$(date +%s)-$$"
# Headless automatic graphics use CPU rendering on macOS and compete with builds.
gpu=auto
if [[ $(uname) == Darwin ]]; then gpu=host; fi
emulator -avd "$avd" -gpu "$gpu" -report-console "unix:$fixture/console.sock,server,max=30" -no-window -no-audio -no-snapshot -no-boot-anim >"$log.emulator.log" 2>&1 &
emulator_pid=$!
port=$(cargo xtask android-console "$fixture/console.sock")
serial="emulator-$port"
boot_deadline=$((SECONDS + 300))
boot_completed=''
while ((SECONDS < boot_deadline)); do
    kill -0 "$emulator_pid" 2>/dev/null || { cat "$log.emulator.log" >&2; exit 1; }
    boot_completed=$(timeout 5 adb -P "$server_port" -s "$serial" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r') || boot_completed=''
    [[ $boot_completed == 1 ]] && break
    sleep 1
done
[[ $boot_completed == 1 ]] || { echo "Android emulator did not complete boot" >&2; exit 1; }
[[ $(adb -P "$server_port" -s "$serial" shell getprop ro.build.version.sdk | tr -d '\r') == 37 ]]
[[ $(adb -P "$server_port" -s "$serial" shell getprop ro.build.version.codename | tr -d '\r') == REL ]]
adb -P "$server_port" -s "$serial" shell input keyevent 82
adb -P "$server_port" -s "$serial" install apps/mobile/build/outputs/apk/debug/mobile-debug.apk
adb -P "$server_port" -s "$serial" install apps/mobile/build/outputs/apk/androidTest/debug/mobile-debug-androidTest.apk
adb -P "$server_port" -s "$serial" shell am start -W -a android.intent.action.MAIN -c android.intent.category.HOME | tee "$log.home.log"
grep -qx 'Status: ok' "$log.home.log"
if [[ $mode == terminal ]]; then
    adb -P "$server_port" -s "$serial" shell pm grant dev.remoteagent.mobile android.permission.ACCESS_LOCAL_NETWORK
else
test_status=0
cargo xtask android-network-permission "$serial" "$log.network.log" "$server_port" || test_status=$?
adb -P "$server_port" -s "$serial" pull /sdcard/Android/data/dev.remoteagent.mobile/files/network-permission.png "$log.permission.png" || test_status=1
if [[ $test_status != 0 ]]; then
    for extension in png xml txt; do
        failure_file="/sdcard/Android/data/dev.remoteagent.mobile/files/network-permission-failure.$extension"
        if adb -P "$server_port" -s "$serial" shell test -f "$failure_file"; then
            adb -P "$server_port" -s "$serial" pull "$failure_file" "$log.permission-failure.$extension" || true
        fi
    done
    adb -P "$server_port" -s "$serial" exec-out screencap -p >"$log.startup-failure.png" || true
    adb -P "$server_port" -s "$serial" logcat -b main -b system -b crash -d -v threadtime -t 2000 >"$log.startup-failure.log" || true
    exit "$test_status"
fi
adb -P "$server_port" -s "$serial" pull /sdcard/Android/data/dev.remoteagent.mobile/files/network-permission-granted.png "$log.granted.png"
fi
# The permission test starts denied and leaves LAN access granted for the real Host checks.
"$target/debug/bex-ui-fixture" "$fixture/host" "$target/debug/bex-codex-fixture" "$fixture/pairing.port" 100 >"$fixture/host.log" 2>&1 &
host=$!
for ((attempt=0; attempt<100; attempt++)); do
    [[ -s $fixture/pairing.port ]] && break
    kill -0 "$host" || { cat "$fixture/host.log"; exit 1; }
    sleep 0.1
done
prepare_pairing() {
    curl --fail --silent --show-error "http://127.0.0.1:$(cat "$fixture/pairing.port")/pairing" >"$fixture/invitation.json"
    adb -P "$server_port" -s "$serial" shell run-as dev.remoteagent.mobile mkdir -p cache
    adb -P "$server_port" -s "$serial" shell "run-as dev.remoteagent.mobile sh -c 'cat > cache/fixture-invitation.json'" <"$fixture/invitation.json"
}
if [[ $mode == all ]]; then
prepare_pairing
adb -P "$server_port" -s "$serial" shell am instrument -w -e cwd "$fixture/host/project" \
    -e class dev.remoteagent.mobile.StorePersistenceTest,dev.remoteagent.mobile.ConversationRecoveryTest,dev.remoteagent.mobile.MarkdownTableTest,dev.remoteagent.mobile.ConversationNavigationTest,dev.remoteagent.mobile.ThreadListTest \
    dev.remoteagent.mobile.test/androidx.test.runner.AndroidJUnitRunner | tee "$log.store.log"
adb -P "$server_port" -s "$serial" exec-out screencap -p >"$log.final.png"
grep -qx 'OK (8 tests)' "$log.store.log"
# Each paired test gets a fresh single-use invitation and isolated credentials.

prepare_pairing
adb -P "$server_port" -s "$serial" shell am instrument -w -e cwd "$fixture/host/project" \
    -e class dev.remoteagent.mobile.VisualizationTest \
    dev.remoteagent.mobile.test/androidx.test.runner.AndroidJUnitRunner | tee "$log.visualize.log"
adb -P "$server_port" -s "$serial" exec-out screencap -p >"$log.visualize-final.png"
if ! grep -qx 'OK (1 test)' "$log.visualize.log"; then
    adb -P "$server_port" -s "$serial" pull /sdcard/Android/data/dev.remoteagent.mobile/files/visualize-failure.png "$log.visualize-failure.png" || true
    adb -P "$server_port" -s "$serial" pull /sdcard/Android/data/dev.remoteagent.mobile/files/visualize-failure.xml "$log.visualize-failure.xml" || true
    exit 1
fi

adb -P "$server_port" -s "$serial" pull /sdcard/Android/data/dev.remoteagent.mobile/files/visualize-selected.png "$log.visualize-selected.png"
adb -P "$server_port" -s "$serial" pull /sdcard/Android/data/dev.remoteagent.mobile/files/visualize-reopened.png "$log.visualize-reopened.png"
fi

prepare_pairing
adb -P "$server_port" -s "$serial" shell am instrument -w -e cwd "$fixture/host/project" \
    -e class dev.remoteagent.mobile.NativeTerminalTest \
    dev.remoteagent.mobile.test/androidx.test.runner.AndroidJUnitRunner | tee "$log.terminal.log"
if ! grep -qx 'OK (1 test)' "$log.terminal.log"; then
    adb -P "$server_port" -s "$serial" pull /sdcard/Android/data/dev.remoteagent.mobile/files/terminal-failure.png "$log.terminal-failure.png" || true
    adb -P "$server_port" -s "$serial" pull /sdcard/Android/data/dev.remoteagent.mobile/files/terminal-failure.xml "$log.terminal-failure.xml" || true
    exit 1
fi
adb -P "$server_port" -s "$serial" pull /sdcard/Android/data/dev.remoteagent.mobile/files/terminal-input.png "$log.terminal-input.png"
adb -P "$server_port" -s "$serial" pull /sdcard/Android/data/dev.remoteagent.mobile/files/terminal-retained.png "$log.terminal-retained.png"
echo "Android API 37: $mode checks passed; records at $log.*"
