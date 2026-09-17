# Termux terminal components

Source: https://github.com/termux/termux-app/tree/084d709fbf23ea83b5cb85fd3d795c775be06676

Pinned revision: `084d709fbf23ea83b5cb85fd3d795c775be06676`. The upstream GPLv3-only notice and Apache-2.0 exception are retained in `LICENSE.md`; full license texts are packaged in `src/main/assets/licenses/termux`. Bex replaces the local-process TerminalSession with a remote-byte-stream session; it does not ship Termux JNI or start an Android shell. Sources from terminal-emulator and terminal-view are combined into the Android library source directory. Emulator query replies are suppressed while feeding output because the Host owns replies.

Unsupported G2/G3 charset designations are consumed instead of leaking their final byte into the display during ANSI checkpoint replay.
