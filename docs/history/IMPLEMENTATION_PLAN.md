# Remote Agent implementation plan

Historical design and evidence. The Rust Store and iroh architecture in [ADR 0005](../adr/0005-rust-store-and-one-iroh-client-path.md) supersedes the Kotlin, Phoenix, SSH and versioned-cache design below.

## Accepted scope — 2026-09-06

Complete the Mac and iPhone applications against a locally running Phoenix relay.
The Mac UI uses **Rust and GPUI Kit**, replacing GPUIX/React as of 2026-09-07.
The iPhone UI remains native SwiftUI with the existing shared Kotlin state and
Rust transport. Android is maintained through shared-contract changes, but is
not a completion gate for this iteration. Production hosting, public signup,
billing, TestFlight distribution, and automatic updates are deferred.

The Mac application must support local Codex work, control of other paired Macs,
and management of its own Host Daemon and Paired Devices. Both applications must
support conversation history, streamed Turns, Steering Input, interruption,
approvals and user-input requests, attachments, downloads, Manual Edits, AI Edits,
and working-tree diffs. Multiple invited devices may operate the same Mac.
The relay operator must not be able to read or modify application traffic.

## Architecture and invariants

- Keep the existing Rust Codex App Server module and Project read projection.
  Codex owns Threads, Turns, Items, and authoritative conversation history.
- One Host Daemon owns one App Server independent of UI windows and remote
  connections. Closing either application does not interrupt a running Turn.
- Phoenix routes bounded **opaque bytes**, with one isolated route per client
  connection. It does not inspect JSONL, rewrite request IDs, or broadcast a
  response to other devices. Each runner has one live owner; stale routes close
  when either peer disappears. Both peers connect outbound to the local relay.
- Establish embedded SSH through each relayed byte stream. Pin the PC Host's
  Ed25519 key from pairing before authentication. Use SSH public-key device
  authentication and an expiring single-use invitation for first pairing.
  SSH provides encryption, integrity, fresh session keys, and replay protection.
  Neither public relay credentials nor routing metadata authorize Codex access.
- Reuse the prior embedded SSH identity/pairing patterns where applicable; do not
  bring back direct TCP listeners, mDNS, router mappings, or the obsolete direct
  connection settings as a parallel connection path.
- Private identity keys live in macOS/iOS Keychain. Non-secret profiles and
  Paired Device records use atomic persistence. Secrets never enter logs or
  durable conversation caches. Removing a Paired Device closes its live sessions
  and rejects future authentication. Invitations are local-management operations.
- Each authenticated SSH subsystem opens its own existing CodexRpcService
  session. Requests with identical IDs on different devices remain independent.
  Notifications fan out only after Host authorization. The first valid approval
  response wins. Disconnects and restarts never grant approvals.
- Local Host management uses an owner-only local IPC connection. Remote peers
  cannot mint invitations, grant access, or change local daemon configuration.
- Any paired device can select directories accessible to the PC account. Codex
  Desktop's global state remains a read-only Project source; no second index.
- Manual Edits are direct file changes, available during active Turns. They do
  not implicitly send Steering Input. Writes require the current Revision and
  use atomic replacement. Conflicts preserve the Editor Draft. UTF-8 editing
  preserves BOM, line endings, and permissions. Binary/large files are downloaded.
- Blob bytes travel separately from structured RPC. Transfers are bounded,
  authenticated, digest-checked, temporary, and cleaned on failure or expiry.
  User-supplied display names are not trusted filesystem paths.
- Reconnect preserves drafts and reconciles cached display state against fresh
  Project/Thread reads. It never silently resends a Turn, an approval, or a write.
  Background iPhone connections may close; foreground reconnects and refreshes.
- Retain unknown Codex fields and Item kinds through the raw JSONL boundary.
  Show unknown content without silently discarding it. Model/schema errors are
  visible; no mock or empty fallback substitutes for unavailable Codex data.

## Work and acceptance evidence

- [x] Reproducible Nix environment for Rust, Elixir and mobile tooling.
- [x] Isolated Phoenix routes, peer lifecycle, bounded traffic, and reconnect.
      Evidence: real Phoenix + Rust client/Host loop with two simultaneous
      clients using identical request IDs, plus close/offline/duplicate tests.
- [x] SSH over relay, pinned keys, invitation consumption, Keychain identities,
      revocation, and separate authenticated sessions.
      Evidence: wrong Host key, unknown/removed device, expired/reused ticket,
      tampered traffic, and captured relay payloads containing no application
      plaintext; real encrypted two-client full-stack loop.
- [x] Host local management, persistent settings, daemon lifecycle, invitations,
      local/remote access, and connection state exposed to the Mac application.
      Evidence: run actual local IPC and daemon; close/reopen UI while a Turn
      remains observable from another client.
- [x] Shared file, Blob, revision-conflict, and working-tree-diff capabilities.
      Evidence: isolated actual file/transfer operations, concurrent edits,
      hash mismatch, cancellation, binary data, and cleanup.
- [x] Rust/GPUI Kit Mac UI replaces the GPUIX/React application. Local/remote
      conversations, pairing management, approvals, user input, attachments,
      editor, and diff are operable with native text input including Japanese.
      Evidence: launch actual GPU window, drive the user flows, inspect rendered
      results. Compilation or source-text tests alone do not establish UI behavior.
- [x] iPhone UI and Kotlin/FFI callers use the new credentials and capabilities.
      Evidence: headless contract/full-stack checks first, then XCUITest against
      the real local relay. Inspect xcresult passed/failed/skipped counts.
- [x] Finish documentation and remove obsolete Tauri/plaintext relay code and
      temporary development scaffolding after behavioral verification.

Routine verification uses isolated fixture keys, settings, files and Codex peers.
A fake Codex peer is a test dependency, never the delivered runtime. Live Codex
verification uses the installed binary and must be reported separately. Physical
iPhone checks require explicit device authorization; simulator results never
establish camera, physical network, or physical-device behavior. No deployment
or purchase is required for this local completion scope.

## Current evidence

- Phoenix route tests: 10 passed, 0 failed. Per-client delivery, cross-runner
  isolation, duplicate ownership, offline joins, close/reconnect and envelope bounds.
- Real Phoenix + Rust byte tunnels: two simultaneous 8 MiB round trips passed,
  including a delayed receiver, reconnect and runner disconnect. No ignored tests.
- Real Phoenix + embedded SSH + Rust Host service + isolated Codex process
  fixture: pinned-key mismatch, unknown device, expired/replayed invitation,
  simultaneous request ID 1, ciphertext confidentiality/integrity, reconnect,
  and per-device live revocation passed.
- The GPUIX app replaces the Tauri entry points. The native Mac bundle and nested
  file picker build and pass local ad-hoc signature verification. The delivered
  runtime starts the installed Codex, with no fixture fallback.

- Host runtime local Unix socket + real encrypted relay: owner-only socket,
  exclusive process lock, pipelined target/RPC header, local-created Thread visible
  remotely after local disconnect, remote management rejection, local revocation,
  cooperative shutdown and socket rebind passed. The binary now uses this runtime;
  its obsolete plaintext relay implementation has been removed.

- Actual Mac GPU window: local and relayed conversations, Japanese input,
  approvals/questions, persisted drafts, native file selection, attachments,
  byte-exact downloads, revision-checked editing, AI-edit preparation and Git
  diff were exercised. Remote uploads/downloads were compared with actual bytes
  on isolated source and destination Hosts.
