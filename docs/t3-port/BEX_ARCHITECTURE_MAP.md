I've mapped the whole workspace. Most of what you want to keep is already cleanly separated: the iroh transport, the per-stream framing and the pairing/trust logic sit outside the conversation code. One part needs splitting before you can replace the runtime. `SessionRouter` in host-daemon holds both the connection registry and the conversation execution state, and the terminal code depends on the registry half.

Root is `/Users/tt/ghq/github.com/ttizze/remote-agent/.worktree/session-yo37yR/remote-agent`. All paths below are relative to it. Line counts exclude test modules and `tests/` directories, using a heuristic that cuts each file at its first `#[cfg(test)] mod`, so treat them as approximate.

## 1. Crates and apps

| Crate / app | Non-test LOC | Purpose | Main modules and entry points |
|---|---|---|---|
| `crates/agent-protocol` | ~3,380 | Wire types, Postcard encoding, the request table, and pure conversation update logic. No I/O. | `protocol.rs` (`Notification` :12, `encode`/`decode` :58/:61, `Response` :115, 16 MiB `MAX_FRAME_BYTES` :10); `protocol/requests.rs` (`contracts!` table :64-120, `Call` enum); `session.rs` (630); `models.rs` (338: `Thread`/`Turn`); `items.rs` (346); `requests.rs` (350: approvals and questions); `operations.rs` (548: params); `execution.rs`, `ids.rs`, `composer.rs`, `permissions.rs`, `browser.rs`, `diagnostics.rs`, `message.rs` (JSON-RPC line parsing for providers). The design rules are in `docs/CRATE_BOUNDARIES.md`. |
| `crates/agent-transport` | ~3,120 | iroh endpoint, sessions, stream framing, pairing gate, blob transfers, provider JSONL RPC peer, diagnostics. | `transport.rs` (478); `client/connection.rs` (395); `framing.rs` (185); `transfers.rs` (213); `peer.rs` + `peer/jsonl.rs` (676: provider stdio only); `diagnostics*` (~1,150) |
| `crates/host-daemon` | ~14,700 | The Host binary and library: accepts connections, runs Codex and Claude, plus files, terminals, worktrees, browser, dictation and accounts. | `main.rs`, `runtime.rs` (startup), `management.rs` (CLI `invite`/`status`/`revoke`), `host_runtime.rs` (connection loop), `host_identity.rs`, `local_host.rs`, `host_rpc/*` (~5,600), `claude.rs` + `claude/*` (~3,640), `codex_accounts.rs`, `account_usage.rs`, `terminals.rs`, `workspace_files.rs`, `worktrees.rs`, `workspace_review.rs`, `projects*`, `browser/*`, `dictation.rs`, `visualize.rs`. Public exports in `lib.rs`: `HostRuntime`, `HostRpcService`, `HostSession`, `SessionId`, `ProjectStore`, `HostCredentials`, `FileKeyStore`, `CredentialStore`, `load_local_identity`, `local_host`, `platform`, `browser` |
| `crates/agent-core` | ~9,600 | Client state owner (`Store`), pure reducer, operations, shared presentation logic, and the UniFFI bindings behind the `bindings` feature. | `store.rs` (1,432), `state.rs` (1,212), `state/operations/*` (~2,600), `state/notifications.rs`, `presentation/*` (~2,500), `client.rs`, `persistence.rs`, `composer.rs`, `bindings/*` (~1,290) |
| `crates/agent-ffi` | 5 | Native library shell: `lib.rs` calls `agent_core::uniffi_reexport_scaffolding!()`; `bin/bindgen.rs` runs the UniFFI generator. Crate types are `staticlib` and `cdylib`. | |
| `crates/agent-cli` | 216 | Headless client over `agent_core::Store` (`--ticket`, `--identity-file`, `--invitation`). | `src/main.rs` |
| `crates/codex-app-server` | ~307 | Starts `codex app-server` under the supervisor; JSON-RPC over stdio via `RpcPeer`. | `CodexAppServer::spawn` (lib.rs:64), `request`, `request_sequenced`, `subscribe`, `send_raw`, `shutdown` |
| `crates/bex-process` | 498 | Process ownership: `bex-provider-supervisor` owns a process group / Job Object, uses stdin as a lifetime pipe, and also hosts PTYs. | `command()` lib.rs:6, `terminal_command()` :41, `PtyCommand`/`PtyEvent`, `pty.rs`, `pty_session.rs` |
| `crates/host-fixture` | ~2,650 | Test fixtures: fake Codex/Claude/UI binaries, isolated real-Host startup, loopback pairing helpers. | `fixture/*`, `pairing.rs`, `test_support.rs`; tests `iroh_host.rs` (3,623), `claude.rs` (2,035), `adapter_conformance.rs` (831), `codex_accounts.rs` |
| `crates/xtask` | ~2,460 | iOS/Android e2e drivers, build cleanup, connection diagnostics, crate-boundary test. | `tests/crate_boundaries.rs` |
| `apps/desktop` (GPUI, `bex-desktop`) | ~11,560 | Desktop client. It also starts the local `host-daemon` as a child process. | `main.rs`, `app.rs` (2,066), `app/hosts.rs`, `app/view/*`, `platform/mod.rs`, `store_session.rs`, `terminal.rs`, `browser.rs`, `diff.rs` |
| `apps/mobile` | iOS ~6,100 Swift; Android ~2,200 Kotlin | SwiftUI and Compose clients on the UniFFI `AgentCore` module. | Detailed in section 5 |

