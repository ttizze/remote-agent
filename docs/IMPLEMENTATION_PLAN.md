# Remote Agent implementation plan

## Current implementation status

- The Codex App Server lifecycle, installed-binary Schema preflight, raw JSONL peer, Project read projection, Mobile Cache rules, and deterministic foundation tests exist in the tree.
- The transport uses embedded `russh` SSH/TCP on both Host and Mobile. Both sides use the `remote-agent-v3` subsystem, and the old QUIC, custom handshake, and length-prefixed frame layers have been removed.
- The raw JSONL boundary and classifier inspect only top-level `method` and `id` for routing. The 256 MiB line ceiling remains a last-resort safety bound, not a Codex payload policy.
- SSH Host-key pinning, device public-key authentication, initial ticket mapping, reconnect authentication, raw routing, and first-response-wins behavior have deterministic tests. A live Host-to-phone pairing run remains unverified.
- The Rust workspace tests and strict Clippy checks pass. Kotlin tests, the iOS Simulator native build, and the Android debug build pass; physical-device behavior and a live Codex/SSH end-to-end run remain unverified.

## Outcome

Build a native Android/iOS control surface for a Codex installation running on a trusted computer. The Mobile Client connects directly to the PC Host over an embedded SSH server and its `remote-agent-v3` subsystem. The Host starts one Codex App Server and keeps it alive independently of individual phone connections. Remote Agent does not provide a web client, hosted backend, relay, or durable authoritative conversation store. A Mobile Client may keep a durable Mobile Cache; Codex remains authoritative.

The first implementation supports Codex only. A common agent adapter interface will not be introduced until a second agent supplies a real variation that needs one.

## Product invariants

- Codex is the source of truth for threads, turns, items, history, and current thread status.
- Codex Desktop's `.codex-global-state.json` is the read-only source for local Projects, Project order, explicit Thread assignment, and explicit projectless status. The Host reloads it for each Project-aware request and does not persist a second Project index.
- Codex App Server remains the source for Threads. The Host applies Desktop's explicit assignment first, explicit projectless status second, and otherwise retains App Server's `Thread.projectId`; it never infers Project membership from `cwd`.
- A Project task supplies the Desktop Project ID as `thread/start.projectId` and one of its roots as `cwd`. Remote Agent does not write Desktop global-state while this integration remains read-only.
- A Codex Thread is identified only by its Codex `threadId`. Remote Agent does not mint a competing session ID or add a provider discriminator while Codex is the only provider.
- The PC Host persists paired device public keys and PC Host settings.
- The PC Host SSH host private key is stored in the operating system secure store; its public key is the Host identity shown in the pairing QR.
- macOS development and installed Host binaries use the stable `dev.remoteagent.host-daemon` code-signing identifier so rebuilding the Host does not create a new Keychain client identity.
- Images and files use temporary Blob storage and are never stored in a Remote Agent database.
- A Paired Device may select any working directory available to the PC Host account.
- The built-in editor sends Manual Edits directly to the Host Daemon; an AI Edit is a distinct action that starts or steers a Codex Turn. Manual Edits remain available while Turns are active.
- Manual Edits do not automatically produce Steering Input. Active Codex Turns observe filesystem changes through their normal Codex behavior.
- Multiple Paired Devices may connect and control the PC Host concurrently. Distinct Codex Threads may run Turns concurrently; input for a Thread with an active Turn is Steering Input for that Turn rather than an independent Turn.
- Cross-device operations are ordered by Host Daemon receipt order. Each live operation is attributed to its Paired Device without creating a durable Remote Agent event or audit history.
- The first valid response to an Approval Request wins; later responses receive an already-resolved result. Disconnect and Host Daemon restart never grant an approval. Ambiguous authorization fails closed.
- Closing the Host Manager does not stop the Host Daemon. Screen lock also leaves it running; logout ends it until the user logs in again.
- On iOS, backgrounding may disconnect the Mobile Client and foregrounding reconnects and synchronizes it. Android may use an explicit, notification-backed Foreground Service for best-effort connection maintenance. Location permission is never used merely to keep a connection alive.
- The local network, unpaired peers, incoming RPC, file names, file content, and Codex output are untrusted. Pairing authorizes a device to control Codex with the PC Host account's filesystem reach.
- Mobile Clients persist Thread lists, messages, tool output, and diffs in a Mobile Cache and reconcile it with Codex through a fresh Snapshot after reconnection. Blobs and Workspace files are not cached durably.
- Pairing tickets and Paired Device removal are available only through the local Host Manager.
- The long-lived PC Host SSH host key is pinned by each Mobile Host Profile, so reconnects do not require a new pairing ticket while that key is unchanged.
- A Mobile Client may maintain multiple PC Host Profiles. Each profile has separate device credentials, addresses, and Mobile Cache keyed by PC Host Identity.
- The Mobile Cache relies on the operating system's device-lock and application-data protection; Remote Agent does not add an application-level biometric gate.
- One Host-owned Codex App Server remains alive after every Mobile Client disconnects. An active Turn therefore continues when phones disappear; reconnection obtains and reconciles its current state from Codex.
- The Mobile Client exposes one-time approval and decline decisions only; it does not expose Codex's broader `acceptForSession` decision.

