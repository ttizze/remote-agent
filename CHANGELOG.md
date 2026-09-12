# Changelog

## Unreleased

- Remove four unreferenced root screenshots and the unused Desktop SVG icon (1.93 MB).

- Run submissions, attachment uploads, and remote Host pairing through the shared operation queue, preserving chained submission completion, failure recovery, and durable results after navigation. Remove the specialized effect variants and completion events.

- Share typed RPC declarations and simple operation result assignment; express constant invalidation and stale-result application policies alongside operation ordering.

- Establish the descriptor budget inside isolated Host fixture startup so the full 80-client admission test also runs under macOS's inherited 256-descriptor limit. Fail before opening endpoints if the process hard limit prevents the required budget.

- Share navigation reset and history-watch release in core. Route already-classified Host RPC messages through one dispatcher, remove unused routing error enums and repeated envelope parsing, and share Desktop connection-failure delivery.

- Resolve native list pagination, history cursors, project grouping and title fallbacks in core, removing unused native Turn/Item getters. Preserve the shared conversation rows, Markdown caches, selection actions and repeated-history handling integrated on main.

- Open iPhone change summaries in a tabbed Changes/All Files sheet. Show per-file diff cards with line numbers, wrapping, and addition/deletion backgrounds, reusing the desktop Git patch parser from core. Closing returns to the conversation.

- Share mobile foreground refresh and recovery in agent-core: retain live connections, reconnect after a disconnect or a request deadline with no incoming RPC traffic, and preserve individual request timeouts while other replies or notifications arrive. Clear stale notices after successful recovery. Verify retained navigation and drafts, refresh retries on the same transport, disconnected/silent transport recovery with real iroh sessions, and foreground list/history refresh in iOS Simulator.

- Match saved project roots through filesystem aliases when assigning worktree conversations, including macOS `/var` and `/private/var` paths.

- Preserve individual history occurrences when turn IDs repeat, so opening a conversation keeps both responses visible instead of duplicating and collapsing their contents.

- Keep new Host connection handshakes alive while prior sessions finish, preventing intermittent reconnection failures.

- Open iPhone tasks immediately while Host history loads, using cached history and the selected task directory when available. Preserve drafts after a failed read, restore the original conversation when side-chat preparation fails, offer retry, and disable submission and attachments until the conversation is ready.

- Share isolated Host and JSONL connection ownership across integration and UI fixtures. Run the UI Host and loopback pairing controls in one process, remove the separate pairing executable and on-disk key exchange. Remove the redundant iOS screen identifier that overrode the existing detail, loading and retry identifiers.

- Open Mac conversation file links in the Files panel using the selected Host, preserving file drafts and revision checks. Show Files and Diff within Side Chat and return to its existing draft.

- Gate native conversation changes with repeated back navigation, all activity families, long-history paging, and repeated turn IDs. Keep the current Swift navigation implementation.

- Refresh conversation lists and titles after submitted messages and completed turns. Show elapsed execution time and the current action from core presentation, and provide command-output copying. Open the desktop diff from change summaries and file rows, with file navigation, readable Git paths, and expandable unchanged context.

- Place desktop dictation controls in the composer with a live microphone waveform, cancel/Escape, stop, and send. Preserve drafts on cancellation and show readable failure reasons.

- List Bex-managed worktrees and their conversations in desktop settings. Confirm removal and refuse modified, ignored-file, locked, detached, or actively used worktrees; retain branches and conversation history. Requires the updated Host.

- Share flat conversation rows, stable row IDs, fork eligibility, and activity expansion defaults across Desktop, iOS, and Android through core presentation. Keep native turn/item and Markdown caches while removing Swift record copies. Restore Desktop file-change headers and diffs by reading typed change fields, shared with expanded activity text.

- Share one normal local Host across desktop and mobile independently of their state directories. Reuse and remember its credential directory and key-storage backend, reject competing starts before provisioning, verify discovered tickets, and require explicit isolation for separate test Hosts. Report conflicting legacy Hosts without stopping active work.

- End successful dictation with no recognized text without an error or submission. Preserve drafts and attachments, including after navigation; malformed responses and failed transcription still report errors.