## 2. Transport and pairing (keep)

### Wire format
- **ALPN:** `remote-agent/streams/5`, defined at `agent-transport/src/transport.rs:31`.
- **Stream kinds:** each QUIC bi-stream starts with one byte, defined at `client/connection.rs:18-21`:
  - `EVENTS=0` is the single per-connection notification feed (Host to client).
  - `CALL=1` is one RPC per stream.
  - `BLOB=2` carries binary transfers.
  - `CLOSE=3` is a graceful close.
- **Framing:** a 4-byte big-endian length prefix (`framing.rs:10` `write_frame`), read with `LengthDelimitedCodec` (`framing.rs:134` `Reader`), capped at 16 MiB.
- **Serialization:** Postcard (`agent-protocol/src/protocol.rs:58-70`).
  - Open-ended provider JSON is nested as a JSON string inside Postcard (`protocol::json` :156).
  - Byte fields go raw over QUIC and as base64 in JSON (`protocol::bytes`).
  - There are no request IDs; the stream itself owns the reply.

### Requests and subscriptions
- **Client side:** `Client::start_call` (`connection.rs:172`) opens a stream, writes `CALL` plus the Postcard `Call`, then reads the first frame as `Response<T>`. `request_stream` (:139) returns the remaining `Reader` as a subscription.
- **Subscriptions:** only `host/session/open` and `host/session/create` keep their response stream open. Each later frame on that stream is a raw Postcard `SessionChange` (`agent-core/src/client.rs:81-98`; `store.rs` run loop around :1130).

### Host accept loop and dispatch (`host-daemon/src/host_runtime.rs`)
- **Accept loop** (`run` :49-113): known peers get up to 64 slots and unpaired peers up to 16. A maintenance tick cleans up merged worktrees.
- **Handshake** (`serve` :114-156):
  - An allowlisted node goes through `IncomingSession::authorize`.
  - Otherwise `incoming.pairing()` reads the `Pair{invitation}` call (`transport.rs:272`), then `pair_node` (:280) persists the new trust, then `PairingRequest::authorize` (`transport.rs:311`) replies `Empty`.
- **Per-connection serving** (`serve_connection` :157-279):
  - It registers a `HostSession` (outbound notification queue) with `service.open_authenticated_session(node)`.
  - Each `CALL` is spawned (max 128 in flight) and goes to `dispatch` (:289).
  - The first frame written is `HostReply.initial`; if `HostReply.updates` is present, update frames follow on the same stream until the client stops it.
  - `BLOB` streams go to `service.files().transfer` (max 16).
  - The outgoing loop writes `HostSession::recv()` frames to the EVENTS stream.
- **Management dispatch** (`dispatch` :289-335, `manage` :336-425): `HostName`, `Pair`, `HostStatus`, `Invite`, `Revoke`, `ListRemotes`, `RegisterRemote` and `RemoveRemote` are handled here. Everything except `Pair` and `HostName` requires the local node identity. All other calls go to `HostRpcService::dispatch` (`host_rpc/service.rs:358`).

