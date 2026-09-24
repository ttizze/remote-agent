# Client state and device storage

`Snapshot` is the immutable runtime read model. Its JSON inspection API is used
by diagnostics and fixtures; it is not the device storage contract.
`agent_core::persistence` owns that contract. Desktop saves `PersistedState`,
and iOS/Android use the same encoder and decoder through the bindings.

The current format stores only the Host storage identity, archived storage
areas, drafts, attachments, unconfirmed submissions, unsaved file edits with
revision, navigation and unread marks. It does not store native conversation
history, lists, models, approvals, connections or other Host authority.
Returning to a different Host storage area preserves its own user data.

Encoding takes immutable values. Decoding requires all user-owned fields and
rejects malformed user data instead of silently creating empty drafts. Unknown
runtime fields are ignored, including invalid history/cache data: they cannot
prevent recovery of user work or restore connection authority. Empty bytes mean
a new installation. There are no old-format decoders or migrations.

Native clients still own private atomic file writes and lifecycle flushes.
Store restoration marks unconfirmed sends as delivery-unknown, retains their
body, attachments, send order and position, and never automatically resends them.
The Host supplies current delivery evidence when a connection is established.

Tests cover the real storage encoder/decoder, required user fields, scope
isolation, discarded runtime authority and desktop shutdown persistence.
