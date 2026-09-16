# Session runtime

Implementation: September 2026, starting from `ad76149` and integrating
`main` through `e052b01`. Codex and Claude Code
are the supported providers. No additional provider scaffolding is introduced.

## Ownership and protocol

- Provider native history is the only persistent conversation source. Host
  keeps current `Thread`/`Turn`/`Item` values in memory, using the same pure
  `SessionChange` reducer as the clients. It has no conversation database,
  event log, history index, or persistent native-ID map.
- `SessionRef { provider, id }` preserves the complete native ID. The existing
  `claude:` string representation is a compatibility boundary, not a Host ID.
- `host/session/open` returns the requested current window, subscription UUID
  and revision. Snapshot adoption, subscription registration and response
  enqueue happen under the same router lock. A concurrent native update makes
  a hydration result retryable; it cannot overwrite a newer current state.
- `host/session/update` sends typed changes. `host/session/close` removes one
  subscription. Disconnects invalidate its authority without stopping execution.
  Reconnect uses open again, preserving the previously requested window.
- Missing revisions invalidate the subscription and trigger a fresh open.
  Slow connections are closed when their bounded queues fill. Other clients
  and provider execution continue. There is no reconnect replay log.
- Unresolved requests live in the common session. The first valid answer is
  claimed after checking connection, execution and response content. Delivery
  states are awaiting, sending and unknown; resolution follows delivery.
  Unknown delivery is never automatically answered again.
- Input receipts are bounded and memory-only. A client ID cannot be reused
  for different input. Confirmation uses IDs, never similar message text.
  Unconfirmed inputs retain their text/attachments and are not automatically
  resent after reconnect or Host restart. Stop includes the native turn ID.

## Provider boundaries

Codex retains one shared app-server process. Native pagination, cursor use,
item hydration, repeated turn IDs, details and response/event ordering belong
to `host_rpc/codex.rs`. Clients request a larger window rather than merging
native cursor pages. Accounts share this process and existing switch guards.

Claude reads native project JSONL without launching the CLI. It resolves exact
UUIDs across actual configured project directories, checks native identities,
follows the selected parent chain, and preserves message/tool/subagent IDs.
It distinguishes unfinished tails, corrupt complete rows, unsupported content,
missing parents and unavailable files. Reads never repair or truncate files.
Anonymized Claude Code **2.1.266** native data is checked in as a fixture;
related subagent files and native tool-output references have bounded reads.
Metadata can remain usable when body display is unavailable; resumption still
uses that exact native ID and verified working directory.

Claude execution uses the installed CLI's stream-json interface and existing
subscription authentication. Consecutive turns reuse a process. Viewing or
listing history does not start one. Unsupported running input, rename and fork
capabilities are surfaced by core; existing Codex side chats and forks remain.

## Process ownership and deployment

Build and ship `bex-provider-supervisor` beside the Host, from the same revision.
Mac bundle scripts include and sign it; missing supervision makes provider
execution unavailable. It relays stdin without parsing provider content and
owns a Unix process group or Windows Job Object. Host input closure, including
abrupt Host death, terminates the CLI and its group. CLI exit also cleans up
remaining tool children. There is no orphan discovery/adoption. This is process
lifetime management, not an OS sandbox against deliberately detached processes.

## Bounds and partial results

| Resource | Bound / behavior |
| --- | --- |
| Host current sessions | 128; active execution, pending requests, unconfirmed sends, reads and subscriptions retain state |
| Requested turn window | 1–1000; previously retained larger windows survive reopen |
| Snapshot | 4 MiB; defer large bodies first, then explicitly mark a reduced window partial |
| Per-item inline text / image | 128 KiB / 2 MiB; full text uses detail reads |
| Outbound connection queue | Both item count and 16 MiB; overflow closes that connection |
| Unresolved requests | 32 per session, 64 KiB each; oversized requests cannot be approved |
| Send receipts | 1024, 15 minutes minimum; unresolved delivery retained, admission fails at capacity |
| Claude live/idle processes | 8; idle retention 60 seconds, record capacity 128 |
| Claude transcript read | Latest 64 MiB, maximum row 8 MiB; incomplete range is explicit |
| Claude native listing | 20,000 files; scan failure is a provider-specific partial result |
| Claude detail / image gallery | 4 MiB detail; 16 MiB gallery transfer |

