#!/usr/bin/env bash
# Runs only owned Simulator/Host fixtures; never accesses physical devices.
set -euo pipefail
cd "$(dirname "$0")/.."
[[ $(uname -s) == Darwin && $(uname -m) == arm64 ]] || {
    echo 'iOS E2E requires an Apple Silicon Mac with Xcode' >&2; exit 2
}
if [[ $# == 0 ]]; then
    set -- \
        testSimulatorEditsHostWorktreeSettingsFromTaskMenu \
        testSimulatorSearchesFromBottomBarAndCreatesInCollapsedProject \
        testSimulatorLoadsLatestFiveTitlesPerProjectAndExpandsOneProject \
        testSimulatorPaginatesRecentProjectsAndUnassignedChats \
        testSimulatorShowsWorkspaceConversationInsideItsProject \
        testSimulatorFetchesNewTaskWhenReturningToList \
        testSimulatorFetchesNewTaskAfterForeground \
        testSimulatorKeepsOpenTaskAndFetchesLatestReplyAfterForeground \
        testSimulatorReconnectClearsHistoryFailureAndPreservesDraft \
        testSimulatorUpdatesAnOpenConversationFromAnotherClient \
        testSimulatorReviewsTheOpenSessionsWorktree \
        testSimulatorStartsOnListAndPreservesDetailOnForeground \
        testSimulatorSwitchesCodexAccountsAndForksConversation \
        testSimulatorReturnsToListWithNativeEdgeSwipeAndRetainsDrafts \
        testSimulatorRepeatedlyReopensTasksAndNewDraftsAfterBackNavigation \
        testSimulatorRepeatedlyReopensTaskAfterContentSwipe \
        testSimulatorUsesNativeHostNavigationAndPairingDismissal \
        testSimulatorRemovesHostAndRequiresPairingAfterRelaunch \
        testSimulatorUsesNativeProjectDisclosureAndDirectoryNavigation \
        testSimulatorCanStartAConversationInAProject \
        testSimulatorKeepsChatUnassignedAfterSendingAndReopening \
        testSimulatorMarksUnseenCompletionUntilOpened \
        testSimulatorDictationPermissionDenialPreservesDraftAndSend \
        testSimulatorDictationContinuesPastThirtySecondsAndReachesHost \
        testSimulatorCopiesOwnMessageIntoComposer \
        testSimulatorCopiesOnlySelectedMessageText \
        testSimulatorSelectsAssistantTextInPlaceAndAddsOnlySelectionToDraft \
        testSimulatorAsksAboutAssistantSelectionInSideChatAndRestoresOriginalDraft \
        testSimulatorRetriesSideChatPreparationWithoutLosingOriginalDraft \
        testSimulatorGroupsLiveCommandsBetweenCommentaryAndExpandsOnTap \
        testSimulatorApprovalEditorAndDraftSurviveReconnect \
        testSimulatorKeepsInputRequestVisibleUntilResolved \
        testSimulatorShowsRetryingStreamErrorThenRecovers \
        testSimulatorKeepsFailedWorkCollapsedWithVisibleTerminalError \
        testSimulatorKeepsInterruptedWorkCollapsed \
        testSimulatorRendersEveryActivityFamilyAndHidesStateOnlyItems \
        testSimulatorKeepsResponsesFromRepeatedTurnIDsWhenReopeningHistory \
        testSimulatorReopensCompletedHistoryCollapsed \
        testSimulatorKeepsEarlierAnswersBetweenFollowupsWhenReopening \
        testSimulatorKeepsDraftDuringLongMarkdownStreamAndReopensFinalText \
        testSimulatorOpensLongInterruptedHistoryAtLatestMessage \
        testSimulatorKeepsSmallOlderScrollDuringLiveUpdate \
        testSimulatorCanSteerAndStopAnActiveTurn \
        testSimulatorShowsAcceptedAdditionalInputBeforeCodexProcessesIt \
        testSimulatorCanAttachDownloadAndPrepareAIEdit \
        testSimulatorCanAddASecondPhoto \
        testSimulatorCanAttachPhotosAndVideos \
        testSimulatorDisplaysImagesInMessagesAndMarkdownAfterReopening \
        testSimulatorShowsGeneratedImagesAndOpensFileLinksAfterReopening
fi
for test in "$@"; do
    [[ $test == testSimulator* ]] || { echo 'Only isolated Simulator tests are allowed' >&2; exit 2; }
done
target=$(cargo metadata --no-deps --format-version 1 | jq -er .target_directory)
mkdir -p "$target/qa"
build=$(mktemp -d "$target/qa/ios-build.XXXXXX")
fixture=$(mktemp -d /tmp/bex-ios.XXXXXX)
simulator=''
host=''
pairing=''
# Job control assigns every background job its own process group, including
# descendants. The final group kill covers abrupt fixture shutdown failures.
set -m
cleanup() {
    result=$?
    for pid in "$pairing" "$host"; do
        if [[ -n $pid ]]; then
            kill -INT "$pid" 2>/dev/null || true
            for ((attempt=0; attempt<100; attempt++)); do
                kill -0 "$pid" 2>/dev/null || break
                sleep 0.1
            done
            kill -KILL -- "-$pid" 2>/dev/null || true
            wait "$pid" 2>/dev/null || true
        fi
    done
    if [[ -n $simulator ]]; then
        xcrun simctl shutdown "$simulator" >/dev/null 2>&1 || true
        xcrun simctl delete "$simulator" || result=1
    fi
    rm -rf "$fixture" "$build"
    exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
scripts/build-agent-ios.sh simulator
cargo build --locked --package host-fixture --bin bex-ui-fixture --bin bex-codex-fixture --bin bex-pairing-fixture
"$target/debug/bex-ui-fixture" "$fixture/host" "$target/debug/bex-codex-fixture" 8000 >"$fixture/host.log" 2>&1 &
host=$!
state="$fixture/host/state"
for ((attempt=0; attempt<300; attempt++)); do
    [[ -f $state/host.ticket && -f $state/local.key ]] && break
    kill -0 "$host" 2>/dev/null || { echo 'UI fixture exited before publishing its identity' >&2; exit 1; }
    sleep 0.1
done
[[ -f $state/host.ticket && -f $state/local.key ]] || { echo 'Host identity timed out' >&2; exit 1; }
"$target/debug/bex-pairing-fixture" "$state" "$fixture/pairing.port" >"$fixture/pairing.log" 2>&1 &
pairing=$!
for ((attempt=0; attempt<300; attempt++)); do
    [[ -s $fixture/pairing.port ]] && break
    kill -0 "$pairing" 2>/dev/null || { echo 'Pairing fixture exited' >&2; exit 1; }
    sleep 0.1
done
[[ -s $fixture/pairing.port ]] || { echo 'Pairing fixture timed out' >&2; exit 1; }
runtime=$(xcrun simctl list runtimes -j | jq -er '[.runtimes[] | select(.isAvailable and .platform == "iOS")][0].identifier')
simulator=$(xcrun simctl create 'Bex isolated E2E' com.apple.CoreSimulator.SimDeviceType.iPhone-17 "$runtime")
xcrun simctl boot "$simulator"
xcrun simctl bootstatus "$simulator" -b
xcrun swift apps/mobile/iosApp/BexUITests/Fixtures/create-video.swift "$fixture/attachment-video.mov"
xcrun simctl addmedia "$simulator" apps/mobile/iosApp/Bex/Assets.xcassets/AppIcon.appiconset/AppIcon.png "$fixture/attachment-video.mov"
result_bundle=${BEX_RELAY_RESULT_BUNDLE:-"$target/qa/Bex-$(date +%s)-$$.xcresult"}
xcodebuild -project apps/mobile/iosApp/Bex.xcodeproj -scheme Bex -sdk iphonesimulator \
    -configuration Debug -derivedDataPath "$build" CODE_SIGNING_ALLOWED=YES \
    CODE_SIGN_IDENTITY=- CODE_SIGNING_REQUIRED=YES BEX_CARGO_TARGET_DIR="$target" build-for-testing
products="$build/Build/Products"
xcrun simctl install "$simulator" "$products/Debug-iphonesimulator/Bex.app"
xcrun simctl privacy "$simulator" grant photos-add dev.remoteagent.mobile.ios
container=$(xcrun simctl get_app_container "$simulator" dev.remoteagent.mobile.ios data)
mkdir -p "$container/Documents"
printf 'Isolated attachment upload fixture.\n' >"$container/Documents/attachment-fixture.txt"
runs=("$products/"*.xctestrun)
[[ ${#runs[@]} == 1 && -f ${runs[0]} ]] || { echo 'Expected exactly one xctestrun file' >&2; exit 1; }
# plistlib preserves non-string values and existing target environments.
python3 - "${runs[0]}" "http://127.0.0.1:$(cat "$fixture/pairing.port")/pairing" <<'PY'
import plistlib, sys
path, url = sys.argv[1:]
with open(path, 'rb') as file:
    configuration = plistlib.load(file)
def configure(value):
    if isinstance(value, dict):
        if 'TestBundlePath' in value:
            value.setdefault('EnvironmentVariables', {})['BEX_PAIRING_URL'] = url
            return 1
        return sum(configure(child) for child in value.values())
    if isinstance(value, list):
        return sum(configure(child) for child in value)
    return 0
if not configure(configuration):
    raise SystemExit('xctestrun contains no test bundle')
with open(path, 'wb') as file:
    plistlib.dump(configuration, file)
PY
selection=()
for test in "$@"; do selection+=("-only-testing:BexUITests/BexLaunchUITests/$test"); done
test_status=0
xcodebuild -xctestrun "${runs[0]}" -destination "platform=iOS Simulator,id=$simulator" \
    -resultBundlePath "$result_bundle" "${selection[@]}" test-without-building || test_status=$?
summary="${result_bundle%.*}.summary.json"
xcrun xcresulttool get test-results summary --path "$result_bundle" --format json >"$summary"
jq -e --argjson expected "$#" '.passedTests == $expected and .failedTests == 0 and .skippedTests == 0' "$summary" >/dev/null || {
    echo "iOS tests did not pass completely; inspect $summary" >&2; exit 1
}
[[ $test_status == 0 ]] || exit "$test_status"
printf '%s iOS UI tests passed; 0 failed, 0 skipped\n%s\n' "$#" "$result_bundle"
