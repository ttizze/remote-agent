# Host and client responsibilities

`agent-domain` owns conversation IDs, commands, facts, the thread state machine
and the fold that Host and clients share. It has no process, filesystem,
database, async runtime, clock or random ID source.

`agent-providers` translates Codex/Claude native traffic into provider events
and commands. `agent-runtime` owns the Host conversation runtime: per-thread
actors, the SQLite fact log, the effect outbox, provider sessions, sync and
import. Neither depends on clients.

`agent-protocol` owns the native RPC table, peripheral records and encoding.
`agent-transport` owns iroh, framing, streams, blob transfers, provider JSONL I/O
and diagnostics. They have no client or UniFFI dependency. `bex-process` and
`codex-app-server` retain provider process management.

`host-daemon` wires the runtime to RPC and owns the independent terminal, files,
browser, worktree, accounts and dictation resources. Connection/identity
ownership is independent of conversations.

`agent-core` owns synced state, cursors, ordered mutations, device persistence
and common presentation; it never links the Host runtime or SQLite. Optional
UniFFI converters expose native records. GPUI, SwiftUI and Compose own
rendering, widgets and platform services. Boundary assertions remain in
`xtask/tests/crate_boundaries.rs`.
