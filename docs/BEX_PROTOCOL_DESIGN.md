# Native orchestration protocol

`remote-agent/streams/8` uses Postcard over iroh QUIC, with one request per
bidirectional stream. The unreleased product supports only the current format.
`orchestration` owns conversation contracts, decisions and projection; the RPC
table is `agent-protocol/src/protocol/requests.rs`.

Conversation operations are dispatchCommand, launchThread, subscribeShell,
subscribeThread, getThreadProjection, getTurnItem, readThreadHistory and
searchThreads and getTurnDiff. Turn differences select persisted root checkpoint
ordinals and optionally ignore whitespace. App-thread IDs are independent of native session and turn IDs.
Shell/thread streams return a typed initial Response followed by typed stream
items. Cursors select bounded replay or snapshot fallback; synchronized marks
the boundary before live delivery. Dropping a subscription drops its stream.
Terminal uses an independent events stream. Blob transfers verify SHA-256 and
byte length.

Enums use external tags for Postcard. Open provider/tool JSON values are wrapped
as JSON strings in binary encoding. Native contracts remain typed at Rust,
UniFFI, Swift and Kotlin boundaries. Provider stdio JSON-RPC is independent.
See [runtime ownership](SESSION_RUNTIME.md) and [crate boundaries](CRATE_BOUNDARIES.md).
