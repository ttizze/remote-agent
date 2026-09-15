# Provider-neutral sessions and execution logs

## Decision

A Host session is the stable conversation identity. An execution is one attempt
within that session and records its provider and model. Provider-native resume
IDs are opaque checkpoints, independent of the Host session ID. Checkpoints must
not contain credentials. Changing the
provider must not require changing the conversation identity.

The Host owns an append-only log of shared conversation and execution state.
`agent_core::session` defines neutral records and a pure projection. Providers
translate their protocols into those records; they do not own a second mutable
display transcript. Drafts, navigation and transport state remain outside this
log. A provider checkpoint remains provider-specific inference state: replaying
the display log alone does not recreate it or promise an equivalent context on
another provider.

Execution identity, content identity, text append versus authoritative completed
content, tool completion and execution outcome are explicit. Starting a second
execution while one remains unfinished is rejected. An imported execution whose
original model was not recorded has an unknown target; it must not be attributed
to the most recently selected model. Opaque legacy content remains inspectable.

## Storage and recovery

Each session has a versioned JSONL file, with an explicit sequence number starting
at one. The file has one locked writer. Creation and legacy import publish a
complete initial log atomically without replacing an existing log. Streaming
text chunks are written before notification; complete entries, execution
boundaries and provider checkpoints additionally sync file data. This does not
promise per-token power-loss durability or durable command acceptance. If input
delivery fails after recording an execution, append its failed outcome instead
of rewriting history to erase the attempt.

The projection advances only after the write succeeds. A failed or canceled
append prevents further appends until reopen because its on-disk outcome may be
uncertain. Replay checks the schema version, contiguous sequence and event
invariants. Only a trailing record without its newline is truncated. Malformed
complete records, sequence gaps and unsupported versions stop recovery without
silently discarding history. An unfinished execution becomes interrupted through
an additional record when its adapter cannot resume the active process.

Sequence numbers are not byte offsets. Efficient bounded `after_seq` reads will
need a byte-offset index. Adding that index belongs with the synchronization
consumer, not with unused storage APIs. A derived index is rebuildable; the log
remains the durable source. Large tool bodies will be immutable blob references
when both adapters and clients support that contract.

## Migration order

1. Replace Claude's mutable Thread/Turn/Item display record with this projection
   and journal. Use a Host-side compatibility projection for existing clients.
   Retain legacy JSON as a migration source, but never write it again. A complete
   journal takes precedence; a corrupt journal must not fall back to older JSON.
2. Move Codex history ingestion, notification reconciliation and imported native
   IDs into its adapter. Introduce Host session lookup across providers, replacing
   provider selection from conversation ID prefixes.
3. Extend the neutral log contract to approvals and the remaining activity kinds;
   persist arbitration independently of RPC correlation IDs. Implement explicit
   context transfer between providers before enabling cross-provider continuation.
4. Migrate core/client synchronization to sequenced records and bounded replay.
   Remove the old client history merging, watch protocol and compatibility
   projection as their callers move. Old and new projections must not remain
   permanent parallel owners.
5. Simplify Store operation declarations and provider process lifetimes against
   the resulting ownership. Preserve synchronous local draft updates, stale
   side-effect reconciliation and ordering guarantees until their replacements
   exist and are exercised.

The first slice implements step 1. Compatibility conversion also preserves input
annotations, attachment paths and unknown input fields. Client RPC names and the `claude:` public ID
namespace are temporarily retained for compatibility; neither the neutral
projection nor its execution targets derive meaning from that spelling. New
Claude sessions allocate separate Host and provider IDs. Existing sessions retain
their IDs and native resume checkpoints. Public cross-provider switching, Codex
log ingestion, approval journaling, seq synchronization and provider supervision
are subsequent slices, not capabilities provided by the storage change alone.

Existing conversation interaction acceptance tests remain the product contract.
Validation covers replay after restart, interrupted execution recovery, legacy
migration, malformed logs, exclusive writers, streamed/final content replacement,
inputs and attachments, approvals, provider failures and resumption.
