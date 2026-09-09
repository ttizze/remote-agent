# Bex

Bex controls a long-lived Codex App Server through a separate Rust Host daemon. The current migration makes `agent-core` the state owner for the headless CLI, GPUI, SwiftUI and Compose clients. See [ADR 0005](docs/adr/0005-rust-store-and-one-iroh-client-path.md).

The core, headless CLI and daemon now use iroh and the same JSONL peer. Native UIs still use their previous interfaces and are replaced in the following stages; their builds remain temporarily broken. The existing UI behavior below records the functionality to preserve during those cutovers.

## Core and headless client

`agent-core` contains typed RPC operations, immutable serde models, `Snapshot`, the pure `reduce` function, and `Store`. Store owns one `RwLock<Arc<Snapshot>>`; subscribers receive immutable snapshots and submit typed `Intent`s. Deltas copy only the changed thread/turn/item path. JSON persistence is `serde_json::to_vec(&snapshot)` with no version wrapper or migration layer. Unknown model fields and raw RPC errors are retained.

`RpcPeer` exposes one typed request and one raw request. One ordered stream carries notifications, server requests and response markers. Store uses the markers to apply typed responses before later deltas while allowing approval responses during other requests. Closing waits for stream shutdown; iroh streams wait for acknowledgment of the final data. Applications must also await endpoint closure before terminating their runtime.

Only `agent-core::transport` exposes application transport through iroh. It provides identities, native endpoint tickets, expiring single-use invitation data, pure NodeId authorization, and bidirectional streams. Incoming connections expose only a node identity and a typed pairing request until the state owner persists trust and authorizes them. Failed handshakes close only that connection; endpoint shutdown ends the accept loop. The state owner commits pairing updates atomically before accepting another invitation use. A session uses one JSONL RPC stream; file transfers use separate streams on that session. Local fixtures disable public relays and address lookup.

Run the migration gates through Nix:

```sh
nix develop . --command cargo test -p agent-core -p agent-cli
nix develop . --command cargo clippy -p agent-core -p agent-cli --all-targets -- -D warnings
```

The tests execute all 87 expanded behavior-corpus cases and launch the actual CLI against isolated iroh fixture servers for listing, sending and numeric/string approval IDs. They also cover ordered Store publication, concurrent approval handling, unchanged draft preservation and ticket authorization. The daemon integration suite exercises real isolated iroh endpoints, pairing, concurrent approvals, binary transfers, title pagination, large history and worktree creation. These checks do not verify native UIs, production Keychain access, physical devices or other operating systems.

The CLI accepts either `--stdio <fixture-executable>` with repeatable `--stdio-arg`, or `--ticket <endpoint-ticket> --identity-file <existing-32-byte-client-key>` for an already paired iroh Host. A new client can supply `--invitation <invitation-UUID>` with its ticket to pair before its first operation. `--no-relay` disables relays and public address lookup for isolated fixtures. It prints JSON results to stdout and errors to stderr. Supported commands:

```sh
agent-cli <connection-options> list
agent-cli <connection-options> send <thread-id> <text> --client-message-id <submission-id>
agent-cli <connection-options> approve 7 --decision 2
agent-cli <connection-options> approve '"request-id"' --decision 2
```

Opening an iroh peer writes a blank JSONL line so a passive approval client registers without sending a dummy request. Host-originated approvals have no request deadline and remain pending until answered, resolved, or disconnected. Upstream App Server closure or lost events stops the Host and disconnects its clients.

Approval IDs are JSON values, preserving the difference between numeric and string IDs. The decision index refers to the request's advertised choices; index 2 declines requests using the default choices. The `send` command reads current thread state, then chooses start, steer or queue and preserves failed drafts in Store.

## Host credentials

Host identity and local client identity occupy one fixed 64-byte keyring entry (`app.bex.host`, account = canonical state directory). Pairing invitations, the allowlist and remote Host tickets are committed atomically to `trust.json` in that directory, with Unix mode 0600. Keyring access and filesystem commits run on blocking workers; failed commits leave the live authorization state unchanged. Unpaired connections have a separate bounded admission pool and cannot consume the 64 authorized-session slots.

