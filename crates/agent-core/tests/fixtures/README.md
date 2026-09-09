# Agent behavior corpus

Source: `ttizze/remote-agent`, commit `ea8aefc`, `crates/agent-client/tests/fixtures/`.

All 31 case-local JSON Pointer references (`resultRef`, `errorRawRef`) are expanded into their corresponding literal values. The 87 cases retain the source RPC exchanges, expected results, failure payloads, and history/event/submission inputs.

The legacy `command.type` values describe scenarios; they are not the public API. New tests invoke typed operations directly. Do not restore the old JSON command dispatcher to consume these fixtures.

Counts: operations 48, host operations 15, history 8, events 7, submission 9.

`daemon-wire.json` was captured on 2026-09-09 from PR #5's real `HostRuntime` over isolated iroh connections (`daemon_model_wire_fixture`). The upstream process is `bex-codex-fixture` with `[items]`; the project state contains a nonempty `rootPaths`. It records `host/thread/list` and `host/thread/read` responses, including object-valued tool results. Only the temporary workspace path is normalized to `/fixture/workspace`; no production data or credentials are used. The daemon integration test compares its generated responses to this corpus.