- Actual Codex 0.153.3, using the existing signed-in account: Mac UI edited an
  isolated file and its exact bytes matched `verified\n`; live user/command/final
  history survived summary-only Turn completion, and the diff showed the change.
  An independent connection observed a Turn complete after its Mac window closed.
- Host restart restored relay credentials and remote profiles from Keychain.
  The restored remote profile loaded its projects through the Mac UI. A second
  controller's Turn appeared live in the same remote conversation. Native device
  revocation closed an authenticated connection and rejected subsequent RPC access.
- iOS final Simulator run: **10 passed, 0 failed, 0 skipped** in
  `target/qa/Bex-20260906-091802.xcresult`. Includes conversation start/history,
  approvals, editor/draft reconnect, questions, retry/terminal/interrupted work,
  activity families, steering/stop, attachment restoration/send, native download
  share sheet and AI-edit preparation. The retained share-sheet screenshot was
  visually inspected. Shared Kotlin contract tests: 107 passed, no failures or skips.
- Mac reducer regression: observed Codex `itemsView: summary` completion erased
  streamed user/command items before the fix; all five reducer tests pass after
  merging summary items into existing history.

- Final Rust workspace suite after removing the Tauri member: **61 passed,
  0 failed, 0 ignored**. This includes the encrypted multi-client relay tests
  and bounded relay transport tests.

## Verification limits

The remote Host instances ran on the same Mac through actual Phoenix/SSH routes;
separate physical Macs and physical iPhones were not tested. iPhone UI checks use
a deterministic Codex process fixture; installed-Codex execution was verified
from the Mac UI separately. Physical camera capture, physical-device networking,
signing for distribution, public hosting and billing remain outside this local
completion scope. Tests establish only the paths named above.

Verification cleanup removed the owned fixture processes, isolated Simulators,
QA Keychain records, temporary transcripts and test-only app bundle. The Mac
bundle, iOS Simulator bundle, xcresult and source project remain available.

## iPhone reference UI revision — 2026-09-06

The supplied official iPhone screenshot now guides the native conversation layout:
dark background, rounded header controls with project/Host context, flat expandable
work rows, Foundation Markdown block/inline rendering, response copy/share/expanded
reading, live workspace diff counts and a floating multiline send/stop composer.
File access moved to the conversation's `…` menu. Existing pending requests,
reconnect drafts, attachments and editor actions remain connected to the same model.

Simulator proof: the three conversation-history/copy, approval/editor/draft and
steering/stop flows passed without skips in `Bex-20260906-113443.xcresult`.
After finishing Japanese duration labels, command rows and the composer Stop
control, history/copy and steering/stop passed again without skips in
`Bex-20260906-113946.xcresult`. The retained collapsed and expanded conversation
screenshots were visually inspected; see `target/qa/iphone-ui-reference`.
The UI revision is included in version 1.0, build 5, installed and launched on the connected iPhone during the task-detail performance fix below.


## iPhone task detail loading
- [x] Reproduce large-history transfer cost with an isolated encrypted relay fixture.
- [x] Load conversation and activity headers first; fetch full large activity bodies on expansion.
- [x] Verify lossless retrieval, errors, and the native iPhone Simulator interaction.

Evidence: the original local Host returned 10,904,070 bytes / 1,371 items in 0.14 seconds for a large real task. An isolated encrypted relay fixture reproduced a 9,100,633-byte initial history response. The final deferred response was 927 bytes (603 ms in that run, versus 1,668 ms before the fix). Full-inline reads and on-demand retrieval returned identical complete command items, including the 8.4 MB output. These timings describe the local fixture, not measured physical iPhone latency.

Validation: 109 Kotlin/Native tests, 22 Host unit tests, and all 3 encrypted relay integration tests passed. The Simulator steering/stop flow passed in `Bex-20260906-165922.xcresult`. The initial expansion test used an identifier hidden by SwiftUI's enclosing accessibility identifier; after correcting the query against the captured hierarchy, history/reopen/expand/full-body/collapse/copy passed without skips in `Bex-20260906-170314.xcresult`. The retained expanded-detail screenshot was visually inspected.

Version 1.0 build 5 was signed, installed and launched on the paired physical iPhone. The updated Mac Host binary passed signing verification, but startup currently waits in `SecItemCopyMatching` for macOS Keychain access confirmation. The UI tool refuses access to SecurityAgent. User interaction is required before physical phone networking and the new production Host can be verified. The existing relay remains running; credentials and pairing records were preserved.


## Persistent Mac Keychain authorization
- [x] Replace ad-hoc signing with a stable certificate identity in both Mac build commands.
- [x] Verify two different signed binaries can share an isolated Keychain item without another prompt.
- [x] Apply the signed Host update while preserving existing credentials and pairing.

The isolated native Keychain experiment disabled UI interaction. With ad-hoc signatures, build 1 created its item and build 2 failed to read it (`-25293`). With one Apple Development certificate and the same signing identifier, a differently compiled build 2 read the item successfully twice. Both binaries had identical certificate-based designated requirements. Both real Mac build commands completed with strict signature verification; explicit `BEX_CODE_SIGN_IDENTITY=-` was rejected. The experiment restored the user's Keychain search list and did not use production credentials. This proves authorization survives changed binaries with a stable signing identity; migration of existing ad-hoc permissions still needs macOS Always Allow.

The installed Mac bundle now uses the certificate-based `app.bex.host` requirement. The previous blocked Host was replaced after confirming it had no Codex child. The new Host still awaits authorization of existing Keychain items under its new stable identity; credentials and device pairing records were not rewritten. Choose Always Allow during this one-time migration; post-approval production connectivity remains to be verified.


After the user completed Always Allow, the production Host started and `host/status` reported `relayConnected: true`. A read-only check on a real large task returned 1,046,655 bytes for initial history versus 11,309,110 bytes for full-inline history (90.7% less). Initial Host processing took 0.101 s; this is not physical-phone latency. Completed-turn item IDs and all user/agent messages matched, and a deferred activity fetched separately matched the original full item exactly. The Keychain startup blocker is resolved. Physical-screen verification was unavailable because iPhone Mirroring was locked behind Mac login; no further permission change was made.


## iPhone session restoration and model selection
- [x] Preserve navigation independently of cache size and reconcile the selected task on reconnect.
- [x] Reuse healthy connections; reconnect on launch, foreground and transport loss while retaining the visible screen.
- [x] Load the Host model catalog, persist model/reasoning choices and apply them to new turns.
- [x] Verify headless contracts and isolated Simulator interactions.
- [x] Install the signed iPhone update.

The final native headless suite passed 116 tests with zero failures or skipped tests. The original restoration and healthy-connection assertions failed before the fix. Added contracts cover oversized cache persistence, model pagination, per-Host turn settings, startup restoration, stream closure followed by a failed retry, duplicate foreground requests and switching Hosts during an in-flight connection.

Simulator result `target/qa/Bex-20260906-183956.xcresult` passed the model-selection and restoration scenario (1 passed, 0 failed, 0 skipped): choose model and effort, start a task, terminate/relaunch, return from Home and send another turn. The isolated Codex fixture rejected mismatched model/effort parameters, so both successful turns verify the settings crossed the actual relay/Host boundary. The preceding result `target/qa/Bex-20260906-183533.xcresult` also passed approval/edit/draft recovery (2 passed, 0 failed, 0 skipped). The final screenshot was visually inspected. A read-only production `model/list` returned 8 models with the expected fields and supported default efforts.

