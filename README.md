# Bex

Bex controls Codex on a Mac from its Rust/GPUI Mac application or native iPhone application. The Mac can also control another paired Host. One Rust daemon owns the Codex App Server independently of application windows; multiple paired devices can operate it concurrently.

Both peers connect outbound to a local Phoenix relay. The relay routes opaque SSH ciphertext. Host-key pinning, per-device keys and expiring single-use invitations protect application access. Codex credentials and workspace files stay on the Host. Private keys and saved relay credentials use Keychain.

Mobile opens paginated histories with the latest five native turns and a 500-item budget, consumed newest first in pages of 100, following the official Codex desktop paging strategy. Scrolling upward loads missing items inside a turn or an older five-turn page using the App Server's opaque cursors. The opening question stays visible when the middle of a turn is unloaded. Loaded prefixes survive overlapping live-history refreshes; returning pages cannot replace a different task after navigation. Legacy threads without paging support use the App Server's complete read.

The iPhone loads activity headers before large command outputs, diffs, reasoning or tool results. Expand an activity to fetch its full details from the Host; failures remain visible with a reload action. This requires the updated mobile app and Host. The Mac client retains its existing inline-history read. Paging uses the documented [App Server history APIs](https://learn.chatgpt.com/docs/app-server).

The iPhone controller delegates display conversion to `IosViewStateProjector`. It applies every incoming event, then projects the latest state in 16 ms windows. Navigation and conversation observations are separate, so body deltas do not rebuild project and task lists. Unchanged items within an updated turn reuse their display objects. Projections preserve source order even when turn or item IDs repeat and release cached objects on eviction or conversation/Host changes. Accepted inputs and deferred-detail changes invalidate the affected display objects. Markdown parsing runs off the UI thread with one worker per visible message; pending text coalesces between parses with a 100 ms interval.

Mobile applies typed events without retaining a second raw notification log. Snapshot metadata retains paging cursors and unknown extensions without duplicating the parsed turn/item bodies; existing version 2 caches normalize on load. Streaming updates avoid sorting unrelated cached histories and remain in memory until completion, another durable checkpoint, or an explicit flush. Checkpoints coalesce over 200 ms before serialization off the UI thread without an intermediate JSON tree. Backgrounding flushes pending state; a forced termination before a checkpoint requires reloading the latest stream from the Host. Completed work folds separately for each user exchange, keeping earlier replies visible when later instructions share the same native turn. Scrolling toward older content suspends automatic bottom following, including small drags near the bottom.

The Mac UI is a native Rust executable built with GPUI Kit. Its bundle contains no React, Bun, or Node runtime. Existing message, attachment and file drafts remain in `desktop-drafts.json`; the Host IPC and pairing protocol are unchanged. Paste copied images into the chat or Side Chat composer with ⌘V or Ctrl+V. Enter sends committed text when the caret is at the end with no selection; Shift+Enter or Enter within the text inserts a newline. IME composition confirmation does not send. Pasted images use the existing attachment upload path; local copies stay in the state directory’s `attachments/` folder so saved drafts and conversation image references survive restarts.

The Mac uses standard GPUI sidebar, buttons, tabs, Markdown, editor and resizable panels. The conversation stays centered with a persistent composer. With the right panel closed, a compact workspace card shows changes and sources. Opening the panel presents Terminal, Side Chat, Browser and Files; tabs switch tools while retaining their state. Completed work expands into activity rows and command output.

Terminal runs a PTY on the selected Host through Codex’s experimental `process/*` APIs. Its standard xterm.js renderer runs in the macOS WebView; Node is only a Nix-provided build tool for fetching locked assets. Closing the panel keeps the shell alive; creating a new terminal, switching Hosts or closing the app ends that shell. Side Chat has an independent conversation and composer, with drafts in `desktop-side-drafts.json`. Browser uses the local Mac’s WebView, including when a remote Host is selected, and receives no Host/terminal IPC bridge. Files and changes use the selected Host’s existing revision-aware editor and Git diff services.

For the Fly.io deployment configuration and rollout procedure, see
[Phoenix relay on Fly.io](docs/fly-relay.md). Its initial topology is one Tokyo
Machine; multiple-region Host routing is not implemented yet.

## Local setup

Requirements: Apple Silicon Mac, Xcode, Nix with flakes, and an installed, signed-in Codex. The daemon prefers Codex bundled with ChatGPT Desktop, then `codex` on PATH. `BEX_CODEX` selects an explicit executable for the Mac app. Public hosting, signup, billing and TestFlight distribution are outside this local setup.

Start the relay in a terminal:

```sh
nix develop .
cd apps/server
mix deps.get
# Enter your local relay token, then press Return.
read -r -s REMOTE_AGENT_RELAY_TOKEN
export REMOTE_AGENT_RELAY_TOKEN
PHX_SERVER=true PHX_BIND_IP=0.0.0.0 PORT=4000 mix run --no-halt
```

Use a private random token shared only with your Hosts. Binding `0.0.0.0` makes the relay reachable on the local network. For an iPhone or another Mac, use this Mac's LAN address in the relay URL; `127.0.0.1` only works on the same computer or its Simulator. The relay token controls relay access; pairing separately authorizes filesystem and Codex access.

Build and open the Mac app from the repository root:

```sh
nix develop . --command scripts/build-desktop-macos.sh
open target/Bex.app
```

Mac builds require a stable signing certificate. Both build commands use the same `BEX_CODE_SIGN_IDENTITY`; when it is unset they select the only available Apple Development or Developer ID Application identity. With multiple identities, set the desired certificate SHA-1 explicitly. Ad-hoc (`-`) signing is rejected because it makes every changed Host binary a new Keychain identity.

When switching an existing installation from ad-hoc signing, choose **Always Allow / 常に許可** for the Host's existing Keychain entries once. Later builds signed with the same certificate identity and `app.bex.host` identifier retain that authorization. This does not grant other applications access or bypass a locked Keychain. Changing the signing identity or resetting permissions requires authorization again. See [Apple's designated-requirement explanation](https://developer.apple.com/library/archive/technotes/tn2206/_index.html).

In **接続・ペアリング**, enter `ws://<relay-Mac-LAN-address>:4000/socket/websocket`, the relay token and a unique runner ID for this Host. The settings are saved to Keychain. The app starts the Host daemon and restores it on later launches. Closing the UI leaves its daemon and active Codex work running.

Choose **iPhone・Mac を招待** to create an invitation. Scan its QR code or paste its invitation in the iPhone app; on another Mac paste it under **別の Mac に接続**. Invitations expire and can be used once. Remove a paired device from the owning Mac to close its current sessions and reject future connections.

## Working with Codex

Choose a project or working folder, then start or open a conversation. On mobile, the new-conversation buttons open the existing chat screen with an empty history and focused composer. The first send creates the conversation; returning without sending creates nothing. Project buttons use the project root, the conversation header retains its working directory, and the unassigned-chat button uses the Host default. The Host groups unassigned conversations by Desktop workspace hints and configured project roots; explicit assignments and projectless choices take precedence. On iPhone, the PC list, task list and conversations use native navigation. The remote task list uses a native navigation bar with the Host name and connection status, one-line indented conversation titles, and a separate compose button on each project row. Expand or collapse a project by tapping its name; its compose button remains available when collapsed. On iOS 26, standard search lives in the bottom toolbar beside new chat; older iOS versions retain the system search placement. Refresh is available from the ellipsis menu or by pulling the list down. Conversations open through native push navigation; the system back button and a rightward swipe from the left edge return to the list while retaining unsent drafts. Returning from a conversation to the list fetches fresh projects and tasks, including conversations created by another client while the app stayed open. Projects are ordered by their most recently updated conversation; projects without conversations follow in their saved order. The list initially shows the five most recent projects, each with its five most recently updated conversations, followed by the five most recent unassigned chats. The Host reads indexed metadata without scanning history files and returns only these titles and identifiers; conversation bodies load when opened. Project, per-project conversation and unassigned-chat **もっと見る** buttons each reveal another ten entries independently until exhausted. Search queries the title index, including conversations outside the currently displayed entries. Conversation bodies retain the local twenty-thread cache limit. Live work supports approvals, questions, additional input and interruption. Accepted sends appear as soon as Codex acknowledges them, including additional inputs while it is busy. Native echoes replace the accepted message by client ID without duplication. Additional inputs stay after the preceding work instead of moving to the start of the turn; queued inputs remain visible until processed. New turns render from subscribed native events without reading an unmaterialized rollout. An open conversation owned by another Codex process refreshes when its persisted history changes, without replacing the screen or composer. That process exposes persisted items, not each unsaved token; its updates can arrive later than the official app’s own stream. Mobile keeps commentary in chronological order and collapses each consecutive command/tool group into a one-line count, including while work is running or interrupted. Tap a group to reveal its activities, then expand a command for its output. Groups retain independent expansion state while work is running. On completion, interim commentary and activities automatically fold into a work-duration row, leaving the final answer visible. Tap that row to reopen the completed work. Additional user inputs remain outside the folds; histories without an explicit final-answer phase use the last unphased assistant message as the answer. Approvals, questions, terminal errors and stop controls remain outside the groups; iPhone copy/share actions remain on answers rather than commentary. Reconnection reloads authoritative history while preserving unsent drafts; it never automatically repeats a send or approval. On launch and foreground return, the app opens the task list using the last selected Host and fetches fresh projects and tasks. It retains a healthy connection across navigation and backgrounding, and retries a lost connection with a 1–30 second backoff. On iPhone, connection attempts use only a header spinner on the list and conversation. Project/task loading uses the same list-header spinner without a loading row; failures retain their error and retry action. Network-only recovery preserves the currently selected task. iOS suspension can stop networking; the app resumes recovery when it runs again. Navigation is saved even when a large history exceeds the disposable display-cache storage budget.

The gauge button at the lower right inside the iPhone composer opens a native settings sheet with the selected model and a segmented reasoning-strength picker. Tap the model row for detailed model and strength selectors. The system back button returns to quick settings; Done dismisses the sheet. Choices are saved per Host; available models and supported strengths come from that Host's `model/list`. A chosen model applies to task creation and new `turn/start` requests. Additional input to active work (`turn/steer` or queued input) retains the running task's configuration. Selecting **現在の設定** uses the task/Host settings.

The iPhone microphone button starts voice input, with a maximum recording duration of 30 seconds. While recording, **Stop** transcribes the audio and appends the result to the input field. **Send**, immediately to its right, transcribes and sends the draft, transcript and attachments directly, without first inserting the transcript into the input field. Cancel discards the recording. Microphone denial and transcription errors preserve existing text; a rejected direct send keeps the transcript in the draft. Changing conversations during transcription cancels direct sending and retains the result in the original conversation's draft. Recording stops on backgrounding or interruption, and temporary audio files are removed when recording finishes or is canceled.

Voice input requires the updated iPhone app and Host, with Codex signed in to ChatGPT on the Host. The Host's `host/dictation/transcribe` operation forwards PCM16 mono audio to the Codex desktop dictation service and returns text. As in the desktop, it first uses `/dictation/stream` and submits the original recording to `/transcribe` if streaming fails. Both transports send the `userAgent` returned by the running App Server's initialization; omitting it caused Cloudflare to reject native transcription requests. The recording upload wraps the phone's unchanged samples in a WAV file, sends it as multipart `file`, and uses the Codex bearer token and its account ID. Stream completion follows the service's `session.updated` closed event; nonfatal session errors do not discard a transcript. Account credentials stay on the Host; no separate API key or browser-cookie sharing is used. These internal endpoints can change independently of the public App Server API.

The iPhone composer's **＋** menu offers **写真・動画**, **カメラ**, and **ファイル**. Select multiple photos and videos together, or capture a photo/video with the camera. Library selections upload in selection order; sending stays disabled until the batch finishes. If an export or upload fails, already attached files remain and the remaining selection stops with an error. Photos use image inputs, while videos use uploaded file references. Camera capture requires camera permission; recording sound requires microphone permission. The existing upload limit is 512 MiB per file. Media exports stay in temporary storage until upload completes, then the local copies are removed.

Chat attachments render as images on Mac and iPhone. Image references survive mobile acknowledgement and history caching; Host files load through authenticated transfer. Markdown image references and inline image data also render in answers.

**ファイル** browses the selected Host, attaches/uploads files, downloads files, and edits UTF-8 text. Directory links use native navigation, with the system back button and left-edge swipe returning to the previous directory. Saving checks the file revision; a conflicting write preserves the draft. **AI に編集を依頼** prepares a request in the composer for sending. **変更** shows the actual Git working-tree diff. Binary and oversized text files can be downloaded instead of edited. A paired device has the Host account's filesystem access; Codex retains its own execution sandbox and approval rules.

## iPhone build and verification

The iPhone conversation uses a dark native layout with expandable work rows, Markdown answers, copy/share controls and a multiline composer. The `…` menu opens files and changes; a change-count pill above the composer opens the actual working-tree diff. Outside voice recording, an empty composer shows Stop during active work; entering text switches it to additional input.

New iPhone chats place the environment and folder menus directly above the composer. Choose a paired Host and one of its project folders, or **チャット** for an unassigned conversation. The folder menu can load more projects. Open conversations retain their own upload directory even when a refreshed recent-task list no longer includes their title, so subsequent attachments continue to use that conversation's workspace.

The iPhone app is SwiftUI over shared Kotlin state and Rust transport. Build its Simulator framework, then open the Xcode project:

```sh
nix develop . --command ./gradlew :apps:mobile:linkDebugFrameworkIosSimulatorArm64
open apps/mobile/iosApp/Bex.xcodeproj
```

Select the Bex scheme and an iPhone Simulator. Physical-device signing and installation are separate from this Simulator workflow. Before a physical-device Release archive, rebuild the device framework from the same checkout with `nix develop . --command ./gradlew :apps:mobile:linkReleaseFrameworkIosArm64`. Xcode links this prebuilt framework; building the Simulator framework or archiving Swift alone does not update the device's shared Kotlin code.

The isolated end-to-end runner builds the app, starts a real Phoenix relay and encrypted Host with a deterministic Codex fixture, creates a fresh Simulator, exercises mobile UI flows and checks the xcresult for failures and skips:

```sh
nix develop . --command apps/mobile/iosApp/BexUITests/Fixtures/run_relay_ui_test.sh
```

Other verification commands:

```sh
nix develop . --command cargo test --workspace
nix develop . --command cargo test --package bex-desktop
nix develop . --command ./gradlew :apps:mobile:iosSimulatorArm64Test
nix develop . --command sh -c 'cd apps/server && mix test'
```

Simulator evidence does not verify physical camera, physical-device networking or distribution. See [implementation evidence](docs/IMPLEMENTATION_PLAN.md).
