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
| `--state-dir` | Overrides the platform application-data directory (macOS: `~/Library/Application Support/app.bex.BEX/`). |
| `--codex-home` | Selects a separate Codex store. |
| `--key-storage keyring\|file` | Where the 64-byte Host and local-client identity lives. Use `file` on headless Linux without a keyring service. |
| `--relay-url <url>…` / `--no-relay` | Custom iroh relays, or local addresses only for isolated fixtures. |

State that must be backed up and never committed: the identity keys (keyring entry `app.bex.host`, or `identity.keys` with `--key-storage file`) and `trust.json` (invitations, allowlist, remote tickets). Errors append to `logs/host.jsonl` (desktop: `logs/desktop.jsonl`), rotated at 5 MiB with four archives and best-effort credential redaction.

Host preferences for worktrees (`bex-worktrees.json`) and the directory for chats without a project (`bex-chats/`) live beside the Codex project state, normally `$CODEX_HOME` or `~/.codex`. Worktree settings are edited from Mac **設定 → ワークツリー** or iPhone **タスク一覧 → … → ワークツリー設定**.

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

Rerun the iOS library build after changing Rust sources. The desktop `BEX_STATE_DIR` and `BEX_KEY_STORAGE=file` mirror the Host flags.

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
nix develop . --command just quality [rust|kotlin|swift]  # fmt, clippy -D warnings, ktfmt, detekt, swiftformat, swiftlint
```

Linux CI uses the `nix develop .#native` shell. Install Lefthook once per clone (`lefthook install`) to queue quality checks after each commit; read the result with `cargo xtask quality-status --wait`. Lint thresholds are the tools' defaults with no baselines; rule exceptions need review.

`crates/host-fixture::test_support` shares isolated Host startup, connections and shutdown. `bex-ui-fixture DIRECTORY CODEX PORT_FILE [STREAM_DELAY_MS]` runs the Host and loopback pairing controls together with credentials in memory; Codex remains a subprocess to exercise the stdio boundary. `ios-e2e` owns this process and a fresh Simulator, removes both after the run, and retains results under `target/qa`. Failed or skipped tests fail the command.

Simulator and fixture runs do not verify physical devices, production Keychain access, camera, or real Codex accounts.