### Identity, pairing, devices and revocation
- **Trust model:** `Trust { allowed: BTreeSet<NodeId>, invitations: BTreeMap<Uuid, expiry> }` and the pure `authorize()` (`transport.rs:109-136`).
- **Host keys:** `host-daemon/src/host_identity.rs` keeps a 64-byte `identity.keys` (Host key + local-client key) and `trust.json` (`Record { trust, remotes }`). `persist` (:121) uses atomic writes. The local client node is pre-allowlisted (:94-104).
- **Invite** (`host_runtime.rs:353-371`) creates an `Invitation { endpoint ticket, invitation uuid, expires_at, host_name, ai_recipients, transcription_recipient }` (`agent-protocol/src/models.rs:10`). Lifetime is set by `--invitation-days` (`command_line.rs`).
  - The QR code is this Invitation serialized as JSON.
  - Desktop renders it with `qrcode` in `apps/desktop/src/app/hosts.rs:183-196`.
  - The CLI equivalent is `host-daemon invite` (`management.rs`).
  - Desktop can also fetch an invitation over SSH (`apps/desktop/src/platform/mod.rs:39` `ssh_invitation`).
- **Client-side parsing and validation:** `agent-core/src/bindings/mod.rs:69-104` (`parse_invitation`, `validate_invitation`, `ticket_identity`, `generate_identity` :55).
- **QR scanners:** iOS `apps/mobile/iosApp/Bex/QRCodeCaptureViewController.swift` (wrapped by `BexQrScannerSheet` in `SwiftUIRoot.swift:338`, with `PairingScreen` :83). Android `AndroidQrScanner.kt` (CameraX + ML Kit).
- **Device key storage:** iOS Keychain (`PlatformServices.swift:46-80`, service `app.bex.iroh.identity`); Android `AndroidCredentialStore.kt`.
- **Revoke** (`host_runtime.rs:372-388`): removes the node from `trust.allowed`, persists, calls `service.revoke_device` (which only affects terminals, `service.rs:284`), and closes the node's live connections. `serve_connection` re-checks the allowlist under the same lock (:164-177).
- **Remote-host pairing from desktop:** `PairRemoteHost` in `agent-core/src/state/operations/hosts.rs:79-148` connects to the remote ticket over the existing endpoint, sends `Pair`, then calls `host/registerRemote` on the local Host.
- **Local Host discovery and lease:** `host-daemon/src/local_host.rs` (registry + `host.lock`, publishes the loopback ticket); desktop `platform/mod.rs:253` `discover_local_host`, `:267` `start_host`.

### Reconnect (client)
- **Store side:**
  - `Store::connect` (`agent-core/src/store.rs:351`).
  - `Store::resume` (:363) probes the old connection with `SessionScope` within 1 s while a replacement connects in parallel. If the old one is still good, it re-reads the open thread, the list and models.
  - `Store::reconnect` (:484) always replaces the connection.
  - `Connection::open` (:81-121) opens the peer, sends `Pair` if there is an invitation, then reads the storage scope and starts the initial list.
- **Callers:**
  - Desktop `platform/mod.rs:196` `recover_local` (backoff loop).
  - iOS `AppViewModel.swift` `connect()` around :243, which calls `owner.resume`.
  - Android `App.kt:273`, which calls `store.reconnect`.
  - The UniFFI wrappers are `AgentStore.connect/reconnect/resume` (`bindings/mod.rs:255-273`); the endpoint is cached per identity (:180-224).

### What is cleanly separable
- **Fully separable:**
  - `agent-transport` except `peer.rs`/`peer/jsonl.rs`, which is provider stdio; keep it if the new adapters still use JSON-RPC over stdio.
  - `host_identity.rs`, `local_host.rs`, `management.rs`, `runtime.rs` (startup wiring needs edits).
  - `host_runtime.rs`: this is generic; it only calls `service.open_authenticated_session`, `service.dispatch`, `service.close_session`, `service.files().transfer`, `service.revoke_device` and `service.shutdown_owned_processes`.
  - In agent-protocol: `protocol.rs` encode/decode/`Response`, `error.rs`, `diagnostics.rs`, and the `Pair`/`Invite`/`Revoke`/`RemoteHost`/`HostStatus` records.
- **Needs splitting:**
  - The `Call` enum and the `results!` `Body` enum are one flat table that mixes management, files, terminals and conversation (`protocol/requests.rs:64-120`, `protocol.rs:94-112`).
  - `Notification` (`protocol.rs:12`) mixes terminal events with conversation `Activity` and `SessionRenamed`.
  - `SessionRouter` (`host_rpc/routing.rs`) holds both the connection registry (`sessions`, `Outbound` 16 MiB budget, `send`, `broadcast`, `principal`, `close_session`) and conversation execution state (`executions`, subscriptions, requests). `terminals.rs:3,83,105,394,402` uses the registry half, so extract that into its own type before removing the conversation half.
  - `ALPN` must be bumped (`transport.rs:31`) whenever Postcard types change.

