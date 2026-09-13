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

# Headless tests of the production iOS Markdown parser.
ios-markdown:
    scripts/test-ios-markdown.sh

# Build and test Store recovery, Markdown and network permission on a fresh Android 17 emulator.
android-e2e:
    cargo build --locked -p host-fixture --bins
    ./gradlew :apps:mobile:assembleDebug :apps:mobile:assembleDebugAndroidTest --console=plain
    nix develop .#android-test --command bash scripts/android-e2e.sh

# Native conversation contracts used by the post-commit Swift check.
conversation-ui:
    scripts/ios-e2e.sh \
        testSimulatorRemovesHostAndRequiresPairingAfterRelaunch \
        testSimulatorRepeatedlyReopensTasksAndNewDraftsAfterBackNavigation \
        testSimulatorRendersEveryActivityFamilyAndHidesStateOnlyItems \
        testSimulatorOpensLongInterruptedHistoryAtLatestMessage \
        testSimulatorRendersMarkdownTableAndReopensIt \
        testSimulatorKeepsDraftDuringLongMarkdownStreamAndReopensFinalText \
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
