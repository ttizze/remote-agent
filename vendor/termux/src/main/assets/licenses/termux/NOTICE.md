The `termux/termux-app` repository is released under [GPLv3 only](https://www.gnu.org/licenses/gpl-3.0.html) license.

### Exceptions

- [Terminal Emulator for Android](https://github.com/jackpal/Android-Terminal-Emulator) code is used which is released under [Apache 2.0](https://www.apache.org/licenses/LICENSE-2.0) license. Check [`terminal-view`](terminal-view) and [`terminal-emulator`](terminal-emulator) libraries.
- Check [`termux-shared/LICENSE.md`](termux-shared/LICENSE.md) for `termux-shared` library related exceptions.

Bex includes terminal-emulator and terminal-view from termux/termux-app revision 084d709fbf23ea83b5cb85fd3d795c775be06676. TerminalSession is replaced with a Host-backed byte-stream adapter; no local shell or Termux JNI is included. Source: https://github.com/ttizze/remote-agent/tree/main/vendor/termux
