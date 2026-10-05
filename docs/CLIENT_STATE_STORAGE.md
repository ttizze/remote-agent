# Client state and device storage

`Snapshot` is the immutable client read model. `agent_core::persistence` encodes
device-owned drafts, project/thread selection, model defaults, unsaved file
edits with original revisions, shelf observations and unconfirmed commands and
launches. History, projections, approvals and connections are not restored.

Each paired PC has its own Store and private atomic file. Orchestration-specific
filenames replace the retired format without migration. Empty bytes initialize
new state; malformed current-format state fails explicitly.

Unconfirmed mutations replay in order with the same command IDs. Host receipts
deduplicate retries. Late responses cannot overwrite newer drafts/navigation.
Native buffers use revisions; the owner publishes before completing receipts.
File saves compare the revision captured when editing began; typing during save
survives with its base revision updated.

Native clients own private writes and lifecycle flushes. QR consent and identity
keys remain platform-owned. Restored drafts cannot authorize connections.
