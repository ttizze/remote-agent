# Remote Agent implementation plan

## Current implementation status

- Milestone 1 is implemented: the Rust workspace, Codex App Server lifecycle, installed-binary Schema preflight, raw JSONL operations, deterministic tests, and a live read-only Codex check.
- Milestone 2 is implemented: version 2 length-prefixed Mobile RPC, arbitrary Codex methods, bidirectional requests, bounded QUIC handling, deadlines, cancellation, and loopback tests.
- Milestone 3 Host work is implemented: expiring one-use pairing tickets, pinned Host proofs, device challenge authentication, atomic settings, and macOS Keychain Host identity storage. QR camera UI remains part of the mobile work.
- Milestone 4 implementation is complete: Android Jetpack Compose and iOS SwiftUI use the same Kotlin application state and raw Codex boundary with local UI projections; both platforms provide QR capture, per-Host secure device identity, Rust/Quinn transport bindings, authenticated mDNS rediscovery, isolated Host Profiles, and the canonical thread/turn flow.
- Milestone 4 verification covers the full Rust workspace, the shared iOS tests, a headless XCUITest that launches the native SwiftUI application in iOS Simulator and verifies that the initial pairing screen remains alive, and signed installation/launch on a physical iPhone. Android emulator and end-to-end Host pairing on physical devices remain to be run where the required SDKs and Host process are available.

## Outcome

Build a native Android/iOS control surface for a Codex installation running on a trusted computer. The Mobile Client connects directly to the PC Host over QUIC. Remote Agent does not provide a web client, hosted backend, relay, or durable authoritative conversation store. A Mobile Client may keep a durable Mobile Cache; Codex remains authoritative.

The first implementation supports Codex only. A common agent adapter interface will not be introduced until a second agent supplies a real variation that needs one.

## Product invariants

- Codex is the source of truth for threads, turns, items, history, and current thread status.
- A Codex Thread is identified only by its Codex `threadId`. Remote Agent does not mint a competing session ID or add a provider discriminator while Codex is the only provider.
- The PC Host persists paired device public keys and PC Host settings.
- The PC Host private key is stored in the operating system secure store.
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
- The long-lived PC Host Identity authenticates replaceable QUIC transport certificates, so routine certificate rotation does not require re-pairing.
- A Mobile Client may maintain multiple PC Host Profiles. Each profile has separate device credentials, addresses, and Mobile Cache keyed by PC Host Identity.
- The Mobile Cache relies on the operating system's device-lock and application-data protection; Remote Agent does not add an application-level biometric gate.
- An active Turn continues when every Mobile Client disconnects. Reconnection obtains and reconciles its current state from Codex.
- The Mobile Client exposes one-time approval and decline decisions only; it does not expose Codex's broader `acceptForSession` decision.

"The PC Host owns no conversation state" means it owns no durable conversation or event history. While running, the Host Daemon necessarily owns ephemeral process handles, pending RPC requests, approval continuations, Blob transfers, and reconnection buffers. A Host Daemon restart can reload a persisted Codex Thread, but does not promise to preserve an in-flight Turn. The Mobile Cache is a non-authoritative display copy.

## Technology and repository shape

- Host Daemon and transport: Rust, Tokio, Quinn, Rustls.
- Codex connection: one `codex app-server` child process using stdio JSONL.
- Mobile application state and RPC DTOs: Kotlin Multiplatform, shared by Android and iOS.
- Mobile UI: Jetpack Compose in `androidMain`; SwiftUI in the iOS application target.
- Mobile platform facilities: `androidMain` and `iosMain` implementations for QR capture, secure storage, file picking, lifecycle, and initially QUIC.
- The minimum iOS version is 15. Platform-specific implementations are permitted where no suitable common API exists.
- Host Manager: Compose Multiplatform Desktop after the daemon interface is stable.
- macOS Host Daemon lifecycle: `launchd`; Windows and Linux service integration follow the same executable later.
- Development environment: Nix shell with Rust, JDK, and Gradle. Xcode and Android SDK remain platform toolchains.