## 3. Conversation runtime today (host-daemon)

### Service and adapters
- **Service:** `HostRpcService` (`host_rpc/service.rs:78-122`).
  - It holds `agents: HashMap<ProviderKind, Arc<dyn Agent>>` (Codex is registered in `new`, Claude through `enable_claude` :239), plus `router: SessionRouter`, `worktrees`, `files`, `terminals`, `dictation`, `browser`, `projects`.
  - Startup wiring is in `host-daemon/src/runtime.rs:63-110`: start Codex, `HostRpcService::new`, `enable_browser`, `enable_claude`, `enable_accounts`, `service.start()`.
- **Adapter trait:** `Agent: Identity` (`host_rpc/agent.rs:100-164`) covers `list`, `open`, `read_history`, `read_item`, `read_turn_items`, `create`, `state`, `submit(input, route, reload, browser)`, `interrupt`, `models`, `catalog`, `fork`, `rename`, `read_permissions`/`update_permissions`, `active_sessions_in`, `discard_workspace_processes`, `event_stream()` and `shutdown`. `Identity` covers the account commands.

### Codex
- One shared `codex app-server` process (`codex-app-server/src/lib.rs:64`), started through `bex_process::command`, i.e. the supervisor.
- The adapter is `host_rpc/codex.rs` (`impl Agent` :563).
- Native calls:
  - Submit (:748): `turn/start`, or `turn/steer` / `thread/queue/add`; `thread/resume` when the thread is not loaded.
  - Interrupt (:794): `turn/interrupt`.
  - Read state (:729): `thread/read`.
  - Create (:717): `thread/start`.
- Notification → `SessionChange` translation: `notification_change` :303, `event_change` :395.
- Approval translation: `request_change` :500, `RequestSource::prepare` :475.
- The `event_stream` pump is at :879; it also answers `account/chatgptAuthTokens/refresh`.
- Native history → BEX models: `host_rpc/native.rs` (`codex_item` :164, `codex_turn` :410, `codex_thread` :446).
- Related: `host_rpc/composer.rs` (skills/plugins catalog) and `host_rpc/permissions.rs` (Codex config / Claude settings permissions).

### Claude
- Adapter: `claude.rs` (`struct Claude` :92, `impl Agent` :1632).
- One CLI process per conversation, started with `-p --input-format stream-json --output-format stream-json --permission-prompt-tool stdio` and `--resume`/`--session-id` (`claude/process.rs:19-118`), also through `bex_process::command`.
- Limits: at most 8 processes (`Semaphore` :160); idle processes are reused (`Record`/`Idle`/`Running` :105-126).
- `Worker::run` (:847) parses stream events (`message` :1075, `stream_event` :1276, `permission` :1047) and turns them into `SessionChange`s.
- Turn control: `start_turn` :664, `additional_input` :620 (queued input), `interrupt` :584 (sends a `control_request` interrupt and waits 15 s).
- History is read from native JSONL without starting the CLI (`claude/history.rs`, `claude/native.rs`).

### Where conversation state lives
- Provider-native storage is the source of truth: Codex thread storage and Claude JSONL. The Host keeps no event log (`docs/SESSION_RUNTIME.md`).
- The Host only holds live execution in `SessionActor { timeline: Timeline, request_origins, leases, submission_lock, subscriptions }` (`host_rpc/session_actor.rs:5-79`).
  - `release()` drops finished turns.
  - `overlay()` merges live turns into a native read.
- `Timeline` (`agent-protocol/src/session.rs:172`) holds status, turns, requests and submissions. `SessionChange` (:99-151) has the variants `Submission`, `Request`, `RequestDelivery`, `ResolveRequest`, `Status`, `Turn`, `Item`, `TurnItems`, `RemoveItem`, `Text`, `ReasoningPart` and `Error`. It is applied by `apply`/`apply_timeline` (:180/:201) on both Host and client.

