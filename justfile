set shell := ["bash", "-euo", "pipefail", "-c"]
set positional-arguments

# List supported development commands.
default:
    @just --list

# Prune inactive Cargo outputs older than 3 days or over the 32 GiB idle budget.
clean-builds *args:
    python3 scripts/clean-builds.py {{args}}

# Build and verify the certificate-signed Host executable.
build-host-macos:
    scripts/build-macos.sh host

# Build and verify the certificate-signed target/Bex.app.
build-desktop-macos:
    scripts/build-macos.sh desktop

# Run isolated iOS Simulator tests; reject skipped and missing results.
ios-e2e *tests:
    scripts/ios-e2e.sh "$@"

# Build and test only on a fresh, owned Android 17 emulator.
android-e2e:
    ./gradlew :apps:mobile:assembleDebug :apps:mobile:assembleDebugAndroidTest --console=plain
    nix develop .#android-test --command bash scripts/android-e2e.sh

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
