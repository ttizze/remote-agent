# Project rules

- Before adding client logic (desktop, iOS, Android), reuse `agent-core` results or extend core instead of re-deriving them.
- Reuse the current task worktree. Serialize updates to `main` and the running Host: worktrees do not isolate either. Preserve other sessions' uncommitted work.
- Build the Host and affected clients from the same revision. Before restarting, identify the running executable and active tasks. Verify changed operations against that process; rebuilding does not update a running Host.
- Preserve approved interactions in `docs/DESKTOP_CONVERSATION_DISPLAY_CONTRACT.md`. Change acceptance assertions only when the product requirement changes.
- Lefthook queues checks after each commit. Before reporting a commit verified, run `nix develop . --command cargo xtask quality-status --wait`. Require `passed` for the current commit with `workingTreeDirty: false`; results are not injected into the conversation.
