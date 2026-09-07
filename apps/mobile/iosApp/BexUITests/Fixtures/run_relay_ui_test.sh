#!/bin/sh
set -eu

# Invoke from the repository's Nix shell; no physical devices or saved profiles are used.
fixture_directory=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repository_root=$(CDPATH= cd -- "$fixture_directory/../../../../.." && pwd)
cd "$repository_root"
# Darwin Unix socket paths are limited to 104 bytes; its default TMPDIR is too long.
fixture_root=$(mktemp -d "/tmp/bex-ios.XXXXXX")
chmod 700 "$fixture_root"
fixture_pid=
pairing_pid=
simulator_id=
cleanup() {
    status=$?
    [ -z "$pairing_pid" ] || kill "$pairing_pid" 2>/dev/null || true
    if [ -n "$fixture_pid" ]; then kill -INT "$fixture_pid" 2>/dev/null || true; wait "$fixture_pid" 2>/dev/null || true; fi
    if [ -n "$simulator_id" ]; then xcrun simctl shutdown "$simulator_id" >/dev/null 2>&1 || true; xcrun simctl delete "$simulator_id" >/dev/null 2>&1 || true; fi
    rm -rf "$fixture_root"
    exit "$status"
}
trap cleanup EXIT HUP INT TERM

./gradlew :apps:mobile:iosSimulatorArm64Test :apps:mobile:linkDebugFrameworkIosSimulatorArm64 --console=plain
cargo build -p host-daemon --example ui_fixture
BEX_FAKE_STREAM_DELAY_SECONDS=8 target/debug/examples/ui_fixture "$fixture_root/host" > "$fixture_root/host.log" 2>&1 &
fixture_pid=$!
python3 - "$fixture_root/host/state/host.sock" <<'PY'
import pathlib,sys,time
path=pathlib.Path(sys.argv[1])
for _ in range(300):
    if path.exists(): break
    time.sleep(.1)
else: raise SystemExit('Host did not create its private socket')
PY
python3 scripts/fixtures/serve-pairing-payload.py --state-dir "$fixture_root/host/state" > "$fixture_root/pairing.log" 2>&1 &
pairing_pid=$!
pairing_port=$(python3 - "$fixture_root/pairing.log" <<'PY'
import pathlib,sys,time
path=pathlib.Path(sys.argv[1])
for _ in range(100):
    text=path.read_text() if path.exists() else ''
    if text.startswith('READY '): print(text.split()[1]); break
    time.sleep(.1)
else: raise SystemExit('Pairing fixture did not start')
PY
)
runtime=$(xcrun simctl list runtimes -j | python3 -c 'import json,sys; print(next(r["identifier"] for r in json.load(sys.stdin)["runtimes"] if r["isAvailable"] and r["platform"] == "iOS"))')
simulator_id=$(xcrun simctl create 'Bex isolated E2E' com.apple.CoreSimulator.SimDeviceType.iPhone-17 "$runtime")
xcrun simctl boot "$simulator_id"
xcrun simctl bootstatus "$simulator_id" -b
xcrun swift "$fixture_directory/create-video.swift" "$fixture_root/attachment-video.mov"
xcrun simctl addmedia "$simulator_id" "$repository_root/apps/mobile/iosApp/Bex/Assets.xcassets/AppIcon.appiconset/AppIcon.png" "$fixture_root/attachment-video.mov"
qa_directory="$repository_root/target/qa"
mkdir -p "$qa_directory"
result_bundle=${BEX_RELAY_RESULT_BUNDLE:-"$qa_directory/Bex-$(date +%Y%m%d-%H%M%S).xcresult"}
derived_data="$qa_directory/DerivedData"
xcodebuild -project apps/mobile/iosApp/Bex.xcodeproj -scheme Bex -sdk iphonesimulator -configuration Debug \
    -derivedDataPath "$derived_data" CODE_SIGNING_ALLOWED=YES CODE_SIGN_IDENTITY=- CODE_SIGNING_REQUIRED=YES build-for-testing
