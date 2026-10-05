# Session runtime

Codex and Claude Code are the supported providers.

## Ownership and protocol

- The Host's SQLite conversation database is the authoritative history, title,
  queue and submission-receipt store. Native history is imported automatically
  on first startup and retained after source files disappear. Import workers
  own native pagination, hydration and bounded retries. Clients read stored
  pages and receive import progress or explicit incomplete-history reasons.
- `SessionRef { id }` contains an opaque Host conversation ID. Provider and
  native identity belong to the persisted binding. Clients cannot change the
  execution provider by changing a reference. Provider metadata reaches core
  separately for presentation and model selection.
- `SessionActor` retains live execution, unresolved request origins and delivery
  evidence. It overlays the database read while registering a subscription
  under that conversation's lock. Completed turns are released from memory;
  committed history remains in the database.
- Each conversation opens on its own iroh bidirectional stream. The response
  precedes subsequent typed `SessionChange` frames on that stream. The client
  assigns its subscription identity locally and ignores obsolete subscriptions.
  Queue overflow closes the affected connection.
- Native events are normalized and committed before publication. Turn lifecycle
  updates preserve independently streamed items; explicit removals delete items.
  Client recovery reopens the current stored window and subscription.
- Native request sources own answer mappings and resources. The router checks
  persisted provider ownership, current execution and normalized answer content
  before claiming a response. Sending or unknown delivery is never retried
  automatically. Runtime origins are retired when their source closes.
- Input IDs and immutable admission payloads persist across completion and Host
  restart. Repeated identical admission returns its durable receipt. Changed
  content under an admitted ID is rejected. A queued message has a separate
  editable execution payload; the original receipt identity remains unchanged.
- The Host claims queued work durably before native IO and retains IO ownership
  after the client disconnects. Restart changes uncertain in-flight delivery to
  unknown rather than sending the input again. Successful stop holds remaining
  queued inputs; queue edits and title changes require no provider process.
- `SendSubmission` opens and applies the conversation to Store before sending.
  Core captures provider metadata separately from its opaque reference and
  normalizes model settings when either metadata or the catalog arrives.
- Database reads, stored list filtering and completed submission receipts do not
  depend on the current native history path. Native execution checks the binding
  against the configured source. Changing a provider path cannot erase already
  imported histories or unrelated client drafts.

## Provider boundaries

Codex retains one shared app-server process. Native pagination, cursor use,
item hydration, repeated turn IDs, details and response/event ordering belong
to `host_rpc/codex.rs`. Clients request normalized history pages; core joins
them to the current window without interpreting native cursors. Native request sources own their answer mappings and
send resources; the Host validates normalized answers and arbitrates delivery.
Accounts accept account commands and return account results, without a generic
RPC request or response crossing that boundary. Accounts share this process and existing switch guards.
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
listing history does not start one. Claude active steering uses priority-now input on the running process. The
Host owns rename and queued input for both adapters; unsupported native fork
capabilities are surfaced by core. Codex execution uses app-server RPC.

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
| Unresolved requests | 256 total, 32 per session, 64 KiB each; oversized requests cannot be approved |
| Unconfirmed input IDs | At most 128 per executing session; released after a finished turn echoes the input |
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

## Client operations

Store captures only the immutable inputs each effect needs before publishing
its transition. Execution does not receive a Snapshot. Operation generations
belong to their target: independent file, workspace, account and conversation
work does not advance navigation epochs. A newer read of the same target wins;
actual navigation still protects the selected view. Runtime operation progress
and errors are available through core to Desktop, Swift and Kotlin and are not
persisted. Successful operation records are released.

The command queue holds 256 entries. Admission precedes publication, so a full
queue cannot clear a draft or create an unconfirmed submission without queuing
its work. Ordinary work shares a limit of 32 running core jobs, with 16 additional
slots reserved for answers and interrupts. List bursts retain the newest prepared
query and join receipts. Item work retains four transfer slots through final
application, with at most 132 distinct reads and 128 waiters per coalesced receipt.
Terminal work has a 128-entry queue and up to 16 independent handles running;
commands for one handle retain their order. Bounded overload is reported as a
pre-delivery failure, preserving submission recovery. Rejected input leaves a
live terminal running; a rejected start ends its loading state with an error.
Rejected catalog loads preserve existing candidates and finish loading so the
composer can retry.

Dictation preparation uses the same bounded command queue and retains at most
32 preparation jobs. It is best effort: skipped preparation leaves the complete
recording available for transcription with a fresh connection. History reads
and cursor pages share core progress and errors; native clients subscribe to
that state without keeping separate loading flags or completion callbacks.

The same pure change operation accepts an owned Timeline on the Host and uses
copy on write for client snapshots. Unique live turns and items append text
without cloning accumulated bodies; shared readers keep their previous values.

## Local data

Client persistence is scoped by Host public identity and a digest of configured
provider storage locations, including canonical existing ancestors. The digest
serializes the provider names and normalized paths directly from a sorted map,
so input order and JSON object-ordering features cannot change the identity. Switching
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

Verification records the exact commit, local unit and integration results and CI
results. Regression coverage includes native file changes between opens,
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
