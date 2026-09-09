# Rust Host, GPUI Mac UI, shared Kotlin and native iPhone UI

Historical design and evidence. The Rust Store and iroh architecture in [ADR 0005](0005-rust-store-and-one-iroh-client-path.md) supersedes the Kotlin, Phoenix, SSH and versioned-cache design below.

Bex uses a Rust Host daemon with one long-lived Codex App Server, a Rust/GPUI Mac UI, and SwiftUI on iPhone over shared Kotlin state. Closing a UI does not terminate Codex work.

Host and remote clients connect outbound to Phoenix. Each client receives its own bounded byte route; embedded SSH runs inside that route using the `remote-agent-v4` subsystem. Host keys are pinned from single-use invitations and devices authenticate with individual public keys. Shell, PTY, exec and forwarding channels are rejected. Phoenix sees routing metadata and ciphertext, never application JSONL.

Each authenticated connection owns an independent Codex RPC session. The Host preserves Codex payloads and unknown fields, correlates request IDs and routes notifications and first-response approval resolution. An owner-only Unix socket exposes local Codex access and Host management to the Mac UI. Remote sessions cannot manage invitations or local configuration.

Kotlin owns mobile reconciliation, cache and presentation state. SwiftUI accesses one Snapshot/intent interface. Native adapters implement Keychain, file picking, QR capture and lifecycle handling; Rust owns the encrypted transport. Mac uses the same Host services through its local IPC, including persisted remote profiles and file transfer.

The Mac UI uses GPUI Kit for native input, Markdown, menus and code editing, with GPUI lists for conversation and diff virtualization. UI replies and notifications enter one queue in wire order. React, its reconciler and the bundled JavaScript runtime are removed; the daemon remains a separate process. The right workbench uses the macOS WebView for an isolated xterm.js terminal renderer and a separate browser without privileged IPC. The terminal uses Codex process RPC rather than enabling SSH shell/PTY channels. Node supplies locked renderer assets at build time only.
