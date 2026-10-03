#!/usr/bin/env bash
# Runs only owned Simulator/Host fixtures; never accesses physical devices.
set -euo pipefail
cd "$(dirname "$0")/.."
[[ $(uname -s) == Darwin && $(uname -m) == arm64 ]] || {
    echo 'iOS E2E requires an Apple Silicon Mac with Xcode' >&2; exit 2
}
options=()
if [[ ${1:-} == --without-codex ]]; then
    options+=(--without-codex)
    shift
fi
if [[ $# == 0 ]]; then
    set -- \
        testSimulatorNativeTerminalRetainsShellAfterReopening \
        testSimulatorNativeTerminalPastesMultilineText \
        testSimulatorNativeTerminalDoesNotDuplicateQueryResponses \
        testSimulatorBrowserIsSeparateFromConversationAndPreservesPage \
        testSimulatorMarksMergedWorktreesToTheRightOfRunningStatus \
        testSimulatorEditsHostWorktreeSettingsFromTaskMenu \
        testSimulatorSearchesFromBottomBarAndCreatesInCollapsedProject \
        testSimulatorLoadsLatestFiveTitlesPerProjectAndExpandsOneProject \
        testSimulatorPaginatesRecentProjectsAndUnassignedChats \
        testSimulatorFetchesNewTaskWhenReturningToList \
        testSimulatorFetchesNewTaskAfterForeground \
        testSimulatorKeepsOpenTaskAndFetchesLatestReplyAfterForeground \
        testSimulatorOpensTasksBeforeHistoryReadFinishes \
        testSimulatorRetriesAFailedTaskOpenWithoutLosingItsDraft \
        testSimulatorReconnectClearsHistoryFailureAndPreservesDraft \
        testSimulatorUpdatesAnOpenConversationFromAnotherClient \
        testSimulatorReviewsTheOpenSessionsWorktree \
        testSimulatorStartsOnListAndPreservesDetailOnForeground \
        testSimulatorOpensAccountManagementFromSettingsAndModelPicker \
        testSimulatorModelDefaultsPersistAndApplyOnlyToNewConversations \
        testSimulatorSignsInDirectlyFromModelSettings \
        testSimulatorGoesBackFromAccountLoginAndCanStartAgain \
        testSimulatorComposerOffersFastModelAndEffortBeforeMicrophone \
        testSimulatorAutomaticallyShowsModelControlsInExistingAndRunningConversations \
        testSimulatorSwitchesCodexAccountsAndForksConversation \
        testSimulatorAddsClaudeAccountAndKeepsCodexSelected \
        testSimulatorRepeatedlyReopensTasksAndNewDraftsAfterBackNavigation \
        testSimulatorUsesNativeHostNavigationAndPairingDismissal \
        testSimulatorRemovesHostAndRequiresPairingAfterRelaunch \
        testSimulatorUsesNativeProjectDisclosureAndDirectoryNavigation \
        testSimulatorCanStartAConversationInAProject \
        testSimulatorKeepsChatUnassignedAfterSendingAndReopening \
        testSimulatorMarksUnseenCompletionUntilOpened \
        testSimulatorDictationPermissionDenialPreservesDraftAndSend \
        testSimulatorDictationContinuesPastThirtySecondsAndReachesHost \
        testSimulatorOpensOnlyTheTappedImageAndSavesIt \
        testSimulatorCopiesOnlySelectedMessageText \
        testSimulatorSelectsAssistantTextInPlaceAndAddsOnlySelectionToDraft \
        testSimulatorOpensSideChatWithoutLosingOriginalDraft \
        testSimulatorRetriesSideChatPreparationWithoutLosingOriginalDraft \
        testSimulatorGroupsLiveCommandsBetweenCommentaryAndExpandsOnTap \
        testSimulatorApprovalEditorAndDraftSurviveReconnect \
        testSimulatorKeepsInputRequestVisibleUntilResolved \
        testSimulatorShowsRetryingStreamErrorThenRecovers \
        testSimulatorKeepsFailedWorkCollapsedWithVisibleTerminalError \
        testSimulatorRendersEveryActivityFamilyAndHidesStateOnlyItems \
        testSimulatorKeepsResponsesFromRepeatedTurnIDsWhenReopeningHistory \
        testSimulatorReopensCompletedHistoryCollapsed \
        testSimulatorKeepsEarlierAnswersBetweenFollowupsWhenReopening \
        testSimulatorRendersMarkdownTableAndReopensIt \
        testSimulatorKeepsDraftDuringLongMarkdownStreamAndReopensFinalText \
        testSimulatorOpensLongInterruptedHistoryAtLatestMessage \
        testSimulatorReopensRunningLongHistoryWithoutBlankViewport \
        testSimulatorKeepsSmallOlderScrollDuringLiveUpdate \
        testSimulatorShowsAcceptedAdditionalInputBeforeCodexProcessesIt \
        testSimulatorCanAttachDownloadAndPrepareAIEdit \
        testSimulatorCanAddASecondPhoto \
        testSimulatorCanAttachPhotosAndVideos \
        testSimulatorRetriesPhotoUploadAfterWorkspaceRecovery \
        testSimulatorDisplaysImagesInMessagesAndMarkdownAfterReopening \
        testSimulatorShowsGeneratedImagesAndOpensFileLinksAfterReopening
fi
for test in "$@"; do
    [[ $test == testSimulator* ]] || { echo 'Only isolated Simulator tests are allowed' >&2; exit 2; }
done
exec python3 scripts/ios-e2e.py "${options[@]}" "$@"
