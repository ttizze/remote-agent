# Client state and device storage

`Snapshot` is the immutable client read model. It holds the folded shell and
thread states (`agent_core::sync`), the outbox of unconfirmed commands
(`agent_core::commands::outbox`) and device-owned edits.

`agent_core::persistence` encodes device-owned state: drafts, the default model,
the follow-up setting, project/thread selection, the outbox and rollbacks whose
message returns to the composer. The native app stores these bytes. Empty bytes
initialize new state; a damaged file resets drafts with a notice instead of
blocking the Host.

The Host data cache lives in the Store's cache directory, one per Host
(`agent_core::sync::DiskCache`). It keeps the active shell and settled threads
with their cursors: a thread with a preparing, starting or running run, or an
expanded history, is not written. A warm start shows the cache as `Cached` and
resumes with `after_sequence`; a new connection reloads the shell snapshot. The
first write waits 500 ms, later writes are at least 10 s apart, and disconnect
and shutdown write the latest value. A deleted thread's entry is removed.
Entries of another state format are ignored.

Unconfirmed commands are sent in order per thread with the same command IDs;
Host receipts deduplicate retries. A command completes once the thread or shell
stream reaches its committed sequence. A send clears the composer at once and
shows the message as pending; a refusal returns the text to the composer.
Lifecycle actions show on the thread list until the Host's row replaces them.

Native clients own private writes and lifecycle flushes. QR consent and identity
keys remain platform-owned. Restored drafts cannot authorize connections.
