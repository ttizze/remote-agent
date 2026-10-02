# Bex

Bex controls Codex and Claude Code on a trusted computer from a Mac app, an iPhone app, an Android app, or a headless CLI. A Rust Host daemon owns the agent processes; every client connects to it over iroh with the same JSONL RPC peer and dispatches intents to the same Rust `Store`. Terminology is in [CONTEXT.md](CONTEXT.md); design decisions are in [docs/adr](docs/adr); behavior changes are in [CHANGELOG.md](CHANGELOG.md).

The workspace pins netwatch 0.19.3 to a fork commit containing [UDP rebind recovery](https://github.com/n0-computer/net-tools/pull/235), while iroh uses its published release. Failed UDP rebinds retry with exponential backoff from 100 ms to 5 s without terminating asynchronous I/O; explicitly closing a socket cancels recovery. Remove the patch when adopting an upstream release containing the fix.

## Supported operating systems

Except for Linux (existing CI baseline retained), BEX supports only the latest generally available OS major, including its stable minor/patch releases. Betas, release candidates, older majors and future majors are outside the support contract. Minimum deployment versions prevent installation on older Apple/Android systems; they do not impose a runtime upper-version kill switch.

Verified on **2026-09-13**:

| Platform | Supported major | Build / test baseline |
| --- | --- | --- |
| iPhone / iPad | iOS / iPadOS 26 | Xcode 26 stable, deployment target 26.0 in the app, UI tests, Rust and Swift bindings; iOS 26 Simulator |
| macOS desktop, Host and CLI | macOS Tahoe 26 | Rust deployment target and app/helper bundle minimum 26.0; Swift helper target 26.0 |
| Android | Android 17 (API 37) | Minimum, compile and target SDK 37; Nix build and test SDK platform 37 |
| Windows desktop, Host and CLI | Windows 11; Windows Server 2025 | Existing `windows-2025` CI builds/tests on Server 2025, not a Windows 11 UI acceptance test |
| Linux desktop, Host and CLI | Ubuntu 24.04 CI baseline | GitHub-hosted `ubuntu-24.04` runner and pinned `nix develop .#native` userspace |

Sources: [Apple security releases](https://support.apple.com/en-ca/100100), [Android 17 release](https://android-developers.googleblog.com/2026/06/Android-17.html), [API 37 SDK configuration](https://developer.android.com/about/versions/17/setup-sdk), [Windows client releases](https://learn.microsoft.com/en-us/windows/release-health/windows11-release-information), and [Windows Server releases](https://learn.microsoft.com/en-us/windows/release-health/windows-server-release-info). Windows feature updates such as 25H2/26H1 do not constitute a new Windows major.

Android 17 asks for nearby-device access before opening a LAN connection. Denial retains the permission screen with retry and Settings actions; users can also continue over the Internet (a LAN-only Host still needs permission). Returning from Settings with permission granted opens the app. The Internet choice survives activity recreation; a new launch checks permission again. Saved hosts and drafts remain in the existing repository.

When a new major becomes generally available, verify the vendor release, update these baselines together, remove obsolete compatibility paths, and run client acceptance tests before claiming support. Do not promote a beta/RC because an SDK or runner happens to include it. This policy does not select a Markdown rendering library.

## Layout

| Path | Role |
| --- | --- |
| `crates/agent-core` | Models, typed RPC operations, `Snapshot`, the `Store`, conversation presentation, iroh transport, diagnostics. The single state owner for every client. |
| `crates/agent-ffi` | UniFFI bindings for `agent-core`, consumed by Swift and Kotlin. |
| `crates/host-daemon` | The Host: pairing, authorization, Host RPC routing, Codex and Claude Code session runtime, worktrees, dictation, file/terminal access. |
| `crates/codex-app-server` | Codex App Server transport. |
| `crates/bex-process` | Shared process supervisor and PTY backend, independent of providers. |
| `crates/agent-cli` | Headless client for scripts and integration tests. |
| `crates/host-fixture` | Deterministic Codex and Claude subprocess fixtures and isolated iroh Host for tests. |
| `crates/xtask` | Background quality worker driven by Lefthook. |
| `apps/desktop` | Mac app (GPUI), with a native Alacritty terminal. |
| `apps/mobile` | iOS (SwiftUI, `iosApp/`) and Android (Compose, `src/`) over the UniFFI Store. |

Clients render immutable `Snapshot` values and never re-derive presentation: `agent-core::presentation` produces `RenderedConversation` rows for GPUI directly and for mobile through the bindings. Add logic to core, not to a client. See [ADR 0005](docs/adr/0005-rust-store-and-one-iroh-client-path.md).
Device storage uses the explicit [client state storage contract](docs/CLIENT_STATE_STORAGE.md).

The session architecture, limits, local data and verification matrix are documented in [Session runtime](docs/SESSION_RUNTIME.md).

## Run the Host

The Host needs Git and at least one available agent for conversations. Codex is optional; the Host prefers Codex bundled with ChatGPT Desktop on macOS, then `codex` on PATH; `--codex <path>` is authoritative.

```sh
nix develop . --command cargo build --locked -p bex-process --bin bex-provider-supervisor
nix develop . --command cargo run -p host-daemon -- --name 'BEX Host'
```

| Flag | Meaning |
| --- | --- |
| `--state-dir` | Credential directory for a new Host; otherwise the remembered directory is used (initial macOS default: `~/Library/Application Support/app.bex.BEX/`). It does not permit a second normal Host. |
| `--codex-home` | Selects a separate Codex store. |
| `--claude <path>` | Claude Code executable; defaults to `claude` on PATH. Desktop passes `BEX_CLAUDE` when set. |
| `--relay-url <url>…` / `--no-relay` | Custom iroh relays, or local addresses only for isolated fixtures. |
| `--isolated --state-dir <directory>` | Explicitly separate development/test Host. It still locks its own directory. |

Normal launches share one Host per OS user, including when desktop and daemon state directories differ. The platform data directory holds `host-instance.json` and a short discovery lock; each Host retains its directory lock for its lifetime. Discovery reuses the Host's identity and remembers its directory after restart. Competing starts are rejected before creating credentials. Desktop authenticates its connection to the registered Host.

Same-machine clients use an explicit IPv4 loopback QUIC connection with relays and address lookup disabled. `host.ticket` contains only the Host’s loopback address; remote invitations retain the ordinary LAN/Internet endpoint. Both paths use the same authenticated RPC and Host state. Local desktop sessions retain their Store across outages, rediscover the current local Host address, and resume with bounded exponential retry delays (250 ms to 5 s). Recovery reloads Host state without automatically resending uncertain submissions. A stopped Host remains stopped during background recovery.

State that must be backed up and never committed: the identity keys (`identity.keys`, 64 bytes, owner-only file permissions) and `trust.json` (invitations, allowlist, remote tickets). Errors append to `logs/host.jsonl` (desktop: `logs/desktop.jsonl`), rotated at 5 MiB with four archives and best-effort credential redaction.

Connection outages also retain `network.change`, `network.socket.rebound`, `network.socket.rebind_failed`, `network.socket.closed`, `network.endpoint.receive_failed`, and QUIC endpoint/connection `network.*.io_error` records. These summarize selected events from the pinned iroh/netwatch/noq dependencies with error kinds and numeric OS error codes; dependency messages, addresses, and payloads are not saved. Identical failures are limited to one record per operation every 30 seconds; changed error codes and failures after a network change or successful rebind are recorded immediately. `previous_suppressed` counts repeats omitted since the preceding emission for that operation, or across operations when a network change/rebind resets the failure window. These records remain enabled independently of the optional connection-performance timeline. When investigating, retain the rotated logs from both Host and Desktop and match their timestamps, PIDs, and build revisions before restarting.

### Headless Linux and iPhone pairing

Build both the Host and its companion supervisor with the pinned Linux environment:

```sh
nix develop .#native --command cargo build --locked --release -p host-daemon -p bex-process -p agent-cli
target/release/host-daemon --name 'Linux development'
```

Run management commands as the same OS user in another shell. They discover and authenticate to the running Host, without starting another daemon or rewriting its credentials:

```sh
target/release/host-daemon status
target/release/host-daemon invite
# With qrencode installed, display the invitation as a QR code:
target/release/host-daemon invite | qrencode -t ANSIUTF8
# Remove a paired device using its node ID from status:
target/release/host-daemon revoke <node-id>
```

On iPhone, open **PC一覧 → PCを追加 → QRコードを読み取る**, or paste the invitation JSON into **QRの内容を手入力**. Each invitation expires after seven days by default and can pair only one new device. Host administrators can set `--invitation-days <1–90>` when starting the Host; this affects newly issued invitations only. Generate a separate invitation for each device; keep invitation output out of logs and source control. Paired devices can reconnect after the invitation expires. The Host retains its identity and paired devices across restarts. For an isolated development Host, put the same `--isolated --state-dir <directory>` before `invite` or `status`.

Keep the default iroh relays enabled for Internet access. Run the service as the development user, with Git, the agent executables and Chromium on its PATH. Authenticate the agent accounts for that user and keep `bex-provider-supervisor` beside `host-daemon`. The iPhone operates the Linux filesystem, terminals and browser; select the Linux checkout when starting a project task. For systemd, use `KillSignal=SIGINT` so the Host shuts down its providers cleanly.

For a trusted shared workspace, Codex can use a team API key configured with `codex login --with-api-key` through standard input. The Host discovers this as an **API key** account and retains its selection across restarts. API billing is separate from ChatGPT subscription usage, so no subscription allowance is shown. Set `forced_login_method = "api"` in the shared Codex configuration to keep it on API authentication. Each paired device shares the Host's accounts and credentials; separate task worktrees isolate source changes.

Host preferences for worktrees (`bex-worktrees.json`) and the directory for chats without a project (`bex-chats/`) live beside the Codex project state, normally `$CODEX_HOME` or `~/.codex`. New worktrees default to `<original-repository>/.worktree/session-XXXXX/<repository-name>`; a custom directory replaces `.worktree` as the storage root. The repository name remains the checkout folder name, including when starting from another worktree. Existing worktrees stay in place. The default `.worktree` directory is excluded through Git’s local `info/exclude`. Worktree settings are edited from Mac **設定 → ワークツリー** or iPhone **タスク一覧 → … → ワークツリー設定**.

To remove a saved PC on iPhone, open **タスク一覧 → PC一覧 → 接続を解除** and confirm. This deletes its authentication key on that iPhone and prevents reconnection after relaunch, while retaining Host conversation data. On Mac, **設定 → 端末と接続 → 接続を解除** revokes the device’s access and closes active connections. Pair again to reconnect. Removing a PC on iPhone does not remove the old device entry from the Mac.

## Build the clients

```sh
# Mac (requires a signing certificate; BEX_CODE_SIGN_IDENTITY selects it)
nix develop . --command just build-desktop-macos && open target/Bex.app

# iPhone (iOS 26): build Simulator libraries, then open Xcode
nix develop . --command scripts/build-agent-ios.sh simulator
open apps/mobile/iosApp/Bex.xcodeproj

# Android 17 (API 37)
nix develop . --command ./gradlew :apps:mobile:assembleDebug
```

Rerun the iOS library build after changing Rust sources. Desktop drafts and logs use `BEX_STATE_DIR`; Host discovery may select a different credential directory. An isolated desktop requires both `BEX_ISOLATED_HOST=1` and `BEX_STATE_DIR`; use a separate Codex home or fixture executable as well so tests cannot read personal provider state.

Run `nix develop . --command just dev` for a separate local Host with shared
provider accounts and conversation history. Check active tasks and stop the old
development Host before rebuilding; closing its window does not stop it. Avoid
running the same conversation on both Hosts at once.

## Conversation controls

On Mac and iPhone, select assistant text to quote it into the draft or ask about it in a side chat. Closing an iPhone side chat restores the original conversation and draft. Mac also supports right-click Copy and Google Search, and own-message hover actions for copying or returning text to the composer. Command activity starts collapsed while running and after reopening; explicit expansion is preserved. See the [conversation display contract](docs/DESKTOP_CONVERSATION_DISPLAY_CONTRACT.md).

Mac and iPhone place borderless Fast, model name and reasoning-strength controls immediately before microphone and send. The model name opens a picker with separate agent and account rows and a searchable catalog. The account row shows reported weekly quota; open it to switch accounts or enter account management for add/login/confirmed sign-out. Settings reaches the same management view. Account changes refresh the catalog while preserving supported model settings, conversation text and attachments. The Host owns authentication and the per-provider account selection shared by connected clients. Current adapters support Codex and Claude; Pi and third-party connection adapters are not yet available. For Claude, open the login page and paste the returned authorization code into Bex.

### Claude Code

Additional accounts use separate Claude configuration directories under the Host state directory; Claude Code owns their credentials. Their `projects` directories link to the native transcript tree, so changing accounts preserves conversation history. Account selection persists across Host restarts and applies to the next turn; an active turn finishes with its original account.

Install Claude Code on the Host through its normal package configuration, then add a Claude subscription account from Bex, or use an existing `claude auth login` session on the Host. Start a new conversation and select a **Claude · …** model in the composer’s model picker. Model names and supported effort levels come from the installed CLI's initialize response, cached for the selected account and invalidated on account selection. Without a Claude executable, the catalog continues to show Codex models. A failed provider is reported alongside the available models. Saved model choices survive an incomplete catalog; new drafts default to an available model.

The Host runs the unmodified CLI with streaming JSON input/output and its own permission callbacks. Claude manages its credentials; Bex does not copy subscription tokens into Codex or call Anthropic's inference API directly. A turn requires subscription authentication: API-key or unauthenticated sessions are rejected before the message is submitted. If an API key is configured in the Host environment, remove that override to use the subscription. The Codex account selector affects Codex only. See Anthropic's [authentication](https://code.claude.com/docs/en/authentication), [programmatic execution](https://code.claude.com/docs/en/headless), and [embedding conditions](https://code.claude.com/docs/en/legal-and-compliance).

Text, PNG/JPEG/GIF/WebP images, file references, tool approvals, user questions, interruption and subsequent turns use the same Store and authenticated transport as Codex. Working-directory selection and automatic worktree settings apply to both. Claude's native transcript is the only persistent conversation source. The Host reads its configured native directory (`--claude-home`, `CLAUDE_CONFIG_DIR`, or `~/.claude`) without launching inference and never writes Bex conversation records. Disconnecting a client leaves execution running; reconnecting opens the current snapshot, including currently valid unanswered requests, without replaying events.

Create a new conversation to switch between Codex and Claude. Claude currently accepts subsequent input after the active turn completes or is interrupted; the composer explains this limit and retains the draft during a running turn. Claude-to-Codex delegation and Claude forks/side chats are not implemented. Codex startup failure or exit does not stop the Host, Claude turns, pairing, files, or workspace selection. The Host owns PTYs independently of Codex, scopes each handle to its connection, and cleans them up on disconnect or shutdown. Dictation and Codex account operations still require Codex. Worktree deletion is blocked while Codex conversation activity cannot be checked. The CLI integration has been exercised with Claude Code 2.1.266.

Mac conversation file links open the Host's parent directory and file editor in the Files panel, retaining unsaved file drafts and revision checks. Side Chat displays files and diffs in its own panel with a return-to-chat control, preserving the conversation draft.

On Mac, open **ブラウザ → Chromeから取り込む** and select a Chrome profile to copy its cookies into Bex's browser. Allow the macOS Keychain prompt for **Chrome Safe Storage** when requested. The import leaves Chrome's database unchanged, merges cookies by domain/name/path, and reloads the current page after checking the saved cookies. Persistent cookies retain their expiration across Bex restarts; session cookies remain session-only. This is a one-time copy: repeat the import to refresh a login. Expired cookies and cookies partitioned by top-level site are excluded and counted. Cookie-independent sign-in state (such as local storage or device-bound credentials) is not copied, so some sites still require signing in inside Bex.

Cookie import supports macOS Chrome database versions 23 and 24. Reader and native-attribute regressions run with `nix develop . --command cargo test --locked -p bex-desktop --bin bex-desktop chrome_tests`. On macOS 26, `nix develop . --command cargo test --locked -p bex-desktop --test chrome_cookie_webview` exercises the real Browser view, encrypted fixture database, denied-access retry, authenticated HTTP, HttpOnly protection, and persistence in a fresh process using a disposable WebKit data store. The test also records the domain-scope loss in the pinned lb-wry `set_cookie` implementation, which is why import uses WebKit's native cookie store directly.

Desktop conversation lists and titles refresh after messages and completed turns. The execution header shows elapsed time and the latest action; command output has a copy control. Click the change summary or a changed file to open the diff, choose files from its selector, and expand long unchanged sections. During dictation the composer shows a microphone waveform with cancel, stop, and send controls; Escape cancels recording and preserves the draft.

Mac settings list Bex-created worktrees and their conversations. Move to another conversation before deleting its worktree, then confirm removal. The Host refuses removal when turns or tracked terminal processes are active, files have changes (including untracked or ignored files), the worktree is locked, or HEAD is detached. Removal retains branches, conversation history, and the worktree’s entry in the list. Sending another message automatically recreates a deleted worktree at the same path, with a new branch based on the original repository’s local `main`. Only the worktree path and its original project path are persisted; the previous branch and deletion status are not stored. This requires the updated Host (`host/worktree/list` and `host/worktree/remove`).

Voice input appends recognized text on Stop or submits it with the draft and attachments on Send. Successful transcription with no recognized text ends quietly and leaves drafts and attachments unchanged, even if Send was pressed. Recording, connection, malformed-response, and transcription failures still report errors without discarding the draft.

## Headless CLI

```sh
agent-cli --ticket <endpoint-ticket> --identity-file <32-byte-key> [--invitation <uuid>] list
agent-cli <connection> send <thread-id> <text> --client-message-id <id> [--model m] [--effort e]
agent-cli <connection> approve 7 --decision 2          # numeric request ID
agent-cli <connection> approve '"request-id"' --decision 2   # string request ID
```

`--stdio <fixture-executable>` replaces the ticket for local fixtures. Results print as JSON on stdout, errors on stderr.

## Agent peer CLI and skill

[`tools/agent-peer`](tools/agent-peer) is the source of the shared Claude/Codex
consultation CLI, skill, tests, and standalone Nix package. Develop it here with
its BEX consumers; the directory also builds independently on macOS and Linux.

```sh
nix profile install .#agent-peer
agent-peer-install-skills
```

The public `ttizze/agent-peer` repository contains only that directory, under MIT.
Publish a committed revision using a subtree split; this excludes the rest of
the BEX source and history:

```sh
peer_commit=$(git subtree split --prefix=tools/agent-peer HEAD)
git push git@github.com:ttizze/agent-peer.git "$peer_commit:refs/heads/main"
```

Make changes here, then publish the subtree again. Install a pinned public
commit on a VPS with `nix profile install github:ttizze/agent-peer/<commit>` and run
`agent-peer-install-skills` as the agent's service user. Provider CLIs and their
authentication remain machine-local; see the standalone README for details.

## Verify

```sh
nix develop . --command cargo test --workspace            # Rust, including the behavior corpus
nix develop . --command just iroh-e2e                     # real daemon over isolated iroh sessions
nix develop . --command just android-e2e                  # fresh Android 17 emulator: network permission, Markdown, persistence and Host recovery
nix develop . --command just ios-e2e [TestMethod…]        # Simulator XCUITest against a fixture Host
nix develop . --command just conversation-ui             # selection, side chat, and activity regressions
nix develop . --command just quality [rust|kotlin|swift]  # lint, core/desktop tests, Android emulator and conversation-ui
```

Claude contracts run with `nix develop . --command cargo test --locked -p host-fixture --test claude`. Build the companion supervisor first (see above). The tests use a deterministic external CLI boundary with real Store, iroh, Host routing, native transcript files and isolated Git/filesystem state. An anonymized transcript from Claude Code 2.1.266 also exercises native format compatibility. The opt-in `live_claude_subscription_completes_and_resumes_through_store_and_host` test uses the real authenticated CLI; set `BEX_LIVE_CLAUDE_PROGRAM` to its absolute path and run that test with `-- --ignored --exact` to verify subscription inference, resumption across a Host restart, interruption and successful input after interruption.

Linux CI uses GitHub-hosted Ubuntu 24.04 runners and the `nix develop .#native` shell. Toolchain lookup runs on the same Ubuntu baseline, and Windows consumes the Rust version from the pinned flake. Install Lefthook once per clone (`lefthook install`) to queue quality checks after each commit; read the result with `cargo xtask quality-status --wait`. Lint thresholds are the tools' defaults with no baselines; rule exceptions need review.

Android CI runs on GitHub-hosted Ubuntu 24.04 with the pinned Nix SDK. It runs Kotlin checks and unit tests and builds both app and instrumentation APKs. Emulator acceptance remains available through `just android-e2e` and the local quality suite.

Development and test builds keep filename/line-number backtraces without full variable debug information. Use `CARGO_PROFILE_DEV_DEBUG=full` when a debugger needs variables. Quality checks disable Rust incremental compilation; normal local builds retain it.

Every completed `just quality` run prunes inactive Cargo outputs across registered worktrees and the shared quality cache. Profiles last modified more than 3 days ago are removed; otherwise the oldest profiles are removed until inactive outputs total at most 32 GiB. Run `nix develop . --command just clean-builds --dry-run` to inspect the JSON plan, or omit `--dry-run` to apply it. Cleanup scans conventional `target` directories, their direct nested Cargo caches, and `.git/bex-quality/cargo-target`; it does not follow symlinked caches.

Cleanup holds Cargo's build/artifact locks and preserves their inodes. Running binaries, active worktree processes and locked builds are excluded from the idle budget. This is a post-check retention policy, not a hard disk quota: active builds can temporarily exceed it. Only recognized Cargo `debug`/`release` output directories are disposable; keep application backups and verification records outside those directories. Bundled apps, `target/qa` results, summaries and source files are retained. Rebuilding a cleaned profile regenerates its outputs.

`crates/host-fixture::test_support` shares isolated Host startup, connections and shutdown. `bex-ui-fixture DIRECTORY CODEX PORT_FILE [STREAM_DELAY_MS]` runs the Host and loopback pairing controls together with credentials in memory; Codex remains a subprocess to exercise the stdio boundary. `ios-e2e` removes this process, its fresh Simulator and Xcode build products after the run, and retains results under `target/qa`. Failed, skipped or missing tests fail the command. `android-e2e` similarly owns a fresh emulator and Host, first verifies permission denial/retry and real LAN traffic, then runs the Markdown, Store persistence and model recovery tests against the shipped JNI library and fixture Host. It retains logs and permission screenshots under `target/qa` and rejects missing or failed tests.

Simulator and fixture runs do not verify physical devices, production Keychain access, camera, or real Codex accounts.

On Unix, isolated Host fixtures raise their process file-descriptor soft limit to at least 4096 before opening endpoints, including direct `cargo test` runs. Higher limits and the hard limit are preserved; an insufficient hard limit fails startup explicitly. This covers the full 80-client admission test.
