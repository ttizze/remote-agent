# Session runtime

Codex and Claude Code are the supported providers.

## Ownership and protocol

- Provider native history is the only persistent conversation source. The Host
  stores only owned execution turns, unresolved requests and in-flight input IDs.
  `SessionActor` never adopts a native history response. Completed turns are
  released, even while clients remain subscribed.
- `SessionRef { provider, id }` preserves the complete native ID. The
  `claude:` prefix identifies Claude sessions in client thread keys.
- `host/session/open` reads native history on every call, including reconnect.
  At response enqueue, the router overlays owned execution and registers the
  subscription under one lock. An execution that completes during a read is
  retained only until those readers finish; cancellation releases the reader.
- `host/session/update` sends typed changes with a subscription UUID. The ordered
  transport carries the response before subsequent updates. There are no history
  hydration revisions, cached snapshots, replay logs or gap-repair protocol.
  Obsolete subscription UUIDs are ignored; queue overflow closes that connection.
- Turn lifecycle notifications upsert their items without removing independently
  streamed items, including when the native notification labels its view `full`.
  Explicit item removals delete items; a fresh history response replaces the view.
  An inapplicable update reopens the current history window and subscription;
  only a failed recovery read becomes a client error.
- Unresolved requests belong to the execution. Provider/session/turn/native request
  identity is shared by every client, with no per-device alias map. The first valid
  answer is claimed after checking connection, execution and content. Delivery can
  be awaiting, sending or unknown; unknown delivery is not automatically retried.
- Input IDs prevent duplicate admission only while their execution is in flight.
  There is no completed receipt, fingerprint, 15-minute retention or repeated
  response delivery. After completion/restart, the Host does not guarantee input
  deduplication. Clients preserve uncertain drafts and never automatically resend.
- `SubscribeSubmission` is folded into `SendSubmission`: open is applied to the
  Store before sending. Native-history selection takes immutable values instead
  of mutating a cloned Snapshot. Navigation epochs and draft protection remain.
- If native history is unavailable, the client supplements the response with
  cached completed turns. The Host's current turns, status and unresolved requests
  remain authoritative; subsequent text changes still target those current turns.

## Provider boundaries

Codex retains one shared app-server process. Native pagination, cursor use,
item hydration, repeated turn IDs, details and response/event ordering belong
to `host_rpc/codex.rs`. Clients request a larger window rather than merging
native cursor pages. Accounts share this process and existing switch guards.
The Codex adapter also owns native execution RPCs, input/reply conversion and
browser MCP configuration encoding. The Host chooses the submission route,
owns worktree preparation, browser scopes and delivery evidence, and passes
the required values to the adapter. Execution RPC names and payloads stay inside
the adapter.

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

Account selection changes credentials, not the user's configuration. Codex thread
RPCs keep the original App Server and `CODEX_HOME`; only authentication helpers
have private homes. Claude conversations, model discovery and usage requests use
the native `CLAUDE_CONFIG_DIR` for settings, skills, plugins and history, and
`CLAUDE_SECURESTORAGE_CONFIG_DIR` for the selected account's credentials. Claude
login/status/logout helpers retain private account metadata. The Host does not
copy or link the shared conversation configuration into account directories.

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
| Requested turn range | Positive client-supplied range; no Host 1,000-turn ceiling or retained window |
| History response | No Host snapshot cache or 4 MiB history budget; large items have explicit deferred bodies |
| Inline items | Tool items above 512 bytes load details on expansion; deferred command headers omit output. Messages and images above 1 MiB load through item details |
| Outbound connection queue | Shared 16 MiB budget including frame entries (no message-count cutoff for buffered bursts); slow queue overflow closes that connection, but an oversized single RPC returns `response_too_large` without disconnecting |
| Unresolved requests | 32 per session, 64 KiB each; oversized requests cannot be approved |
| In-flight input IDs | At most 128 per executing session; released when execution completes |
| Claude live/idle processes | 8; idle retention 60 seconds, record capacity 128 |
| Claude native reads | Transcript/output-file read budget 64 MiB, maximum JSONL row 8 MiB; incomplete range is explicit |
| Claude native listing | 20,000 files; scan failure is a provider-specific partial result |
| Detail / image transfer | Item details above 1 MiB use the existing binary stream: 512 MiB, 8 pending grants per connection / 64 total, 120-second validity, same connection and digest verification; gallery retains its 16 MiB client limit |

Lists obtain provider metadata concurrently. Provider failure preserves the
other provider and cached summaries, labelled saved/unconfirmed. Search and
history display expose partial/unavailable results rather than implying absence.
Large item details use anonymous temporary transfer files, not persisted history
or an index. The existing grant owner releases them on consumption/disconnect and
purges expired grants. Core automatically loads deferred messages and images;
tool output loads on demand. Base64 is never truncated. A gallery that
cannot be completed reports its limit and leaves individual conversation images
accessible.

If the combined item bodies still exceed the physical RPC limit, open defers
those bodies too, preserving every requested turn and item identity. Only an
oversized metadata-only response returns `response_too_large`; it does not create
a subscription or close the connection.

## Local data

Client persistence is scoped by Host public identity and a digest of configured
provider storage locations, including canonical existing ancestors. Switching
storage archives the old client scope in the same persisted snapshot; returning
to that configured area restores its drafts and unsaved file edits. The task
list explains this recovery path. Only drafts (including attachments and model
choices), pending submissions, unsaved file edits, navigation and unread markers
are persisted. Conversation bodies, task lists, model/account catalogs, workspace
results and connection state remain in memory and are fetched from the Host after
launch. Archived scopes retain the same client-owned data, without history caches.

Uploaded attachments and worktree files retain their storage and
revision/permission protections.

## Verification

The PR records the exact commit, workspace tests, Clippy, mandatory quality result
and CI results. Regression coverage includes native file changes between opens,
completion during read, live approvals after reconnect, cancellation cleanup,
release of finished execution while subscribed, untrimmed responses above the old
limits, duplicate in-flight input, and provider-scoped approval identity.
Existing Desktop/iOS/Android display acceptance assertions are preserved.
Further regression tests cover unavailable native history with cached turn A and
live turn B, pending requests and subsequent text updates; repeated opens of a
history containing 17 MiB text/image/tool items; lossless binary item reads through
the shared Store; and explicit oversized-RPC errors on a still-usable connection.

Build Host and affected clients from the same revision.

Protocol references: [Codex app-server](https://developers.openai.com/codex/app-server/),
[Claude sessions](https://code.claude.com/docs/en/sessions),
[Claude native storage](https://code.claude.com/docs/en/claude-directory), and
[process-wrap ownership](https://docs.rs/process-wrap/10.0.0/process_wrap/).
