# Agent behavior corpus

Source: `ttizze/remote-agent`, commit `ea8aefc`, `crates/agent-client/tests/fixtures/`.

All 31 case-local JSON Pointer references (`resultRef`, `errorRawRef`) are expanded into their corresponding literal values. The 87 cases retain the source RPC exchanges, expected results, failure payloads, and history/event/submission inputs.

The legacy `command.type` values describe scenarios; they are not the public API. New tests invoke typed operations directly. Do not restore the old JSON command dispatcher to consume these fixtures.

Counts: operations 48, host operations 15, history 8, events 7, submission 9.

`model-settings.json` adds seven desktop model-selection scenarios: catalog refresh, option removal and reordering, unsupported defaults, empty catalogs, model changes, and replacement by the catalog default. These preserve the existing desktop model-settings behavior through typed `Snapshot` transitions; they are separate from the original 87 cases.

`activity.json` adds six task activity/read-state scenarios. `submission-drafts.json` adds three draft reconciliation scenarios. Store integration tests also cover create-then-send with filtered list refresh, partial draft field edits interleaved with attachments, new-chat/catalog load ordering, binary workspace counts, retry after partial success, edits and navigation during transcription, synchronous intent enqueueing, and terminal stream retention/acknowledgement with serialized input and early process exit.