### How events reach clients
- Adapter `event_stream()` produces an `AgentEvent{AgentChange}`. `start_event_pumps` (`service.rs:1406`) calls `AgentChange::apply(router)` (`agent.rs:192`), which goes to `SessionRouter::session_change`/`request` (`routing.rs:651`/:603), then `change_locked` (:848).
- `change_locked` applies the change to the actor's Timeline, encodes the `SessionChange`, and pushes it to every subscription's `Outbound`. On `Status`/`Turn` changes it also broadcasts `Notification::Activity` on all EVENTS streams.
- **Open:** `session_open` (`service.rs:550`) reads native history, then `finish_session_read` (`routing.rs:459`) overlays live execution, defers large items, encodes `OpenedSession`, and returns a `HostReply` with a `HostSubscription`.
- **Client side:** each subscription is a `Reader` decoding `SessionChange`, fed into `Event::SessionUpdate` and then `state/notifications.rs:102` `session_update`. That applies the change to `snapshot.conversations[id]`; on failure it re-reads the thread.

### Approvals
- Native requests are normalized in `host_rpc/requests.rs` (`codex()` :165, `claude()` :311, `NativeAnswers::translate` :79) into `agent_protocol::requests::Request` and registered in `routing.rs:603`. Limits: 256 total, 32 per session, 64 KiB each.
- Answering: `host/session/answer` goes to `answer_request` (`service.rs:524`), then `claim_response` (`routing.rs:697`), then `origin.source.prepare(...)`, a write, and `RequestDelivery` updates.

### Queue, steer and stop
- **Routing:** `submission_target` (`host_rpc/submission.rs:17`) decides `Steer` / `Queue` / `Start`.
- **Submit path:** `dispatch` (`service.rs:358-455`) handles input-ID dedup (`begin_submission`/`finish_submission`, `routing.rs:304`/`351`), per-conversation `submission_lock`, and the worktree read lock. `submit_input` (:457) reads `agent.state`, overlays live execution, picks a route, runs `worktrees.ensure_available`, then `agent.submit`.
- **Stop:** `Call::Interrupt` → `agent.interrupt` (`service.rs` around :978).

## 4. agent-core (client state and view models)

### Store
- `Store` (`store.rs:263`) is the single state owner. A `watch::Sender<Arc<Snapshot>>` publishes snapshots and a command queue (256) feeds the run loop (`run` :979). Job limits: 32 RPC jobs + 16 control, 16 terminal, 4 item transfers.
- `Snapshot` (`state.rs:204-255`) contains `conversations: BTreeMap<SessionRef, Arc<Thread>>`, `threads: ThreadList`, `drafts`, `pending_submissions`, `subscriptions`, `navigation`, `activity`, `models`, `account`, `terminals`, `workspace`, `management`, `operations`, `connected` and `error`.
- The reducer is `reduce` (`state.rs:371`). `Event` (:347) has `Intent`, `Notification`, `SessionUpdate`, `Connected`/`Disconnected`, and others.
- `Intent` (`state/operations.rs:41-170`) has about 75 variants. Each operation implements `Operation` (:279: `capture`, `run`, `apply`, `stale`, `key`, `scheduling`).
- Conversation operations live in `state/operations/threads.rs` (658: `ReadThread`, `ReadOlder`, `ReadItem`, `LoadTurnItems`, `CreateSession`, `ForkSession`, `ListSessions`, `Interrupt`) and `submission.rs` (382: `StartSubmission`, `Respond`, `Dictate`). Others: `hosts.rs`, `accounts.rs`, `terminal.rs`, `workspace.rs`, `composer.rs`, `permissions.rs`.
- Persistence (`persistence.rs`) stores drafts, pending submissions, file drafts, navigation, unread markers and model defaults, scoped by storage scope.

### Presentation (shared by all clients)
- `presentation/conversation.rs` (`project_conversation` :250, `RenderedConversation`/`RenderedTurn`/`RenderedItem`/`ConversationRow`, `request()` :544, answer builders).
- `presentation/mod.rs` (`project`/`project_items` grouping, `item_presentation`).
- Also: `body.rs`, `list.rs`, `markdown.rs`, `diff.rs`, `model_settings.rs`, `permissions.rs`, `connections.rs`, `error.rs`.

### UniFFI bindings
Feature `bindings`; `uniffi = "=0.32.0"` with tokio; scaffolding is set up in `lib.rs`.
- `bindings/mod.rs`:
  - Free functions: `generate_identity`, `parse_invitation`, `validate_invitation`, `ticket_identity`, `apply_model_preferences`, `account_error_message`.
  - Object `AgentStore` (:106): `offline`, `connect`, `reconnect`, `resume`, `snapshot`, `next_snapshot`, `dispatch(Intent) -> Receipt`, `browser`, `prepare_dictation`, `record_connection_event`, `shutdown`.
  - `Receipt.wait() -> Outcome` (:357).