Version 1.0 build 6 was archived and exported locally as `target/device/export-session-20260906/Bex.ipa`; the exported app passed strict signature and bundle-version verification. No TestFlight upload or Mac Host restart was required. Physical installation is tracked separately below. iOS suspension and physical network recovery are not established by these Simulator results.

Build 6 was installed successfully on the already paired physical iPhone (`dev.remoteagent.mobile.ios`); the initial disconnected tunnel was re-established by CoreDevice. Launch was denied by iOS because the device was locked (`FBSOpenApplicationErrorDomain` code 7). iPhone Mirroring also remained at the Mac-login lock screen. Physical launch and on-device UI/network behavior remain unverified until the user unlocks the phone; no unlock or authentication bypass was attempted.

## Reference composer model control
- [x] Move conversation model settings into a gauge button at the lower right inside the composer; remove the separate settings row.
- [x] Verify the actual Simulator placement and settings interaction, then install the signed update.

Reference-composer verification: `target/qa/Bex-20260906-193302.xcresult` passed 1 test with 0 failures and 0 skipped tests. The actual UI asserts the gauge is right of screen center and below the message field, opens model settings, changes effort and sends a turn with the expected settings. The screenshot with the keyboard visible was inspected at `target/qa/gauge-images/9F00B48C-E9CF-4AD8-B5C7-2DE1E38437D9.png`. Model names and effort no longer occupy a separate conversation row. Version 1.0 build 7 passed archive/export and strict signature verification and was installed on the paired physical iPhone.

CoreDevice launched installed build 7 successfully. Physical-screen interaction was not observed; placement, keyboard visibility and settings behavior were verified in the isolated Simulator.

## Fresh conversation state on foreground return
- [x] Verify a conversation created by another client appears in the task list on foreground return.
- [x] Reproduce stale conversation content when foreground return reuses a healthy connection.
- [x] Refresh the visible conversation and task/project lists without replacing the transport or changing navigation.
- [x] Verify an update made while the app is in the background becomes visible after return; install the signed update.

The headless regression failed before the change, then the full native suite passed 117 tests (0 failures, 0 errors, 0 skipped). The first Simulator run still failed to fetch the background reply with the UIApplicationDelegate callbacks. After routing background/active transitions through SwiftUI `scenePhase`, the same actual Simulator scenario passed: another client updates the isolated conversation without a live notification, and foreground return fetches its new reply without navigating away from the task. No new connection or repeated send is required. The production Host's read response also contained the latest assistant response from the sampled conversation's rollout; raw user messages were not logged.

Final Simulator result `target/qa/Bex-20260906-195339.xcresult`: 2 passed, 0 failed, 0 skipped, including the latest-reply scenario and existing task/model restoration. The latest reply was visually confirmed in `target/qa/latest-conversation-images/F08F2D1A-F75F-47D0-A830-37FF5DF1C328.png`. The final release framework, archive and local export passed; exported app `target/device/install-latest-20260906/Payload/Bex.app` passed strict signature and build-8 checks.

Build 8 was installed and launched successfully on the paired physical iPhone. Physical-screen interaction remains unobserved; fresh-reply rendering was verified in the isolated Simulator. The production Mac Host and relay were left running without replacement.

The independent task-list scenario also passed (`target/qa/Bex-20260906-200313.xcresult`: 1 passed, 0 failed, 0 skipped). A second local client creates a conversation while iOS is backgrounded; returning to the app displays the exact new thread ID without manual refresh. The resulting list was visually inspected in `target/qa/latest-list-images/86FE39E4-C705-4174-B5BA-DB72CC7EED3D.png`. This additional verification changes only fixtures, the UI test and its runner; the installed build 8 already contains the verified application fix. The isolated Host, pairing server and Simulator were removed by the runner after completion.

## New conversations when returning to the list
- [x] Reproduce another client creating a conversation while the mobile app stays in task detail.
- [x] Refresh project and task lists on list entry through the shared controller; migrate iOS and Android callers.
- [x] Verify headless behavior and the actual Simulator back-to-list interaction, then install the signed update.

The headless assertion that the newly created conversation appears on list entry failed before the change. The original iOS back button also failed the isolated Simulator scenario (new exact thread ID absent). After routing iOS and Android list entry through `MobileController.showThreadList`, the full native suite passed 118 tests (0 failures, 0 errors, 0 skipped). The Simulator passed list-entry and foreground-refresh scenarios in `target/qa/Bex-20260906-204714.xcresult` (2 passed, 0 failed, 0 skipped). The new conversation was visually confirmed in `target/qa/list-entry-images/B6384CC6-8144-4701-8177-B0B3B229C360.png`. Android compilation and the iOS release framework build passed. Android runtime interaction remains unverified.

Build 9 was archived and exported locally, passed strict signature and bundle-version verification, then installed and launched successfully on the paired iPhone. Physical-screen interaction remains unobserved; list freshness was verified on the isolated Simulator. The fixture Host, pairing server and Simulator were cleaned up by the test runner. The production Mac Host and relay were not restarted.

## Desktop project membership for newly created conversations
- [x] Reproduce a conversation with no explicit assignment being omitted from its configured project despite matching its workspace root.
- [x] Match Desktop membership using explicit assignments/projectless overrides, existing upstream membership, and configured workspace roots.
- [x] Verify boundary cases, the real Host/relay path and Simulator project placement; update the running signed Mac Host and inspect this conversation’s returned project.

The affected conversation was present at rank 0 in the Host response and mobile cache selection, but had no `projectId`. Desktop groups this unassigned conversation using its workspace root. The previous Host overlay did not perform that grouping, placing it in the ungrouped chat section. The regression failed with null instead of the expected project ID before the fix. Host unit tests now pass 24 cases, including explicit overrides, worktree hints, secondary roots, path-component boundaries, specific nested roots and ambiguous equal matches. The encrypted-relay integration passed with project membership asserted on start, list and read responses.

Simulator result `target/qa/Bex-20260906-210556.xcresult`: 2 passed, 0 failed, 0 skipped. The membership test proves that the exact newly created conversation disappears when its project is collapsed and returns when expanded. Its project placement was visually confirmed in `target/qa/project-membership-images/BF3B5E67-60DD-489C-AD05-43FB119D2184.png`. The first UI attempt exposed a fixture-only `/tmp` versus `/private/tmp` mismatch; the external-client fixture now uses the same canonical workspace as its Host.

Both Mac build scripts passed with stable certificate signing. The running Host was replaced gracefully using its existing state directory and pairing identity. A read-only inspection of the actual affected conversation now returns the `remote-agent` project ID, and the updated Host reports its relay connected with the existing paired device retained. The relay and unrelated Host were not restarted. Physical-screen placement remains unobserved; iPhone build 9 remains installed because this fix changes Host-side membership. All isolated fixture services and Simulator resources were cleaned up by the runner.

## Open the app at the task list and retain its connection
- [x] Route cold launch and foreground return to the task list before restoring connectivity.
- [x] Retain healthy connections and automatic retry; preserve a chosen conversation during network-only recovery.
- [x] Verify shared lifecycle contracts and isolated Simulator launch/foreground behavior; install the signed iPhone update.

