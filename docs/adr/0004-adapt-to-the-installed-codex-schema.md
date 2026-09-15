# Adapt to the installed Codex schema

The Host starts the installed Codex App Server and completes its initialize handshake before readiness. It no longer generates a schema at startup: the extracted method list had no consumers, and method presence did not validate parameter shapes or runtime behavior. Unsupported methods are handled through their RPC errors.

Codex-native RPC payloads pass through without a second stable RPC schema. The mobile adapter projects known response and notification shapes; unknown item payloads remain visible. Unsupported requests return explicit errors. Installed-Codex runtime checks remain necessary: for example, lifecycle notifications with `itemsView: summary` contain only a subset of items and must preserve previously streamed history.

## Host provider boundary

`host_rpc::providers` owns provider selection and the adaptation of Codex and
Claude protocols. The Codex adapter owns its account state, event subscription,
process-directory tracking, external-thread watches and history hydration. The
Claude adapter owns its CLI sessions and persisted conversation records. The
Host service prepares workspaces, enriches project membership and manages files
and authenticated client connections; it does not inspect provider ID prefixes
or decode provider history pages. A shared catalog merges histories before the
Host applies project limits, retaining partial-provider errors and blocking
worktree removal when activity cannot be checked.

Server-request routing stores the issuing provider alongside the opaque upstream
ID. Response arbitration, reconnect replay and provider shutdown use that pair,
so identical IDs from different providers remain independent. Provider identity
is never inferred from an approval ID's spelling.

This is an internal ownership boundary, not a new public wire schema. Existing
conversation IDs, model IDs, unknown payload fields and per-provider unsupported
operations retain their contracts. Switching between Codex and Claude still
requires a new conversation; no context transfer or durable input queue is added.


## iPhone history projection

`host/thread/read` accepts optional `deferItemDetails: true`. The Host strips this Host-only parameter and reads native thread metadata first. Paginated histories use `thread/turns/list` with `itemsView: full`, descending order and a ten-turn limit, then reverse that page for chronological display. Legacy histories use `thread/read` with `includeTurns: true`. User and agent messages remain inline. Activities exceeding the 4 KiB inline budget retain their IDs, types and compact scalar headers; file-change paths and kinds remain available. Each turn records deferred IDs in `deferredItemIds`. Existing full-inline callers omit the option.

`host/thread/item/read` requires `threadId`, `turnId` and `itemId`. It pages `thread/items/list` for the specified turn and returns the complete matching item, including unknown fields. It never substitutes a different turn/item or a truncated body. A missing item returns `item_not_found`; upstream errors remain errors. Pairing remains the access boundary. No persistent Host cache of tool results is introduced.

The iPhone opts into this projection and requests a deferred body when its activity is expanded. Detail fetch and decoding run outside the main dispatcher. A fetch failure offers a reload action. This reduces initial transfer and cache serialization. Legacy reads still hydrate upstream history; explicitly expanding a large activity still transfers its full body.

## Model selection

The mobile adapter obtains available models and supported reasoning strengths through paginated `model/list`; no model names are embedded in the client. Per-Host selections are sent as `model` on `host/thread/start` and as `model` / `effort` on `turn/start`. Steering and queueing retain their existing installed-Codex contracts and do not override a running turn. Catalog errors remain visible and can be retried.

## Claude Code routing

The Host appends the installed Claude Code catalog to the first `model/list` page, using `claude:<CLI model value>` to avoid collisions. Selecting it on `host/thread/start` creates a Host-owned `claude:<UUID>` conversation after the same workspace/worktree preparation as Codex. Requests for that conversation are never sent to the Codex App Server. Switching inference providers requires a new conversation.

