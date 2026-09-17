# Agent behavior corpus

Source: `ttizze/remote-agent`, commit `ea8aefc`, `crates/agent-client/tests/fixtures/`.

The retained corpus covers operation exchanges, failure payloads, events and submissions. Native history paging and synchronization now run through Host integration and shared Session tests rather than client-side cursor fixtures.

The `command.type` values describe test scenarios. The test harness invokes the current typed operations and compares their exchanges and results.

`model-settings.json` adds six desktop model-selection scenarios: catalog refresh with reordered options, option removal, unsupported defaults, empty catalogs, model changes, and replacement by the catalog default. These preserve the existing desktop model-settings behavior through typed `Snapshot` transitions; they are separate from the original 87 cases.

`activity.json` adds six task activity/read-state scenarios. `submission-drafts.json` contains three draft reconciliation scenarios exercised through Store dispatch and actual JSONL replies, including attachments and model changes made during a pending submission. Store integration tests also cover create-then-send with filtered list refresh, partial draft field edits interleaved with attachments, new-chat/catalog load ordering, binary workspace counts, retry after partial success, edits and navigation during transcription, synchronous intent enqueueing, and terminal stream retention/acknowledgement with serialized input and early process exit.

`daemon-wire.json` was captured on 2026-09-09 from PR #5's real `HostRuntime` over isolated iroh connections (`daemon_model_wire_fixture`). The upstream process is `bex-codex-fixture` with `[items]`; the native project catalog contains a nonempty `roots`. It records `host/thread/list` and current `host/session/open` responses, including object-valued tool results. Only the temporary workspace path is normalized to `/fixture/workspace`; no production data or credentials are used. The list also records the explicit `worktreeMerged: false` returned for this non-worktree workspace. The September 16 core redesign adds explicit `session` identities and adapter-supplied `capabilities` to both responses. The daemon integration test compares its generated responses to this corpus.

`markdown/table.md` is the reported Japanese table, shared by Rust contract tests, the iOS headless adapter test, the isolated Host response, and Android UI test assets. It contains no account or Host data.

`markdown/document.md` exercises the shared GFM document contract: headings, nested inline styles, quotes, ordered/task lists, code, and whole-document link/image references. Core, Swift FFI and Android UI tests consume the same source.

See [test maintenance](../../../../docs/TEST_MAINTENANCE.md) for retained boundaries and runner changes.
