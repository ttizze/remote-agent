# Code shape

- Before adding logic to a client (desktop, iOS, Android), check whether `agent-core` already computes it. Consume it; if it almost does, extend core instead of re-deriving in the client.
- The same rule in two files is a bug. When you find one, delete one in the same change.
- Do not add an enum variant, error type, generic parameter, trait method, option, or module that has one user. Add it when the second user appears.
- Each change reports lines added, lines removed, and which existing code the new code replaces. A change that only adds is suspect.

# Integration

- Reuse the current task worktree. Serialize updates to `main` and the running Host; separate worktrees do not isolate either. Integrate dependencies explicitly and preserve other sessions' uncommitted work.
- Build the Host and affected clients from the same revision. Before deployment, identify the executable actually running and account for active tasks before restarting it. Verify the changed operation against that process; rebuilding a file does not update a running Host.

# Tests

- Assert the user's complete outcome, not an intermediate state: the input reaches the conversation, the turn completes, the draft clears, no error remains. Reopen when persistence is part of the contract.
- Cover interacting states (input kind × new/existing conversation × selected folder × relevant settings on/off), not implementation branches.
- Run the production path (Store, serialization, transport, Host routing, real isolated filesystem/Git). Use doubles only at external boundaries that are unavailable, nondeterministic, expensive, or unsafe. Fixtures must never inherit the developer's directory, credentials, or preferences.
- Match the shipped binary's features and first-use initialization (TLS providers, auth, process-global state) when they could hide the failure.
- A failure-path test proves error handling, not the successful operation. Report exactly which boundary was exercised and what remains unverified.
- For each escaped bug, add the smallest regression that fails for its cause. Never weaken an assertion or substitute a simpler configuration.

- When migrating fixtures or runners, preserve initial state, values and counts, response/event order, fixture lifetime, and selected tests. Compare both versions before changing assertions. Navigation and concurrency checks must exercise overlapping operations and late callbacks.

# Debugging mobile

1. Headless: reproduce with an assertion that fails red. Logs alone are not evidence.
2. iOS Simulator + XCUITest: read the `xcresult`; a skipped test is not a pass.
3. Physical device: only after 1–2 pass, and only with explicit user authorization.

Use a fresh isolated pairing fixture, change one variable at a time, keep secrets out of logs, and preserve unrelated work in progress.

# Post-commit quality

- When a previously implemented feature is reported missing, inspect relevant dirty worktrees as well as branches and reflogs. A patch left in another worktree is not part of the current branch; preserve it and identify the missing integration before attributing the loss to a later refactor.
- Record behavior changes and their acceptance tests in the same commit. Before claiming a fix is integrated or released, identify its commit and verify that the target contains it; working-tree edits and tests from another checkout do not establish integration.
- Preserve the approved interaction contracts in `docs/DESKTOP_CONVERSATION_DISPLAY_CONTRACT.md`. When an existing acceptance assertion fails, determine whether the product requirement changed before changing the assertion; current implementation output alone is not evidence of a new requirement.

Lefthook queues checks after each commit. Run `nix develop . --command cargo xtask quality-status --wait` before reporting a commit as verified. Only `passed` for the current commit with `workingTreeDirty: false` counts; results are not injected into the conversation.