- Add iPhone PC-list connection removal with confirmation and credential deletion, preventing automatic reconnection after relaunch. Refresh Mac device management when opening settings, place removal above pairing QR codes, and fix the revoke request’s `nodeId` field so removing access closes active sessions and rejects reconnection.

- Add saved Codex account switching to the Mac composer’s account/model menu, with current-account selection and model refresh after switching.
- Run core/desktop behavior tests and isolated Simulator conversation regressions in post-commit quality. Honor Cargo's configured output directory in mobile binding/library builds and pass it through to Xcode. Correct the stale live-expansion contract and require integration evidence for changes developed in separate worktrees. Keep completed fixture output consistent between streaming and history, and verify both cached reopening and uncached deferred detail loading.

- Add desktop selection actions for quoting, requesting an AI explanation in a new side conversation, and asking in the current side chat, plus right-click Copy and Google Search. Show timestamp, Copy and an action to return text to the composer on own-message hover. On iPhone, keep assistant text selectable in place with native selection actions, while own messages use a separate long-press menu. Preserve the original conversation and draft when closing an iPhone side chat, including after retrying its preparation.

- Keep command activity collapsed by default while running and after reopening; preserve explicit expansion.

- Keep chats without a selected project unassigned after sending, reopening, and restarting the Host. Use a dedicated `bex-chats` working directory beside the Codex project state instead of inheriting the App Server's checkout, while retaining the real directory for attachments and file operations. Preserve unsent drafts when that directory cannot be prepared, and skip automatic Git reviews for unassigned chats so a parent repository’s changes do not appear.

- Render desktop approval labels, queued requests, images and thread summaries from core presentation. Share Store session subscription, shutdown and snapshot persistence across desktop windows; split desktop views and core operations by responsibility, and remove the Swift presentation mirror.

- Keep submissions moving while account helpers initialize, and move account persistence off async workers. Cache Desktop project state with source-file invalidation; share private JSON writes, Git execution and account-token handling.

- Allocate typed RPC IDs before serialization and share response decoding. Remove unused schema discovery, legacy project-list routing and dependencies; move isolated Host fixtures and integration tests into `host-fixture`, and archive superseded plans. Rotate diagnostics with fallible filesystem operations while preserving error recovery.

- Remove the iPhone connection error subheader from task lists and conversations; keep connection progress in the navigation header.

- Exercise account-switch and fork history through the Host hydration API in integration tests, and compare startup diagnostics against native OS errors without assuming English Unix messages.

- Persist rotating private JSONL error logs for desktop and Host, including RPC causes and turn errors, startup failures, timeouts, and panic diagnostics. Retain records across restarts and redact common credentials and structured payloads. Preserve string-valued errors and watch failures, distinguish intentional closes from failures, report the caller's application version, and recover from rotation failures without recursive logging deadlocks or panics on closed stderr.

- Show command and integration activity by action during live conversations, without separate internal reasoning rows. Fold completed commentary and activity under a past-message count while keeping the final answer visible.

- Keep desktop chat images inside their message bubbles, including wide screenshots and narrow windows. Clear recovered Store error banners while preserving newer local errors and dismissed notices.

- Run native Linux builds, Core and isolated Host tests, and Rust toolchain lookup on the repository-scoped shared Hetzner runner managed by `nix-config`. Keep native Windows CI, restrict shared-runner execution to same-repository changes, and bound Linux Cargo builds to two jobs.

- Apply automatic worktree creation only when a new session has an explicitly selected checkout. Global chats can send with the setting enabled. Cover saved settings on/off, global/project scope, text/photo input, first/follow-up turns, and reopened history through the real Store and isolated Host; exercise native photo/video submission after saving and reloading the enabled setting.

- Store uploads from chats without a project in the Host's private `$CODEX_HOME/bex-attachments` directory. Preserve explicit absolute destinations and reject other relative paths. Return an empty review for folders without a Git worktree while preserving real Git errors. Verify complete photo/video submission without an error notice; keep unscoped fixture threads inside their isolated directory instead of inheriting the developer's checkout.

- Initialize the Host's Rustls provider before the first dictation WebSocket handshake. Prevent voice submission from closing the iPhone's JSONL session when desktop builds enable both TLS backends; cover cold-process TLS failure followed by recording-upload recovery.

