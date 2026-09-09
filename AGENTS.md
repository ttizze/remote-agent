# Architecture boundaries

- Rust owns agent operations and all behavior shared by desktop and mobile: protocol contracts, request construction and response interpretation, conversation state and event reduction, history reconciliation, accounts, worktree settings, and workspace file listing, reading, writing, and diffs. Both clients must call the same Rust implementation.
- Kotlin owns only mobile-specific coordination and lifecycle behavior. Do not implement desktop/mobile shared rules or agent state machines in Kotlin.
- Swift and Android platform code own UI rendering, user interaction, and APIs that require their platform. Keep platform adapters thin; send intents to the shared implementation and render its state.
- Choose ownership by responsibility and cross-platform reuse, not by the screen that first needs a feature. Do not duplicate shared behavior in Kotlin, Swift, or Android code. During a cutover, migrate every caller and remove the obsolete implementation.
- Do not introduce mutable service, manager, or controller classes to own agent behavior. Represent state as values and compute transitions with pure functions; pass operation inputs explicitly. Review UI classes by responsibility: retain thin framework adapters for rendering and interaction, but keep agent rules and state transitions out of them. In Rust, move owned state through transitions instead of copying conversation bodies.

# Mobile debugging order

Support only the latest stable iOS and Android major versions: currently iOS 26 and Android 17 (API 37). Keep deployment targets and Android SDK settings aligned; remove obsolete compatibility branches when advancing the minimum.

When debugging mobile behavior, especially task loading, use this escalation order:

1. Start with a deterministic headless unit, integration, and full-stack loop. Reproduce the user's symptom with an exact assertion that can fail red (for example, the expected task is present in the loaded project/task list); do not rely on logs alone.
2. If headless checks pass, reproduce with the iOS Simulator and XCUITest. Inspect the generated `xcresult` for passed, failed, and skipped tests; a skipped test is never a pass.
3. Use a physical device only after the lower layers pass or the evidence indicates a device-specific issue. Do not start physical-device work without explicit user authorization.

For every debugging run:

- Use a fresh, isolated pairing fixture and change one variable at a time.
- Keep credentials, pairing secrets, and sensitive payloads out of logs and reports; clean up temporary payloads and artifacts afterward.
- Preserve unrelated work-in-progress changes.
- Report the exact surfaces verified and the surfaces that remain unverified.
