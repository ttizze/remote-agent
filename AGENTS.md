# Project rules

- The product is unreleased. Keep only the current format and behavior; do not add backward-compatibility paths, old-format migrations or retired implementations.

- Before adding client logic (desktop, iOS, Android), reuse `agent-core` results or extend core instead of re-deriving them.
- Reuse the current task worktree. Serialize updates to `main` and the running Host: worktrees do not isolate either. Preserve other sessions' uncommitted work.
- Build the Host and affected clients from the same revision. Before restarting, identify the running executable and active tasks. Verify changed operations against that process; rebuilding does not update a running Host.
- Preserve approved interactions in `docs/DESKTOP_CONVERSATION_DISPLAY_CONTRACT.md`. Change acceptance assertions only when the product requirement changes.
- Follow `docs/TEST_MAINTENANCE.md`: run proptest in normal tests, use cargo-mutants for focused audits of changed logic, and reserve Kani for small, important pure functions with explicit verification bounds.
- Run automated verification in the Native clients GitHub Actions workflow. Before reporting a commit verified or merging, require successful CI checks for the current commit and a clean working tree. Do not queue local background QA or wait for local E2E; local check commands are for explicitly requested debugging.
