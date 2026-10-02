Vendored from crates.io `alacritty_terminal-0.26.0`. See the source diff against
that exact release when updating; this is a Bex-specific fork.

Local changes:

- ANSI checkpoints include the state needed by Bex reconnects, paired with the
  vendored VTE parser's preceding character and unfinished input
- The EventLoop publishes ordered raw output and size-matched checkpoints,
  reports native write/resize completion, and drains owner-generated query replies
- Commands and pending writes are bounded; shutdown is independent of queue
  capacity. Synchronized-output deadlines are checked even during ready traffic
- The final output drain retains Bex's one-second bound, with a distinction between
  temporarily empty Windows output and true EOF
- Unix child observation does not reap; the supervisor can take the native Child
  and perform session-wide cleanup before reaping. Resize and cwd failures are
  reported; passwd lookup checks errors before reading the initialized result
- Windows buffered writes report native completion/errors. Shutdown drains output
  before closing ConPTY; child-exit readiness survives late registration and is
  consumed once. Native creation/resize failures return errors

These are maintained local contracts, not features supplied unchanged by upstream.
The supervisor's lifetime pipe, Unix session cleanup, Windows Job Object, and Host
attachment/authorization logic remain Bex responsibilities. The backend's Windows
process/wait callback handle lifetime remains process-scoped; Bex starts one
supervisor process per terminal.

Run `python3 scripts/test-terminal.py` from the repository (or `python` on Windows)
to test this fork in an isolated workspace using only the repository's locked
remote package versions. The native build must have populated that target's cache.
The runner fails if resolution introduces a different package/version/checksum;
it never updates the repository lockfile. Keep native Windows/macOS execution
separate from cross-target type checking.