- Make remote `Intent` variants hold owned operation records directly. Derive UniFFI records on the same concrete payloads, remove generic borrowed operation variants and reducer field repacking, and migrate Rust, Swift and Kotlin callers. Keep terminal input chunks borrowed during wire serialization.

- Export core `Intent` and `Answer` through UniFFI directly and fill internal flags in operation constructors. Remove the FFI intent mirror and `host-protocol`; consolidate JSONL framing, classification and response envelopes in `agent-core::peer`. Type Host parameters/results, history detail deferral and Desktop project/title models while preserving upstream extensions, errors, opaque cursors and complete image output. Capture each iOS test row identifier once when recording a moving list.

- Unify Store and RPC payloads in the same operation types. Move conversation projection into agent-core and export core records/shared objects through UniFFI directly, removing the FFI snapshot and projection wrappers. Store now owns connection list/model/conversation reloads and automatic workspace reviews; preserve query limits, draft state and effect epochs across reconnection. Native observers retain updates that arrive before their first wait. Regenerate native bindings without stale namespace files.

- Make each remote intent own its operation payload and route it once. Move invalidation, preparation and stale application into the operation; remove Store intent repacking and response-event adapters. Keep events serializable, cover every Snapshot publication field, and verify fresh list/conversation loading through UniFFI after replacing a connected transport.

- Queue iOS list/activity snapshots immediately so completion badges are not lost to the text/stream debounce when the app closes after showing them.

- Replace the Store operation macro DSL and loaded-response enum with ordinary typed `Operation` implementations and one generic executor. Replace per-operation freshness counters with a single Snapshot epoch checked atomically at completion. Preserve background sends, transcription and saved draft revisions after navigation; establish navigation before initial/reconnect reads on all clients. Move the full conversation projection into `agent-core::presentation`; GPUI, Swift and Kotlin share its rows, item bodies and immutable identities. Remove desktop’s separate turn projection; retain on-demand activity details and explicit notification transitions.

- Raise the mobile minimum versions to iOS 17 and Android 9 (API 28). Remove the obsolete iOS 15 single-line message-field fallback. Upgrade to AGP 9.3.2 and Gradle 9.5.0, use AGP built-in Kotlin, and retain generated UniFFI source and native library inputs.

- Kill the entire quality process group or Windows job on timeout before removing its checkout. Derive the Windows CI Rust version from the pinned Nix shell, declare one inherited Rust 1.98 minimum for every workspace crate, and cache native Cargo dependencies and build artifacts.

- Replace Rust Xcode/build wrappers with `just` recipes and scripts. Preserve certificate signing, bundle rollback, isolated Simulator/Host cleanup and exact zero-skip test counts. Keep the Rust fixture servers and quality queue/status; use standard file locks and process-wrap for managed quality subprocesses. Add native Linux/Windows CI and a Nix native development shell.

- Preserve every typed character while iPhone Markdown responses stream. Update the message field’s editing buffer before synchronous Store notification, retain Store-driven send clears and transcription, and reset the control when its Host or task changes.

- Close native snapshot waits on Store shutdown, replace stale foreground transports, and recover interrupted pending sends into drafts. Keep session requests and activity out of restored connection state. Serialize and write iOS snapshots off MainActor, await ordered background writes, and cover native Android FFI shutdown and per-Host persistence.

- Use SwiftUI scroll positioning for iPhone conversations instead of mutating UIKit offsets. Preserve measured bottom coordinates when SwiftUI combines default preferences, and measure the whole lazy content while targeting the final row. Parse Markdown in the background conversation cache before publishing rows, retaining offscreen parsed bodies so lazy layout never swaps a provisional height for a full paragraph. Coalesce pending projections while keeping draft edits synchronous; embedded image data never becomes a temporary oversized text row. Restore image history after reopening, scroll long histories to the latest message after layout, and preserve the reading position when live updates arrive after a small drag toward older content.

- Preserve mobile title expansion in ten-entry increments and wait for an existing iPhone conversation to load before navigating. Render changing activity groups in a lazy scroll stack so completing a live turn cannot invalidate native List indices; retain independent expansion and history paging. Reset isolated fixture controls between UI tests and treat HTTP control failures as failures.

