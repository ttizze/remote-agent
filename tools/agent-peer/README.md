# agent-peer

A small CLI and shared skill for asking Claude Code or Codex for analysis,
design, and code review. Either agent can call the other and continue the same
native conversation. No team registration, database, or daemon.

## Install

On Apple Silicon macOS or x86-64/ARM64 Linux, including a VPS with Nix:

```sh
nix profile add github:ttizze/agent-peer
agent-peer-install-skills
```

Install and authenticate Claude Code and/or Codex separately on that machine.
The package supplies Python; it does not bundle provider CLIs, credentials,
or conversation history. For reproducible deployments, use
`github:ttizze/agent-peer/<commit>` instead of the unpinned URL.

`agent-peer-install-skills` links the packaged skill into
`${CODEX_HOME:-~/.codex}/skills/agent-peer` and
`${CLAUDE_CONFIG_DIR:-~/.claude}/skills/agent-peer`. It updates existing symlinks
but refuses to overwrite files or directories. Run it again after upgrading
the package. For a separate service account, run the installation as that
account or supply `--codex-home` and `--claude-home`. Ensure the account running
the agents has `agent-peer` on PATH. Reload the agent's skills after installation.

Without Nix, Python 3.9+ on macOS or Linux is sufficient:

```sh
git clone https://github.com/ttizze/agent-peer.git
cd agent-peer
./scripts/agent-peer-install-skills
export PATH="$PWD/scripts:$PATH"
```

Keep that checkout while its skill links are in use.

## Use

```sh
agent-peer ask claude --cwd /path/to/project 'Analyze this design…'
agent-peer ask codex --cwd /path/to/project 'Review this proposal…'
agent-peer reply claude SESSION_UUID --cwd /path/to/project 'Explain this finding…'
agent-peer reply codex SESSION_UUID --cwd /path/to/project 'Consider this evidence…'

# Large prompts, diffs, or analysis from the other agent:
agent-peer ask codex --cwd /path/to/project - < prompt.txt
```

Stdout contains JSON with `provider`, `session_id`, `cwd`, `status`, and `text`.
Only `status: ok` exits with code 0. Failed runs may include partial text.
Follow-ups require the same provider, session ID, and original project directory.
The caller's conversation is not automatically sent: include the question,
relevant context, files or diff, and desired output in the prompt.

`--model` selects a native model; otherwise the provider's CLI/session default
applies. `--timeout` defaults to 600 seconds. Expiry or interruption terminates
the child process group; there is no automatic retry. A partial native session
may remain. Concurrent writes to the same native session are not supported.

This version is for consultation: Claude has Read/Glob/Grep tools, with hooks,
skills, and MCP disabled; Codex uses its read-only sandbox and existing CLI
configuration. The caller performs any authorized implementation itself.

Override executable discovery with `AGENT_PEER_CLAUDE_BIN` or
`AGENT_PEER_CODEX_BIN`. Codex prefers the bundled ChatGPT/Codex CLI on macOS,
then PATH and `~/.local/bin`. Claude uses PATH and `~/.local/bin`.

## Develop

```sh
nix flake check --print-build-logs
nix develop --command python3 -B -m unittest discover -s tests -v
```

Checks run deterministic provider fixtures and test the installed package and
skill links without contacting either AI service. Real inference additionally
requires working provider authentication and quota.

MIT licensed. See [LICENSE](LICENSE).
