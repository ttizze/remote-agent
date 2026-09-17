# Native terminal

The Host owns each PTY and its Alacritty terminal state. A terminal is scoped to an authenticated device and a working directory. Closing its UI detaches; reopening the UI, restarting a client or reconnecting the transport attaches to the existing shell. Exiting the shell, choosing **終了**, revoking the device or shutting down the Host ends it. Host restarts do not preserve processes.

Clients use the existing authenticated iroh connection and agent-core operations:

- Desktop: `alacritty_terminal` with a GPUI element and native text input, selection and clipboard.
- iOS: SwiftTerm's UIKit view, native selection, IME and keyboard accessory. A right-hand chat panel switches between terminal and files using icon-only tabs with an underline on the selected tool. There is no terminal title, terminate button or panel close button. Swipe left from the chat's right edge to open; swipe right across the tab bar or terminal to close. File navigation keeps its native back gesture. Edge activation preserves horizontal scrolling inside chat content such as Markdown tables. Compact screens slide the chat aside; wide screens keep both panes visible. The files tab reuses browsing, editing, downloads and workspace diffs; switching tools detaches the terminal without terminating its shell.
- Android: Termux's terminal-emulator and terminal-view with a remote byte-stream `TerminalSession` and a Compose keyboard bar. JNI and the Android-local shell are omitted.

`agent-core` owns stable terminal IDs, lifecycle, output acknowledgement and the native snapshot binding. Each attached client consumes raw bytes; reconnect replaces its screen using an ANSI checkpoint followed by the unfinished UTF-8/VT parser input. Checkpoint and live output are emitted by the same Host worker, in order. The worker's screen is resized before generating the checkpoint for the attaching client's dimensions. Only one connection attaches to a terminal at a time.

The Host alone answers terminal queries. Client emulator-generated replies are suppressed; native keyboard, paste and mouse input still go to the PTY. This keeps query responses independent of a phone's suspension or network disconnect.

Checkpoints support text terminal screens, primary/alternate buffers, bounded scrollback, wrapping, SGR colors and attributes, saved/current cursors, tab stops and terminal modes. They are not a bitmap/image transfer protocol. Client emulators retain their own platform-specific text rendering and selection behavior.

Alacritty and VTE are pinned, vendored forks with checkpoint additions. Their `UPSTREAM.md` files identify the source releases. SwiftTerm is pinned to an exact SPM revision, with `Package.resolved` checked in. Its reviewed build-info generator runs as an SPM build plugin; the isolated iOS build uses `-skipPackagePluginValidation` for unattended builds of this pinned dependency. Termux source revision, local adaptations and packaged license notices are recorded in `vendor/termux/UPSTREAM.md`.

Verification includes Host PTY lifecycle and single-responder tests, checkpoint continuation at every byte boundary of UTF-8 and VT sequences, Store acknowledgement tests, and native iOS/Android tests that set a shell variable, detach, reattach and exit with the retained value.
