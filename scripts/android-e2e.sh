#!/usr/bin/env bash
# Run through just android-e2e; never installs on an existing emulator or device.
set -euo pipefail
cd "$(dirname "$0")/.."
fixture=$(mktemp -d /tmp/bex-android.XXXXXX)
export ANDROID_AVD_HOME="$fixture/avd" ANDROID_USER_HOME="$fixture/user"
emulator_pid=''
cleanup() {
    result=$?
    if [[ -n $emulator_pid ]]; then
        kill "$emulator_pid" 2>/dev/null || true
        wait "$emulator_pid" 2>/dev/null || true
    fi
    rm -rf "$fixture"
    exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
mkdir -p "$ANDROID_AVD_HOME" "$ANDROID_USER_HOME" target/qa
case $(uname -m) in arm64|aarch64) abi=arm64-v8a ;; *) abi=x86_64 ;; esac
avd="bex-os-$$"
avdmanager create avd --name "$avd" --package "system-images;android-37.0;google_apis;$abi" --device pixel_7 <<< no
log="target/qa/android-$(date +%s)-$$"
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
    [[ $(adb -s "$serial" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r') == 1 ]] && break
    sleep 1
done
[[ $(adb -s "$serial" shell getprop sys.boot_completed | tr -d '\r') == 1 ]]
[[ $(adb -s "$serial" shell getprop ro.build.version.sdk | tr -d '\r') == 37 ]]
[[ $(adb -s "$serial" shell getprop ro.build.version.codename | tr -d '\r') == REL ]]
adb -s "$serial" shell input keyevent 82
adb -s "$serial" install apps/mobile/build/outputs/apk/debug/mobile-debug.apk
adb -s "$serial" install apps/mobile/build/outputs/apk/androidTest/debug/mobile-debug-androidTest.apk
test_status=0
python3 - "$serial" "$log.tests.log" <<'PYTHON' || test_status=$?
import pathlib, re, socketserver, subprocess, sys, threading

class Host(socketserver.StreamRequestHandler):
    def handle(self):
        self.wfile.write(b"bex-os-network-ok\n")

with socketserver.TCPServer(('127.0.0.1', 0), Host) as host:
    threading.Thread(target=host.serve_forever, daemon=True).start()
    result = subprocess.run(
        ['adb', '-s', sys.argv[1], 'shell', 'am', 'instrument', '-w', '-r',
         '-e', 'networkPort', str(host.server_address[1]),
         'dev.remoteagent.mobile.test/androidx.test.runner.AndroidJUnitRunner'],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, timeout=180,
    )
    host.shutdown()
pathlib.Path(sys.argv[2]).write_text(result.stdout)
print(result.stdout)
assert result.returncode == 0 and re.search(r'^OK \(5 tests\)', result.stdout, re.M), "Android tests did not all pass"
PYTHON
adb -s "$serial" pull /sdcard/Android/data/dev.remoteagent.mobile/files/network-permission.png "$log.permission.png"
[[ $test_status == 0 ]] || exit "$test_status"
adb -s "$serial" pull /sdcard/Android/data/dev.remoteagent.mobile/files/network-permission-granted.png "$log.granted.png"
echo "Android API 37: 5 tests passed; $log.tests.log"
