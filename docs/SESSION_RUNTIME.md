# Orchestration runtime

The frozen T3 specification is recorded in [the port plan](t3-port/PLAN.md).
`orchestration` owns app threads, runs, attempts, execution nodes, provider
sessions/threads/turns, messages, items, runtime requests and plans. Native
provider IDs belong to provider-thread records; clients use app-thread IDs.

A pure decider returns events and effects. The SQLite owner atomically commits
events, command receipts, projections and an effect outbox, then publishes.
Command IDs deduplicate retries. Host and clients share one projector. Provider
adapters return complete entities; Host rejects superseded run attempts.

Effects execute serially per thread, with four workers across threads. Leases
and bounded retry backoff recover delivery. Process loss ends active entities
and holds queued runs. A held queue requires explicit resume. Normal send queues
behind an active run; an idle thread starts immediately. Steer, restart, stop
and queue edit/reorder/cancel are distinct commands.

Subscriptions register under the commit lock and deliver snapshot or bounded
replay, synchronized, then live events. Replay retains 128 events and 1 MiB;
projections retain 200 messages; shells retain 1,000 threads and 8 MiB. Missing
replay falls back to snapshot. Clients track sequence cursors, retain 16 idle
thread caches and bound owner ingress to 64 events.

Native transcript import is asynchronous and read-only: latest 100 files per
provider within 30 days, 200 messages per thread including the first user
message. Native session ID and cwd support resumption. Imported threads are
never overwritten; tool/approval details are not imported.

Terminal, files, browser, worktrees, accounts, dictation, pairing and revocation
have independent Host owners. A build does not replace a running Host.