"The PC Host owns no conversation state" means it owns no durable conversation or event history. While running, the Host Daemon necessarily owns ephemeral process handles, pending RPC requests, approval continuations, Blob transfers, and reconnection buffers. A Host Daemon restart can reload a persisted Codex Thread, but does not promise to preserve an in-flight Turn. The Mobile Cache is a non-authoritative display copy.

## Technology and repository shape

- Host Daemon and transport: Rust, Tokio, and embedded `russh` over TCP.
- Codex connection: one Host-owned `codex app-server` child process using stdio JSONL. Its lifetime is independent of SSH sessions.
- Mobile application state and UI projections: Kotlin Multiplatform, shared by Android and iOS. The transport does not introduce typed `params`, `result`, or `error` DTOs.
- Mobile UI: Jetpack Compose in `androidMain`; SwiftUI in the iOS application target.
- Mobile platform facilities: `androidMain` and `iosMain` implementations for QR capture, secure storage, file picking, lifecycle, and SSH connection setup.
- The minimum iOS version is 15. Platform-specific implementations are permitted where no suitable common API exists.
- Host Manager: Compose Multiplatform Desktop after the daemon interface is stable.
- macOS Host Daemon lifecycle: `launchd`; Windows and Linux service integration follow the same executable later.
- Development environment: Nix shell with Rust, JDK, and Gradle. Xcode and Android SDK remain platform toolchains.

Planned top-level shape:

```text
remote-agent/
├── crates/
│   ├── codex-app-server/   # Codex process and JSONL protocol ownership
│   ├── host-protocol/      # SSH pairing data and raw Codex JSONL classification
│   └── host-daemon/        # Pairing, connections, composition root
├── apps/
│   ├── mobile/             # Shared Kotlin state, Android Compose, and iOS SwiftUI application
│   └── host-manager/       # Compose Desktop management application
├── schemas/
│   └── codex/              # Versioned schemas generated by the installed Codex CLI
└── docs/
```

Directories are added only when their capability begins; this document does not require empty placeholder modules.

## Codex App Server module

The module owns the one Host-wide process, initialization handshake, request correlation, JSONL decoding, timeouts, server notifications, server-initiated requests, and shutdown. A phone disconnect closes only its SSH session; it does not stop this process. The raw gateway interface is deliberately small:

- spawn and initialize one App Server;
- send any Codex request or notification as a native JSONL line;
- subscribe to raw notifications and server requests;
- respond to a server request with its native raw JSONL response;
- shut down the child process.

The implementation sends `initialize`, waits for the response, and then sends `initialized` before exposing the ready instance. Codex-generated JSON Schema is the compatibility reference. The module must not leak its internal request IDs into the Mobile RPC; the Host restores each phone's original request ID on the way back.

