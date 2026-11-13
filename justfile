set shell := ["bash", "-euo", "pipefail", "-c"]
set positional-arguments

# List supported development commands.
default:
    @just --list

# Check Rust without generating executables; use -p to select changed crates.
[group('portable')]
check *args:
    cargo check --locked "$@"

# Check handwritten functions against the configured complexity limits.
complexity:
    #!/usr/bin/env bash
    set -uo pipefail
    failed=0
    # Two rankings; each invocation enforces the limits in cccc.toml.
    cccc --table --top-cognitive 20 . || failed=1
    cccc --table --top-cyclomatic 20 . || failed=1
    exit "$failed"

# Run platform-independent unit tests on the configured builder.
[group('portable')]
unit-tests *targets:
    #!/usr/bin/env bash
    set -uo pipefail
    failed=0
    just typescript || failed=1
    node --test scripts/*.test.mjs || failed=1
    node --test crates/agent-protocol/src/browser_input.test.cjs crates/host-daemon/src/browser/password-fill.test.cjs apps/native-browser/src/macos/snapshot.test.cjs || failed=1
    npm --prefix crates/host-daemon/src/providers/claude/sdk test || failed=1
    node scripts/build-claude-sdk.ts target/debug || failed=1
    cargo build --locked -p agent-process --bin agent-provider-supervisor || failed=1
    export AGENT_RUNTIME_DIRECTORY="${AGENT_RUNTIME_DIRECTORY:-$PWD/target/debug}"
    cargo nextest run --locked --no-fail-fast --workspace --lib --bins --features agent-core/bindings "$@" &
    rust_pid=$!
    cargo test --locked --no-fail-fast --manifest-path tools/agent-peer/Cargo.toml || failed=1
    wait "$rust_pid" || failed=1
    exit "$failed"

# Check source types and regenerate scripts embedded in native clients.
typescript:
    npm ci --ignore-scripts --no-audit --no-fund
    npm --prefix crates/host-daemon/src/providers/claude/sdk ci --ignore-scripts --no-audit --no-fund
    npm run check
    npm run build

# macOS-only unit tests; CI runs these separately from portable validation.
apple-unit-tests: macos-unit-tests ios-unit-tests

# Drive the Mac app through apps/nsh/acceptance scenarios, each against a fresh fixture Host.
# Only on request: the app activates its window while it runs. Report: target/nsh-acceptance/.
nsh-acceptance *scenarios:
    AGENT_CODE_SIGN_IDENTITY=- bash scripts/build-macos.sh desktop dev
    cargo build --locked -p host-fixture -p agent-process --bins --target aarch64-apple-darwin
    cargo xtask nsh-acceptance "$@"

# Compare the Mac app with the installed T3 Code state by state (apps/nsh/parity/map.toml);
# T3_CODE_APP names the app (the Nightly by default) and T3_CODE_ROOT a checkout with its
# dependencies installed, for Playwright and the Codex schema. `--compare RUN` measures a run
# again. Only on request, as nsh-acceptance. Report: target/nsh-parity/run-*/report.md.
nsh-parity *args:
    AGENT_CODE_SIGN_IDENTITY=- bash scripts/build-macos.sh desktop dev
    cargo build --locked -p host-fixture -p agent-process --bins --target aarch64-apple-darwin
    cargo xtask nsh-parity "$@"

# List worktrees whose build outputs went unused for three days; --delete removes those outputs.
prune-idle-builds *args:
    node scripts/prune-idle-builds.ts "$@"

macos-unit-tests:
    cargo test --locked -p native-focus --test native_focus

ios-unit-tests:
    bash scripts/test-ios-scroll.sh

# Build and verify the certificate-signed Host executable (dev or release).
build-host-macos profile="release":
    scripts/build-macos.sh host "$1"

# Build and verify the certificate-signed Mac app (NSH) in the selected profile directory.
# AGENT_CODE_SIGN_IDENTITY=- signs ad hoc for checks without the BEX Developer ID.
build-desktop-macos profile="release":
    scripts/build-macos.sh desktop "$1"

# Launch a debug build with a separate Host and shared provider accounts.
dev: (build-desktop-macos "dev")
    #!/usr/bin/env bash
    set -euo pipefail
    target=$(cargo metadata --no-deps --format-version 1 | jq -er .target_directory)
    AGENT_ACCOUNT_STATE_DIR="${AGENT_ACCOUNT_STATE_DIR:-${HOME}/Library/Application Support/app.bex.BEX}" "$target/debug/bex.app/Contents/MacOS/bex"

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

# Exercise native pages through the Host using an isolated Mac app window.
macos-e2e:
    AGENT_CODE_SIGN_IDENTITY=- bash scripts/build-macos.sh desktop dev
    AGENT_RUNTIME_DIRECTORY="$PWD/target/debug" cargo nextest run --locked --workspace --lib --bins --test browser_native --features agent-core/bindings --run-ignored only -E 'binary(browser_native) & test(=shared_browser_live)'

