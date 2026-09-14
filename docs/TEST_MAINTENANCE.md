# Test maintenance

Tests should protect observable behavior and failure recovery through the owning
implementation. Keep protocol, persistence, navigation, authorization, and
concurrency boundaries even when their final values resemble another test.
Avoid testing a fixture's own behavior, third-party serialization, private ID
formatting, or a second implementation of the production UI.

## September 2026 audit

The audit reviewed 320 test definitions, the JSON corpora, standalone WebView
harness, and runners. The following changes replace redundant checks while
preserving the listed acceptance boundaries. A-number references identify the
55 recommendations in the original review.

| Recommendations | Replacement or retained boundary |
| --- | --- |
| A01–A06 | Require `experimentalApi` in the fixture's real initialize handshake; exercise typed project membership directly; remove four test-side/equivalent operation cases and duplicate model round trips; keep one opaque-field/null/absence wire case. |
| A07–A14 | Remove JSONL/library semantics, duplicated message classification, ID exhaustion, and mock transfer round trips. Keep protocol validation, escaped/duplicate IDs, cancellation plus unknown/late replies, write-before-close, and real Host transfer/session authorization. |
| A15–A20 | Assert terminal process shutdown in its stream lifecycle; assert Git statuses and counts against real files; consolidate missing state and Unix logging recovery; remove upstream-fork mock self-validation and fixture size assertions. The portable log-destination failure test remains on non-Unix systems. |
| A21 | Merge distinct proxy ID and unknown-field assertions into the request resolution lifecycle. Keep notification byte preservation and replay after the original session disappears as separate short boundary tests. |
| A23–A26 | Run overlapping account listing/login and late fork replies through Store; reuse all three draft reconciliation cases with actual JSONL replies; consolidate immediate dispatch, discarded receipts, and reverse receipt polling in an offline Store. |
| A27–A33 | Remove equivalent silence, restored-cache, invalid scalar, and command-status combinations. Preserve current/stale navigation, empty/whitespace input, nullable targets, rendered order and stable identity. Merge catalog refresh with reordered options. Keep focused source-order tests; remove only their metadata-serialization self-check. |
| A34–A35 | Share executable resolution setup across precedence/fallback/missing phases; combine stale-ticket/restart identity and simultaneous/sequential Host exclusion without changing their filesystem boundaries. |
| A36 | Check request-timeout policy with virtual time. Run passive replay/approval completion on real iroh in the normal suite. Preserve the real 301-second QUIC soak as an ignored test selected by the manual Native clients workflow. |
| A39–A40 | Keep one malformed transcription response, whitespace silence, and two audio chunks plus a sample-aligned tail. Exercise both start methods, missing/empty/blank cwd, saved settings, real worktrees, and membership after restart without their full equivalent cross product. |
| A41–A44 | Keep desktop-specific Unicode emphasis, rendered expansion, native quote events, and independent search URL checks. Render production conversation images instead of a hand-built test bubble. The latter exposed narrow-window overflow, now covered at both window widths. |
| A45–A50 | Remove the idle launch test; shorten direct side-chat preparation while retaining complete retry/submission/reopen coverage; combine full/partial copy, accepted input/stop/interrupted expansion, foreground/project disclosure, and draft-preserving back navigation with cancelled edge gestures. |
| A51–A54 | Use 16 projects, chats, and titles for 5 → 15 → 16 pagination boundaries and independent expansion. Keep oldest-row opening and expansion after return. Move image saving to the gallery test and include it in maintained runners. Remove fixture metrics; retain older-history loading/scroll position. Combine physical navigation with explicit pairing; physical execution still requires separate authorization. |
| A55 | Seed existing WebView cookies without asserting a pinned Wry bug. Keep actual import, HTTP authentication, attributes, and persistence checks. |

Three conditional recommendations remain unchanged:

- **A22:** the independent Snapshot-field publication check is the only direct
  guard for omissions from the publication comparator. Reading a correct
  `store.snapshot()` after a receipt does not prove that native subscribers
  were notified. Replacing this small deterministic test with another list of
  operations would add setup while losing that independence.
- **A37:** the 128-reconnect regression remains. Reducing repetitions does not
  deterministically exercise cancellation of an in-flight accept during prior
  session cleanup. A test-only imitation of the select loop would not test the
  production loop.
- **A38:** the 80-connection admission regression remains. A 17-connection test
  cannot catch the former shared 64-slot exhaustion. Transport establishment
  alone also does not prove that each connection holds an admission permit.

A24's late fork is now checked through Store. Its small durable-upload/remote-
pairing state check is retained: replacing it requires controlled completion
across real transfer and pairing boundaries. Reintroducing a second fake
transfer server just to delete that check would reverse A14.

## Execution

- `nix develop . --command just quality rust` checks formatting, workspace
  Clippy, core library tests, and desktop rendering/input tests.
- `nix develop . --command cargo test --locked -p agent-core -p codex-app-server
  -p host-daemon -p host-fixture` covers the integration tests omitted by the
  library-only quality invocation.
- `nix develop . --command just conversation-ui` runs the maintained native
  conversation contracts on a fresh isolated Simulator/Host. `just ios-e2e`
  also covers pagination, foreground refresh, files, dictation, and images.
  Missing or skipped requested tests fail the runner.
- Run the retained real-time soak explicitly with
  `cargo test --locked -p host-fixture --test iroh_host
  passive_client_can_approve_after_five_minutes_without_reconnecting -- --ignored`,
  or dispatch the Native clients workflow. A virtual clock is not a QUIC soak.

The full Git/worktree/submission matrices, unseen/cached restored history,
actual dictation duration, and authorization/revocation boundaries remain.

Integration with the independent-provider Host changes also updates the fatal
startup logging fixture: a missing Codex executable is recoverable, so corrupt
isolated trust state now supplies the fatal error. Both process exits, distinct
process IDs, startup records, and persisted error messages remain asserted.

The Android 17 integration exposed Compose's transitive Espresso 3.5.0 dependency
calling the removed `InputManager.getInstance` API before Markdown assertions
could run. An explicit Espresso 3.7.0 test dependency uses the upstream fix;
the native Markdown cases and their assertions remain unchanged.