This specification supersedes the earlier behavior that reopened the selected conversation at launch/foreground return. `MobileController.openApp` clears task selection before restoring connectivity, even while offline or connecting. iOS and Android route app entry through it; automatic network-only recovery still preserves the user's current task. Android now uses the same scoped connection maintainer as iOS. Existing healthy transports and event subscriptions are retained.

The native headless suite passed 120 tests with 0 failures, 0 errors and 0 skipped tests; Android compilation and the iOS release framework build passed. Simulator result `target/qa/Bex-20260906-211605.xcresult` passed all 3 tests with no failures or skipped tests: cold relaunch/foreground return opens the list, model choices survive, another client's new conversation appears, and reopening a conversation fetches a background reply. The actual foreground list screenshot was visually inspected at `target/qa/startup-images/995CF31B-04CE-427F-AC03-02CE1056F99F.png`. The runner cleaned up its isolated Host, pairing service and Simulator after completion. Existing restoration, manual connection and draft tests intentionally remain applicable to network-only recovery. No temporary fixture code was added. Physical-screen interaction and Android runtime behavior remain unverified.

Build 10 passed local archive/export, strict signature and bundle-version verification, and was installed and launched successfully on the paired iPhone. The Mac Host and relay were retained. No TestFlight upload was performed.

## Recent projects and incremental task-list display
- [x] Order projects by their most recently updated conversation, retaining catalog order for projects without conversations.
- [x] Show 5 projects and 5 unassigned chats initially; each section's trailing “もっと見る” reveals another 10 until exhausted.
- [x] Verify sorting and repeated expansion on the isolated Simulator, then install the signed update.

The native suite passed 122 tests (0 failures, 0 errors, 0 skipped), including project recency/reordering, pagination past 64 summaries and opening an old conversation after the body cache is full. Android compilation and the iOS release framework build passed. Summary retrieval now follows all pages while keeping 64-entry requests; the obsolete total-item/page-budget configuration and its callers were removed. The twenty-conversation limit applies to bodies, not the list of summaries.

Simulator result `target/qa/Bex-20260906-214455.xcresult`: 2 passed, 0 failed, 0 skipped. A fresh fixture contains 26 projects and 40 unassigned chats (66 conversations). The UI test observes every project in recency order through 5/15/25/26 entries, independently observes chats through 5/15/25/35/40 entries, verifies each exhausted “もっと見る” disappears, and opens the oldest chat's actual history. The subsequent project-membership scenario also passed after fixture restoration. The project/chat boundary and final chat rows were visually inspected in `target/qa/pagination-images/9BD8BED5-6532-42C4-BFBB-4901CF8CA298.png` and `08B8E7EC-BED1-4EC6-B97D-B918A7FB9610.png`. Early test attempts incorrectly counted unrealized list rows and tapped below the floating search field; the final test scrolls, records actual row identifiers, and keeps the tap point above that field. No app workaround was added for the test. All isolated fixture services and Simulator resources were cleaned up, and the temporary video-inspection script was removed. Android runtime and physical-screen interaction remain unverified.

Build 11 was archived/exported locally, passed strict signature and bundle-version verification, and installed on the paired iPhone. CoreDevice's launch command returned error 10004 because it could not determine the process ID. A subsequent device process listing confirmed Bex running as PID 55709 from the exact bundle installation URL returned by this build's install, and the Bex crash-log listing was empty. This establishes the installed app is running; its physical screen remains unobserved. The Mac Host and relay were not restarted, and no TestFlight upload was performed.

## Fast task-list entry with latest titles only
- [x] Return the latest 5 conversations for the latest 5 projects, plus 5 unassigned chats, from a single DB-only Host projection. Fetch bodies only when opened.
- [x] Keep project, per-project conversation and unassigned-chat expansion independent (+10); search the indexed titles and discard obsolete responses.
- [x] Verify title/count/payload contracts through the real encrypted Host/relay path and on Simulator, measure the real Host path, then install the signed update.

The revised request supersedes fetching the full summary catalog before display. `useStateDbOnly` avoids App Server rollout scan/repair. Read-only measurements on the existing Host returned 30 DB entries in 14ms and 100 entries in 22ms; the prior scan-backed request took about 1.2s for 64 entries. These are Host-local measurements, not physical-phone display latency. The title projection uses one Desktop membership snapshot, including explicit assignments, explicit unassignment, worktree hints and root matching. It retains only requested titles plus per-section lookahead and omits previews, turns, Git metadata and rollout paths from the mobile response.

The final common API is one scoped `host/thread/list` call with `titleOnly`, independent project/chat/per-project limits and an optional indexed search term. The response includes only the requested project metadata, title rows and per-section continuation flags. Obsolete mobile project-list calls, cursor loops and separate project-loading state were removed; project-change notifications refresh the combined result atomically. No hidden total-title limit is imposed on repeated expansion. iOS list decoding runs off the main dispatcher. The bounded response also avoids normalizing/persisting the entire summary catalog on app entry.

Headless verification: 124 native tests passed with 0 failures, errors or skips; Android compilation passed. Host verification passed all 27 unit tests and all 4 real encrypted-relay integration tests, including title counts, recency, explicit/worktree/projectless membership, a deterministic initial payload budget, independent expansion, indexed search and opening an old body. The first full-stack measurement returned 30 titles in 8,178 bytes and 46ms before limiting project metadata itself to the requested page.

Simulator `target/qa/Bex-20260906-222507.xcresult` passed all 3 scenarios (0 failed/skipped): 25 latest project titles plus 5 latest unassigned chats, one project's 5/15/18-title expansion and oldest body, repeated project/chat expansion, and workspace membership. Initial and expanded project screenshots were visually inspected in `target/qa/title-list-images/3DFBF7FD-E0B4-446D-83C0-5771DF39964A.png` and `79A8BE72-D8C7-42B7-929A-D2CA3A894538.png`. A subsequent single-scenario run also passed after removing the separate project-loading pipeline. Final bounded-project response verification and signed build installation are tracked below.

The signed Mac Host was updated using the existing `~/.bex` identity and paired device; the relay was retained. Before limiting project metadata to five entries, real-data initial title requests completed in 681ms cold and 172ms warm with an 11,575-byte response, returning 5 projects' latest 5/5/5/1/5 titles plus 5 unassigned chats. The one-title project has fewer than five conversations. These are local Host RPC measurements; physical-phone display latency remains unmeasured.

Final bounded-project verification: `target/qa/Bex-20260906-223801.xcresult` passed both title-window and repeated-pagination scenarios with 0 failures and 0 skipped tests. Final initial-title and project/chat-boundary screenshots were visually inspected at `target/qa/title-list-final-images/F9F631D7-2CF7-404F-BA06-44829B27B9F7.png` and `A9D4C9ED-520F-46CD-B896-548ADFDC6F7A.png`. The runner completed cleanup of its isolated Host, pairing service and Simulator. The final real Host request returns exactly five project metadata entries and an 8,251-byte response: 519ms on the first request after Host restart and 115ms on the next request. Existing pairing and the running relay were preserved.

