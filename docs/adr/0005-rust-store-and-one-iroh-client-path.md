# Rust orchestration and one iroh client path

The T3 port replaces the previous session model. `orchestration` owns contracts,
pure decisions and the shared projector. SQLite atomically commits receipts,
events, projections and effects before publish. Adapters return complete
entities; only Host applies durable state.

`agent-core` owns client state, cursors, pending commands and presentation.
Native clients own rendering/private storage. All use iroh streams/6 with one
stream per RPC/subscription. Pairing/revocation are independent responsibilities.
Only current formats remain; old session fixtures and CLI are removed.
See [the port plan](../t3-port/PLAN.md).
