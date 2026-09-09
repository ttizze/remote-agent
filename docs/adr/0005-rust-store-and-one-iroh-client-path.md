# Rust Store and one iroh client path

This decision supersedes ADR 0001's Kotlin state owner, SSH/Phoenix transport and owner-only Unix socket. It preserves the separate Host daemon and long-lived Codex process, ADR 0002's concurrent control, ADR 0003's pairing authorization boundary and ADR 0004's installed-Codex schema adaptation.

`agent-core` owns typed operations, immutable snapshots, a pure reducer and one Store per client process. Data contains no sockets, locks or callbacks. Only Store performs state transitions and runs effects. Desktop, SwiftUI, Compose and the headless `agent-cli` consume the same snapshots and dispatch typed intents. Core is designed from the behavior corpus; existing UI controllers are not moved into it.

Every client uses iroh, including desktop on the daemon's machine. A session is one bidirectional stream carrying JSONL JSON-RPC. The same transport-independent peer serves Codex stdio, daemon sessions and clients. It exposes one typed request API, one raw request API and one ordered incoming event stream. The transport module alone owns iroh endpoints, pairing and stream lifecycle.

A local client is paired during daemon startup using protected identity material. Management operations require the local client's endpoint identity; a remotely paired identity does not gain management access. Invites remain expiring and single-use. Revocation closes live sessions. Secrets use platform secure storage through existing libraries.

The product is unreleased. No protocol-version wrappers, cache migrations or compatibility paths are introduced. Core tests and agent-cli are the migration gates; legacy UI may be temporarily broken. Core and CLI precede the daemon, desktop and mobile cutovers. Once generated UniFFI bindings are available, Kotlin common and hand-written FFI are removed together.

The 87-case behavior corpus originates at `ea8aefc:crates/agent-client/tests/fixtures/`. Expanded copies retain RPC exchanges, expected outcomes and unknown fields. Its old JSON command tags label scenarios only; they do not define the new public API. Physical iroh adoption was approved following the isolated phone experiment. Cross-platform daemon/UI execution and production mobile behavior require their own verification.

Daemon authorization and both private identities share one keyring record. A mutex orders persistent updates; a failed write leaves the invitation usable and authorization unchanged. A paired remote identity cannot mint invitations, revoke peers or register another Host. Revocation and active-session registration use the same lock order. Registration of a remote Host pairs the local client identity, so no daemon transfer proxy remains.

Daemon project and history transforms use core models. Codex's paginated turn/item envelopes remain boundary types; opaque cursors, opening questions, full image output and on-demand activity details retain their existing behavior. File and settings replacement uses atomicwrites for platform-specific durability, while upload staging uses tempfile. macOS headless checks do not establish Linux, Windows, Keychain or physical-device behavior.