- Move SwiftUI and Android Compose onto generated UniFFI bindings for the common Rust Store. Remove Kotlin common state and handwritten FFI; retain native identity storage, media, QR and lifecycle adapters. Cache immutable conversation projections, expose each activity as a lazy native row, and persist per-Host snapshots. Commit iOS native text before accepting a send and flush submissions and media changes atomically before reporting completion.
- Publish Store notifications after releasing the state lock so a synchronous native observer can read its snapshot without deadlocking. Add a reentrant-observer regression test and preserve synchronous draft updates and ordered effects.

- Reset session-only approval/watch/activity state and pending submissions when connecting or disconnecting; recover unsent dictation from saved snapshots. Let Store shutdown close snapshot subscriptions, retain enqueued drafts, and release only its own transport session. Share one Desktop endpoint, serialize local daemon startup, and wait for draft persistence and PTY termination in GPUI quit callbacks.

- Move GPUI Desktop, Side Chat, Host management and terminal interactions onto the common Store and immutable snapshots. Preserve native conversation rendering, approvals, image attachments, history paging, revision-aware file editing, worktree settings and PTY lifecycle. Persist per-Host snapshots and flush Japanese drafts on shutdown; remove the obsolete Desktop RPC/conversation implementations and macOS file-picker helper. Verify macOS production startup through the actual daemon, sending and conversation/draft restoration against isolated fixtures.

- Extend the common Store with draft-safe submission and transcription, task activity, model selection, workspace operations, and terminal state. Enqueue intents synchronously, preserve interleaved attachment/settings changes during text editing, initialize new-chat model defaults regardless of catalog load order, and refresh a loaded thread list after creating a chat. Preserve binary review counts. Retain terminal output until acknowledged, serialize shell input/resize/close, select the shell on the Host, and terminate live PTYs when their Store closes.

- Pair remote Hosts through the client's existing endpoint, then save destinations with `host/registerRemote`. Remove the daemon path that opened another endpoint with the same local-client identity.

- Isolate failed iroh handshakes, gate incoming streams behind persisted authorization, and reserve a separate admission pool for pairing. Keep unanswered approvals pending beyond five minutes, register passive clients without a dummy RPC, and stop the daemon when its App Server event stream ends. Add an explicit file-key backend for headless servers, move blocking credential work off Tokio workers, and replace the obsolete iOS fixture socket wait. Cover malformed file deltas, real daemon wire models, Store pagination, expired invitations, credential growth and upstream disconnects.

- Reject malformed file-change delta targets without poisoning Store state. Require incoming iroh sessions to pass the persisted allowlist before exposing streams. Distinguish endpoint shutdown from individual handshake failures. Preserve daemon project-root objects and structured tool results, with captured daemon wire responses and Store pagination regression coverage.

- Move the daemon and fixture servers to authenticated iroh sessions and the common peer. Keep Host/local identity keys in a fixed 64-byte keyring entry and persist invitations, trust and remotes atomically in an owner-only file; restrict management to the local identity and disconnect revoked clients. Use typed core models for project lists and history, preserve per-client approval IDs, and transfer blobs on session-bound streams. Remove the SSH/Phoenix server and mobile transport crates. Add `cargo xtask iroh-e2e`; preserve worktrees, files, accounts, dictation and installed-Codex schema adaptation.

- Add a fresh typed `agent-core` client, immutable Snapshot/reducer/Store, and headless `agent-cli` against the expanded 87-case corpus. Replace peer delivery modes and request variants with one ordered event stream and typed/raw requests. Add iroh endpoints, native tickets, expiring one-use authorization data and acknowledged stream shutdown. Verify the actual CLI over isolated iroh sessions for listing, sending and numeric/string approvals. Native UI callers are intentionally replaced in subsequent migration stages.

- Consolidate desktop, mobile, App Server and fixture JSONL request correlation in `agent-core`. Share generic-stream file transfers, use clap for daemon arguments, and normalize outer JSON object formatting when rewriting RPC IDs while preserving nested raw values.

- Refresh the iPhone composer's file and line counts during active commands, tools, and file edits instead of waiting for the turn to finish. Coalesce activity notifications and discard canceled review responses. Verify live worktree edits on iOS Simulator while the turn remains running.

- Keep only Copy and Branch beneath iPhone answers; remove Share and Expand along with their dedicated sheets.

