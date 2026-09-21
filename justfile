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

# Launch the normal build with a separate Host and shared provider accounts.
dev: build-desktop-macos
    #!/usr/bin/env bash
    set -euo pipefail
    accounts="$HOME/Library/Application Support/app.bex.BEX"
    if [[ -f "$accounts/host-instance.json" ]]; then
        accounts=$(/usr/bin/plutil -extract directory raw -o - "$accounts/host-instance.json")
    fi
    target=$(cargo metadata --no-deps --format-version 1 | jq -er .target_directory)
    open -n "$target/Bex.app" --env BEX_ISOLATED_HOST=1 \
        --env "BEX_STATE_DIR=$HOME/Library/Application Support/app.bex.BEX-Dev" \
        --env "BEX_ACCOUNT_STATE_DIR=$accounts"

# Run isolated iOS Simulator tests; reject skipped and missing results.
ios-e2e *tests:
    scripts/ios-e2e.sh "$@"

# Headless tests of the production iOS Markdown parser.
ios-markdown:
    scripts/test-ios-markdown.sh

# Build and test Store recovery, Markdown and network permission on a fresh Android 17 emulator.
android-e2e:
    cargo build --locked -p host-fixture -p codex-app-server -p bex-process --bins
    ./gradlew :apps:mobile:assembleDebug :apps:mobile:assembleDebugAndroidTest --console=plain
    nix develop .#android-test --command bash scripts/android-e2e.sh

# Native conversation contracts used by the post-commit Swift check.
conversation-ui:
    scripts/ios-e2e.sh \
        testSimulatorNativeTerminalRetainsShellAfterReopening \
        testSimulatorNativeTerminalPastesMultilineText \
        testSimulatorNativeTerminalDoesNotDuplicateQueryResponses \
        testSimulatorBrowserIsSeparateFromConversationAndPreservesPage \
        testSimulatorUsesNativeProjectDisclosureAndDirectoryNavigation \
        testSimulatorMarksMergedWorktreesToTheRightOfRunningStatus \
        testSimulatorRemovesHostAndRequiresPairingAfterRelaunch \
        testSimulatorUsesNativeHostNavigationAndPairingDismissal \
        testSimulatorRepeatedlyReopensTasksAndNewDraftsAfterBackNavigation \
        testSimulatorRendersEveryActivityFamilyAndHidesStateOnlyItems \
        testSimulatorOpensLongInterruptedHistoryAtLatestMessage \
        testSimulatorReopensRunningLongHistoryWithoutBlankViewport \
        testSimulatorRendersMarkdownTableAndReopensIt \
        testSimulatorKeepsDraftDuringLongMarkdownStreamAndReopensFinalText \
        testSimulatorKeepsResponsesFromRepeatedTurnIDsWhenReopeningHistory \
        testSimulatorBrowsesAllSessionImagesAndSavesTheSelection \
        testSimulatorCopiesOnlySelectedMessageText \
        testSimulatorSelectsAssistantTextInPlaceAndAddsOnlySelectionToDraft \
        testSimulatorOpensSideChatWithoutLosingOriginalDraft \
        testSimulatorRetriesSideChatPreparationWithoutLosingOriginalDraft \
        testSimulatorCanStartAConversationInAProject \
        testSimulatorOpensTasksBeforeHistoryReadFinishes \
        testSimulatorRetriesAFailedTaskOpenWithoutLosingItsDraft \
        testSimulatorShowsAcceptedAdditionalInputBeforeCodexProcessesIt \
        testSimulatorGroupsLiveCommandsBetweenCommentaryAndExpandsOnTap \
        testSimulatorKeepsFailedWorkCollapsedWithVisibleTerminalError \
        testSimulatorReopensCompletedHistoryCollapsed

# Exercise the real iroh Host through the headless client.
iroh-e2e:
    cargo build --locked --package bex-process --bin bex-provider-supervisor
    cargo test --locked --package host-fixture --test iroh_host

# Run Rust, Kotlin and Swift quality checks, or one selected language.
quality language="all":
    scripts/quality.sh "$1"