On headless Linux without a keyring service, run `host-daemon --key-storage file --state-dir <private-directory>`. The explicit file backend keeps the same 64 key bytes in `identity.keys` (0600 on Unix). The default remains `--key-storage keyring`; an unavailable keyring is reported, never silently replaced. Windows state inherits the current user's application-data directory DACL. Back up both identity keys and `trust.json`; existing trust with missing keys fails startup.

The old in-development combined JSON credential format is no longer accepted. These storage formats contain credentials and must not be logged or committed.

## Existing application behavior

Mac and iPhone conversations show generated images outside collapsed work, including after reopening history. Images load from the selected Host's saved path or the inline result when no path is available. Image output is never truncated as an activity detail. Markdown file links resolve against the conversation's working directory on that Host, including escaped spaces and line suffixes. Mac opens image files in its image viewer and other files in the system application; iPhone downloads a temporary copy into a Quick Look sheet with a Close button. HTTP/HTTPS links open in the system browser.

Mobile opens paginated histories with the latest five native turns and a 500-item budget, consumed newest first in pages of 100, following the official Codex desktop paging strategy. Scrolling upward loads missing items inside a turn or an older five-turn page using the App Server's opaque cursors. The opening question stays visible when the middle of a turn is unloaded. Loaded prefixes survive overlapping live-history refreshes; returning pages cannot replace a different task after navigation. Legacy threads without paging support use the App Server's complete read.

Returning to the mobile task list clears conversation and new-task selection even while the Host is disconnected. The return does not require a live connection; fetching refreshed titles still does.