xcrun simctl install "$simulator_id" "$derived_data/Build/Products/Debug-iphonesimulator/Bex.app"
app_container=$(xcrun simctl get_app_container "$simulator_id" dev.remoteagent.mobile.ios data)
mkdir -p "$app_container/Documents"
printf 'Isolated attachment upload fixture.\n' > "$app_container/Documents/attachment-fixture.txt"
xctestrun=$(python3 - "$derived_data/Build/Products" "$pairing_port" <<'PY'
import pathlib,plistlib,sys
path=next(pathlib.Path(sys.argv[1]).glob('*.xctestrun'))
data=plistlib.loads(path.read_bytes())
for value in data.values():
    if isinstance(value,dict) and 'TestBundlePath' in value:
        value.setdefault('EnvironmentVariables',{})['BEX_PAIRING_URL']=f'http://127.0.0.1:{sys.argv[2]}/pairing'
path.write_bytes(plistlib.dumps(data))
print(path)
PY
)
if [ "$#" -eq 0 ]; then
    set -- testSimulatorSearchesFromBottomBarAndCreatesInCollapsedProject \
        testSimulatorLoadsLatestFiveTitlesPerProjectAndExpandsOneProject \
        testSimulatorPaginatesRecentProjectsAndUnassignedChats \
        testSimulatorShowsWorkspaceConversationInsideItsProject \
        testSimulatorFetchesNewTaskWhenReturningToList \
        testSimulatorFetchesNewTaskAfterForeground \
        testSimulatorFetchesLatestReplyWhenOpeningTaskAfterForeground \
        testSimulatorUpdatesAnOpenConversationFromAnotherClient \
        testSimulatorOpensListAndKeepsModelAfterRelaunchAndForeground \
        testSimulatorReturnsToListWithNativeEdgeSwipeAndRetainsDrafts \
        testSimulatorUsesNativeHostNavigationAndPairingDismissal \
        testSimulatorUsesNativeProjectDisclosureAndDirectoryNavigation \
        testSimulatorCanStartAConversationInAProject \
        testSimulatorDictationPermissionDenialPreservesDraftAndSend \
        testSimulatorGroupsLiveCommandsBetweenCommentaryAndExpandsOnTap \
        testSimulatorApprovalEditorAndDraftSurviveReconnect \
        testSimulatorKeepsInputRequestVisibleUntilResolved \
        testSimulatorShowsRetryingStreamErrorThenRecovers \
        testSimulatorKeepsFailedWorkCollapsedWithVisibleTerminalError \
        testSimulatorKeepsInterruptedWorkCollapsed \
        testSimulatorRendersEveryActivityFamilyAndHidesStateOnlyItems \
        testSimulatorKeepsResponsesFromRepeatedTurnIDsWhenReopeningHistory \
        testSimulatorReopensCompletedHistoryCollapsed \
        testSimulatorOpensLongInterruptedHistoryAtLatestMessage \
        testSimulatorCanSteerAndStopAnActiveTurn \
        testSimulatorShowsAcceptedAdditionalInputBeforeCodexProcessesIt \
        testSimulatorCanAttachDownloadAndPrepareAIEdit \
        testSimulatorCanAddASecondPhoto \
        testSimulatorCanAttachPhotosAndVideos \
        testSimulatorDisplaysImagesInMessagesAndMarkdownAfterReopening
fi
expected_tests=$#
for method do
    case "$method" in testSimulator*) ;; *) echo "Only isolated simulator tests are allowed" >&2; exit 2 ;; esac
    set -- "$@" "-only-testing:BexUITests/BexLaunchUITests/$method"
    shift
done
xcodebuild -xctestrun "$xctestrun" -destination "platform=iOS Simulator,id=$simulator_id" -resultBundlePath "$result_bundle" \
    "$@" test-without-building || test_status=$?
xcrun xcresulttool get test-results summary --path "$result_bundle" --format json > "$qa_directory/ios-summary.json"
python3 - "$qa_directory/ios-summary.json" "$expected_tests" <<'PY'
import json,sys
s=json.load(open(sys.argv[1]))
counts = {key: s[key] for key in ('passedTests', 'failedTests', 'skippedTests')}
assert s['failedTests']==0 and s['skippedTests']==0 and s['passedTests']==int(sys.argv[2]), counts
print(f"{s['passedTests']} iOS UI tests passed; 0 failed, 0 skipped")
PY
printf '%s\n' "$result_bundle"