# Build and test Store recovery, Markdown and network permission on a fresh Android 17 emulator.
android-e2e:
    cargo build --locked -p host-fixture -p codex-app-server -p agent-process --bins
    ./gradlew :apps:mobile:assembleDebug :apps:mobile:assembleDebugAndroidTest --console=plain
    nix develop .#android-test --command bash scripts/android-e2e.sh

# Native journeys cover navigation, rendering and OS integration; agent-core owns state contracts.
conversation-ui *options:
    scripts/ios-e2e.sh "$@" \
        testSimulatorShowsChangesMarkToTheRightOfRunningStatus \
        testSimulatorNativeTerminalRetainsShellAfterReopening \
        testSimulatorKeepsDraftDuringLongMarkdownStreamAndReopensFinalText \
        testSimulatorCopiesSelectedOrWholeUserMessageText \
        testSimulatorOpensLongInterruptedHistoryAtLatestMessage \
        testSimulatorKeepsLatestVisibleAcrossRepeatedLongHistorySubmissions \
        testSimulatorRepeatedlyReopensTasksAndNewDraftsAfterBackNavigation \
        testSimulatorOpensSideChatWithoutLosingOriginalDraft \
        testSimulatorReopensCompletedHistoryCollapsed \
        testSimulatorSelectsAssistantTextInPlaceAndAddsOnlySelectionToDraft \
        testSimulatorUsesNativeHostNavigationAndPairingDismissal \
        testSimulatorOpensQrScannerWhileAddingHost \
        testSimulatorRemovesHostAndRequiresPairingAfterRelaunch \
        testSimulatorUsesNativeProjectDisclosureAndWorkspaceTree \
        testSimulatorRendersMarkdownTableAndReopensIt \
        testSimulatorNativeTerminalCopiesTextOnLongPress \
        testSimulatorNativeTerminalPastesMultilineText \
        testSimulatorKeepsSmallOlderScrollDuringLiveUpdate \
        testSimulatorReopensRunningLongHistoryWithoutBlankViewport \
        testSimulatorOpensOnlyTheTappedImageAndSavesIt \
        testSimulatorCanStartAConversationInAProject \
        testSimulatorShowsHistoryLoadingBeforeHostReplies \
        testSimulatorGroupsLiveCommandsBetweenCommentaryAndExpandsOnTap \
        testSimulatorModelPickerUsesNativeFormAndAgentPicker \
        testSimulatorNativeTerminalDoesNotDuplicateQueryResponses \
        testSimulatorShowsAcceptedAdditionalInputBeforeCodexProcessesIt \
        testSimulatorKeepsFailedWorkCollapsedWithVisibleTerminalError \
        testSimulatorKeepsInputRequestVisibleUntilResolved \
        testSimulatorSendsQuestionChoiceOnTap \
        testSimulatorCanSkipQuestion \
        testSimulatorRetriesAFailedTaskOpenWithoutLosingItsDraft \
        testSimulatorLoadsEarlierHistoryOnTapAtTheReadingPosition \
        testSimulatorWorkbenchStartsEmptyAndOwnsOnlyOpenedTabs \
        testSimulatorBrowserUsesOneSharedImportMenu

# Exercise the real iroh Host through the headless client.
[group('portable')]
iroh-e2e:
    cargo build --locked --package agent-process --bin agent-provider-supervisor
    AGENT_RUNTIME_DIRECTORY="$PWD/target/debug" cargo test --locked --package host-fixture --test iroh_host

# Lint and validate contracts without building native clients or starting E2E.
quality language="apple":
    scripts/quality.sh "$@"

# Audit diff presentation tests in an isolated copy; extra arguments go to cargo-mutants.
# Inspect missed.txt and logs; distinguish equivalent mutations from missing assertions.
# Report timeouts and unbuildable mutants separately from assertion-based catches.
[group('portable')]
mutants-diff *args:
    mkdir -p target/mutation-diff
    PROPTEST_RNG_SEED=20260925 cargo mutants --copy-target=false --package agent-core --file crates/agent-core/src/presentation/diff.rs --output target/mutation-diff --jobs 2 --build-timeout 900 --timeout 60 --cargo-arg=--locked --cargo-arg=--lib "$@" -- presentation::diff::tests

# Verify bounded Git path decoding with the dedicated `nix develop .#kani` environment.
# Keep unwinding assertions enabled; a timeout is a failure, not a proof.
kani-diff *args:
    mkdir -p target/kani-diff
    cargo kani --package agent-core --lib --harness presentation::diff::proofs --output-format terse -Z unstable-options --harness-timeout 120 --export-json target/kani-diff/results.json "$@"