Claude runs as an unmodified `claude -p --input-format stream-json --output-format stream-json` subprocess. The Host performs the CLI initialize handshake, requires subscription authentication before sending input, and adapts its events into the existing Thread/Turn/Item presentation. Text and thinking blocks share Claude message IDs, so streaming block indices distinguish them; a completed block replaces its streamed body. Tool calls and results stay inspectable. Permission requests use the existing session router's per-client aliases and first-response semantics, then return to Claude's `can_use_tool` control callback. Credentials stay in Claude Code.

Claude retains its native session transcript; the Host appends neutral session events under its own state directory and derives the existing client response from that log (see [ADR 0006](0006-provider-neutral-session-log.md)). New Host session IDs and Claude resume IDs are allocated independently. Subsequent turns use the same native resume ID and cwd with `--resume`, including after restart. Legacy display JSON is imported once and is no longer rewritten. Unfinished persisted turns recover as interrupted, not completed. Model catalogs are cached until Host restart; failed catalog initialization can be retried. Claude steering/queueing and fork/delegation requests return explicit unsupported errors. No AGMSG dependency is added.

## An open conversation owned by another process

`thread/read` does not subscribe to another Codex process’s notifications, and resuming its active thread can conflict with its writer lock. The mobile controller uses native events for threads loaded by the Host’s Codex process. For a selected `notLoaded` thread, `host/thread/watch` watches its rollout parent through OS filesystem notifications; it only reports changes for that conversation to the requesting authenticated session. `host/thread/unwatch`, navigation and disconnect release the watch. Monotonic watch IDs prevent delayed registration/cancellation from replacing a newer watch.

A rollout path alone does not make a thread eligible for watching. Codex advertises the path before its first history is materialized, and a read during that interval can fail with `list_turns is not supported yet`. Opening an `idle` or `active` thread therefore relies on native events; a status notification that loads a previously external thread also removes its watch. This avoids racing file creation against history hydration without hiding upstream errors or substituting empty history. The subprocess fixture advertises paths and rejects unmaterialized history so submission checks cover this boundary.

The mobile controller coalesces change bursts into a quiet history refresh. The initial refresh after watch registration closes the read/subscribe gap. Navigation and generation checks discard stale callbacks. No interval polling or separate persisted history cache is introduced. Another process’s unsaved token deltas are not available through this path; refresh follows persisted item changes. Successful start/steer acknowledgements retain the live snapshot instead of immediately reading a rollout that may still be empty.


## Accepted mobile messages

Installed Codex can acknowledge `turn/steer` before emitting its user-message item. The mobile controller supplies `clientUserMessageId` for start, steer and queue requests and records the accepted body after a successful acknowledgement. These receipts are separate from native turn items, survive display-cache serialization and stale history reads, and disappear only when a native user message with the same `clientId` arrives. An echo that precedes the acknowledgement creates no receipt; equal text with different client IDs remains separate. Failed sends keep the composer draft and create no accepted receipt.

The receipt retains the actual turn ID and the preceding native item ID for placement. Presentation splits a native turn at each user message so additional inputs follow the preceding work. Segment display IDs remain separate from the actual turn ID used for interruption and item-detail requests. Queued receipts without a turn ID render after the current history until their matching native item arrives.

## Independent provider lifetime

`HostRpcService` owns routing and client sessions. Codex startup is optional and its event pump stops only that provider. On Codex exit, active Codex turns fail, terminal sessions receive `host/terminal/failed` without inventing an exit code, and only its pending requests resolve; Claude approvals and authenticated connections remain intact. Model and conversation lists merge available providers and expose `providerErrors`; an incomplete model catalog preserves saved selections. Worktree deletion fails closed when Codex activity is unavailable. iOS model choices are independent of the Codex account list.

Acceptance coverage includes `missing_codex_keeps_claude_inputs_workspaces_and_resumed_history_usable`, `codex_exit_preserves_claude_approval_and_completes_after_reconnect`, `upstream_exit_keeps_host_management_connected`, the core incomplete-catalog restoration test, and `testSimulatorUsesClaudeWithoutCodexAndRestoresConversation` (`just ios-e2e --without-codex` with that selector).