Build 12 passed the final release framework build, unsigned archive, dedicated-keychain distribution preflight and local release-testing export. The exported `target/device/install-titles-final-20260906/Payload/Bex.app` passed strict signature and bundle-version verification and was installed successfully on the paired iPhone. Android compilation passed; Android runtime and physical-screen interaction remain unverified. No TestFlight upload, commit or push was performed.

The build-12 launch request was rejected by iOS because the physical device was locked (`CoreDevice 10002`, `FBSOpenApplicationErrorDomain Locked`). Installation succeeded; this does not establish physical launch or screen behavior. No unlock or authentication bypass was attempted.

## Open new conversations directly in the chat composer
- [x] Replace the new-task sheet/dialog with an empty instance of the normal chat screen; retain the selected working directory.
- [x] Create the conversation on first send through the existing message pipeline, preserving model choices, attachments and failed-send drafts.
- [x] Verify shared behavior and the actual Simulator composer, then install the signed update.

Evidence (2026-09-07): 125 shared/native tests passed with zero failures/errors/skips; Android Kotlin compilation passed. Four encrypted-relay integration tests passed. The isolated Simulator run `target/qa/Bex-20260907-073743.xcresult` passed all four selected UI tests with zero failures/skips: direct empty-chat entry and first send, model persistence, reconnect/draft/approval flow, and existing attachment/download/AI-edit flow. Inspected the empty-chat screenshot in `target/qa/direct-chat-images`; the existing composer has keyboard focus and no creation form. Exported signed Release build 13 from `target/device/Bex-direct-chat-final-20260907.xcarchive`, verified its strict signature, installed the exported app on the paired iPhone 17 and confirmed its launch via CoreDevice. Physical-device screen interaction and Android runtime were not exercised. The isolated runner removed its pairing fixture and Simulator.


## Restore mobile first-send behavior
- [x] Reproduce with installed Codex in an isolated home and local deterministic provider: `thread/start` followed by `thread/resume` fails before a rollout exists; direct `turn/start` streams and completes.
- [x] Resume only unloaded threads; retain open conversation bodies independently of the recent-title window.
- [x] Verify the shared contract, the Simulator first send, and the signed device update.

Evidence (2026-09-07): installed Codex 0.153.3 with an isolated home and local SSE provider rejected `thread/resume` before a first turn (`no rollout found`); direct `turn/start` produced three deltas and a completed readable answer. The recent-title/body regression failed red, then passed with the fix. Shared/native tests: 127 passed, zero failures/errors/skips; Android Kotlin compilation passed. Four encrypted-relay tests passed. `target/qa/Bex-20260907-080126.xcresult` passed both first-send/model UI tests with no failures/skips against a fixture that now rejects resuming an unmaterialized thread. Build 14 passed release framework build, archive, dedicated-keychain export and strict signature verification. Installed from `target/device/install-first-send-20260907/Payload/Bex.app`; CoreDevice confirmed launch at 08:11 JST. Physical screen interaction remains unverified.

## Keep the open conversation updated while work is active
- [x] Reproduce the missing live update with an isolated, deterministic assertion; distinguish an unjoined thread from work owned by another Codex process.
- [x] Remove post-send disk reads of unmaterialized rollouts; load paginated official histories with the installed pagination API.
- [x] Fix the subscription/update path and preserve in-flight history reconciliation, navigation, and the existing streaming UI.
- [x] Verify the changed contract headlessly, then on Simulator, and ship the verified update.

Evidence (2026-09-07): the unmaterialized-rollout regression failed on the prior post-send read, and the encrypted paginated-history regression failed on the prior full-history request. Shared/native tests now pass 128 cases, with zero failures/errors/skips; Android Kotlin compilation passed. Host tests passed 29 cases; four encrypted-relay tests passed, including paginated conversation/item reads and real OS file-change delivery across SSH and Phoenix. Installed Codex 0.153.3 was exercised with an isolated home and local deterministic SSE provider: metadata/paged reads worked across two processes, and persisted rollout size/mtime changed after the answer completed. Separate-process native token notifications were unavailable.

`target/qa/Bex-20260907-083048.xcresult`: four passed, zero failed/skipped. Actual Simulator scenarios cover first send, steering/interruption, reopening/expanding history, and opening an external paginated conversation then receiving its persisted reply without navigation while keeping an unsent draft. The latter was visually inspected in `target/qa/live-history-images/D9F8C072-469A-44A5-BA37-9A5181BFD61F.png`. The signed Mac bundle was rebuilt and its running Host replaced gracefully with no active owned turns. Its existing identity and paired device were preserved, and the relay reconnected. Read-only inspection through the updated Host opened five actual installed-Codex histories, fetched one full item, and registered/unregistered a native rollout watch. Build 15 passed release framework build, archive, dedicated-keychain export and strict signature/bundle-version verification. CoreDevice confirmed installation from `target/device/install-live-history-20260907/Payload/Bex.app` at 08:38 JST and launch at 08:39 JST. The isolated UI fixture and Simulator were removed by the runner. Android runtime and physical-screen interaction remain unverified. No TestFlight upload was performed.

## Show accepted messages without waiting for native processing
- [x] Verify native client message IDs and reproduce delayed steering-message presentation with an exact headless assertion.
- [x] Project accepted sends immediately, reconcile native echoes by client ID, and keep additional inputs in conversation order.
- [x] Verify headless/full-stack and actual Simulator send/steer behavior, then install the signed update.


Evidence (2026-09-07): an isolated installed-Codex 0.153.3 run with a local deterministic provider acknowledged steering before its user-message notification, then emitted and persisted both supplied client IDs. The conversation-order regression failed before the fix. Shared/native tests passed 132 cases with zero failures/errors/skips; Android Kotlin compilation passed. Four encrypted-relay integration tests passed, including first-send and steering acknowledgements while native messages were held behind an explicit fixture barrier, followed by two matching client IDs in events and persisted history.

`target/qa/Bex-20260907-090645.xcresult` passed all five selected Simulator tests with zero failures/skips: accepted additional input before native processing, steering/interruption, first send, collapsed reopened history, and external-history updates. The new test verifies the accepted body appears within two seconds after send, below the preceding command, before releasing the native notification; after release and interruption it still appears exactly once. Visually inspected `target/qa/sent-message-verified-images/123004EC-B137-40C2-9215-B21DA2C69C1B.png`. The runner removed its isolated pairing fixture and Simulator.


Build 16 passed the Release framework build, archive, dedicated-keychain local export and strict signature/bundle-version verification. CoreDevice confirmed installation from `target/device/install-sent-message-20260907/Payload/Bex.app` at 09:11 JST and launch at 09:11 JST. Existing Host and relay processes were retained. Android runtime and physical-screen interaction remain unverified. No TestFlight upload was performed.


## iPhone photo, video and camera attachments

- [x] Add native photo/video selection and photo/video camera capture to the existing composer attachment menu.
- [x] Reuse authenticated uploads and image/file inputs; preserve media exports until transfer completion and remove their temporary copies afterward.
- [x] Verify library selection, upload, sending, history presentation and camera-unavailable feedback on an isolated Simulator; retain existing file/download/AI-edit coverage.

Evidence (2026-09-07): before concurrent shared-message changes, 128 shared/native tests and four encrypted-relay integration tests passed with zero failures/skips. `target/qa/Bex-20260907-084915.xcresult` passed the existing attachment/download/AI-edit flow; its new media test sent a PNG image but failed because XCTest could not compute a hit point for the visible video thumbnail. After changing the test to tap the thumbnail's observed frame center, `target/qa/Bex-20260907-085545.xcresult` passed the complete media test with zero failures/skips: native photo/video picker, PNG and generated MOV uploads, send/history reflection, and camera-unavailable feedback. Inspected menu, picker and history screenshots in `target/qa/media-final-images`.

