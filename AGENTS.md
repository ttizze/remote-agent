# Project rules

- The product is unreleased. Keep only the current format and behavior; do not add backward-compatibility paths, old-format migrations or retired implementations.

- Before adding client logic (desktop, iOS, Android), reuse `agent-core` results or extend core instead of re-deriving them.
- Reuse the current task worktree. Serialize updates to `main` and the running Host: worktrees do not isolate either. Preserve other sessions' uncommitted work.
- Build the Host and affected clients from the same revision. Before restarting, identify the running executable and active tasks. Verify changed operations against that process; rebuilding does not update a running Host.
- Preserve approved interactions in `docs/DESKTOP_CONVERSATION_DISPLAY_CONTRACT.md`. Change acceptance assertions only when the product requirement changes.
- Lefthook queues checks after each commit. Before reporting a commit verified, run `nix develop . --command cargo xtask quality-status --wait`. Require `passed` for the current commit with `workingTreeDirty: false`; results are not injected into the conversation.

## Cursor Cloud specific instructions

- Use `nix develop .#native --command` for Rust, Just, and the Host. That shell provides Rust 1.98. Pass `cargo` or the binary directly; a login shell (`bash -l`) reloads `/usr/local/cargo/bin` (Rust 1.83) and hides the Nix toolchain. `nix` is available at `/usr/local/bin/nix`.
- Set `CARGO_BUILD_JOBS=2` so rustc stays within this machine's memory.
- Use `nix develop .` for JDK 21, Android SDK 37, Gradle, and Lefthook. iOS builds need Xcode on macOS.
- Start an isolated Host with `./target/debug/host-daemon --isolated --state-dir <dir> --no-relay --name 'Linux development'`, and pass those same flags to `status` and `invite`. Without Codex or Claude on `PATH`, `status` reports `codex_unavailable`.
- `bex-desktop` builds in the native shell. Opening its window needs a Vulkan surface the Nix dynamic loader can use.
