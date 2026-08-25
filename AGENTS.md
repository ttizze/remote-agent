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