The final UI run built the current app but ran independently of shared tests because concurrent client-user-message-ID changes temporarily broke that suite. A subsequent shared-test run compiled but failed `baseline_operations_emit_native_payloads_and_decode_results` and `steer_turn_forwards_the_expected_turn_and_text_input` on payload expectations missing `clientUserMessageId`; those unrelated changes were preserved. The final history screenshot also contains duplicate projected user bubbles while that separate reconciliation work is in progress. This attachment verification does not establish message deduplication. Physical photo/video capture, microphone permission/recording, iCloud exports, other media formats and device installation were not exercised. Temporary UI runner overrides were removed; the permanent runner still requires shared tests before UI execution.

## Match the desktop conversation layout to the supplied Codex reference

- [x] Replace the desktop's dashboard-like controls with a neutral three-column layout: project/thread sidebar, bounded conversation and floating composer, and a contextual environment/files panel.
- [x] Keep existing conversation, approval, attachment, file editing, diff and pairing actions reachable; use actual project, model and workspace data for visible controls.
- [x] Verify the native desktop surface against an isolated Host fixture, including sidebar navigation, sending, work expansion and context-panel actions; package and open the updated Mac app.

Evidence (2026-09-07): native Mac visual inspection through CUA exercised a fresh isolated Host fixture: first send and Japanese history reopening, project expansion, title search filtering, work and command-output expansion, workspace diff and file-editor navigation, model-menu opening/selection, and both sidebar/panel toggles. The initial narrow user bubble and split diff counts were corrected and visually rechecked. GPUIX's stdin automation crashed during a tree request; native CUA interaction provided the visual evidence instead. No dependency source was modified.

The Nix desktop packaging command passed TypeScript checking, Release compilation, bundling and strict signature verification. Restarted only the desktop UI and visually confirmed actual project titles, the model catalog label and workspace branch/change counts in the signed app. Existing Host processes and relay were retained. The isolated preview and fixture were stopped. This run did not re-exercise remote pairing, approval responses, attachment transfer, file saving or physical iPhone behavior.


## Render chat images on Mac and iPhone

Attached local images and inline image URLs now render in user messages; Markdown image references render inside assistant responses. Mobile protocol decoding retains image sources, accepted submissions carry them before the native echo, and the durable cache preserves them. iPhone Host paths use the existing authenticated download and are decoded off the main thread into bounded thumbnails. Mac Host paths use local files or authenticated remote transfer; temporary remote copies are removed when the image view is released. Android retains its text attachment labels.

Validation: the new protocol assertion failed before the fix (132 tests, exactly 1 failure). After the fix, 133 shared/native tests passed with no failures, errors or skipped tests, including accepted-image display, cache serialization and native-echo reconciliation. Android compilation and the iPhone Release framework build passed. Simulator result `target/qa/Bex-20260907-120852.xcresult` passed both photo/video upload with decoded chat-image display and Host/inline Markdown image display across relaunch (2 passed, 0 failed, 0 skipped). Screenshots in `target/qa/chat-image-captures` were visually inspected. Native Mac CUA inspection used an isolated Host fixture and confirmed a local image in the user bubble and Host-path/inline-data images in the response, including reopening the conversation. The signed Mac bundle was rebuilt and its UI restarted. Mac remote-Host image rendering, external HTTP image retrieval and Android image rendering were not exercised.

Final build-18 verification: `target/qa/Bex-20260907-121821.xcresult` passed image rendering and reopen/restoration again (1 passed, 0 failed, 0 skipped). Release archive and local release-testing export succeeded; `target/device/install-chat-images-18/Payload/Bex.app` passed strict signature verification and contains build number 18. CoreDevice confirmed installation on the paired iPhone at 12:19 JST. CoreDevice confirmed launch at 12:20 JST. Physical image-screen interaction remains unverified; rendering evidence is from the Mac surface and isolated Simulator. No TestFlight upload, commit or push was performed.

## Mobile history paging (2026-09-07)

Mobile follows the installed official Codex client's paging strategy: fetch the latest five native turns with `itemsView: notLoaded`, hydrate a global budget of 500 items newest first in requests of 100, and retain the App Server's opaque cursors. Scrolling upward loads missing turn items or older turn pages. Partially loaded turns retain their opening question. Live refreshes preserve loaded prefixes; navigation and connection generations reject obsolete page responses. Legacy non-paginated threads use the complete native read.

The iPhone uses a flat native `List`. Native content size and offsets control bottom following; scrolling and detail expansion suspend it. This removes SwiftUI size-change anchors and queued row-index scroll requests. The deterministic 3,718-item fixture reproduced the original missing-body failure. Build 21 exceeded the physical device's CPU budget in SwiftUI layout (48 CPU seconds over 49 seconds, approximately 59 MB memory). The final native-list implementation passed all seven public-WSS Simulator scenarios with zero failures/skips: latest-message visibility and older history, completed-history expansion/collapse and copying, approvals/editor/drafts across reconnect, pending input requests, activity families, image display after reopening, and external-client updates. Inspected the long-history screenshots in `target/qa/history-public22-images`. Shared/native tests passed 137 cases with zero failures/errors/skips. Android Kotlin compilation and the release iOS framework build passed.

The signed production Host was updated while its loaded task was idle. It reconnected to the public relay; a read of the affected real conversation returned five turns and 500 items, including the expected latest item, with older history available. Sanitized run counts are in `target/qa/history-paging-evidence.json`; temporary invitation-bearing raw results and fixture files were removed. Android runtime and cellular connectivity remain unverified.

The final build 22 archive passed strict signature verification and was installed on the physical iPhone. Its 58.076-second UI scenario passed with zero failures/skips: the affected real conversation showed its body on open, upward scrolling increased the projected UI item count from 482 to 554, the initial visible item moved offscreen, and relaunch restored the public task list without a reconnect warning. Inspected all three screenshots in `target/qa/history-device22-images`. The real conversation continued receiving changes from another client, so its physical smoke check asserts visible content rather than a stale final ID; the isolated Simulator checks the exact latest ID before any scroll. No Codex prompt was sent during device verification.

## Native iPhone controls (2026-09-07)

PC selection, the task list and conversations now use native `NavigationLink` transitions instead of replacing the contents of one navigation screen. The system owns the back button and interactive edge swipe; the conversation-pop binding calls the existing list refresh action. Workspace directory links and explicit path navigation also push native destinations. iOS 15 remains supported through `NavigationView`.

Project lists and individual activity details use `DisclosureGroup`. Project creation is the first row inside each expanded project. Disclosure identifiers belong on their labels so they do not override identifiers on child controls. Model settings use a native sheet, `Form`, navigation to detailed selectors and a segmented reasoning-strength `Picker`. Standard button styles own action colors and disabled appearance; pairing and scanner cancellation use native toolbars.

The flat conversation `List`, bounded history loading, deferred activity fetches and native scroll-offset following retain their existing contracts. Photos, camera capture and sharing continue to use the platform controllers; authenticated Host effects remain in the existing view model.

