# Conversation runtime

Behaviour follows T3 Code commit `4ee6bfd50ef4a089440d5c3662db2298da9cc50e`.
The design and its dated decisions are in
[the runtime architecture](t3-port/ARCHITECTURE.md); test mappings are in
[the port map](t3-port/PORT_MAP.md).

`agent-domain` owns IDs, entities, commands, facts and the thread state machine.
`ThreadMachine::step` is pure: it takes the current state and one input (client
command, provider event, effect result, timer or recovery) and returns facts,
effects and a reply. Time and ID seeds come from the input, so replays are
deterministic. Host and clients fold facts with the same function.

`agent-runtime` runs one writer actor per thread over a SQLite (WAL) fact log.
Each step commits its facts, command receipts, outbox effects and the shell row
in one transaction, then publishes. Thread snapshots are a rebuildable cache,
written every 256 facts. A repeated command ID returns the stored result; a
different command under the same ID is rejected. Effects run at least once from
the outbox and return as effect results; the state machine discards results and
provider events from superseded attempts. On start every actor receives
`Recover`, which applies T3's recovery and queue-hold rules.

`agent-providers` translates Codex app-server and Claude CLI traffic into
normalized provider commands and events. It keeps only native correlation
state; app IDs and entities are created by the state machine.

Checkpoints are stored under `refs/orchestration/checkpoints/…` with a private
index; HEAD and the user's index are unchanged. Rollback restores the absolute
head recorded in a checkpoint.

`subscribeThread` returns a snapshot or, after a cursor, a replay of at most 128
facts and 1 MiB, then an optional synchronized marker and live facts. Snapshot
timelines are bounded (10 user turns, 75 items, 1 MiB); older rows are paged with
`readHistory`. `subscribeShell` replays at most 1,000 rows and 8 MiB, otherwise a
snapshot. A subscriber more than 1,000 updates or 8 MiB behind is closed and
resumes from its last applied sequence. `agent-core` keeps folded thread state
for five minutes after the last view and retries refused subscriptions with a
backoff from 250 ms to 30 s.

Native transcript import scans at most 100 recent transcripts from the last 30
days and imports up to 200 messages per thread. Imported threads keep their
native session for resumption.

Claude execution uses the official Agent SDK with existing subscription
authentication. The SDK is pinned by the npm lockfile in
`crates/host-daemon/src/claude/sdk`, vendored as `sdk.mjs` and embedded in the
Host executable; before a launch the Host writes that copy under its Claude
configuration directory (`remote-agent-sdk/<version>-<sha256>/`) and runs the
small bridge (`bridge.mjs`) with Node under the process supervisor, using `node`
beside the Host executable when present and otherwise `node` on PATH (18 or
newer). The bridge owns only SDK I/O: the persistent query and prompt stream,
permission and dialog callbacks, interrupt, usage and session fork. Rust owns
conversation state and permission decisions through `agent-providers`. The SDK
spawns the unmodified CLI with the selected account's configuration and
credential directories; credential variables from the Host environment and
`NODE_OPTIONS` are not passed on. Viewing or listing history starts no process.

Terminals, files, browser, worktrees, accounts, dictation, pairing and
revocation have independent Host owners. Thread terminals are keyed by thread
and terminal ID. A build does not replace a running Host.