The iPhone loads activity headers before large command outputs, diffs, reasoning or tool results. Expand an activity to fetch its full details from the Host; failures remain visible with a reload action. This requires the updated mobile app and Host. The Mac client retains its existing inline-history read. Paging uses the documented [App Server history APIs](https://learn.chatgpt.com/docs/app-server).

The iPhone controller delegates display conversion to `IosViewStateProjector`. It applies every incoming event, then projects the latest state in 16 ms windows. Navigation and conversation observations are separate, so body deltas do not rebuild project and task lists. Unchanged items within an updated turn reuse their display objects. Projections preserve source order even when turn or item IDs repeat and release cached objects on eviction or conversation/Host changes. Accepted inputs and deferred-detail changes invalidate the affected display objects. Markdown parsing runs off the UI thread with one worker per visible message; pending text coalesces between parses with a 100 ms interval.

Mobile applies typed events without retaining a second raw notification log. Snapshot metadata retains paging cursors and unknown extensions without duplicating the parsed turn/item bodies; existing version 2 caches normalize on load. Streaming updates avoid sorting unrelated cached histories and remain in memory until completion, another durable checkpoint, or an explicit flush. Checkpoints coalesce over 200 ms before serialization off the UI thread without an intermediate JSON tree. Backgrounding flushes pending state; a forced termination before a checkpoint requires reloading the latest stream from the Host. Completed work folds separately for each user exchange, keeping earlier replies visible when later instructions share the same native turn. Scrolling toward older content suspends automatic bottom following, including small drags near the bottom.

The Mac UI is a native Rust executable built with GPUI Kit. Its bundle contains no React, Bun, or Node runtime. Existing message, attachment and file drafts remain in `desktop-drafts.json`; the Host IPC and pairing protocol are unchanged. Paste copied images into the chat or Side Chat composer with ⌘V or Ctrl+V. Enter sends committed text when the caret is at the end with no selection; Shift+Enter or Enter within the text inserts a newline. IME composition confirmation does not send. Pasted images use the existing attachment upload path; local copies stay in the state directory’s `attachments/` folder so saved drafts and conversation image references survive restarts.

Mac **設定 → ワークツリー** and iPhone **タスク一覧 → 右上の「…」→ ワークツリー設定** edit the same Host preferences: independent switches for creating a worktree for new sessions and copying files into it, a worktree destination directory, and one repository-relative copy path per line (for example `.env`, `.env.local`, or `config/local`). On Mac, click the Host name at the bottom of the sidebar (for example **この Mac**) to open settings. Switches save immediately; the destination and copy paths save when their input loses focus, and Enter also saves the destination. There is no Mac Save button. Writes run in order while text inputs remain editable, and failed saves preserve the inputs and show the error. On iPhone, choose the Host, edit the preferences, then select **保存**. iPhone **キャンセル** discards the draft; failed saves keep it available for correction. Both switches default to off. Preferences apply to new Mac, Side Chat, and iPhone sessions on that Host and survive app/Host restarts.

On the first message, the Host creates a `bex/session-*` branch from the selected checkout's current `HEAD`. **ワークツリーの保存先** accepts an absolute directory on the selected Host, with a separate `session-*` folder created inside it for each session. An empty destination keeps the default: `bex-worktrees/` under the repository's Git common directory. Changing the destination does not move existing worktrees. The session and Mac workspace tools use the returned worktree directory. Uncommitted changes are included only for explicitly configured copy paths. Regular files and directories are copied recursively, preserving file permissions and replacing the fresh checkout's versions of configured files. Missing paths are skipped, so a Host-wide list can cover several repositories. Copy paths reject absolute paths, parent traversal, Git metadata, symbolic links, and special files. Copy failure prevents session start and removes the newly created worktree/branch. The worktree root is owner-only, and copies stay on the selected Host.

Opening an existing session does not create another worktree or repeat copying. Turning either setting off does not remove existing worktrees or copies. Worktree sessions retain their original project membership, including projects rooted in repository subdirectories. Host preferences and worktree-to-project roots are stored in `bex-worktrees.json` beside the Codex Desktop project state (normally `$CODEX_HOME`, or `~/.codex`). The Host RPCs are `host/worktree/settings/read` and `host/worktree/settings/update`, with `createOnNewSession`, `copyOnCreate`, `copyPaths`, and `worktreeDirectory` (empty by default); both native `thread/start` and `host/thread/start` apply the preference.

Mac chat and Side Chat show submitted text and attachments immediately, before the Host replies. Accepted messages remain visible until the native echo supplies the matching `clientId`; requests use `clientUserMessageId`, as on mobile. Failed sends keep the draft available for correction or retry. Pending display state is held in memory.

The Mac uses standard GPUI sidebar, buttons, tabs, Markdown, editor and resizable panels. The conversation stays centered with a persistent composer. With the right panel closed, a compact workspace card shows changes and sources. Opening the panel presents Terminal, Side Chat, Browser and Files; tabs switch tools while retaining their state. Conversation presentation is implemented once in the `conversation-presentation` Rust crate, used directly by GPUI and through C/JNI by mobile: live commentary stays in source order, consecutive activities collapse into a count summary, and completed work folds separately for each user exchange while its answer stays visible. Expanding an activity shows a single-line command or descriptive tool title; opening its details shows the full command and output. Expansion resets on a turn-status change. The Rust projector returns source indices and display policy. Kotlin passes only item metadata, retaining message bodies and tool output locally; the PC borrows native values. Both clients also share event classification, lifecycle precedence, retry-error handling, automatic approval-review transitions, pending-input anchors and client-ID reconciliation. Native adapters apply the shared decisions to their own bodies. Rendering, Markdown, and expansion state remain native to each UI.

The Mac caches each turn's projection and remeasures changed rows in 16 ms update windows. Item bodies move into the conversation without a second copy; pending-input reconciliation stores only pending indices. Paginated native histories open through the same Host APIs as mobile, with five recent turns and a total initial budget of 500 items. Scrolling to the top or choosing **以前の履歴を読み込む** fetches earlier items and turns; current live values win overlapping pages. Large activity details load when expanded, with explicit retry on failure. Command, reasoning and raw activity output use a bounded scrollable text area so asynchronous Markdown parsing does not move the conversation. Request generations and cursors reject stale responses. Conversations owned by another Codex process follow Host history-change notifications while preserving fetched prefixes and details.

Mac sidebar tasks show a spinner while active and a white dot when they finish successfully outside the selected conversation. Opening the task successfully or starting new work clears the dot; failed and interrupted turns do not create one. Live status takes precedence over list responses already in flight. Completion dots last for the current app session and reset when switching Hosts.

The Mac sidebar omits Files, Changes, and Settings navigation rows. Above the composer, a change card shows the workspace file count, total additions/deletions, and the first three paths with their individual line counts. Expand the remaining paths or select **レビューする** to open the diff. Binary files display **バイナリ** instead of invented line counts.

Terminal runs a PTY on the selected Host through Codex’s experimental `process/*` APIs. Its standard xterm.js renderer runs in the macOS WebView; Node is only a Nix-provided build tool for fetching locked assets. Closing the panel keeps the shell alive; creating a new terminal, switching Hosts or closing the app ends that shell. Side Chat has an independent conversation and composer, with drafts in `desktop-side-drafts.json`. Browser uses the local Mac’s WebView, including when a remote Host is selected, and receives no Host/terminal IPC bridge. Files and changes use the selected Host’s existing revision-aware editor and Git diff services.

## Host setup

The daemon requires an installed Codex App Server and Git. It prefers Codex bundled with ChatGPT Desktop on macOS, then `codex` on PATH. An explicit `--codex` path is authoritative. Build and launch it through Nix:

```sh
nix develop . --command cargo run -p host-daemon -- --name 'BEX Host'
```

`--state-dir` overrides the platform application data directory; `--codex-home` selects a separate Codex store. A process lock prevents two daemons from using the same state. Host and local-client private keys, invitations, paired NodeIds and registered remote Hosts are stored as one record through `keyring`. The public `host.ticket` file contains the current endpoint address. Unix state directories are owner-only; Windows directories inherit their parent DACL, with the default under the user's local application data directory.

All clients connect directly through iroh. Management RPCs (`host/status`, `host/invite`, `host/revoke`, `host/listRemotes`, `host/registerRemote`, `host/removeRemote`) require the local client's NodeId. Invitations expire after five minutes and can be consumed once; persistence succeeds before authorization is published. Revocation closes active sessions. The client pairs through its existing endpoint, then registers the resulting ticket with its local Host. The daemon never starts a second endpoint using the client's identity.

Public iroh relays are the default. `--relay-url` supplies a custom list; `--no-relay` restricts isolated fixtures to direct local addresses. SSH, the custom Phoenix relay, and their repository deployment scripts are removed. This source change does not decommission previously deployed Fly infrastructure.

## Existing Mac setup (pending UI cutover)

Build and open the Mac app from the repository root:

```sh
nix develop . --command cargo xtask build-desktop-macos
open target/Bex.app
```

Mac builds require a stable signing certificate. Both build commands use the same `BEX_CODE_SIGN_IDENTITY`; when it is unset they select the only available Apple Development or Developer ID Application identity. With multiple identities, set the desired certificate SHA-1 explicitly. Ad-hoc (`-`) signing is rejected because it makes every changed Host binary a new Keychain identity.

When switching an existing installation from ad-hoc signing, choose **Always Allow / 常に許可** for the Host's existing Keychain entries once. Later builds signed with the same certificate identity and `app.bex.host` identifier retain that authorization. This does not grant other applications access or bypass a locked Keychain. Changing the signing identity or resetting permissions requires authorization again. See [Apple's designated-requirement explanation](https://developer.apple.com/library/archive/technotes/tn2206/_index.html).

The old UI's relay settings and invitation screen are not compatible with this daemon. The next migration replaces them with Store and iroh connections.

## Working with Codex

Mac main chat and Side Chat use the same composer implementation. Its layout follows iPhone: environment and project/folder menus above a new conversation, attachment chips above the rounded input, and attachment, model-settings, microphone and send controls along the bottom. The global **新しいチャット** opens with **チャット** (no selected project); project-row buttons retain their explicit project. The first send without a folder omits `cwd` and uses the Host default. The redundant top-right new-chat button is removed. Model settings and the model list open toward the conversation so the right panel's native browser and terminal cannot cover them.

Mac voice input records PCM16 mono audio at 24 kHz using a signed native helper and the existing `host/dictation/transcribe` route. **Stop** appends the transcript to the draft; **Send** submits the draft, transcript and attachments without first inserting the transcript into the input. Permission denial, recording/transcription errors and rejected sends preserve the draft. Navigation or hiding the composer cancels recording; switching away during transcription keeps the result in the original draft without sending. Closing the app closes the helper's input pipe, stops recording and removes its temporary audio directory. The first recording requires macOS microphone permission. Main chat and Side Chat keep independent drafts and recording state.

Choose a project or working folder, then start or open a conversation. On mobile, the new-conversation buttons open the existing chat screen with an empty history and focused composer. The first send creates the conversation; returning without sending creates nothing. Project buttons use the project root, the conversation header retains its working directory, and the unassigned-chat button uses the Host default. The Host groups unassigned conversations by Desktop workspace hints and configured project roots; explicit assignments and projectless choices take precedence. On iPhone, the PC list, task list and conversations use native navigation. The remote task list uses a native navigation bar with the Host name and connection status, one-line indented conversation titles, and a separate compose button on each project row. Expand or collapse a project by tapping its name; its compose button remains available when collapsed. On iOS 26, standard search lives in the bottom toolbar beside new chat; older iOS versions retain the system search placement. Refresh is available from the ellipsis menu or by pulling the list down. Conversations open through native push navigation; the system back button and a rightward swipe from the left edge return to the list while retaining unsent drafts. Returning from a conversation to the list fetches fresh projects and tasks, including conversations created by another client while the app stayed open. Projects are ordered by their most recently updated conversation; projects without conversations follow in their saved order. The list initially shows the five most recent projects, each with its five most recently updated conversations, followed by the five most recent unassigned chats. The Host reads indexed metadata without scanning history files and returns only these titles and identifiers; conversation bodies load when opened. On iPhone, project folders start collapsed; tap a folder to expand it. Manual folder expansion, **もっと見る** display counts, and the current search stay in place during list refresh, conversation navigation, and foreground return. Project, per-project conversation and unassigned-chat **もっと見る** buttons each reveal another ten entries independently until exhausted. Search queries the title index, including conversations outside the currently displayed entries. Conversation bodies retain the local twenty-thread cache limit. Live work supports approvals, questions, additional input and interruption. Accepted sends appear as soon as Codex acknowledges them, including additional inputs while it is busy. Native echoes replace the accepted message by client ID without duplication. Additional inputs stay after the preceding work instead of moving to the start of the turn; queued inputs remain visible until processed. New turns render from subscribed native events without reading an unmaterialized rollout. An open conversation owned by another Codex process refreshes when its persisted history changes, without replacing the screen or composer. That process exposes persisted items, not each unsaved token; its updates can arrive later than the official app’s own stream. Mobile keeps commentary in chronological order and collapses each consecutive command/tool group into a one-line count, including while work is running or interrupted. Tap a group to reveal its activities, then expand a command for its output. Groups retain independent expansion state while work is running. On completion, interim commentary and activities automatically fold into a work-duration row, leaving the final answer visible. Tap that row to reopen the completed work. Additional user inputs remain outside the folds; histories without an explicit final-answer phase use the last unphased assistant message as the answer. Approvals, questions, terminal errors and stop controls remain outside the groups; iPhone copy and branch actions remain on answers rather than commentary. Reconnection reloads authoritative history while preserving unsent drafts; it never automatically repeats a send or approval. On a fresh launch, the app opens the task list using the last selected Host. Foreground return refreshes data while preserving the current list or conversation, including a new-conversation draft. It retains a healthy connection across navigation and backgrounding, and retries a lost connection with a 1–30 second backoff. On iPhone, connection attempts use only a header spinner on the list and conversation. Project/task loading uses the same list-header spinner without a loading row; failures retain their error and retry action. Network-only recovery preserves the currently selected task. iOS suspension can stop networking; the app resumes recovery when it runs again. Navigation is saved even when a large history exceeds the disposable display-cache storage budget.

On Mac, the composer model button opens compact controls for the model, reasoning effort, and speed. The effort slider and speed menu use the connected Host’s model catalog; Fast appears only when advertised, and standard speed appears once. Reconnecting preserves supported choices; changing models, including when opening a conversation, synchronizes the effort slider and speed with that model’s supported defaults. Switching Hosts clears the previous Host’s options. New turns use the values selected when Send was pressed, including an explicit standard-speed override after Fast. Additional input to a running turn keeps its existing configuration.

On Mac, the composer model button opens compact controls for the model, reasoning effort, and speed. The effort slider and speed menu use the connected Host’s model catalog; Fast appears only when advertised. Changing models restores that model’s defaults. New turns use the values selected when Send was pressed, including an explicit standard-speed override after Fast. Additional input to a running turn keeps its existing configuration.

The gauge button inside the iPhone composer opens **アカウントとモデル**. Select a Codex ChatGPT account, then a model and reasoning strength. **Codex アカウントを追加** starts Codex's device-code login; open the supplied HTTPS page and enter the displayed code. Model and strength choices are saved per Host and use the active account's `model/list` catalog. New tasks and `turn/start` use those choices; additional input to an active turn retains its existing configuration.

Accounts share the Host's original App Server, `CODEX_HOME`, projects, and conversation history. Switching changes authentication without restarting that App Server or moving sessions. The Host restores the selected account on restart. It stores account metadata under its state directory's `codex-accounts/`; Codex manages added credentials in Keychain through authentication-only helpers. Access tokens and refresh requests stay on the Host. This integration uses Codex's documented experimental `chatgptAuthTokens` mode. If saved authentication cannot be restored, history remains accessible and new turns report the error until an account is selected successfully. Update both the iPhone app and Host to use these controls.

The branch icon beneath a completed answer creates a new conversation through that native turn, inclusive, using `thread/fork` with `lastTurnId`. Later turns stay in the original conversation. The new conversation opens with the inherited history and accepts further messages. Codex forks only at completed native-turn boundaries, so intermediate answers within the same running turn do not show the branch action.

The iPhone microphone button starts voice input and records until Stop or Send is pressed; the phone and Host impose no 30-second recording limit. While recording, **Stop** transcribes the audio and appends the result to the input field. **Send**, immediately to its right, transcribes and sends the draft, transcript and attachments directly, without first inserting the transcript into the input field. The recording controls contain Stop and Send, without a separate cancel button. Microphone denial and transcription errors preserve existing text; a rejected direct send keeps the transcript in the draft. Changing conversations during transcription cancels direct sending and retains the result in the original conversation's draft. Recording stops on backgrounding or interruption, and temporary audio files are removed when recording finishes or is canceled.

Voice input requires the updated iPhone app and Host, with Codex signed in to ChatGPT on the Host. The Host's `host/dictation/transcribe` operation forwards PCM16 mono audio to the Codex desktop dictation service and returns text. As in the desktop, it first uses `/dictation/stream` and submits the original recording to `/transcribe` if streaming fails. Both transports send the `userAgent` returned by the running App Server's initialization; omitting it caused Cloudflare to reject native transcription requests. The recording upload wraps the phone's unchanged samples in a WAV file, sends it as multipart `file`, and uses the Codex bearer token and its account ID. Stream completion follows the service's `session.updated` closed event; nonfatal session errors do not discard a transcript. Account credentials stay on the Host; no separate API key or browser-cookie sharing is used. These internal endpoints can change independently of the public App Server API.

An `unknown variant` error naming `host/dictation/transcribe` means the connected Host predates the dictation route and forwarded it to Codex. Rebuild the Host with `nix develop . --command cargo xtask build-host-macos`, replace the Host executable used by the Mac app, and restart that Host. Updating the iPhone alone or rebuilding a file while leaving the old Host process running does not activate the route. Restart only after accounting for active tasks.

The iPhone composer's **＋** menu offers **写真・動画**, **カメラ**, and **ファイル**. Select multiple photos and videos together, or capture a photo/video with the camera. Library selections upload in selection order; sending stays disabled until the batch finishes. If an export or upload fails, already attached files remain and the remaining selection stops with an error. Photos use image inputs, while videos use uploaded file references. Camera capture requires camera permission; recording sound requires microphone permission. The existing upload limit is 512 MiB per file. Media exports stay in temporary storage until upload completes, then the local copies are removed.

Chat attachments render as images on Mac and iPhone. Image references survive mobile acknowledgement and history caching; Host files load through authenticated transfer. Markdown image references and inline image data also render in answers. On iPhone and Mac, tap an image to open the image viewer. A small thumbnail rail on the left lists images generated in the current session, including older turns and within-turn history gaps; selecting a thumbnail changes the main image. Uploaded attachments remain viewable without entering the generated-image list. Save and Close appear together at the top right, in that order; iPhone uses download and × icons with accessible labels. iPhone uses Quick Look and saves the selected original to Photos with add-only permission. Mac uses the native save dialog and preserves the original file bytes. Image file-link previews offer the same controls.

**ファイル** browses the selected Host, attaches/uploads files, downloads files, and edits UTF-8 text. Directory links use native navigation, with the system back button and left-edge swipe returning to the previous directory. Saving checks the file revision; a conflicting write preserves the draft. **AI に編集を依頼** prepares a request in the composer for sending. **変更** shows the actual Git working-tree diff. Binary and oversized text files can be downloaded instead of edited. A paired device has the Host account's filesystem access; Codex retains its own execution sandbox and approval rules.

## iPhone build and verification

The iPhone conversation uses a dark native layout with expandable work rows, Markdown answers, copy and branch controls and a multiline composer. The `…` menu opens files and changes; a change-count pill above the composer shows the actual working-tree changes. The file count and added/deleted line counts refresh as commands, tools, and file edits progress, without waiting for the turn to finish. Counts, diffs, and workspace files use the open session's current working directory, even when refreshed task titles refer to a different project directory. Outside voice recording, an empty composer shows Stop during active work; entering text switches it to additional input.

New iPhone chats place the environment and folder menus directly above the composer. Choose a paired Host and one of its project folders, or **チャット** for an unassigned conversation. The folder menu can load more projects. Open conversations retain their own upload directory even when a refreshed recent-task list no longer includes their title, so subsequent attachments continue to use that conversation's workspace.

The iPhone app is SwiftUI over shared Kotlin state and Rust transport. Build its Simulator framework, then open the Xcode project:

```sh
nix develop . --command ./gradlew :apps:mobile:linkDebugFrameworkIosSimulatorArm64
open apps/mobile/iosApp/Bex.xcodeproj
```

Select the Bex scheme and an iPhone Simulator. Physical-device signing and installation are separate from this Simulator workflow. Before a physical-device Release archive, rebuild the device framework from the same checkout with `nix develop . --command ./gradlew :apps:mobile:linkReleaseFrameworkIosArm64`. Xcode links this prebuilt framework; building the Simulator framework or archiving Swift alone does not update the device's shared Kotlin code.

The fixture runner now starts an isolated iroh Host with a deterministic Codex process. The existing Simulator tests require the mobile UI cutover before they can use that Host; their runner still rejects failures and skipped tests:

```sh
nix develop . --command cargo xtask ios-e2e
```

Development commands, the Codex subprocess fixture, and the pairing HTTP fixture live in the Rust `crates/xtask` package. Run `cargo xtask --help` inside the Nix shell for available commands. Project-owned build and test tooling requires no Python or Shell scripts; the upstream Gradle wrapper remains the entry point for Kotlin builds.

`ios-e2e` runs the existing 36-test selection by default. Append Simulator test method names to run a specific selection. Each run builds the app once and owns one fresh Host, loopback pairing server, and Simulator shared by the selected tests, matching the former shell runner. The runner removes these fixtures and its Xcode build products on completion or interruption. Results and their JSON summaries remain under `target/qa`; `BEX_RELAY_RESULT_BUNDLE` selects an explicit result bundle path. A nonzero Xcode exit, failed or skipped test, or unexpected pass count fails the command.

The headless command runs the real daemon over isolated iroh sessions:

```sh
nix develop . --command cargo xtask iroh-e2e
```

Other verification commands:

Code quality runs locally on macOS with Xcode installed. Install the Nix-pinned Lefthook once per clone to run checks asynchronously after each commit:

```sh
nix develop .
lefthook install
nix-store --realise "$(dirname "$(dirname "$(command -v lefthook)")")" --add-root "$(git rev-parse --path-format=absolute --git-common-dir)/bex-lefthook"
```

The GC root keeps the installed hook executable available outside the development shell. Commit and push do not wait for checks. One worker checks immutable commits in temporary worktrees; consecutive queued commits from the same source worktree are replaced by its newest request. Results and logs remain under the shared Git directory in `bex-quality/`; no desktop notification is sent. GitHub Actions is no longer configured.

Agents and humans can inspect the current commit with `nix develop . --command cargo xtask quality-status --wait` (omit `--wait` for an immediate result). JSON includes the commit, status, log paths, and `workingTreeDirty`. Exit status is successful only for a passed commit and clean worktree. Waiting is bounded to one hour; missing, queued, failed, interrupted, and superseded results are not passes. An idle queue can be drained with `cargo xtask quality-worker`. A worker crash may leave its temporary checkout under `bex-quality/worktrees/`; logs are retained for diagnosis.

Manual checks remain available:

```sh
nix develop . --command cargo xtask quality
nix develop . --command cargo xtask quality rust # or kotlin, swift
```

The command checks every selected language and returns a failure if any check fails. It does not rewrite files. The background worker reuses Cargo caches and checks the committed worktree. Each check has a one-hour timeout.

| Language | Configuration and policy |
| --- | --- |
| Rust | `cargo fmt --all --check` and Clippy over all workspace targets, with warnings denied. Keep Clippy's default lint groups; do not enable `restriction` or `pedantic` wholesale. The toolchain is pinned by `flake.lock`. |
| Kotlin | ktfmt Gradle plugin 0.26.0 with Kotlin style and 120-column wrapping, plus detekt 1.23.8 with `buildUponDefaultConfig`, validated `detekt.yml`, and all `src` source sets, including tests and Native. Apply the official Compose naming/default-parameter adjustments. This stable release runs source analysis; its Kotlin 2.0 compiler does not establish Kotlin 2.3 type-resolution coverage. Kotlin compilation and tests remain separate checks. |
| Swift | Nix-pinned SwiftLint and SwiftFormat, `.swiftlint.yml` and `.swiftformat`, Swift 6.3 formatting syntax with Swift 5 language mode matching Xcode, four-space indentation, LF, 120-column wrapping, and inline commas. Lint handwritten iOS/macOS sources and UI fixtures; build products and dependencies are outside the included roots. |

Default thresholds remain enabled. There are no baselines or blanket failure suppression. New tool versions and individual rule exceptions require review. Formatting can be applied with `cargo fmt --all`, `./gradlew :apps:mobile:ktfmtFormat`, and `swiftformat apps/mobile/iosApp/Bex apps/mobile/iosApp/BexUITests apps/desktop/macos` in the Nix shell.

Configuration references: [Clippy lint groups](https://doc.rust-lang.org/stable/clippy/lints.html), [detekt configuration](https://detekt.dev/docs/1.23.8/gettingstarted/gradle/), [Compose adjustments](https://detekt.dev/docs/1.23.8/introduction/compose/), [SwiftLint](https://github.com/realm/SwiftLint), [SwiftFormat](https://github.com/nicklockwood/SwiftFormat).

```sh
nix develop . --command cargo test --workspace
nix develop . --command cargo test --package bex-desktop
nix develop . --command ./gradlew :apps:mobile:iosSimulatorArm64Test
```

Simulator evidence does not verify physical camera, physical-device networking or distribution. See [implementation evidence](docs/IMPLEMENTATION_PLAN.md).
