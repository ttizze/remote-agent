# Mobile debugging order

When debugging mobile behavior, especially task loading, use this escalation order:

1. Start with a deterministic headless unit, integration, and full-stack loop. Reproduce the user's symptom with an exact assertion that can fail red (for example, the expected task is present in the loaded project/task list); do not rely on logs alone.
2. If headless checks pass, reproduce with the iOS Simulator and XCUITest. Inspect the generated `xcresult` for passed, failed, and skipped tests; a skipped test is never a pass.
3. Use a physical device only after the lower layers pass or the evidence indicates a device-specific issue. Do not start physical-device work without explicit user authorization.

For every debugging run:

- Use a fresh, isolated pairing fixture and change one variable at a time.
- Keep credentials, pairing secrets, and sensitive payloads out of logs and reports; clean up temporary payloads and artifacts afterward.
- Preserve unrelated work-in-progress changes.
- Report the exact surfaces verified and the surfaces that remain unverified.

# Test acceptance and coverage

- Define success and failure from the user's complete operation before writing a test. A request reaching the Host, an attachment appearing in the composer, a thread being created, or an answer row appearing is an intermediate state. Successful submission requires the intended input to reach the conversation, the turn to complete successfully, the sent draft/attachments to clear, and no unexpected error to remain. Reopen the conversation when persistence is part of the contract.
- Derive cases from interacting state, not implementation branches alone. For submission, cover text, attachments, and voice; new and existing conversations; no selected folder and an explicit project; and relevant saved settings both on and off. Settings tests must exercise the operation affected by the setting, including after reload. Defaults-only fixtures do not cover a configured installation.
- Exercise the production path through Store, serialization, transport, Host routing, and real isolated filesystem/Git resources where those boundaries participate in the bug. Use test doubles only at unavailable, nondeterministic, expensive, or unsafe external boundaries. A fixture must accept and reject the same relevant protocol states as the real provider, and must never inherit the developer's working directory, credentials, or saved preferences.
- Match the shipped binary's relevant dependency features and initialization conditions. Test first-use behavior in a fresh process when process-global state, TLS providers, authentication initialization, or test order could hide the failure. A passing crate-only build does not establish coverage of a different desktop feature graph.
- Test failure contracts separately from successful operations. Permission denial or missing authentication can prove error handling and draft preservation; neither proves successful recording, transcription, or voice submission. Report the exact boundary exercised and explicitly identify external-provider, native UI, and physical-device behavior that was not verified.
- For each escaped bug, identify the missing acceptance condition, state combination, fixture behavior, or build condition. Add or strengthen the smallest regression that fails for that cause and reaches the affected user outcome. Do not weaken assertions, substitute a simpler configuration, or count an expected failure as a successful end-to-end operation.

# Post-commit quality

- When a previously implemented feature is reported missing, inspect relevant dirty worktrees as well as branches and reflogs. A patch left in another worktree is not part of the current branch; preserve it and identify the missing integration before attributing the loss to a later refactor.
- Record behavior changes and their acceptance tests in the same commit. Before claiming a fix is integrated or released, identify its commit and verify that the target contains it; working-tree edits and tests from another checkout do not establish integration.
- Preserve the approved interaction contracts in `docs/DESKTOP_CONVERSATION_DISPLAY_CONTRACT.md`. When an existing acceptance assertion fails, determine whether the product requirement changed before changing the assertion; current implementation output alone is not evidence of a new requirement.
- Lefthook queues asynchronous quality checks after each commit. Before reporting a committed change as verified, run `nix develop . --command cargo xtask quality-status --wait` and inspect its JSON and referenced log on failure.
- Only `passed` for the current commit with `workingTreeDirty: false` verifies the current worktree. Queued, running, missing, interrupted, superseded, and failed are not passes. Uncommitted edits require their own verification.
- Results are not automatically injected into the agent conversation. Read the status explicitly; no desktop notification is configured.
