# Adapt to the installed Codex schema

The Host starts the installed Codex App Server and completes its initialize handshake before readiness. It no longer generates a schema at startup: the extracted method list had no consumers, and method presence did not validate parameter shapes or runtime behavior. Unsupported methods are handled through their RPC errors.

Codex-native RPC payloads pass through without a second stable RPC schema. The mobile adapter projects known response and notification shapes; unknown item payloads remain visible. Unsupported requests return explicit errors. Installed-Codex runtime checks remain necessary: for example, lifecycle notifications with `itemsView: summary` contain only a subset of items and must preserve previously streamed history.


## iPhone history projection

`host/thread/read` accepts optional `deferItemDetails: true`. The Host strips this Host-only parameter and reads native thread metadata first. Paginated histories use `thread/turns/list` with `itemsView: full`, descending order and a ten-turn limit, then reverse that page for chronological display. Legacy histories use `thread/read` with `includeTurns: true`. User and agent messages remain inline. Activities exceeding the 4 KiB inline budget retain their IDs, types and compact scalar headers; file-change paths and kinds remain available. Each turn records deferred IDs in `deferredItemIds`. Existing full-inline callers omit the option.

`host/thread/item/read` requires `threadId`, `turnId` and `itemId`. It pages `thread/items/list` for the specified turn and returns the complete matching item, including unknown fields. It never substitutes a different turn/item or a truncated body. A missing item returns `item_not_found`; upstream errors remain errors. Pairing remains the access boundary. No persistent Host cache of tool results is introduced.

The iPhone opts into this projection and requests a deferred body when its activity is expanded. Detail fetch and decoding run outside the main dispatcher. A fetch failure offers a reload action. This reduces initial transfer and cache serialization. Legacy reads still hydrate upstream history; explicitly expanding a large activity still transfers its full body.

## Model selection

The mobile adapter obtains available models and supported reasoning strengths through paginated `model/list`; no model names are embedded in the client. Per-Host selections are sent as `model` on `host/thread/start` and as `model` / `effort` on `turn/start`. Steering and queueing retain their existing installed-Codex contracts and do not override a running turn. Catalog errors remain visible and can be retried.

## An open conversation owned by another process

`thread/read` does not subscribe to another Codex process’s notifications, and resuming its active thread can conflict with its writer lock. The mobile controller uses native events for threads loaded by the Host’s Codex process. For a selected `notLoaded` thread, `host/thread/watch` watches its rollout parent through OS filesystem notifications; it only reports changes for that conversation to the requesting authenticated session. `host/thread/unwatch`, navigation and disconnect release the watch. Monotonic watch IDs prevent delayed registration/cancellation from replacing a newer watch.

A rollout path alone does not make a thread eligible for watching. Codex advertises the path before its first history is materialized, and a read during that interval can fail with `list_turns is not supported yet`. Opening an `idle` or `active` thread therefore relies on native events; a status notification that loads a previously external thread also removes its watch. This avoids racing file creation against history hydration without hiding upstream errors or substituting empty history. The subprocess fixture advertises paths and rejects unmaterialized history so submission checks cover this boundary.

The mobile controller coalesces change bursts into a quiet history refresh. The initial refresh after watch registration closes the read/subscribe gap. Navigation and generation checks discard stale callbacks. No interval polling or separate persisted history cache is introduced. Another process’s unsaved token deltas are not available through this path; refresh follows persisted item changes. Successful start/steer acknowledgements retain the live snapshot instead of immediately reading a rollout that may still be empty.


## Accepted mobile messages

Installed Codex can acknowledge `turn/steer` before emitting its user-message item. The mobile controller supplies `clientUserMessageId` for start, steer and queue requests and records the accepted body after a successful acknowledgement. These receipts are separate from native turn items, survive display-cache serialization and stale history reads, and disappear only when a native user message with the same `clientId` arrives. An echo that precedes the acknowledgement creates no receipt; equal text with different client IDs remains separate. Failed sends keep the composer draft and create no accepted receipt.

The receipt retains the actual turn ID and the preceding native item ID for placement. Presentation splits a native turn at each user message so additional inputs follow the preceding work. Segment display IDs remain separate from the actual turn ID used for interruption and item-detail requests. Queued receipts without a turn ID render after the current history until their matching native item arrives.