- Add iPhone Codex account and model selection with device-code login, Host-side credential refresh, and persisted account selection. Keep one shared App Server and conversation history across accounts; retain original Codex credentials and store added credentials through Codex's Keychain support. Keep history accessible if saved authentication needs renewal, and report generation errors until account selection succeeds.
- Add a branch action beneath completed iPhone answers. Fork through the selected native turn into a new conversation, open its inherited history, and preserve later turns in the original conversation.
- Use icon-only Save and Close controls in iPhone image previews while retaining accessible labels and the original image-saving behavior.

- Open Mac settings directly from the sidebar Host name and remove the Files, Changes, and Settings navigation rows. Replace the composer file-count button with an expandable file change card, per-file additions/deletions, and a review action. Preserve rename destinations and binary counts in the Host response. Automatically save worktree switches on toggle and text fields on blur (or Enter for the destination); remove the Save button and serialize writes without disabling text input.

- Add reproducible quality checks through `cargo xtask quality`: Clippy/rustfmt, strict Credo and Dialyxir, ktfmt and detekt over all Kotlin source sets, and SwiftLint/SwiftFormat over handwritten Swift. Pin tools through Nix, Gradle, and Mix; run local checks asynchronously through Lefthook post-commit instead of GitHub Actions. Serialize checks in isolated commit worktrees and expose commit-specific JSON results and logs through `cargo xtask quality-status`. Resolve existing violations while preserving default thresholds, without baselines.

- Split mobile controller, native transport ownership, conversation rendering, and UI tests by responsibility. Preserve state ownership, cancellation, history ordering, and stale-response guards. Route typed Codex calls through one client, propagate known transport failures, and handle Swift JSON serialization errors. Match the Android JNI connection key parameter to Kotlin’s byte array.

- Fix desktop Clippy diagnostics after merging main. Retain typed JSON request IDs without repeated serialization and update diff views without copying the workspace patch on each render.

- Move project-owned Python and Shell development tooling into Rust `cargo xtask` commands: Mac builds and signing, isolated iOS E2E orchestration, Codex scenarios, pairing controls, and Fly secret generation. Run the current encrypted relay tests through `cargo xtask relay-e2e`; remove the obsolete plaintext Python harness and the explicit Python development dependency. Preserve fixture behavior and signing identities. Preserve the original per-run Simulator and Host lifecycle and reject failed or skipped tests.

- Share Simulator fixture HTTP controls and project expansion setup across UI tests; complete the dictation fixture draft before the next test. Restore temporary list fixtures through XCTest teardown even after a failed assertion, and restart title traversal after page insertions. Exercise production TLS provider initialization directly when workspace features enable multiple backends.
- Align the shared Mac main/Side Chat composer with iPhone and add native voice input through the existing Host transcription route. Keep Stop-to-draft and Send-directly behavior, preserve drafts on failure, and cancel hidden recordings. Open global new chats without a project, add the project/folder menu above the input, remove the top-right new-chat button, and open model menus toward the conversation to keep them clear of native right-panel views.

- Use the open iPhone session's current working directory for change counts, diffs, and workspace files, preserving its worktree when refreshed task titles contain a different directory.

- Add iPhone worktree settings to the task list's top-right menu, with Host-backed loading/saving, cancel, and error handling. Let Mac and iPhone configure an absolute Host directory for new worktrees; an empty directory preserves the existing Git-directory default, and existing worktrees keep their locations and project membership.

- Clear mobile conversation and new-task selection when returning to the list even if the Host disconnected. Prevent native back navigation from leaving the detail route active and blocking subsequent task/new-task openings. Cover disconnected returns and reconnection in shared tests, plus repeated native back, cancelled edge swipes, and existing/new-task openings on iOS Simulator.

- Remove unused Rust dependencies and error variants, replace the manual JSON ID scanner with serde traversal, and borrow JSONL source lines and raw payloads. Move desktop RPC results without cloning, consolidate request cancellation cleanup, and remove temporary notification and disconnect collections.

- Initialize the relay's TLS provider using the Host's existing ring selection pattern. Preserve an explicitly installed provider and prevent WSS connection panics when workspace dependencies enable both TLS backends.

- Preserve supported Mac model settings across reconnects, synchronize effort and speed when opening a different model or switching Hosts, and show standard speed only once. Consolidate model lookup and settings normalization; move model-catalog pages into UI state without copying their JSON bodies.

