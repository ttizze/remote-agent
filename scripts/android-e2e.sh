#!/usr/bin/env bash
# Owns the emulator and Host; never uses saved application data or a physical device.
set -euo pipefail
cd "$(dirname "$0")/.."
: "${ANDROID_HOME:?Run through nix develop .#android-test}"
nix develop . --command sh -c 'cargo build --locked -p host-fixture --bins && ./gradlew :apps:mobile:assembleDebug :apps:mobile:assembleDebugAndroidTest'
target=${CARGO_TARGET_DIR:-target}
mkdir -p "$target/qa"
target=$(cd "$target" && pwd -P)
fixture=$(mktemp -d /tmp/bex-android.XXXXXX)
export ANDROID_AVD_HOME="$fixture/avd" ANDROID_USER_HOME="$fixture/user"
mkdir -p "$ANDROID_AVD_HOME" "$ANDROID_USER_HOME"
port_open() { (: <"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
server_port=$((40000 + RANDOM % 15000))
while port_open "$server_port"; do server_port=$((40000 + RANDOM % 15000)); done
export ANDROID_ADB_SERVER_PORT=$server_port
emulator_port=5580
while port_open "$emulator_port" || port_open "$((emulator_port + 1))"; do
    emulator_port=$((emulator_port + 2))
    [[ $emulator_port -le 5682 ]] || { echo 'No free emulator port' >&2; exit 1; }
done
serial="emulator-$emulator_port"
host='' emulator_pid=''
cleanup() {
    result=$?
    if [[ -n $host ]]; then kill -INT "$host" 2>/dev/null || true; wait "$host" || true; fi
    if [[ -n $emulator_pid ]]; then kill "$emulator_pid" 2>/dev/null || true; wait "$emulator_pid" || true; fi
    adb -P "$server_port" kill-server >/dev/null 2>&1 || true
    rm -rf "$fixture"
    exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
case $(uname -m) in arm64|aarch64) abi=arm64-v8a ;; *) abi=x86_64 ;; esac
printf 'no\n' | avdmanager create avd --name bex-test --package "system-images;android-36;google_apis;$abi" --device pixel_7
emulator -avd bex-test -port "$emulator_port" -no-window -no-audio -no-snapshot -gpu swiftshader_indirect >"$fixture/emulator.log" 2>&1 &
emulator_pid=$!
adb -P "$server_port" start-server
for ((attempt=0; attempt<180; attempt++)); do
    [[ $(adb -P "$server_port" -s "$serial" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r') == 1 ]] && break
    kill -0 "$emulator_pid" || { cat "$fixture/emulator.log"; exit 1; }
    sleep 2
done
[[ $(adb -P "$server_port" -s "$serial" shell getprop sys.boot_completed | tr -d '\r') == 1 ]]
"$target/debug/bex-ui-fixture" "$fixture/host" "$target/debug/bex-codex-fixture" "$fixture/pairing.port" 100 >"$fixture/host.log" 2>&1 &
host=$!
for ((attempt=0; attempt<100; attempt++)); do
    [[ -s $fixture/pairing.port ]] && break
    kill -0 "$host" || { cat "$fixture/host.log"; exit 1; }
    sleep 0.1
done
curl --fail --silent --show-error "http://127.0.0.1:$(cat "$fixture/pairing.port")/pairing" >"$fixture/invitation.json"
adb -P "$server_port" -s "$serial" install -r apps/mobile/build/outputs/apk/debug/mobile-debug.apk
adb -P "$server_port" -s "$serial" install -r apps/mobile/build/outputs/apk/androidTest/debug/mobile-debug-androidTest.apk
adb -P "$server_port" -s "$serial" shell run-as dev.remoteagent.mobile mkdir -p cache
adb -P "$server_port" -s "$serial" shell "run-as dev.remoteagent.mobile sh -c 'cat > cache/fixture-invitation.json'" <"$fixture/invitation.json"
result_log="$target/qa/Android-$(date +%s)-$$.log"
adb -P "$server_port" -s "$serial" shell am instrument -w -e cwd "$fixture/host/project" \
    -e class dev.remoteagent.mobile.StorePersistenceTest,dev.remoteagent.mobile.ConversationRecoveryTest \
    dev.remoteagent.mobile.test/androidx.test.runner.AndroidJUnitRunner | tee "$result_log"
grep -qx 'OK (4 tests)' "$result_log"
