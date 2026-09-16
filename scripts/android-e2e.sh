#!/usr/bin/env bash
# Run through just android-e2e; owns the emulator, adb server, and Host.
set -euo pipefail
cd "$(dirname "$0")/.."
: "${ANDROID_HOME:?Run through nix develop .#android-test}"
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
emulator -avd "$avd" -report-console "unix:$fixture/console.sock,server,max=30" -no-window -no-audio -no-snapshot -no-boot-anim >"$log.emulator.log" 2>&1 &
emulator_pid=$!
port=$(python3 - "$fixture/console.sock" <<'PY'
import socket, sys, time
with socket.socket(socket.AF_UNIX) as connection:
    connection.settimeout(30)
    for attempt in range(300):
        try:
            connection.connect(sys.argv[1])
            break
        except (FileNotFoundError, ConnectionRefusedError):
            time.sleep(0.1)
    else:
        raise SystemExit("Emulator did not publish its console socket")
    print(int(connection.recv(32)))
PY
)
serial="emulator-$port"
for ((attempt=0; attempt<180; attempt++)); do
    kill -0 "$emulator_pid" 2>/dev/null || { cat "$log.emulator.log" >&2; exit 1; }
    [[ $(adb -P "$server_port" -s "$serial" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r') == 1 ]] && break
    sleep 1
done
[[ $(adb -P "$server_port" -s "$serial" shell getprop sys.boot_completed | tr -d '\r') == 1 ]]
[[ $(adb -P "$server_port" -s "$serial" shell getprop ro.build.version.sdk | tr -d '\r') == 37 ]]
[[ $(adb -P "$server_port" -s "$serial" shell getprop ro.build.version.codename | tr -d '\r') == REL ]]
adb -P "$server_port" -s "$serial" shell input keyevent 82
adb -P "$server_port" -s "$serial" install apps/mobile/build/outputs/apk/debug/mobile-debug.apk
adb -P "$server_port" -s "$serial" install apps/mobile/build/outputs/apk/androidTest/debug/mobile-debug-androidTest.apk
test_status=0
python3 - "$serial" "$log.network.log" "$server_port" <<'PYTHON' || test_status=$?
import pathlib, re, socketserver, subprocess, sys, threading

class Host(socketserver.StreamRequestHandler):
    def handle(self):
        self.wfile.write(b"bex-os-network-ok\n")

with socketserver.TCPServer(('127.0.0.1', 0), Host) as host:
    threading.Thread(target=host.serve_forever, daemon=True).start()
    result = subprocess.run(
        ['adb', '-P', sys.argv[3], '-s', sys.argv[1], 'shell', 'am', 'instrument', '-w', '-r',
         '-e', 'networkPort', str(host.server_address[1]),
         '-e', 'class', 'dev.remoteagent.mobile.NetworkPermissionTest',
         'dev.remoteagent.mobile.test/androidx.test.runner.AndroidJUnitRunner'],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, timeout=180,
    )
    host.shutdown()
pathlib.Path(sys.argv[2]).write_text(result.stdout)
print(result.stdout)
assert result.returncode == 0 and re.search(r'^OK \(1 test\)', result.stdout, re.M), "Android tests did not all pass"
PYTHON
if [[ $test_status != 0 ]]; then
    adb -P "$server_port" -s "$serial" exec-out screencap -p >"$log.startup-failure.png" || true
    adb -P "$server_port" -s "$serial" logcat -d -t 150 -s AndroidRuntime ActivityManager >"$log.startup-failure.log" || true
    exit "$test_status"
fi
adb -P "$server_port" -s "$serial" pull /sdcard/Android/data/dev.remoteagent.mobile/files/network-permission.png "$log.permission.png"
adb -P "$server_port" -s "$serial" pull /sdcard/Android/data/dev.remoteagent.mobile/files/network-permission-granted.png "$log.granted.png"
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
echo "Android API 37: 10 tests passed; $log.network.log, $log.store.log and $log.visualize.log"

adb -P "$server_port" -s "$serial" pull /sdcard/Android/data/dev.remoteagent.mobile/files/visualize-selected.png "$log.visualize-selected.png"
adb -P "$server_port" -s "$serial" pull /sdcard/Android/data/dev.remoteagent.mobile/files/visualize-reopened.png "$log.visualize-reopened.png"
