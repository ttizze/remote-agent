# Agent behavior corpus

Source: `ttizze/remote-agent`, commit `ea8aefc`, `crates/agent-client/tests/fixtures/`.

All 31 case-local JSON Pointer references (`resultRef`, `errorRawRef`) are expanded into their corresponding literal values. The imported corpus originally contained 87 cases. It now retains 83 after removing test-side JSON validation, test-side cwd normalization, and two equivalent standalone operation cases. Remaining RPC exchanges, failure payloads, and history/event/submission inputs are preserved.

The legacy `command.type` values describe scenarios; they are not the public API. New tests invoke typed operations directly. Do not restore the old JSON command dispatcher to consume these fixtures.

Counts: operations 44, host operations 15, history 8, events 7, submission 9.

`model-settings.json` adds six desktop model-selection scenarios: catalog refresh with reordered options, option removal, unsupported defaults, empty catalogs, model changes, and replacement by the catalog default. These preserve the existing desktop model-settings behavior through typed `Snapshot` transitions; they are separate from the original 87 cases.

`activity.json` adds six task activity/read-state scenarios. `submission-drafts.json` contains three draft reconciliation scenarios exercised through Store dispatch and actual JSONL replies, including attachments and model changes made during a pending submission. Store integration tests also cover create-then-send with filtered list refresh, partial draft field edits interleaved with attachments, new-chat/catalog load ordering, binary workspace counts, retry after partial success, edits and navigation during transcription, synchronous intent enqueueing, and terminal stream retention/acknowledgement with serialized input and early process exit.

`daemon-wire.json` was captured on 2026-09-09 from PR #5's real `HostRuntime` over isolated iroh connections (`daemon_model_wire_fixture`). The upstream process is `bex-codex-fixture` with `[items]`; the project state contains a nonempty `rootPaths`. It records `host/thread/list` and `host/thread/read` responses, including object-valued tool results. Only the temporary workspace path is normalized to `/fixture/workspace`; no production data or credentials are used. The daemon integration test compares its generated responses to this corpus.

`markdown/table.md` is the reported Japanese table, shared by Rust contract tests, the iOS headless adapter test, the isolated Host response, and Android UI test assets. It contains no account or Host data.

`markdown/document.md` exercises the shared GFM document contract: headings, nested inline styles, quotes, ordered/task lists, code, and whole-document link/image references. Core, Swift FFI and Android UI tests consume the same source.

See [test maintenance](../../../../docs/TEST_MAINTENANCE.md) for retained boundaries and runner changes.
