# Rust Host, GPUI Mac UI, shared Kotlin and native iPhone UI

Bex uses a Rust Host daemon with one long-lived Codex App Server, a Rust/GPUI Mac UI, and SwiftUI on iPhone over shared Kotlin state. Closing a UI does not terminate Codex work.

Mobile supports only the latest stable iOS and Android major versions: currently iOS 26 and Android 17 (API 37). Advancing the minimum removes obsolete platform compatibility paths.

Host and remote clients connect outbound to Phoenix. Each client receives its own bounded byte route; embedded SSH runs inside that route using the `remote-agent-v4` subsystem. Host keys are pinned from single-use invitations and devices authenticate with individual public keys. Shell, PTY, exec and forwarding channels are rejected. Phoenix sees routing metadata and ciphertext, never application JSONL.

Each authenticated connection owns an independent Codex RPC session. The Host preserves Codex payloads and unknown fields, correlates request IDs and routes notifications and first-response approval resolution. An owner-only Unix socket exposes local Codex access and Host management to the Mac UI. Remote sessions cannot manage invitations or local configuration.

Rust owns all agent behavior shared by Mac and mobile: operations, request and response semantics, event reduction, history reconciliation, submission, approval forms, errors, and model-selection policy. Kotlin owns mobile lifecycle and the storage/projection of native bodies. Its state is represented by immutable values; transition functions are separate from I/O and publication. C/JNI carry metadata and source indices for shared decisions without copying entire conversations.

SwiftUI and Android render state, collect intents, and access platform APIs such as secure storage, file picking, recording, QR capture, and application lifecycle. UI framework objects remain thin adapters. Rust owns native connection IDs and retains a connection for every in-flight C/JNI call; closing an ID prevents new calls without invalidating existing ones. Common Kotlin handles mobile subscriptions and polling. The Host keeps pure request-alias state separate from its bounded outbound queues. Mac uses the same Host services through local IPC, including persisted remote profiles and file transfer.

The Mac UI uses GPUI Kit for native input, Markdown, menus and code editing, with GPUI lists for conversation and diff virtualization. UI replies and notifications enter one queue in wire order. React, its reconciler and the bundled JavaScript runtime are removed; the daemon remains a separate process. The right workbench uses the macOS WebView for an isolated xterm.js terminal renderer and a separate browser without privileged IPC. The terminal uses Codex process RPC rather than enabling SSH shell/PTY channels. Node supplies locked renderer assets at build time only.