At Host Daemon startup, Remote Agent resolves the Codex executable, preferring the Codex bundled with ChatGPT Desktop on macOS and falling back to `codex` on `PATH`; an explicit `--codex <PATH>` remains authoritative. It asks the exact resolved executable to write its embedded App Server JSON Schema into a private temporary directory, then spawns that same executable. Schema generation is a readiness preflight and capability advertisement, not a Host-side method allow-list. After the Host-owned `initialize`/`initialized` handshake, authenticated SSH sessions forward Codex-native JSONL without a second Codex object model. The Project-aware `host/project/list` and `host/thread/{list,read,start}` methods form a separate Host-owned read projection over Desktop Project state and App Server Thread results; only those Host-owned methods need application-level interpretation.

## SSH subsystem and Codex JSONL

The Host Daemon embeds a `russh` SSH server and listens on TCP. Each authenticated session may open only the `remote-agent-v3` subsystem. Shell, PTY, exec, and port-forwarding channels are rejected. The subsystem is a newline-delimited stream of Codex JSON objects with no additional application framing, handshake, or RPC framework.

The wire messages are the Codex messages themselves:

```json
{"id":1,"method":"thread/list","params":{}}
{"id":1,"result":{}}
{"id":1,"error":{}}
{"method":"thread/started","params":{}}
```

The JSONL reader preserves each source line and applies the 256 MiB last-resort safety ceiling before allocation. This is a framing guard, not a product-level limit on Codex payloads. The Host does not define `RpcRequest`, `RpcResponse`, or typed `params`/`result`/`error` DTOs. It validates only the top-level envelope needed for routing:

- `method` and `id` means request;
- `method` without `id` means notification;
- `id` with `result` or `error` means response;
- anything else is invalid.

For one phone, the Host can behave like a direct JSONL pipe. With multiple phones, the Host is a thin multiplexer around the one Host-owned Codex process:

- A phone request is assigned a Host-local upstream ID. The Host rewrites only the top-level `id`, remembers the originating SSH session and original ID, and restores that ID on the response. The nested JSON remains raw.
- A Codex response is routed by that upstream ID to the originating phone. Unknown or late responses are ignored or reported as an already-completed request without being delivered to another phone.
- Codex notifications are forwarded as raw lines to every live subscribed phone.
- A Codex server request is forwarded as a raw line to the live phones that can answer it. The first valid response wins; later responses are rejected as already resolved. The selected response is forwarded to Codex with its original request ID.
- Host-owned `host/project/list` and `host/thread/{list,read,start}` operations may inspect their own application fields, but they do not require a second Codex object model. All other Codex methods and extension fields pass through unchanged.

The Codex method set remains a capability hint, not a Host allow-list. Unknown methods are forwarded so the installed Codex App Server decides whether they are valid. Queue bounds, request deadlines, cancellation, disconnect cleanup, and the JSONL safety ceiling are resource and lifecycle controls at the Host seam, not new wire message types.

## Pairing and connection authentication

The QR payload contains a protocol version, SSH Host public key, candidate TCP addresses, a random single-use pairing ticket, and an expiry. The Mobile Client pins the scanned Host key before opening SSH. During the initial public-key authentication, the device proves possession of its own Ed25519 private key and presents the ticket as pairing metadata. Ticket consumption is atomic and the ticket is not persisted as Paired Device state.

The Host stores the device public key as a Paired Device. Later connections authenticate with the same SSH public key and do not carry a pairing ticket. Replay, expired ticket, removed device, unsupported key, and mismatched SSH Host key are rejected before the subsystem opens. A ticket is never part of the Codex JSONL stream and is never logged.

Only the local Host Manager can create a pairing ticket or remove a Paired Device. Pairing grants control over Codex and its available working directories, so removing a device revokes its future connections.

## Direct reachability