- Add Host-persisted Mac settings for automatic worktrees on new sessions and optional file/directory copying, including `.env`. Apply both switches to Mac and iPhone session starts, keep worktree tasks in their original projects, and update the Mac workspace path and branch after creation. Defaults remain off; reopening existing sessions does not create or copy again.

- Preserve mobile task-list pagination and search when returning from a conversation. Keep the current screen when returning from the background; open the task list only on a fresh app launch.

- Start iPhone task-list project folders collapsed; preserve manual expansion during refresh and conversation navigation.

- Add a left thumbnail rail to iPhone and Mac image viewers for all generated images in the current session, including older paginated history. Switch the main image by selecting a thumbnail; keep Save then Close at the top right. Save the selected original to Photos on iPhone or a chosen file on Mac. Keep uploaded attachments out of the generated-image list.

- Open iPhone conversation images in a full-screen preview on tap. Place Save followed by Close at the top right, also in image file-link previews. Save original image data to Photos with add-only permission, prevent duplicate saves, and report errors.

- Render generated images in Mac and iPhone conversations outside collapsed work, preserve complete image results in paginated history, and support saved Host paths and inline image data. Open Markdown file links on the selected Host; show iPhone files in a dismissible Quick Look sheet. Update and relaunch the signed Mac app; verify image rendering, file links, and reopened history on iOS Simulator.

- Add compact, rounded Mac model controls with a blue stepped reasoning-effort slider, white draggable thumb, Japanese effort labels, and catalog-backed speed menu. Capture model, effort, and speed when submitting; apply them to new turns, and reset to supported defaults when changing models.

- Update and restart the signed Mac Host to activate `host/dictation/transcribe` instead of forwarding it to Codex. Remove the iPhone and Host 30-second voice-recording limit and the separate recording-cancel button. Keep Stop-to-draft and Send-directly behavior; verify longer recordings through the encrypted relay and iOS Simulator.

- Install and launch iPhone build 36 and update the signed Mac app. Share event classification and state-transition policy across PC and mobile, including denied automatic approval reviews and late lifecycle/retry events. Move owned desktop item bodies, remove history-sized echo reconciliation storage, cache turn projections and coalesce changed-row measurements. Add mobile-equivalent paginated Mac history, deferred activity details, stale-response guards and external-history watches. Preserve scroll position while adding history and expanding fetched output.

- Install and launch iPhone build 35 and update the signed Mac app. Share conversation segmentation, activity summaries, visibility, titles, pending-input placement, and echo reconciliation in one Rust crate across PC and mobile. Keep bodies in their native owners and pass metadata through the C/JNI bridge; remove duplicated Kotlin/Swift/desktop rules.

- Align Mac conversation activity grouping, chronological commentary, per-exchange answers, and completion folding with iPhone. Keep multiline commands out of fixed-height headers, show descriptive tool names, and retain full details on expansion. Project borrowed item slices instead of allocating separate message lists.

- Display Mac chat submissions before Host acknowledgement, retain accepted input through delayed native echoes, and reconcile by client message ID. Preserve drafts on failure. Share draft-key and input construction, avoid attachment-extension allocations and pending-message render copies, and remove unreachable review-item rendering.

- Show active-task spinners and white dots for unseen successful completions in the Mac sidebar. Clear dots after opening the conversation or restarting work; keep live status stable across in-flight list refreshes.

- Install and launch iPhone build 34. Reuse unchanged item display objects within streamed turns and parse Markdown off the UI thread with coalesced updates. Remove duplicate raw bodies, unused notification logs, and obsolete mobile state APIs; retain paging metadata and normalize existing version 2 caches on load.

- Support Ctrl+V alongside Command+V in Mac chat composers. Send on Enter only at the end of committed text without a selection; preserve Shift+Enter, mid-text newlines, and IME confirmation.

- Restore image paste in the Mac chat and side-chat composers. Preserve selected draft text, reuse attachment uploads, convert clipboard TIFF/BMP images to PNG, and keep pasted files available across app restarts.

- Install iPhone build 33. Separate navigation and conversation observation, project streamed state in 16 ms windows, and persist streams at completion or explicit flush. Keep small drags toward older messages detached from automatic bottom following.

