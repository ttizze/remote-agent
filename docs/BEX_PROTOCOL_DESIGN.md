# Native protocol

`remote-agent/streams/13` uses Postcard over iroh QUIC, with one request per
bidirectional stream. The unreleased product supports only the current format.
The RPC table is `agent-protocol/src/protocol/requests.rs`; conversation records
come from `agent-domain` and `agent-protocol/src/conversation.rs`.

Conversation operations are `conversation/dispatch`, `launch`, `subscribeThread`,
`subscribeShell`, `getThread`, `getTurnItem`, `readHistory`, `search`,
`turnDiff`, `agentSessions/scan`, `agentSessions/import`,
`subscribeWorktreeSetup` and `cancelWorktreeSetup`. Turn differences select
persisted checkpoint ordinals and optionally ignore whitespace. App-thread IDs
are independent of native session and turn IDs.

Subscriptions return a typed initial response followed by typed stream items.
Cursors select bounded replay or snapshot fallback; the synchronized marker
separates replay from live delivery. Dropping a subscription drops its stream.
Terminals use their own output notifications and metadata stream. Blob
transfers verify SHA-256 and byte length.

Enums use external tags for Postcard. Open provider/tool JSON values are wrapped
as JSON strings in binary encoding. Native contracts remain typed at Rust,
UniFFI, Swift and Kotlin boundaries. Provider stdio JSON-RPC is independent.
See [the conversation runtime](SESSION_RUNTIME.md) and
[crate boundaries](CRATE_BOUNDARIES.md).