The Host Manager offers a user-initiated `Set up direct external connection` action. It starts or confirms the SSH/TCP listener, attempts a time-bounded router mapping through supported PCP, NAT-PMP, or UPnP mechanisms, and asks the Mobile Client to verify reachability. It does not alter the PC firewall and does not claim success when only a local listener or router mapping exists. While the feature remains enabled, the Host Daemon renews expiring mappings and removes them when disabled or during clean shutdown. Mappings are reported separately from end-to-end verification, and unsupported or unreachable networks fall back to manual configuration, IPv6, or a user-managed virtual network.

On the local network, the Host Daemon advertises a non-secret `_bex._tcp` service identifier through mDNS. A Mobile Client resolves candidates and accepts a rediscovered address only after verifying the saved SSH Host key. Saved addresses and manual address entry remain fallback paths; an IP address change does not require re-pairing.

## Blob transfer

Blob transfer remains a later capability. Its control grant must travel as an authenticated JSONL operation and bind a transfer ID to direction, purpose, declared byte length, SHA-256 digest, MIME type, and expiry. The data path must be bounded and owned by the Host subsystem; this transport migration does not claim a binary-transfer implementation and must not add shell, exec, or forwarding access to SSH.

The receiver enforces byte and disk quotas while streaming, verifies the digest before use, uses owner-only temporary files, and deletes incomplete, expired, consumed, or disconnected transfers. User-provided filenames are display metadata and never become filesystem paths.

The built-in editor accepts UTF-8 text files up to a configured size limit, initially 2 MiB, while preserving detected BOM and line-ending style. Binary and larger files remain preview/download-only. A Paired Device may open files anywhere the PC Host account can access.

Opening a file returns its content and Revision hash. Direct file writes require that Revision to still match, then use a temporary sibling file followed by atomic replacement. A mismatch returns the latest Revision and content for comparison instead of overwriting it. Delete operations use the operating system trash where available; irreversible deletion requires a separate explicit confirmation.

Filesystem changes update an open editor automatically when it has no unsaved content. When an Editor Draft exists, the Mobile Client preserves it and presents a comparison against the new file Revision. Offline editing stores only an Editor Draft; reconnect never applies it automatically.

The Mobile Client presents Manual Edit and AI Edit as distinct actions. A Manual Edit does not create or steer a Codex Turn and may be saved while one or more Turns are active in the same working directory.

## Reconnection

On every connection, the Mobile Client loads the Project and Thread lists and opens the task list; it does not restore a durable detail selection automatically. Opening a thread uses `host/thread/read` with `includeTurns` to obtain the latest 10 App Server Turns plus current Desktop Project membership without loading a writer. The Host trims only this Mobile read projection; live `turn/*` and `item/*` events continue to stream without translation. Immediately before starting a later Turn, it calls native `thread/resume` with that thread's working directory, followed by `turn/start`. It projects returned Codex objects into its UI cache while retaining unknown notifications and Item kinds as raw JSON. The Host Daemon does not synthesize a durable Snapshot.

No event cursor is persisted. A new transport connection always performs Project and Thread list synchronization from Codex; a detail Snapshot is fetched only after the user opens a Thread.

## Milestones and acceptance criteria

### 1. Foundation and Codex connection

- Save this plan, the glossary, and the technology decision.
- Provide a reproducible Rust/JDK/Gradle development shell.
- Spawn and initialize the installed Codex App Server.
- Send arbitrary Codex methods through a raw Remote Agent gateway.
- Correlate concurrent responses and expose server notifications/requests.
- Reject malformed, timed-out, or closed JSONL connections deterministically without inventing a fixed Codex message-size limit.
- Verify deterministic tests with an in-memory App Server peer and a read-only live compatibility check against the installed Codex CLI.

### 2. SSH/TCP control channel

- Embed a `russh` SSH server in the Host Daemon and a `russh` client in the Rust Mobile transport.
- Pin the SSH Host key from the QR payload, authenticate each device with its SSH public key, and expose only the `remote-agent-v3` subsystem.
- Carry Codex-native JSONL unchanged wherever no ID rewrite is needed; classify only top-level `method` and `id`.
- Run one Host-owned Codex App Server and multiplex multiple authenticated phones by rewriting and restoring request IDs.
- Fan out raw Codex notifications and server requests, with first-response-wins handling for server requests.
- Bound JSONL lines, queues, concurrency, and deadlines; test disconnects, cancellation, malformed JSONL, slow peers, and graceful shutdown.

