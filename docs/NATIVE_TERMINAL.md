# Native terminal

The Host owns each terminal through its companion supervisor. The supervisor owns the Alacritty PTY, EventLoop, parser, and terminal state. A terminal is scoped to an authenticated device and a working directory. Closing its UI detaches; reopening the UI, restarting a client or reconnecting the transport attaches to the existing shell. Exiting the shell, choosing **終了**, revoking the device or shutting down the Host ends it. Host restarts do not preserve processes.

Clients use the existing authenticated iroh connection and agent-core operations:

- Desktop: `alacritty_terminal` with a GPUI element and native text input, selection and clipboard.
- iOS: SwiftTerm's UIKit view, native selection, IME and keyboard accessory. The native navigation stack has three destinations: the conversation list, a conversation-only screen, and a separate full-width workspace. Open the workspace from the conversation's grid button or a leftward swipe from its right edge; swipe back from the left edge to return to the conversation with its draft intact. The workspace's icon-only tabs select terminal, browser or files; conversation is never a tab. The browser displays the conversation’s Host-owned BEX browser over the authenticated connection. Human takeover, input validation and agent suspension belong to the Host; the phone renders frames and forwards typed input. Pages and login state stay on the Host when navigating back to chat. The edge-back gesture belongs to app navigation. See `REMOTE_BROWSER.md`. File navigation retains its own native back gesture. The files tab provides browsing, editing, downloads and workspace diffs. Leaving the terminal detaches its view without terminating the shell; view teardown stops pending input and resize callbacks. Terminal and files require a selected working directory.
- Android: Termux's terminal-emulator and terminal-view with a remote byte-stream `TerminalSession` and a Compose keyboard bar. JNI and the Android-local shell are omitted.

`agent-core` owns stable terminal IDs, lifecycle, output acknowledgement and the native snapshot binding. Each attached client consumes raw bytes; reconnect replaces its screen using an ANSI checkpoint followed by the unfinished UTF-8/VT parser input. Checkpoint and live output are emitted by the supervisor's EventLoop in order, then relayed by the Host. The native PTY and screen are resized before generating the checkpoint for the attaching client's dimensions. The Host keeps the previous attachment until that ordered checkpoint is delivered, and rechecks session liveness before switching owners. Only one connection attaches to a terminal at a time.

The Host's supervisor alone answers terminal queries. Client emulator-generated replies are suppressed; native keyboard, paste and mouse input still go to the PTY. This keeps query responses independent of a phone's suspension or network disconnect.

Checkpoints support text terminal screens, primary/alternate buffers, bounded scrollback, wrapping, SGR colors and attributes, saved/current cursors, tab stops and terminal modes. They are not a bitmap/image transfer protocol. Client emulators retain their own platform-specific text rendering and selection behavior.

Alacritty and VTE are pinned, vendored forks. Alacritty's local EventLoop/backend adaptations provide ordered checkpoints, native completion/error reporting, bounded queues, non-reaping Unix child observation, and Windows output draining; these are maintained Bex patches. Their `UPSTREAM.md` files identify the source releases. SwiftTerm is pinned to an exact SPM revision, with `Package.resolved` checked in. Its reviewed build-info generator runs as an SPM build plugin; the isolated iOS build uses `-skipPackagePluginValidation` for unattended builds of this pinned dependency. Termux source revision, local adaptations and packaged license notices are recorded in `vendor/termux/UPSTREAM.md`.

Verification includes Host PTY lifecycle and single-responder tests, checkpoint continuation at every byte boundary of UTF-8 and VT sequences, Store acknowledgement tests, and native iOS/Android tests that set a shell variable, detach, reattach and exit with the retained value.

## Library-backed supervisor trial

`bex-process` uses Alacritty for PTY creation and I/O scheduling instead of
`portable-pty` plus separate reader/writer threads. Its bounded event bridge and
private command protocol still belong to Bex. The supervisor's lifetime pipe,
whole-session Unix cleanup, and Host-owned Windows Job Object remain in place.
Input/resize acknowledgements report completed native operations, not queued work.
Final output drains until EOF or the existing one-second bound before exit.

`python3 scripts/test-terminal.py` runs the vendored backend's tests with the
repository's locked dependency versions. Both Linux and Windows CI run it before
the broader core tests so unrelated baseline failures cannot hide backend results.
`cargo test --locked -p bex-process` also checks real supervisor I/O and process
ownership. The dedicated supervisor byte-stream tests currently run on Unix;
native Windows behavior still needs its platform jobs and acceptance checks.

This trial consolidates dependencies and ownership; it does not claim a reduction
in maintained production source. The fork adaptations and their tests must be
weighed against the removed dependency and scheduling glue. Host-side parser
timeout handling does not by itself drive Desktop's separate raw-byte parser.
