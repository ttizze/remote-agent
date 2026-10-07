# Rust conversation runtime and one iroh client path

The T3 port replaces the previous session model. `agent-domain` owns contracts,
the pure thread state machine and the fold shared by Host and clients.
`agent-runtime` runs one writer actor per thread and atomically commits facts,
receipts and outbox effects in SQLite before publishing. Provider translation in
`agent-providers` returns normalized events; only the state machine creates app
IDs and entities.

`agent-core` owns client state, cursors, pending commands and presentation.
Native clients own rendering/private storage. All use iroh streams/12 with one
stream per RPC/subscription. Pairing/revocation are independent responsibilities.
Only current formats remain; the retired orchestration crates, old session
fixtures and CLI are removed. See [the runtime architecture](../t3-port/ARCHITECTURE.md).