### 3. Pairing and device authentication

- Display and scan an expiring QR payload.
- Pin the SSH Host key and persist a Paired Device after one successful ticket-bearing SSH public-key authentication.
- Carry the pairing ticket only during initial authentication; reconnect with the stored device key without a ticket.
- Store private keys in OS secure storage and non-secret settings with atomic replacement.
- Let an authenticated Paired Device select any working directory available to the PC Host account.
- Reject shell, PTY, exec, and forwarding requests; expose only `remote-agent-v3`.
- Test Host-key mismatch, replay, expiry, signature failure, unsupported keys, and device removal.

### 4. Android/iOS vertical slice

- Create the Android Compose and iOS SwiftUI applications over common Kotlin application state.
- Implement QR capture and secure device identity on both platforms.
- Support multiple isolated PC Host Profiles and authenticated `_bex._tcp` mDNS rediscovery.
- Connect and load the paginated Codex Desktop Project index and App Server Thread list through the Host-owned read projection.
- Group Threads by Desktop's explicit assignment in Desktop Project order and show explicitly projectless, unassigned, and unknown-Project Threads in a final Chat section.
- Start a task from a Project using `thread/start.projectId` and one of that Project's roots as `cwd`, then submit its first prompt in the same creation flow.
- Start an unassigned task only after the user supplies its working directory and first prompt.
- Read a thread, send later text turns, stream output, and interrupt an active turn.
- Persist and render the bounded Mobile Cache, retaining unknown raw events/items, then reconcile it with a fresh native Codex read.
- Verify the canonical flow on Android emulator, iOS simulator, and then physical devices.

### 5. Approvals, Blobs, files, and diffs

- Surface command and file-change Approval Requests and return one explicit decision.
- Upload images/files without embedding binary content in RPC.
- Download generated files through the authenticated PC Host connection.
- Provide a built-in text editor with direct Manual Edit saves and a separate AI Edit action.
- Show the current Git working-tree diff and untracked files without claiming turn-specific attribution.
- Keep pending approvals visible across a transport reconnect while the Host Daemon remains alive.

### 6. Reconnection and failure recovery

- Reconcile mobile projections with native Codex reads and the raw Live Event stream.
- Restart a failed Codex App Server without inventing conversation state.
- Reconcile each Mobile Cache against a fresh Snapshot after reconnection.
- Define visible outcomes for interrupted turns, lost approvals, incompatible generated Codex schemas, full temporary storage, and unreachable PC Hosts.
- Verify event ordering, duplicate response handling, retry bounds, and cleanup.

### 7. PC Host management and packaging

- Add the Compose Desktop management application.
- Manage pairing tickets, Paired Devices, direct reachability, and connection status.
- Install and update the daemon without tying its lifetime to the window.
- Install the macOS Host Daemon as a per-user `launchd` service that starts at login and can be disabled from the Host Manager.
- Sign and package the supported host and mobile targets.

### 8. Security and beta acceptance

- Threat-model the transport, pairing, local IPC, unrestricted filesystem access, temporary files, logs, and update path.
- Fuzz protocol decoders and file-operation inputs.
- Exercise packet loss, sleep/wake, network changes, daemon restart, Codex crash, and storage exhaustion.
- Perform explicit live Codex and physical-device checks without production fixtures or destructive commands.

## Verification evidence

Routine tests use deterministic in-process peers and temporary directories. They do not use a developer's saved Codex history or credentials. A live App Server handshake/list operation and `cargo run -p host-daemon --example check_desktop_projects` are read-only compatibility verification, not routine tests; running a model-backed Turn is a separate explicit smoke check.

Builds establish compilation and packaging only. Simulator tests do not establish physical-device behavior, local-network permissions, camera behavior, background lifecycle, or reachability through Tailscale/IPv6. Each milestone report must state those unverified surfaces explicitly.