- `bindings/snapshot.rs`: `Snapshot` is a `uniffi::Object` with accessors (`conversation_source`, `conversation`, `draft`, `navigation`, `models`, `accounts`, …), plus `Thread`/`RenderedConversation`/`RenderedTurn`/`RenderedItem` objects and `project_conversation` (:229).
- `bindings/protocol.rs` (592): `#[uniffi::remote(Record/Enum)]` mirrors of protocol types (Invitation, Thread, Turn, Item, Request, …). `bindings/json.rs`: `JsonValue`.
- **Desktop** uses `agent_core::store::Store` and `Snapshot` directly in Rust, without the bindings (`apps/desktop/src/store_session.rs`, `platform/mod.rs:84-104`).

## 5. Clients

### Desktop (GPUI)
- `main.rs` sets up a tokio runtime, the `Runtime` global and `Desktop::new(Mode::Main)`.
- `app.rs`:
  - `Desktop` struct :172; `connect` :540; `dispatch`/`perform` :656-670; `accept_snapshot` :874; `sync_rows` :1119.
  - `send` :1340 and `respond` :1732.
  - Modes are `Main` and `SideChat`.
  - Panels (`Panel`): Home, Terminal, SideChat, Browser, Files, Diff.
  - Tabs: Chat and Settings.
- Views (`app/view/*`):
  - `conversation.rs` (`item` :159, `turn` :441, `request_card` :695)
  - `composer.rs` (`chat` :189)
  - `sidebar.rs` (thread list)
  - `workbench.rs`, `settings.rs`, `model_settings.rs`, `permissions.rs`, `onboarding.rs`, `media.rs`
  - `hosts.rs` (Host manager, QR invite, pairing to remote hosts)
- Other pieces: `terminal.rs` (alacritty), `browser.rs` (wry), `diff.rs`, `app/dictation.rs`, `platform/microphone.rs`.

### iOS (SwiftUI, module `AgentCore`)
- Screens: `AppScreen { pairing, profiles, threads, thread }` (`AppPresentationData.swift:5`).
  - Root: `SwiftUIRoot.swift` (`BexSwiftUIRoot`, `PairingScreen` :83, `ProfilesScreen` :245).
  - `TaskList.swift` (`ThreadsScreen`), `ConversationScreen.swift` (`ThreadScreen`), `ConversationRows.swift`, `ConversationRequest.swift`, `ConversationComposer.swift`, `ConversationSideChat.swift`.
  - Also `WorkspaceScreen.swift`, `TerminalScreen.swift`, `ModelSettingsSheet.swift`, `AgentSettings.swift`, `WorktreeSettings.swift`.
- `AppViewModel.swift`:
  - Holds `AgentStore`.
  - `initialize` → `AgentStore.offline` (:111); `confirmPairing` → `AgentStore.connect` (:185); `perform` → `dispatch` + `receipt.wait` (:216-242).
  - `connect` → `resume` (:267); `observe` → `nextSnapshot` loop (:289).
  - `projectConversation` (:347) feeds `ConversationPresentation.swift:21`, which calls `AgentCore.projectConversation`.
- Linking: Xcode links `libagent_ffi.a` + `libAgentCore.a` and the `AgentCoreFFI.modulemap` from `target/agent-bindings` (`project.pbxproj:408-441`).

### Android (Compose, package `dev.remoteagent.core`)
- `MainActivity.kt` creates the `AndroidAppModel` view model.
- `App.kt` (`AgentStore.offline` :179, `parseInvitation` :217, `AgentStore.connect` :223, `reconnect` :273, `observe`/`nextSnapshot` :294-299, `dispatch` :142; screens `RemoteAgentApp` :423, `PairingScreen` :470, `ProfilesScreen` :494, `ConversationPane` :516).
- Other files: `AndroidThreadList.kt`, `AndroidConversationScreen.kt` (`ThreadDetailScreen` :59, `ThreadComposer` :206), `AndroidConversationRows.kt`/`Cards.kt`, `ConversationProjection.kt`, `AndroidTerminalScreen.kt`, `AndroidQrScanner.kt`, `AndroidCredentialStore.kt`, `AndroidMobileRepository.kt`.

## 6. Build and test commands

