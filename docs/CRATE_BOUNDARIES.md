# Host and client responsibilities

`orchestration` owns T3 contracts, pure decisions/projector, transactional SQLite
and outbox scheduling. `provider-adapters` owns Codex/Claude process interaction
and returns events for Host to commit. Neither depends on clients.

`agent-protocol` owns the native RPC table, peripheral records and encoding.
`agent-transport` owns iroh, framing, streams, blob transfers, provider JSONL I/O
and diagnostics. They have no client or UniFFI dependency. `bex-process` and
`codex-app-server` retain provider process management.

`host-daemon` owns persistence, effects, native import and independent terminal,
files, browser, worktree, accounts and dictation resources. Connection/identity
ownership is independent of conversations.

`agent-core` owns immutable state, cursors, ordered mutations, device persistence
and common presentation. Optional UniFFI converters expose native records.
GPUI, SwiftUI and Compose own rendering, widgets and platform services.
Boundary assertions remain in `xtask/tests/crate_boundaries.rs`.