Validation: the latest results for ten isolated Simulator scenarios passed: PC navigation and pairing-sheet dismissal; model/effort selection across restart and foreground return; project disclosure and native directory back swipe; approval, editor and draft recovery; attachment/download/AI-edit preparation; steering/interruption; large-history latest-message opening and upward paging; 26-project/40-chat pagination; completed-history disclosure and deferred details; and conversation back swipe with draft retention. Earlier UI-test iterations exposed inherited disclosure identifiers, ambiguous sheet-close selectors, viewport-dependent pagination traversal and stale fixture-project selection; the final checks use label-local identifiers, specific close actions, complete list traversal and the intended fixture project. Selected screenshots in `target/qa/native-controls-images` were visually inspected. Exact latest test results and original run counts are retained in `target/qa/native-ui-evidence.json`.

Build 23 passed strict signature verification and was installed from `target/qa/Bex23-standard.xcarchive`. CoreDevice confirmed version 1.0, build 23. The physical iPhone scenario passed in 21.282 seconds, with one pass and zero failures/skips: a saved conversation opened, the system back button returned to the list, reopening showed content, and a left-edge swipe returned to the list again. Its native navigation screenshot was visually inspected. No Codex prompt was sent. Other changed controls were exercised in Simulator; physical QR-camera interaction and the iOS 15 runtime remain unverified. Raw pairing-bearing and private device captures were removed after retaining sanitized results. No TestFlight upload was performed.


## Collapsed mobile command groups (2026-09-07)

The existing conversation-segment projection now preserves all commentary in chronological order and divides consecutive commands, reasoning and tools into independent collapsed groups. Group identities survive streaming completion and later commentary; requests, errors and stop controls stay visible outside the group. Group headers show bounded one-line counts. iPhone commentary omits answer action rows and unnecessary separators. Deferred command details and native list virtualization remain intact. Android uses the same projection and keeps Stop outside collapsed activity.

Validation: 137 shared/native tests passed with no failures, errors or skips. Android Kotlin compilation and the device Release Kotlin framework build passed. Twelve distinct isolated Simulator scenarios have passing latest results with zero latest failures/skips, covering live independent groups and command output, task creation, failed/interrupted work, input requests, additional-input position, long-history loading, reopening with collapsed work, steering/stopping, tool families, retry recovery and native back-swipe draft preservation. The collapsed and expanded screenshots in `target/qa/grouped-activity-images` were visually inspected. Sanitized results are in `target/qa/grouped-activity-evidence.json`.

Physical verification caught an intermediate archive linked against an old device Kotlin framework. Rebuilt `linkReleaseFrameworkIosArm64` before creating build 25 and documented this prerequisite in README. The final archive passed strict signature verification and was installed on the iPhone; device inspection after the UI run confirmed version 1.0, build 25. Its saved-conversation open/reopen, back-button and left-edge-swipe scenario passed (1 passed, 0 failed, 0 skipped). Visual inspection confirmed that actual command groups were collapsed between commentary. No prompt was sent. Group expansion/output interaction was verified in Simulator; that interaction on the physical device, Android runtime and iOS 15 runtime were not exercised. Raw pairing-bearing results, private device captures and intermediate archives were removed after extracting sanitized evidence.

## GPUI migration — 2026-09-07

- Native Rust executable with GPUI Kit; remove React/TypeScript entry points,
  npm metadata, Node/Bun tooling and the bundled JavaScript runtime. Existing
  Host IPC, encrypted relay and native file-picker contracts remain in use.
- Desktop Rust tests: **9 passed, 0 failed, 0 ignored**. Five conversation-state
  contracts, Unicode word diffs, reconnect without request replay, response/event
  dispatch and snapshot-before-delta ordering.
- Actual native window with an isolated Host, real Phoenix/SSH relay and
  deterministic Codex fixture: Japanese send/stream/completion, work/output
  expansion, approval denial, user-input response, local attachments, Host and
  inline images, file editing/save and diff rendering.
- Paired remote connection through the relay: history/images, upload and
  download exercised; transferred files match their sources byte for byte.
  Remote Host runs on the same Mac.
- Final release executable: reopen the same remote history and restore Japanese
  composer text plus uploaded attachments, then send them and observe live work and final completion.
- `just build-desktop-macos` is the current entry point for the native `target/Bex.app`; the
  build verifies the complete bundle with strict code-signature verification.
  The Host keeps its existing certificate identity.
- No physical second Mac or mobile surface was exercised for this migration.
  The earlier installed-Codex/mobile evidence above belongs to prior revisions.
- Signed production bundle launched and displayed existing local projects and
  the installed model catalog. Existing Host process stayed running; only the
  old Bun UI was replaced. Bundle contains native executables and assets (74 MiB),
  with no JavaScript runtime.
- Removed the owned preview bundle and isolated fixture state after shutting down
  its Host/Codex/Phoenix processes. Production Host and relay were preserved.

## Completed mobile work disclosure (2026-09-07)

Completed turns with a final answer now fold interim assistant commentary and activities into a duration summary. Live, failed and interrupted grouping remains unchanged. Additional user inputs keep their chronological boundaries. Older histories without a final-answer phase keep the last unphased assistant message visible; commentary-only histories remain visible. iPhone expansion overrides are tied to the turn status so an expanded live group closes at completion.

Two regression assertions failed before the projection change. The final shared headless suite passed 139 tests with no failures or skips. An isolated iOS Simulator run passed four UI scenarios with no failures or skips: automatic completion folding and re-expansion, collapsed history reopening with deferred command output, failed work and interrupted work. The completion screenshot showed one duration row above the final answer. Physical-device, Android and iOS 15 runtime behavior were not exercised for this change.

After removing avoidable expansion-key string allocations, the final build passed both completion/re-expansion and history-reopening UI scenarios again (2 passed, 0 failed, 0 skipped). Sanitized screenshot: `target/qa/completion-collapsed.png`. Isolated fixtures and raw result bundles were cleaned up.

## Mac workspace layout — 2026-09-07

- Replace custom sidebar rows and text icons with GPUI Sidebar, Button and Icon controls. Center the conversation/composer; distinguish the closed workspace summary card from the open resizable workbench.
- The workbench opens to four choices and retains each tool while switching: Terminal, Side Chat, Browser and Files. File editing and Git diff reuse the existing Host services and standard Editor/DiffView. Side Chat reuses the conversation implementation with independent state and a separate draft file.
- Terminal uses locked xterm.js assets inside macOS WebView and the installed Codex experimental process RPC. One ordered request worker preserves PTY input order; hiding retains the shell, replacing the terminal or closing its client ends it. Browser is a separate native WebView without terminal IPC. Node is Nix-managed build tooling, not a bundled runtime.
- Actual native-window verification with an isolated Host and deterministic conversation fixture: project expansion, main conversation loading, independent side-chat send/stream/completion and retention after switching, Japanese file edit/save with matching disk content, closed workspace card and open workbench. Browser displayed two local HTTP pages and followed links/back navigation.
- A separate isolated Host used the installed real Codex process service: shell startup, typed input, Japanese clipboard paste/output, PTY width changes (66 to 57 columns in the final debug revision), hide/reopen retention, exit status and new-shell creation. The release executable with bundled assets also accepted Japanese input and rendered its output. Closing that window removed its observed shell PID.
- The signed release bundle passed strict signature verification and the updated production app displayed local projects and all four tool choices. The existing production Host remained running. No model prompts were sent to production. No test suite was run for this UI change; the evidence above is native interaction. Remote PTY execution, physical second-Mac behavior and mobile surfaces were not exercised for this revision.