- **Unit tests:**
  - Run through the environment wrapper, as `AGENTS.md` requires: `scripts/dev-env.sh just unit-tests`.
  - `scripts/dev-env.sh` loads the Nix dev env from `nix print-dev-env` (cached under the git common dir), sets `CARGO_TARGET_DIR=target` and a per-worktree `CARGO_HOME` seeded from `~/.cargo`, optionally with a `bex.buildRoot` symlink, then execs its arguments.
  - The `unit-tests` recipe builds `bex-provider-supervisor`, runs `cargo nextest run --locked --no-fail-fast --workspace --lib --bins --features agent-core/bindings`, runs the `tools/agent-peer` tests, and on macOS runs `cargo xtask ios-markdown`.
- **Integration tests:**
  - `just iroh-e2e`: `host-fixture --test iroh_host`.
  - `scripts/quality.sh` (`just quality`) runs fmt, clippy `-D warnings`, unit tests, then the integration tests `errors`, `iroh`, `iroh_host`, `browser_bridge`, `management`, `codex_accounts`, `claude`, `adapter_conformance`, `crate_boundaries`, `build_cleanup` and `diagnostics`. It also runs `macos-e2e` and `conversation-ui`.
  - CI is `.github/workflows/native.yml` (cargo build, nextest per package, an Android e2e job, iOS shards).
  - The crate boundary check is `cargo test -p xtask --test crate_boundaries`.
- **Desktop:** `just build-desktop-macos` / `build-host-macos` (`scripts/build-macos.sh`); `just dev` runs an isolated Host with `BEX_STATE_DIR` and `BEX_ISOLATED_HOST=1`.
- **Bindings:** `scripts/build-agent-bindings.sh` builds `agent-ffi --features bindgen` and generates Swift and Kotlin into `target/agent-bindings`.
- **iOS:** `scripts/build-agent-ios.sh simulator|device` runs the bindings script, builds `agent-ffi --release --target aarch64-apple-ios[-sim]`, and compiles `AgentCore.swift` into `libAgentCore.a`.
  - `just ios-e2e` / `conversation-ui` run `scripts/ios-e2e.sh`, which calls `cargo xtask ios-e2e` (`crates/xtask/src/ios_e2e.rs:616` calls the build script, then xcodebuild).
  - `just ios-archive` runs `scripts/archive-ios.sh`.
- **Android:**
  - Gradle task `generateAgentBindings` runs the bindings script.
  - `buildAgentAndroid` runs `cargo ndk --target arm64-v8a --target x86_64 build -p agent-ffi --release` (`apps/mobile/build.gradle.kts:24-63`).
  - Assemble with `./gradlew :apps:mobile:assembleDebug`; run e2e with `just android-e2e` (`nix develop .#android-test`, `scripts/android-e2e.sh`).

## 7. Keep, rewrite or delete

### Keep as-is (transport, pairing, non-conversation features)
- `agent-transport`:
  - `transport.rs`, `framing.rs`, `client/connection.rs`, `transfers.rs`, `diagnostics/*`.
  - `peer.rs`/`jsonl.rs` only if the new provider adapters still speak JSON-RPC over stdio.
  - Bump the ALPN when wire types change.
- `host-daemon`:
  - Keep: `host_identity.rs`, `local_host.rs`, `management.rs`, `main.rs`, `command_line.rs`, `platform/*`, `host_runtime.rs` (generic aside from the five `service.*` hooks).
  - Keep the non-conversation features: `terminals.rs` (after the router split), `workspace_files.rs`, `workspace_review.rs`, `worktrees.rs`, `git.rs`, `visualize.rs`, `browser/*`, `dictation.rs` (it needs `CodexAppServer` for auth, `dictation.rs:69-88`), `codex_accounts.rs`, `account_usage.rs`, `claude/accounts.rs`, `projects.rs`/`projects/state.rs`.
- `bex-process` (supervisor + PTY) and `codex-app-server` (process handle).
- `agent-protocol`: `protocol.rs` codec and `Response`, `error.rs`, `diagnostics.rs`, `browser.rs`, `permissions.rs`, `composer.rs`, and the management, file, terminal, account and worktree params in `operations.rs`/`models.rs`.
- `agent-core`:
  - The store/run-loop skeleton: connect/resume/reconnect, the command queue, `Operation`/`Effect` scheduling, persistence scoping.
  - Operations in `hosts.rs`, `terminal.rs`, `workspace.rs`, `accounts.rs`, `composer.rs`, `permissions.rs`.
  - Presentation: `markdown.rs`, `diff.rs`, `connections.rs`, `model_settings.rs`, `error.rs`.
  - `bindings/mod.rs` connection and invitation functions.