Lists obtain provider metadata concurrently. Provider failure preserves the
other provider and cached summaries, labelled saved/unconfirmed. Search and
history display expose partial/unavailable results rather than implying absence.
Large inline images use item details without truncating base64. A gallery that
cannot be completed reports its limit and leaves individual conversation images
accessible.

## Local data and retired paths

Client persistence is scoped by Host public identity and a digest of configured
provider storage locations, including canonical existing ancestors. Switching
storage archives the old client scope in the same persisted snapshot; returning
to that configured area restores its drafts and unsaved file edits. The task
list explains this recovery path. Cache replacement does not replace drafts.

Old Bex Claude files are **not deleted** and are **not a permanent fallback**.
Keep them until any information absent from native history has been recovered.
Uploaded attachments and worktree files retain their existing storage and
revision/permission protections; no new automatic cleanup is introduced.

Removed: Bex Claude conversation writes/reads, external rollout watching,
client history overlap/cursor merging, per-device approval aliases/replay,
and raw provider conversation notifications on the client path. Old read/watch/
page RPCs reject requests; current-session open and item-detail RPCs replace them.
Pairing, revocation, files, worktrees, terminal, voice and native input remain.

## Cleanup before review

See [the size and responsibility audit](SESSION_REDESIGN_AUDIT.md) for the original
reduction estimate, measured production growth, corrected duplication, and remaining
required responsibilities. The original net-reduction estimate has not been achieved.

The old provider forwarding layer and catalog wrapper are removed. Native query
types now belong to the Host/Codex adapter, not client core. Shared sessions no
longer carry unused history/item cursors; the adapter keeps native paging cursors
locally. Desktop asks whether older history exists directly, without dummy
turn/cursor pairs. The unused client-history fixture was deleted. Current main
account login/logout and connection-attempt cancellation remain intact. Approval
routing uses the owning Session provider instead of inferring it from request-ID
prefixes; identical native IDs in different providers remain independent.

## Verification

Verified on macOS on September 16, 2026, using the Nix development environment:

- `cargo fmt --all`, workspace Clippy with `-D warnings`, and
  `cargo test --locked --workspace`: **291 passed, 0 failed, 2 ignored** (after main integration and cleanup).
- The final Host regression run includes the eight-image gallery RPC regression,
  provider-scoped request-ID collision test, and account logout/relogin coverage.
- `just conversation-ui`: 19 of 21 passed initially. The failed side-chat
  preparation test and image-gallery test both passed after fixes, with their
  acceptance assertions preserved.
- Five additional iOS tests passed for approval editing across reconnect,
  unresolved questions, failed-read recovery, account switching/forking, and
  live updates from another Bex connection. The last fixture now sends through
  `session/open` and `turn/start`; it no longer expects the retired external
  file watcher to deliver updates.
- The isolated `--without-codex` iOS test passed: Claude creates a conversation,
  restores it after app relaunch, and accepts a follow-up in the same session.
- `just android-e2e`: **10 passed**, including selection and reopening in the
  native WebView. Kotlin formatting and Detekt also passed.
- Swift formatting/lint, `just ios-markdown`, and the Python script suite
  (**10 tests**) passed.
- The additional stale-Claude-stop regression passed: a stop targeting the
  previous native turn is rejected while the current turn continues.
- Three diagnostic tests passed after removing the obsolete watch-failure
  notification branch.

Automated coverage includes native transcripts/subagents, current-state races, delivery
uncertainty, simultaneous approvals, stale subscriptions/stops, scoped drafts,
large history/details, process reuse, and an isolated Host SIGKILL test.

Real authenticated provider inference, physical devices, TestFlight, Linux and
Windows process-lifetime execution have not been run in this macOS workspace.
Production Host and installed apps have not been restarted or replaced.

Protocol references: [Codex app-server](https://developers.openai.com/codex/app-server/),
[Claude sessions](https://code.claude.com/docs/en/sessions),
[Claude native storage](https://code.claude.com/docs/en/claude-directory), and
[process-wrap ownership](https://docs.rs/process-wrap/10.0.0/process_wrap/).
