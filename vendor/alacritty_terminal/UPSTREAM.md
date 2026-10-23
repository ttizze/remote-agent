Vendored from crates.io `alacritty_terminal-0.26.0`. Local changes expose ANSI terminal checkpoints for Bex reconnects. See the source diff against this exact release when updating.

This is an application-specific emulator fork, not an API-compatible replacement
for the full upstream library. Bex uses `Term`, its parser, grid, selection, and
terminal events in the Host and desktop client. PTY creation, process ownership,
and I/O belong to `bex-process` and the Host terminal session.

The unused upstream `event_loop`, `tty` (Unix and Windows), `sync`, and `thread`
modules and their backend-only dependencies are omitted. The four tests inside
`tty` exercised only that removed backend: Unix passwd lookup and Windows
argument quoting, command construction, and child watching. They do not exercise
Bex's supervisor. Keep the emulator tests, Host terminal lifecycle/checkpoint
tests, and `bex-process` ownership tests when updating.
