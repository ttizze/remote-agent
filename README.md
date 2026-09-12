# Bex

Bex controls a long-lived Codex App Server on a trusted computer from a Mac app, an iPhone app, an Android app, or a headless CLI. A Rust Host daemon owns the Codex process; every client connects to it over iroh with the same JSONL RPC peer and dispatches intents to the same Rust `Store`. Terminology is in [CONTEXT.md](CONTEXT.md); design decisions are in [docs/adr](docs/adr); behavior changes are in [CHANGELOG.md](CHANGELOG.md).

## Layout

| Path | Role |
| --- | --- |
| `crates/agent-core` | Models, typed RPC operations, `Snapshot`, the `Store`, conversation presentation, iroh transport, diagnostics. The single state owner for every client. |
| `crates/agent-ffi` | UniFFI bindings for `agent-core`, consumed by Swift and Kotlin. |
| `crates/host-daemon` | The Host: pairing, authorization, Codex RPC routing, thread watches, worktrees, dictation, file/terminal access. |
| `crates/codex-app-server` | Spawns and initializes the Codex App Server process. |
| `crates/agent-cli` | Headless client for scripts and integration tests. |
| `crates/host-fixture` | Deterministic Codex simulator and isolated iroh Host for tests. |
| `crates/xtask` | Background quality worker driven by Lefthook. |
| `apps/desktop` | Mac app (GPUI). xterm.js assets under `web/` are fetched at build time; no Node runtime ships. |
| `apps/mobile` | iOS (SwiftUI, `iosApp/`) and Android (Compose, `src/`) over the UniFFI Store. |

Clients render immutable `Snapshot` values and never re-derive presentation: `agent-core::presentation` produces `RenderedConversation` rows for GPUI directly and for mobile through the bindings. Add logic to core, not to a client. See [ADR 0005](docs/adr/0005-rust-store-and-one-iroh-client-path.md).

## Run the Host

The Host needs Git and a Codex App Server. It prefers Codex bundled with ChatGPT Desktop on macOS, then `codex` on PATH; `--codex <path>` is authoritative.

```sh
nix develop . --command cargo run -p host-daemon -- --name 'BEX Host'
```

| Flag | Meaning |
| --- | --- |
| `--state-dir` | Credential directory for a new Host; otherwise the remembered directory is used (initial macOS default: `~/Library/Application Support/app.bex.BEX/`). It does not permit a second normal Host. |
| `--codex-home` | Selects a separate Codex store. |
| `--key-storage keyring\|file` | Where the 64-byte Host and local-client identity lives. Use `file` on headless Linux without a keyring service. |
| `--relay-url <url>…` / `--no-relay` | Custom iroh relays, or local addresses only for isolated fixtures. |
| `--isolated --state-dir <directory>` | Explicitly separate development/test Host. It still locks its own directory. |

Normal launches share one Host per OS user, including when desktop and daemon state directories differ. The platform data directory holds `host-instance.json` and a short discovery lock; each Host retains its directory lock for its lifetime. Discovery reuses the Host's identity and key-storage backend and remembers them after restart. Competing starts are rejected before creating credentials. Upgrades check the platform directory, the desktop's `BEX_STATE_DIR`, and the former `~/.bex` directory for a running Host; conflicting legacy Hosts are reported without stopping active work. Desktop verifies the live management route before using a discovered ticket.

State that must be backed up and never committed: the identity keys (keyring entry `app.bex.host`, or `identity.keys` with `--key-storage file`) and `trust.json` (invitations, allowlist, remote tickets). Errors append to `logs/host.jsonl` (desktop: `logs/desktop.jsonl`), rotated at 5 MiB with four archives and best-effort credential redaction.

Host preferences for worktrees (`bex-worktrees.json`) and the directory for chats without a project (`bex-chats/`) live beside the Codex project state, normally `$CODEX_HOME` or `~/.codex`. Worktree settings are edited from Mac **設定 → ワークツリー** or iPhone **タスク一覧 → … → ワークツリー設定**.

To remove a saved PC on iPhone, open **タスク一覧 → PC一覧 → 接続を解除** and confirm. This deletes its authentication key on that iPhone and prevents reconnection after relaunch, while retaining Host conversation data. On Mac, **設定 → 端末と接続 → 接続を解除** revokes the device’s access and closes active connections. Pair again to reconnect. Removing a PC on iPhone does not remove the old device entry from the Mac.

## Build the clients