Planned top-level shape:

```text
remote-agent/
├── crates/
│   ├── codex-app-server/   # Codex process and JSONL protocol ownership
│   ├── host-protocol/      # Mobile RPC and Blob framing, added with QUIC
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

The module owns the process, initialization handshake, request correlation, JSONL decoding, timeouts, server notifications, server-initiated requests, and shutdown. It does not impose a fixed per-message limit that is absent from the Codex stdio protocol. Its gateway interface is deliberately raw:

- spawn and initialize one App Server;
- send any Codex request or notification with its native method and JSON params;
- subscribe to notifications and server requests;
- respond to a server request with a native result or error;
- shut down the child process.

The implementation sends `initialize`, waits for the response, and then sends `initialized` before exposing the ready instance. Codex-generated JSON Schema is the compatibility reference. The module must not leak its internal request IDs into the Mobile RPC.

At Host Daemon startup, Remote Agent resolves the Codex executable, preferring the Codex bundled with ChatGPT Desktop on macOS and falling back to `codex` on `PATH`; an explicit `--codex <PATH>` remains authoritative. It asks the exact resolved executable to write its embedded App Server JSON Schema into a private temporary directory, then spawns that same executable. Schema generation is a readiness preflight and capability advertisement, not a Host-side method allow-list. After the Host-owned `initialize`/`initialized` handshake, authenticated Mobile RPC forwards Codex-native methods and JSON without a second Codex object model.

## Mobile RPC

One client-initiated bidirectional QUIC stream carries all structured traffic. Frames are a four-byte big-endian length followed by one UTF-8 JSON object. Every decoder enforces a configured maximum before allocation.

```json
{
  "id": 42,
  "method": "turn/start",
  "params": {
    "threadId": "019...",
    "input": [{ "type": "text", "text": "continue" }]
  }
}
```

Responses contain the request `id` and either `result` or `error`. IDs may be JSON integers or strings. Notifications omit `id`. Codex-originated requests contain `id`, `method`, and `params`; a Mobile Client answers them with a response carrying the same connection-local proxy ID. Method names, params, results, errors, notifications, and extension fields cross the gateway without Host DTO translation.

RPC version negotiation, message size, queue length, request deadline, and the installed Codex method set are part of the connection handshake. The method set is a capability hint only. Unknown methods are forwarded so Codex itself decides whether they are valid.

## Pairing and connection authentication

The QR payload contains a protocol version, PC Host identity/public key, candidate addresses, a random single-use pairing ticket, and an expiry. The Mobile Client first authenticates the PC Host by pinning the scanned key, then submits its own public key and the ticket through the encrypted connection. Ticket consumption is atomic and never persisted after use.

Later connections authenticate the PC Host with the saved pin and authenticate the device by signing a fresh Host Daemon challenge. Replay, expired ticket, removed device, and mismatched PC Host identity are rejected before Codex data is returned.

Only the local Host Manager can create a pairing ticket or remove a Paired Device. Pairing grants control over Codex and its available working directories, so removing a device revokes its future connections.

## Direct reachability

The Host Manager offers a user-initiated `Set up direct external connection` action. It starts or confirms the QUIC UDP listener, attempts a time-bounded router mapping through supported PCP, NAT-PMP, or UPnP mechanisms, and asks the Mobile Client to verify reachability. It does not alter the PC firewall and does not claim success when only a local listener or router mapping exists. While the feature remains enabled, the Host Daemon renews expiring mappings and removes them when disabled or during clean shutdown. Mappings are reported separately from end-to-end verification, and unsupported or unreachable networks fall back to manual configuration, IPv6, or a user-managed virtual network.

On the local network, the Host Daemon advertises a non-secret service identifier through mDNS. A Mobile Client resolves candidates and accepts a rediscovered address only after authenticating the saved PC Host Identity. Saved addresses and manual address entry remain fallback paths; an IP address change does not require re-pairing.

## Blob transfer

Each Blob uses a temporary unidirectional QUIC stream. An RPC grant precedes the stream and binds a transfer ID to direction, purpose, declared byte length, SHA-256 digest, MIME type, and expiry.

The receiver enforces byte and disk quotas while streaming, verifies the digest before use, uses owner-only temporary files, and deletes incomplete, expired, consumed, or disconnected transfers. User-provided filenames are display metadata and never become filesystem paths.

The built-in editor accepts UTF-8 text files up to a configured size limit, initially 2 MiB, while preserving detected BOM and line-ending style. Binary and larger files remain preview/download-only. A Paired Device may open files anywhere the PC Host account can access.

Opening a file returns its content and Revision hash. Direct file writes require that Revision to still match, then use a temporary sibling file followed by atomic replacement. A mismatch returns the latest Revision and content for comparison instead of overwriting it. Delete operations use the operating system trash where available; irreversible deletion requires a separate explicit confirmation.

Filesystem changes update an open editor automatically when it has no unsaved content. When an Editor Draft exists, the Mobile Client preserves it and presents a comparison against the new file Revision. Offline editing stores only an Editor Draft; reconnect never applies it automatically.

The Mobile Client presents Manual Edit and AI Edit as distinct actions. A Manual Edit does not create or steer a Codex Turn and may be saved while one or more Turns are active in the same working directory.

## Reconnection

For a selected thread, the Mobile Client consumes the raw notification stream and uses native `thread/resume` followed by `thread/read` with `includeTurns`. It projects the returned Codex object into its UI cache while retaining unknown notifications and Item kinds as raw JSON. The Host Daemon does not synthesize a Snapshot RPC or translate events.

No event cursor is persisted. A new transport connection always performs synchronization from Codex.

## Milestones and acceptance criteria

### 1. Foundation and Codex connection

- Save this plan, the glossary, and the technology decision.
- Provide a reproducible Rust/JDK/Gradle development shell.
- Spawn and initialize the installed Codex App Server.
- Send arbitrary Codex methods through a raw Remote Agent gateway.
- Correlate concurrent responses and expose server notifications/requests.
- Reject malformed, timed-out, or closed JSONL connections deterministically without inventing a fixed Codex message-size limit.
- Verify deterministic tests with an in-memory App Server peer and a read-only live compatibility check against the installed Codex CLI.

### 2. QUIC control channel

- Add the versioned Mobile RPC types and length-prefixed framing.
- Complete bidirectional request/response and notification exchange over loopback QUIC.
- Bound frame sizes, queues, concurrency, and deadlines.
- Test disconnects, cancellation, malformed frames, slow peers, and graceful shutdown.

### 3. Pairing and device authentication

- Display and scan an expiring QR payload.
- Pin the PC Host Identity and persist a Paired Device after one successful ticket use.
- Store private keys in OS secure storage and non-secret settings with atomic replacement.
- Let an authenticated Paired Device select any working directory available to the PC Host account.
- Test replay, expiry, signature failure, and device removal.

### 4. Android/iOS vertical slice

- Create the Android Compose and iOS SwiftUI applications over common Kotlin application state.
- Implement QR capture and secure device identity on both platforms.
- Support multiple isolated PC Host Profiles and authenticated mDNS rediscovery.
- Connect, choose a working directory, list/read/start a thread, send a text turn, stream output, and interrupt it.
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

Routine tests use deterministic in-process peers and temporary directories. They do not use a developer's saved Codex history or credentials. A live App Server handshake/list operation is a compatibility verification, not a routine test; running a model-backed Turn is a separate explicit smoke check.

Builds establish compilation and packaging only. Simulator tests do not establish physical-device behavior, local-network permissions, camera behavior, background lifecycle, or reachability through Tailscale/IPv6. Each milestone report must state those unverified surfaces explicitly.