- Show a white dot on iPhone task rows for unseen successful completions. Keep the running spinner, preserve unread completions across relaunch, and clear the dot after the conversation loads. Existing idle tasks and completions already being viewed remain unmarked.

- Keep earlier AI replies visible when several user instructions share one completed native turn; fold work within each exchange. Avoid intermediate JSON trees during mobile persistence and repeated attributed-string copies while rendering Markdown paragraphs.

- Install iPhone build 31. Preserve earlier AI responses when turn IDs are missing or repeated; accept legacy `turnId` when `id` is blank. Reduce streaming work by updating raw and typed state together, bounding only the affected Host cache, coalescing persistence off the UI thread, and removing redundant row observers and unused gateway defaults.

- Prepare iPhone build 30: separate display conversion from the controller and reuse unchanged turn projections while streaming. Release projections on history eviction and conversation/Host changes; preserve accepted inputs and same-length content updates. Remove the unused iOS directory-selection API and unreachable directory screen state.

- Install and launch iPhone build 29 with the new-chat context selectors and attachment-directory fix; rebuild the Release Kotlin framework for the physical-device archive.

- Place environment and project-folder menus directly above the iPhone new-chat composer, with a blank conversation area and native back navigation. Preserve the open conversation's upload directory when refreshed recent titles omit it, preventing subsequent attachments from failing with an absolute-path error.

- Fix clipped corners in iPhone AI responses by rounding message backgrounds without clipping their content.

- Add iPhone voice input through the Host's Codex dictation connection. Recording Stop returns the transcript to the composer; the adjacent Send button transcribes and sends directly. Forward the App Server's User-Agent to fix native transcription requests being rejected by Cloudflare. Follow the desktop's recording-upload fallback when streaming fails, recognize session-closed events, and retain transcripts after nonfatal stream errors. Preserve drafts on errors, keep failed sends editable, and remove temporary recordings on completion or cancellation.

- Show iPhone connection attempts with a header spinner on the task list and conversation; task-list loading uses the same spinner. Remove recovery and list-loading status banners; retain errors and manual retry.

- Align the iPhone remote task list with the reference layout: native Host/status toolbar and menu, independent project compose actions, indented single-line conversation rows, and native bottom search on iOS 26. Preserve native navigation, project expansion and paginated task loading.

- Rework the Mac layout around standard GPUI sidebar, tabs, resizable panels, Markdown and editor controls. Distinguish the closed workspace card from the open right workbench. Add a Host PTY terminal using xterm.js, an independent side chat, a native WebView browser, and file editing alongside the main conversation.

- Replace the GPUIX React Mac UI with a native Rust/GPUI Kit executable. Preserve Host IPC, pairing, conversation streaming, approvals, attachment and image handling, file editing and existing drafts. Use virtualized conversation/diff views, retain word-level diff highlights, and remove the bundled JavaScript runtime.

- Keep mobile commentary in chronological order and collapse consecutive command/tool groups independently during live, failed and interrupted work. On completion, automatically fold interim commentary and activities into a work-duration row while keeping the final answer and user inputs visible; tapping reopens the completed work. Show one-line activity counts; tapping reveals commands and their expandable output. Preserve visible requests/errors and stop controls. Remove iPhone answer-action rows from commentary.

- Use native iPhone navigation for PC selection, conversations and workspace directories, including the system back button and left-edge back swipe. Retain unsent drafts and refresh the task list when returning from a conversation.

- Replace custom iPhone project and individual activity disclosure controls with `DisclosureGroup`, inline model controls with a native settings sheet and segmented `Picker`, and custom action-button decoration with system button styles. Place pairing and QR cancellation in standard toolbars.

- Page mobile conversation history using the official Codex strategy: latest five turns, 500 initial items, and older items in pages of 100 on upward scroll. Preserve opening questions, loaded prefixes and live updates; reject pages from obsolete navigation. Use native list virtualization and scroll offsets on iPhone so large histories open at the latest message without SwiftUI layout loops or invalid row-index scrolling. Suspend bottom following while scrolling or opening details.

- Bundle public TLS trust roots on iOS so physical devices can connect to the public WSS relay. Track `relay-transport` changes in mobile Rust build inputs. Install and verify iPhone build 20 against the public relay.

