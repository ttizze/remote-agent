#!/usr/bin/env bash
# Runs only owned Simulator/Host fixtures; never accesses physical devices.
set -euo pipefail
cd "$(dirname "$0")/.."
[[ $(uname -s) == Darwin && $(uname -m) == arm64 ]] || {
    echo 'iOS E2E requires an Apple Silicon Mac with Xcode' >&2; exit 2
}
options=()
if [[ ${1:-} == --build-only || ${1:-} == --prebuilt || ${1:-} == --await-build ]]; then
    options+=("$1")
    shift
fi
if [[ ${1:-} == --maestro ]]; then
    options+=(--maestro)
    shift
fi
if [[ ${1:-} == --without-codex ]]; then
    options+=(--without-codex)
    shift
fi
if [[ $# == 0 ]]; then
    set -- \
        testSimulatorWorkbenchStartsEmptyAndOwnsOnlyOpenedTabs \
        testSimulatorBrowserUsesOneSharedImportMenu \
        testSimulatorNativeTerminalRetainsShellAfterReopening \
        testSimulatorNativeTerminalPastesMultilineText \
        testSimulatorNativeTerminalDoesNotDuplicateQueryResponses \
        testSimulatorMarksMergedWorktreesToTheRightOfRunningStatus \
        testSimulatorEditsHostWorktreeSettingsFromTaskMenu \
        testSimulatorSearchesFromBottomBarAndCreatesInCollapsedProject \
        testSimulatorLoadsLatestFiveTitlesPerProjectAndExpandsOneProject \
        testSimulatorPaginatesRecentProjectsAndUnassignedChats \
        testSimulatorFetchesNewTaskWhenReturningToList \
        testSimulatorFetchesNewTaskAfterForeground \
        testSimulatorKeepsOpenTaskAndFetchesLatestReplyAfterForeground \
        testSimulatorShowsHistoryLoadingBeforeHostReplies \
        testSimulatorRetriesAFailedTaskOpenWithoutLosingItsDraft \
        testSimulatorReconnectClearsHistoryFailureAndPreservesDraft \
        testSimulatorUpdatesAnOpenConversationFromAnotherClient \
        testSimulatorReviewsTheOpenSessionsWorktree \
        testSimulatorStartsOnListAndPreservesDetailOnForeground \
        testSimulatorOpensAccountManagementFromSettingsAndModelPicker \
        testSimulatorModelPickerUsesNativeFormAndAgentPicker \
        testSimulatorSignsInDirectlyFromModelSettings \
        testSimulatorGoesBackFromAccountLoginAndCanStartAgain \
        testSimulatorComposerOffersFastModelAndEffortBeforeMicrophone \
        testSimulatorAutomaticallyShowsModelControlsInExistingAndRunningConversations \
        testSimulatorSwitchesCodexAccountsAndForksConversation \
        testSimulatorAddsClaudeAccountAndKeepsCodexSelected \
        testSimulatorRepeatedlyReopensTasksAndNewDraftsAfterBackNavigation \
        testSimulatorUsesNativeHostNavigationAndPairingDismissal \
        testSimulatorOpensQrScannerWhileAddingHost \
        testSimulatorRemovesHostAndRequiresPairingAfterRelaunch \
        testSimulatorUsesNativeProjectDisclosureAndWorkspaceTree \
        testSimulatorCanStartAConversationInAProject \
        testSimulatorKeepsChatUnassignedAfterSendingAndReopening \
        testSimulatorMarksUnseenCompletionUntilOpened \
        testSimulatorDictationPermissionDenialPreservesDraftAndSend \
        testSimulatorDictationContinuesPastThirtySecondsAndReachesHost \
        testSimulatorOpensOnlyTheTappedImageAndSavesIt \
        testSimulatorCopiesSelectedOrWholeUserMessageText \
        testSimulatorSelectsAssistantTextInPlaceAndAddsOnlySelectionToDraft \
        testSimulatorOpensSideChatWithoutLosingOriginalDraft \
        testSimulatorGroupsLiveCommandsBetweenCommentaryAndExpandsOnTap \
        testSimulatorApprovalEditorAndDraftSurviveReconnect \
        testSimulatorKeepsInputRequestVisibleUntilResolved \
        testSimulatorSendsQuestionChoiceOnTap \
        testSimulatorCanSkipQuestion \
        testSimulatorShowsRetryingStreamErrorThenRecovers \
        testSimulatorKeepsFailedWorkCollapsedWithVisibleTerminalError \
        testSimulatorReopensCompletedHistoryCollapsed \
        testSimulatorRendersMarkdownTableAndReopensIt \
        testSimulatorKeepsDraftDuringLongMarkdownStreamAndReopensFinalText \
        testSimulatorOpensLongInterruptedHistoryAtLatestMessage \
        testSimulatorLoadsEarlierHistoryOnTapAtTheReadingPosition \
        testSimulatorReopensRunningLongHistoryWithoutBlankViewport \
        testSimulatorKeepsSmallOlderScrollDuringLiveUpdate \
        testSimulatorShowsAcceptedAdditionalInputBeforeCodexProcessesIt \
        testSimulatorCanAttachDownloadAndPrepareAIEdit \
        testSimulatorCanAddASecondPhoto \
        testSimulatorCanAttachPhotosAndVideos \
        testSimulatorRetriesPhotoUploadAfterStorageRecovery \
        testSimulatorDisplaysImagesInMessagesAndMarkdownAfterReopening \
        testSimulatorShowsGeneratedImagesAndOpensFileLinksAfterReopening
fi
for test in "$@"; do
    [[ $test == testSimulator* ]] || { echo 'Only isolated Simulator tests are allowed' >&2; exit 2; }
done
if [[ ${options[0]:-} == --await-build ]]; then
    exec "$PWD/target/qa/xtask" ios-e2e "${options[@]}" "$@"
fi
if [[ ${options[0]:-} == --prebuilt ]]; then
    exec "$PWD/target/debug/xtask" ios-e2e "${options[@]}" "$@"
fi
exec cargo xtask ios-e2e "${options[@]}" "$@"
