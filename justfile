set shell := ["bash", "-euo", "pipefail", "-c"]
set positional-arguments

# List supported development commands.
default:
    @just --list

# Build and verify the certificate-signed Host executable.
build-host-macos:
    scripts/build-macos.sh host

# Build and verify the certificate-signed target/Bex.app.
build-desktop-macos:
    scripts/build-macos.sh desktop

# Run isolated iOS Simulator tests; reject skipped and missing results.
ios-e2e *tests:
    scripts/ios-e2e.sh "$@"

# Run Store and model recovery checks against an owned Android emulator and Host.
android-e2e:
    nix develop .#android-test --command scripts/android-e2e.sh

# Native conversation contracts used by the post-commit Swift check.
conversation-ui:
    scripts/ios-e2e.sh \
        testSimulatorRemovesHostAndRequiresPairingAfterRelaunch \
        testSimulatorRepeatedlyReopensTasksAndNewDraftsAfterBackNavigation \
        testSimulatorRendersEveryActivityFamilyAndHidesStateOnlyItems \
        testSimulatorOpensLongInterruptedHistoryAtLatestMessage \
        testSimulatorKeepsResponsesFromRepeatedTurnIDsWhenReopeningHistory \
        testSimulatorCopiesOwnMessageIntoComposer \
        testSimulatorCopiesOnlySelectedMessageText \
        testSimulatorSelectsAssistantTextInPlaceAndAddsOnlySelectionToDraft \
        testSimulatorAsksAboutAssistantSelectionInSideChatAndRestoresOriginalDraft \
        testSimulatorRetriesSideChatPreparationWithoutLosingOriginalDraft \
        testSimulatorCanStartAConversationInAProject \
        testSimulatorOpensTasksBeforeHistoryReadFinishes \
        testSimulatorRetriesAFailedTaskOpenWithoutLosingItsDraft \
        testSimulatorGroupsLiveCommandsBetweenCommentaryAndExpandsOnTap \
        testSimulatorKeepsFailedWorkCollapsedWithVisibleTerminalError \
        testSimulatorKeepsInterruptedWorkCollapsed \
        testSimulatorReopensCompletedHistoryCollapsed

# Exercise the real iroh Host through the headless client.
iroh-e2e:
    cargo test --locked --package host-fixture --test iroh_host

# Run Rust, Kotlin and Swift quality checks, or one selected language.
quality language="all":
    scripts/quality.sh "$1"