- Clients: pairing, profile, QR and identity code on all three platforms; terminal, files, browser and settings views.

### Rewrite or delete (old conversation model)
- **`host-daemon/src/host_rpc/*` (~5,600 LOC):**
  - `routing.rs` conversation half: `executions`, `SessionActor`, subscriptions, `requests`/`native_requests`, `change_locked`, `finish_session_read`, submission receipts. Keep the `sessions`/`Outbound`/`send`/`broadcast`/`principal` registry as its own type.
  - `session_actor.rs`, `submission.rs`, `agent.rs` (the `Agent` trait and `AgentChange`).
  - `service.rs` conversation dispatch: `session_open`, `submit_input`, `answer_request`, `read_item`, `create_session`, `host_title_list`, `start_event_pumps`, the dispatch prelude :358-455. Keep the routing for files, terminals, worktrees, accounts, browser and dictation.
  - `codex.rs`, `native.rs` and `requests.rs` are provider adapters; port them into T3-style provider adapters that emit events instead of `SessionChange`.
- **Claude adapter:** `claude.rs`, `claude/{process,history,native}.rs` (~3,100 LOC). `process.rs`'s spawn flags and `history.rs`'s JSONL reader may be reusable.
- **`agent-protocol`:**
  - `session.rs` (`SessionChange`, `Timeline`, `OpenedSession`, history paging/merge).
  - `models.rs` `Thread`/`Turn`/`ThreadList`.
  - `items.rs`, `execution.rs`, `requests.rs` (keep the approval shapes if useful).
  - The conversation `Call`s in `protocol/requests.rs`: `OpenSession`, `ReadHistory`, `ReadTurnItems`, `AnswerSession`, `RequestSession`, `ReadItem`, `ListSessions`, `CreateSession`, `ForkSession`, `Submit`, `Interrupt`, `RenameSession`, `SessionScope`.
  - The `Activity` and `SessionRenamed` notifications.
  - The subscription-on-response-stream pattern can carry an orchestration event stream instead.
- **`agent-core`:**
  - `state.rs` conversation fields and reducers (`conversations`, `subscriptions`, `pending_submissions`, `submission()`, `reconcile_pending`, `upsert_item`).
  - `state/notifications.rs` `session_update`.
  - `state/operations/threads.rs` and `submission.rs`.
  - `client.rs` `open_subscription`/`respond`/`session_images`.
  - The `store.rs` run-loop subscription `StreamMap` (:1128-1145), which decodes `SessionChange`.
  - `presentation/{conversation,mod,body,list}.rs`.
  - `bindings/snapshot.rs` conversation objects and the Thread/Turn/Item records in `bindings/protocol.rs`.
- **Clients:**
  - Desktop `app/view/conversation.rs`, the composer, sidebar thread list, `app.rs` row sync and request handling.
  - iOS `Conversation*.swift`, `TaskList.swift`, the `projectConversation` part of `AppViewModel.swift`.
  - Android `AndroidConversation*.kt`, `ConversationProjection.kt`, `AndroidThreadList.kt`.
  - These consume `RenderedConversation`/`Snapshot.conversation`, so they need rewriting against the new read model, or core needs to keep projecting into the same row types.
- **Tests that encode the old model:**
  - `agent-core/tests/{store,state,session}.rs` (~6,600).
  - `host-fixture/tests/{iroh_host,claude,adapter_conformance}.rs` (~6,500) and the fake Codex/Claude fixtures in `host-fixture/src/fixture/*` and `bin/bex-claude-fixture.rs`.
  - Inline tests in `routing.rs`, `service.rs`, `claude.rs`.
  - iOS `BexUITests` and the Android `androidTest` conversation tests.
  - The approved UI interaction contract in `docs/DESKTOP_CONVERSATION_DISPLAY_CONTRACT.md` (see `AGENTS.md`).
- **Docs that describe the old model:** `docs/SESSION_RUNTIME.md`, `docs/BEX_PROTOCOL_DESIGN.md`, `docs/BEX_PROTOCOL_NATIVE_CONTRACTS.md`, `CONTEXT.md` (its glossary still mentions an SSH host key, which is outdated), and `docs/adr/0005-rust-store-and-one-iroh-client-path.md` (records the iroh, stream and pairing decisions you are keeping).