## iPhone remote list reference layout — 2026-09-07

The list now uses a native toolbar with a centered Host/status title, PC selector and ellipsis menu. Each project has a separate compose button that remains accessible while collapsed. Project titles and indented conversation titles use single lines; native List row insets and a 52-point minimum restore the reference density. Native navigation and paginated data loading remain unchanged.

On iOS 26, `DefaultToolbarItem(kind: .search, placement: .bottomBar)` places standard searchable UI beside a system prominent compose button. Older iOS versions retain their system search placement. Only the project-row arrangement/expansion uses custom layout; lists, buttons, search, menus and navigation remain native. No voice-session feature or nonfunctional voice button was added.

Verification: the iPhone 17 / iOS 26.5 isolated Simulator run passed four UI scenarios with zero failures/skips: returning-list refresh, search/clear/dismiss/menu refresh/compose actions, Host/pairing navigation, and project disclosure/directory back swipe. After the final spacing adjustment, two UI scenarios passed with zero failures/skips: latest-five titles and independent pagination, plus bottom search and both compose actions. The final list screenshot was visually inspected. Evidence and screenshot are `target/qa/mobile-remote-list-evidence.json` and `target/qa/mobile-remote-list.png`. The existing isolated runner's Host fixture is retained; raw pairing-bearing result bundles and temporary exports were removed. No physical iPhone or iOS 15–25 runtime was exercised, and no device installation or TestFlight upload was performed.

On subsequent request, rebuilt the device Release framework and archived version 1.0 build 26 in `target/qa/Bex26.xcarchive`. The archive passed strict signature verification. CoreDevice installed the archive product over Wi-Fi on the paired iPhone 17 / iOS 26.6.1, confirmed installed build 26, and successfully launched it. Physical screen interaction was not exercised; UI interaction evidence remains the Simulator results above. No TestFlight upload was performed.

## iPhone connection status — 2026-09-07

Replace the generic recovery banner with native header progress on the task list and conversation. Actual connection errors retain their message and manual retry. Shared connection/retry state is unchanged.

The existing isolated Simulator relaunch/foreground/model-restoration UI scenario passed (1 passed, 0 failed, 0 skipped). After that scenario, temporarily paused only its isolated relay process and relaunched the app: visual inspection showed cached content and a header spinner with no recovery banner. Resuming the relay restored the green connected indicator and loaded project rows. Screenshots: `target/qa/connection-header-recovering.png` and `target/qa/connection-header-restored.png`. The runner cleaned its fixture and Simulator after the smoke check; raw pairing-bearing results were removed. Conversation connecting-state appearance and connection-error interaction were not separately exercised.

Version 1.0 build 27 passed archive and strict signature verification, was installed over Wi-Fi on the paired iPhone, and launched successfully. CoreDevice confirmed installed build 27. Physical screen interaction was not exercised. No TestFlight upload was performed.

The subsequent list-loading change removes the project/task loading row and keeps the list-header spinner active during data fetching. The isolated return-to-list refresh scenario passed (1 passed, 0 failed, 0 skipped). Temporarily pausing only the fixture Codex process during app relaunch showed cached titles and header progress without loading text; resuming it populated project rows and restored the green indicator. Both screenshots were visually inspected (`target/qa/list-header-loading.png`, `target/qa/list-header-loaded.png`). The fixture and Simulator were cleaned up, and raw pairing-bearing results were removed. Build 28 passed archive/signature verification, was installed over Wi-Fi, and launched on the paired iPhone; CoreDevice confirmed version 1.0 build 28. Physical interaction and older iOS runtimes were not exercised.



### Voice routing and recording controls — 2026-09-08

The running Host still forwarded `host/dictation/transcribe` to Codex, which returned `unknown variant`. The signed Host built from `codex/fix-mobile-dictation` replaced the executable in the Mac bundle; after the approved restart, an empty-audio request to the live local socket returned Host-owned `dictation_failed` / `録音データがありません。`, and management reported `relayConnected: true`. The bundle passed deep, strict signature verification.

Remove the phone and Host 30-second recording checks and the separate cancel button. The 31-second encrypted-relay assertion failed against the old limit and passed after its removal. Six dictation protocol tests passed, including 65-second lossless chunk delivery. The Kotlin Simulator suite passed 157 tests with no failures, errors or skips. `target/qa/Bex-20260908-072656.xcresult` passed one XCUITest with zero failures and skips: recording persisted beyond 32 seconds, Send delivered the recorded PCM through the mobile bridge and encrypted relay to the isolated Host's account check, and the existing draft survived the expected account error. Its exported screenshot was visually inspected and contains Stop and Send without a cancel button or duration limit.

No physical iPhone update was performed in this task. Actual speech transcription against the authenticated provider and physical-device recording remain unverified; the isolated provider tests cover stream completion and recording-upload fallback.


### iPhone task-list folder defaults

Project folders now start collapsed and retain manual expansion during refresh and conversation navigation. Verified in the isolated Simulator runner: 155 Kotlin tests passed with zero failures/errors/skips; `target/qa/Bex-20260908-113819.xcresult` passed three UI tests with zero failures/skips covering initial collapse, expansion retention, project title pagination, disclosure and directory navigation. The collapsed-list screenshot was visually inspected. The first UI run failed before pairing because this new worktree lacked Relay dependencies; `mix deps.get` in the Nix shell resolved the fixture setup. Physical devices and the full UI suite were not exercised.


### Preserve mobile navigation and task-list expansion

Returning from a conversation retains project, chat and per-project title limits and the current search. iOS and Android foreground callbacks restore the connection and visible data without opening another screen; fresh app initialization still opens the task list with its initial limits. The regression assertion failed before the fix: expected limits 15/15/{project:15} with search `retained`, received 5/5/{} with an empty search. After the fix, all 156 Kotlin Simulator tests passed with zero failures/errors/skips. The real encrypted-Relay title-list test passed (1 passed, 0 failed/ignored), and Android Debug Kotlin compilation succeeded. `target/qa/Bex-20260908-115918.xcresult` passed four UI tests with zero failures/skips: all 18 expanded project titles survive a detail round trip; foreground return retains the open conversation and unsent draft while fetching an external reply; a visible list fetches external tasks on foreground; a terminated/relaunched app starts on the list while a backgrounded/reactivated app keeps its conversation and selected model. Exported screenshots of retained expanded titles and the foreground conversation/draft were visually inspected. The isolated runner cleaned up its temporary pairing fixture and Simulator. Physical devices, Android UI behavior and the full iOS UI suite were not exercised.


### Native tooling cutover

Build/sign/iOS orchestration now lives in `justfile` and `scripts/`, while the Codex and pairing fixture servers remain in `crates/xtask`. The asynchronous post-commit queue retains its commit-specific JSON status and runs `just quality` in an isolated checkout. Linux uses the pinned Nix native shell; Windows uses the matching Rust version and the hosted Windows SDK because Nix does not provide a native Windows shell. CI compiles desktop/Host/CLI and runs core and isolated Host contracts. OS deployment minimums and AGP/Gradle changes remain in a separate PR.
