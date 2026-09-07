# Changelog

## Unreleased

- Install iPhone build 31. Preserve earlier AI responses when turn IDs are missing or repeated; accept legacy `turnId` when `id` is blank. Reduce streaming work by updating raw and typed state together, bounding only the affected Host cache, coalescing persistence off the UI thread, and removing redundant row observers and unused gateway defaults.

- Prepare iPhone build 30: separate display conversion from the controller and reuse unchanged turn projections while streaming. Release projections on history eviction and conversation/Host changes; preserve accepted inputs and same-length content updates. Remove the unused iOS directory-selection API and unreachable directory screen state.

- Install and launch iPhone build 29 with the new-chat context selectors and attachment-directory fix; rebuild the Release Kotlin framework for the physical-device archive.

- Place environment and project-folder menus directly above the iPhone new-chat composer, with a blank conversation area and native back navigation. Preserve the open conversation's upload directory when refreshed recent titles omit it, preventing subsequent attachments from failing with an absolute-path error.

- Fix clipped corners in iPhone AI responses by rounding message backgrounds without clipping their content.

- Add iPhone voice input through the Host's Codex dictation connection. Recording Stop returns the transcript to the composer; the adjacent Send button transcribes and sends directly. Preserve drafts on errors, keep failed sends editable, and remove temporary recordings on completion or cancellation.

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
