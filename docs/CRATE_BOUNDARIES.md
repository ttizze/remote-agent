# Host / client boundaries

`agent-protocol` owns the shared wire records, request/result contracts,
notifications, serialization, and deterministic validation and model operations.
It has no Store, presentation, UniFFI, async runtime, or network dependency.
The operation table is defined once in `agent-protocol/src/protocol/requests.rs`.
Session-scoped operations expose thread and in-flight input identity on `Call`;
Host dispatch must not re-derive that table. Request records are grouped by
domain under `agent-protocol/src/operations/`.

`agent-transport` owns QUIC framing, connections, transfers, provider JSONL I/O,
and diagnostic collection. Both Host and the client use it. It depends on
`agent-protocol`, never on `agent-core`.

`agent-core` owns the client Store, state transitions, client workflows, and
presentation. Its optional bindings implement UniFFI converters for protocol
records and wrap shared objects only at the native ABI boundary. Wire types do
not depend on these bindings.

`host-daemon` and `codex-app-server` use protocol and transport directly. Host
owns workspace execution and sandboxed visualization-document generation;
clients receive the resulting document. Host's dev-dependency on `agent-core`
is solely for integration tests that exercise real client state and rendering
against Host behavior. Host also owns submission routing and delivery evidence;
clients send `host/session/submit` intents and reconcile subscribed evidence with
local drafts. The deterministic routing decision and delivery records live in
`agent-protocol`; client caches never choose start, steer, queue, or resume.

`python3 -B -m unittest discover -s scripts/tests` checks these production
crate boundaries, including indirect local dependencies. Rust CI and quality
checks run the protocol and transport tests as well as the client tests.