- Enable the ring crypto provider for native relay TLS, fixing a Host/mobile process panic when connecting to a `wss://` relay. Add a regression test for TLS negotiation failure.

- Add a digest-pinned, non-root Phoenix release image, a single-Machine Tokyo Fly.io configuration, and rollout instructions. Include flyctl in the Nix development shell. Multi-region Host routing remains unimplemented.

- Render attached images and Markdown images inside Mac and iPhone conversations. Preserve mobile image references through acknowledgement, native echoes and cached history; fetch Host images through authenticated transfer.

- Align the Mac conversation layout with the supplied Codex reference: nested project/conversation sidebar with title search, centered conversation and floating composer, neutral dark surfaces, collapsible work, and an environment panel backed by actual workspace changes, files and Host selection. Populate the composer model menu from the connected Codex catalog.

- Open quick model and reasoning-strength controls from the iPhone composer gauge; tap the model name to open detailed settings.

- Keep iPhone conversation text white while streaming; retain gray activity labels. Select multiple photos and videos together and upload them in selection order before enabling send.

- Show accepted mobile messages immediately after acknowledgement, retain them across delayed history reads, and reconcile native echoes by client ID. Keep additional inputs in conversation order and queued messages visible.

- Add photo/video library selection and camera capture to the iPhone chat attachment menu. Reuse authenticated file uploads, retain temporary exports through transfer completion, and explain camera/microphone access.

- Keep newly started mobile turns on their native event stream instead of reading an empty rollout after acknowledgement. Open paginated Codex histories through the native paging APIs. Refresh the selected external conversation on persisted history changes without reopening it or losing its draft.

- Start the first mobile turn without resuming the new in-memory thread before it has a rollout. Keep open conversation bodies when a refreshed recent-title window omits them, so their live events continue to render.

- Open new mobile conversations directly in the existing chat screen. Remove the separate new-task form and create the conversation on the first send; retain the created thread when that send fails.

- Load the five latest titles in each of the five most recent projects, plus five recent unassigned chats, in one small DB-only response. Avoid rollout history scans and fetch bodies only when opened. Expand projects, project conversations and unassigned chats independently by ten; search indexed titles and discard obsolete responses.

- Place conversations without explicit project assignments under their configured Desktop workspace roots instead of leaving them in the ungrouped chat list. Preserve explicit projectless and project-assignment choices.

- Refresh projects and conversations when returning to the task list on iOS and Android, so conversations created by another client appear without manually refreshing or backgrounding the app.

- Open the task list on app launch and foreground return, refresh projects and tasks, and retain the healthy transport. Opening a conversation fetches its current history.

- Move conversation model settings into a gauge button at the lower right inside the two-row iPhone composer; remove the separate model/effort row.

- Restore the last selected Host on launch and maintain its connection automatically on iOS and Android. Network-only recovery preserves the visible task and retries lost connections without duplicating sends.
- Add per-Host model and reasoning-strength selection to iPhone task creation and the composer, using the installed Codex model catalog and forwarding selections on new turns.

- Sign both Mac build outputs with a stable certificate identity and fixed Host identifier so Keychain authorization survives binary updates; reject ad-hoc signing.

- Reduce iPhone task-detail transfer cost by deferring activity bodies larger than 4 KiB until expansion; preserve full conversation text and lossless detail retrieval. Decode task history off the iOS main dispatcher.

- Align the iPhone conversation presentation with the supplied official-app reference: dark surfaces, compact rounded header actions, flat activity rows, Markdown paragraphs/lists, response copy/share/expanded reading, live diff counts and a floating multiline composer with send/stop controls.

- Replace the Tauri Mac UI with a packaged GPUIX React app supporting local and remote Codex work.
- Run encrypted, independently authenticated SSH sessions through the local Phoenix relay; add one-use pairing, Keychain credentials and live device revocation.
- Keep the Host running independently of UI windows; preserve drafts across reconnects and restarts.
- Add native file selection, authenticated attachments/downloads, revision-checked manual edits, AI edit requests and working-tree diffs on Mac and iPhone.
- Complete iPhone conversation, approval, question, steering, interruption and activity presentation flows with isolated Simulator coverage.
- Preserve streamed user and command history when Codex completes a Turn with a summary-only item list.
