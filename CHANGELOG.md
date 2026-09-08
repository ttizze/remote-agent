# Changelog

## Unreleased

- Centralize Mac/mobile agent operations and pure conversation, history, lifecycle, and submission decisions in Rust. Keep Kotlin mobile coordination and native UI adapters thin; preserve unknown payload metadata without copying conversation bodies through the native bridge. Exercise both adapters with 72 shared fixture cases.
- Share the asynchronous JSONL RPC engine and preserve snapshot-before-notification delivery. Move Host title/history projection into its own owner while retaining session routing and authorization in the RPC service.
- Split Mac main and Side Chat into independent native conversation views and watches with shared Host catalogue, management, and draft resources. Replace mutable Host/draft services with plain state values and pure transitions.

- Share one Mac draft writer between main and Side Chat, preserving their separate files and pending saves when either view closes.

- Restore workspace Rust quality checks: format Mac and Host code, disambiguate the NUL separator in workspace-review fixtures, and simplify the hidden Side Chat recording-cancellation condition without changing its behavior.

- Remove unused Rust JSON-value response wrappers from the mobile client; C and JNI responses continue through the raw JSON path. Replace custom Codex schema and executable-test temporary-directory management with `tempfile`, preserving owner-only schema permissions and automatic cleanup; remove the obsolete schema-random error and direct `ring` dependency from `codex-app-server`.

- Add iPhone Codex account and model selection with device-code login, Host-side credential refresh, and persisted account selection. Keep one shared App Server and conversation history across accounts; retain original Codex credentials and store added credentials through Codex's Keychain support. Keep history accessible if saved authentication needs renewal, and report generation errors until account selection succeeds.
- Add a branch action beneath completed iPhone answers. Fork through the selected native turn into a new conversation, open its inherited history, and preserve later turns in the original conversation.
- Use icon-only Save and Close controls in iPhone image previews while retaining accessible labels and the original image-saving behavior.

- Open Mac settings directly from the sidebar Host name and remove the Files, Changes, and Settings navigation rows. Replace the composer file-count button with an expandable file change card, per-file additions/deletions, and a review action. Preserve rename destinations and binary counts in the Host response. Automatically save worktree switches on toggle and text fields on blur (or Enter for the destination); remove the Save button and serialize writes without disabling text input.

- Add reproducible quality checks through `cargo xtask quality`: Clippy/rustfmt, strict Credo and Dialyxir, ktfmt and detekt over all Kotlin source sets, and SwiftLint/SwiftFormat over handwritten Swift. Pin tools through Nix, Gradle, and Mix; share local checks with a read-only GitHub Actions workflow. Resolve existing violations while preserving default thresholds, without baselines.

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
