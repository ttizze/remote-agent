set shell := ["bash", "-euo", "pipefail", "-c"]
set positional-arguments

# List supported development commands.
default:
    @just --list

# Run all unit tests; CI can add explicit integration targets to the same build.
unit-tests *targets:
    #!/usr/bin/env bash
    set -uo pipefail
    failed=0
    cargo build --locked -p bex-process --bin bex-provider-supervisor || failed=1
    cargo nextest run --locked --no-fail-fast --workspace --lib --bins --features agent-core/bindings "$@" &
    rust_pid=$!
    CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target}" cargo test --locked --no-fail-fast --manifest-path tools/agent-peer/Cargo.toml || failed=1
    if [[ $(uname -s) == Darwin ]]; then
        cargo xtask ios-markdown || failed=1
    fi
    wait "$rust_pid" || failed=1
    exit "$failed"

# Prune inactive Cargo outputs older than 3 days or over the 32 GiB idle budget.
clean-builds *args:
    cargo xtask clean-builds {{args}}

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

# Compare native navigation assertions through the pinned Maestro iOS driver.
ios-maestro:
    scripts/ios-e2e.sh --maestro testSimulatorUsesNativeHostNavigationAndPairingDismissal

# Archive iOS with the pinned package plugins trusted from the first build.
ios-archive archive-path derived-data-path *args:
    scripts/archive-ios.sh "$@"

# Headless tests of the production iOS Markdown parser.
ios-markdown:
    cargo xtask ios-markdown

# Exercise the production Mac Browser view and WebKit persistence in fresh processes.
macos-e2e:
    cargo test --locked --features agent-core/bindings -p bex-desktop --test chrome_cookie_webview

# Build and test Store recovery, Markdown and network permission on a fresh Android 17 emulator.
android-e2e:
    cargo build --locked -p host-fixture -p codex-app-server -p bex-process --bins
    ./gradlew :apps:mobile:assembleDebug :apps:mobile:assembleDebugAndroidTest --console=plain
    nix develop .#android-test --command bash scripts/android-e2e.sh

# Native conversation acceptance; interleaved shards balance measured CI durations.
conversation-ui:
    scripts/ios-e2e.sh \
        testSimulatorModelDefaultsInheritAndPersistAcrossScopes \
        testSimulatorNativeTerminalRetainsShellAfterReopening \
        testSimulatorModelDefaultsPersistAndApplyOnlyToNewConversations \
        testSimulatorGroupsLiveCommandsBetweenCommentaryAndExpandsOnTap \
        testSimulatorRetriesSideChatPreparationWithoutLosingOriginalDraft \
        testSimulatorKeepsDraftDuringLongMarkdownStreamAndReopensFinalText \
        testSimulatorRepeatedlyReopensTasksAndNewDraftsAfterBackNavigation \
        testSimulatorBrowserIsSeparateFromConversationAndPreservesPage \
        testSimulatorUsesNativeHostNavigationAndPairingDismissal \
        testSimulatorKeepsSmallOlderScrollDuringLiveUpdate \
        testSimulatorKeepsResponsesFromRepeatedTurnIDsWhenReopeningHistory \
        testSimulatorKeepsLatestVisibleAcrossRepeatedLongHistorySubmissions \
        testSimulatorUsesNativeProjectDisclosureAndDirectoryNavigation \
        testSimulatorOpensLongInterruptedHistoryAtLatestMessage \
        testSimulatorCopiesOnlySelectedMessageText \
        testSimulatorCanStartAConversationInAProject \
        testSimulatorReopensCompletedHistoryCollapsed \
        testSimulatorSelectsAssistantTextInPlaceAndAddsOnlySelectionToDraft \
        testSimulatorReopensRunningLongHistoryWithoutBlankViewport \
        testSimulatorOpensSideChatWithoutLosingOriginalDraft \
        testSimulatorMarksMergedWorktreesToTheRightOfRunningStatus \
        testSimulatorRendersEveryActivityFamilyAndHidesStateOnlyItems \
        testSimulatorOpensOnlyTheTappedImageAndSavesIt \
        testSimulatorRemovesHostAndRequiresPairingAfterRelaunch \
        testSimulatorRendersMarkdownTableAndReopensIt \
        testSimulatorShowsAcceptedAdditionalInputBeforeCodexProcessesIt \
        testSimulatorKeepsFailedWorkCollapsedWithVisibleTerminalError \
        testSimulatorModelPickerUsesAgentRailAndCompactControls \
        testSimulatorNativeTerminalPastesMultilineText \
        testSimulatorOpensTasksBeforeHistoryReadFinishes \
        testSimulatorNativeTerminalDoesNotDuplicateQueryResponses \
        testSimulatorFillsInitialHistoryViewportWithoutScrolling \
        testSimulatorRetriesAFailedTaskOpenWithoutLosingItsDraft

# Exercise the real iroh Host through the headless client.
iroh-e2e:
    cargo build --locked --package bex-process --bin bex-provider-supervisor
    cargo test --locked --package host-fixture --test iroh_host

# Check the local Mac Host/desktop and iPhone client, or one selected language.
quality language="apple":
    scripts/quality.sh "$1"

# Audit diff presentation tests in an isolated copy; extra arguments go to cargo-mutants.
mutants-diff *args:
    mkdir -p target/mutation-diff
    PROPTEST_RNG_SEED=20260925 cargo mutants --package agent-core --file crates/agent-core/src/presentation/diff.rs --output target/mutation-diff --jobs 2 --build-timeout 900 --timeout 60 --cargo-arg=--locked --cargo-arg=--lib "$@" -- presentation::diff::tests

# Verify bounded Git path decoding with the dedicated `nix develop .#kani` environment.
kani-diff *args:
    mkdir -p target/kani-diff
    cargo kani --package agent-core --lib --harness presentation::diff::proofs --output-format terse -Z unstable-options --harness-timeout 120 --export-json target/kani-diff/results.json "$@"
