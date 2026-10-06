# Provider protocols

These translators consume normalized commands from `agent-domain`. They retain only native RPC, message, block, tool and task correlation. They do not allocate application IDs, assemble projections, impose process capacity, or buffer application wake turns. The thread machine owns those decisions.

Claude CLI controls follow `@anthropic-ai/claude-agent-sdk` **0.3.276**, as pinned by the reference commit `4ee6bfd50ef4a089440d5c3662db2298da9cc50e`. The npm archive was downloaded and its SHA-512 checked against the lockfile. The version and integrity are recorded in `claude_control.rs`. No SDK implementation or executable is vendored. Launch arguments, JSON-lines framing, initialization, permission callbacks, cancellation, model changes and permission mode changes are implemented directly in Rust. Image bytes must be prepared by the resource owner and supplied as `PreparedImage`; a filesystem path is never sent as an image URL.

The 71 NDJSON transcripts under `src/fixtures` are copies of the reference orchestration replay fixtures at that commit, with the recording client's name replaced by the Host's and its repository paths and package names by neutral placeholders. `manifest.json` records their source scenario and SHA-256. Their MIT notice is in `src/fixtures/LICENSE`. SDK callback frames in the recordings are mapped to CLI control requests by the test harness. Application IDs are deliberately different; visible content, order and ownership are the acceptance criteria.

Session supervision, outbox execution, restart persistence, native history reads and process replacement belong to the Host runtime (the next stage). `StdioProcess` is only an uncapped JSON-lines transport with explicit lifetime ownership; its stderr must be drained by the caller.
