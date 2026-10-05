# Native contracts and verification

The conversation contract comes from T3 Code commit
`4ee6bfd50ef4a089440d5c3662db2298da9cc50e`, documented in
[t3-port/T3_ORCHESTRATION_SPEC.md](t3-port/T3_ORCHESTRATION_SPEC.md).
`orchestration` owns command/event/entity records. Host owns persistence and
effects. `provider-adapters` maps Codex app-server and Claude stream-json into
complete entities. `agent-core` owns subscriptions, drafts, ordered unconfirmed
commands and presentation. GPUI, SwiftUI and Compose render common records.

Unit/property tests cover duplicate delivery, queues, steer, interruption,
requests, recovery, transaction/publish order, replay, import and persistence.
Native builds verify UniFFI and both mobile compilers. This port does not run CI,
E2E or Simulator UI tests. Build success does not claim interactive acceptance.
Only current contracts remain; retired session types, tests, fixtures and the
old headless client have been removed.