```sh
# Mac (requires a signing certificate; BEX_CODE_SIGN_IDENTITY selects it)
nix develop . --command just build-desktop-macos && open target/Bex.app

# iPhone (iOS 17+): build Simulator libraries, then open Xcode
nix develop . --command scripts/build-agent-ios.sh simulator
open apps/mobile/iosApp/Bex.xcodeproj

# Android (API 28+)
nix develop . --command ./gradlew :apps:mobile:assembleDebug
```

Rerun the iOS library build after changing Rust sources. Desktop drafts and logs use `BEX_STATE_DIR`; Host discovery may select a different credential directory. `BEX_KEY_STORAGE=file` selects file keys for a new Host. An isolated desktop requires both `BEX_ISOLATED_HOST=1` and `BEX_STATE_DIR`; use a separate Codex home or fixture executable as well so tests cannot read personal provider state.

## Conversation controls

On Mac and iPhone, select assistant text to quote it into the draft or ask about it in a side chat. Closing an iPhone side chat restores the original conversation and draft. Mac also supports right-click Copy and Google Search, and own-message hover actions for copying or returning text to the composer. Command activity starts collapsed while running and after reopening; explicit expansion is preserved. See the [conversation display contract](docs/DESKTOP_CONVERSATION_DISPLAY_CONTRACT.md).

The composer gauge opens **アカウントとモデル**. On Mac, **Codex アカウント** selects a saved account; opening the menu refreshes the account list, and switching refreshes the model catalog while retaining the conversation and draft. Add accounts through **Codex アカウントを追加** on iPhone. The Host owns authentication and persists the selected account across restarts; account switching shares the existing Codex process and conversation history.

Mac conversation file links open the Host's parent directory and file editor in the Files panel, retaining unsaved file drafts and revision checks. Side Chat displays files and diffs in its own panel with a return-to-chat control, preserving the conversation draft.

Desktop conversation lists and titles refresh after messages and completed turns. The execution header shows elapsed time and the latest action; command output has a copy control. Click the change summary or a changed file to open the diff, choose files from its selector, and expand long unchanged sections. During dictation the composer shows a microphone waveform with cancel, stop, and send controls; Escape cancels recording and preserves the draft.

Mac settings list Bex-created worktrees and their conversations. Move to another conversation before deleting its worktree, then confirm removal. The Host refuses removal when turns or tracked terminal processes are active, files have changes (including untracked or ignored files), the worktree is locked, or HEAD is detached. Removal retains branches and conversation history, but the working directory is unavailable for resuming work. This requires the updated Host (`host/worktree/list` and `host/worktree/remove`).

Voice input appends recognized text on Stop or submits it with the draft and attachments on Send. Successful transcription with no recognized text ends quietly and leaves drafts and attachments unchanged, even if Send was pressed. Recording, connection, malformed-response, and transcription failures still report errors without discarding the draft.

## Headless CLI

```sh
agent-cli --ticket <endpoint-ticket> --identity-file <32-byte-key> [--invitation <uuid>] list
agent-cli <connection> send <thread-id> <text> --client-message-id <id> [--model m] [--effort e]
agent-cli <connection> approve 7 --decision 2          # numeric request ID
agent-cli <connection> approve '"request-id"' --decision 2   # string request ID
```

`--stdio <fixture-executable>` replaces the ticket for local fixtures. Results print as JSON on stdout, errors on stderr.

## Verify

```sh
nix develop . --command cargo test --workspace            # Rust, including the behavior corpus
nix develop . --command just iroh-e2e                     # real daemon over isolated iroh sessions
nix develop . --command just ios-e2e [TestMethod…]        # Simulator XCUITest against a fixture Host
nix develop . --command just conversation-ui             # selection, side chat, and activity regressions
nix develop . --command just quality [rust|kotlin|swift]  # lint, core/desktop tests, and conversation-ui
```

Linux CI uses the `nix develop .#native` shell. Install Lefthook once per clone (`lefthook install`) to queue quality checks after each commit; read the result with `cargo xtask quality-status --wait`. Lint thresholds are the tools' defaults with no baselines; rule exceptions need review.

`crates/host-fixture::test_support` shares isolated Host startup, connections and shutdown. `bex-ui-fixture DIRECTORY CODEX PORT_FILE [STREAM_DELAY_MS]` runs the Host and loopback pairing controls together with credentials in memory; Codex remains a subprocess to exercise the stdio boundary. `ios-e2e` removes this process, its fresh Simulator and Xcode build products after the run, and retains results under `target/qa`. Failed, skipped or missing tests fail the command.

Simulator and fixture runs do not verify physical devices, production Keychain access, camera, or real Codex accounts.
