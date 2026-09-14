# Mobile visualize acceptance contract

The iOS and Android conversation views recognize a standalone `visualize{"path":"…"}` paragraph through agent-core's Markdown parser. Fenced/inline code, malformed or incomplete references, and ordinary Markdown retain their previous rendering. Desktop source extraction is unchanged.

`LoadVisualization` uses the authenticated Store → RPC → Host file service path. Core resolves relative paths against the conversation cwd; the Host accepts bounded (1 MiB), UTF-8, regular `.html`/`.htm` files. It archives the latest successfully read fragment in its private attachment directory under a digest of the resolved path. A subsequent read after source deletion uses that archive. Archives survive Store and Host recreation. A file deleted before its first successful display cannot be recovered; the UI explains that it must be recreated. Selection state is local to the displayed page and resets when reopened.

Both platforms consume the same sandbox document. Lucide 0.468.0 is vendored with its upstream license and archive digest; no runtime download is needed. Shared base styles support common fragments; interaction handlers belong to the supplied HTML. Tweak controls are not provided; the visible compatibility disclosure explains this. Unsupported `window.openai` calls and JavaScript failures show a message, and never dispatch a native operation. The original 12-option fragment is copied without changes into the regression fixture.

The iframe allows scripts only, with no same-origin privilege, popup, top navigation, form submission or native bridge. CSP disables network connections, external scripts/styles/fonts, nested frames and file resources; embedded data images/fonts remain available. iOS uses a nonpersistent WKWebView data store and cancels navigation away from internal about documents. Android disables network, content/file access, DOM storage and popups, and blocks external resource requests and navigation. Internal `about:`, `data:` and `blob:` resources stay available: inline loading uses internal `data:` requests, and embedded data resources are part of the shared contract. The Android UI regression also caught zero-height iframe layout: DevTools showed all twelve choices in the DOM but a child viewport height of 0. The Android WebView must use `MATCH_PARENT` layout parameters to establish a nonzero CSS viewport inside its fixed Compose frame. The shared wrapper also uses viewport-relative height and a block iframe. Camera, microphone, location and clipboard are not delegated. External CDN libraries and companion files are intentionally unsupported and produce a compatibility notice instead of silently disappearing.

Acceptance checks:

- Core: distinct visualization block; surrounding Markdown and code/malformed input preservation; shared resolution and sandbox contract; existing conversation and Markdown tests.
- Swift headless: production document adapter preserves visualization position and adjacent text.
- Isolated Host/Store: submit the original comparison request, complete the turn, clear the draft without errors, read the original fragment through authenticated transport, delete the source, serialize/recreate the Store, reopen history and obtain the same archived document.
- iOS Simulator/XCUITest: show the original comparison in the actual conversation, tap option 02, verify the changed preview, reopen history, repeat; retain the existing Markdown table test. Inspect xcresult and screenshots.
- Android instrumentation: fresh paired model and isolated Host, submit and complete the conversation, render production conversation/WebView, tap option 02, verify preview, dispose/reopen, repeat; retain existing Android tests and screenshots.

No running development Host or physical device is part of these checks.

Acceptance evidence recorded in task worktree `session-Mbrd3H` before integration:

- Core regression was observed failing before the parser change; final core suite: 58 passed.
- Isolated Host archive test: 1 passed. Real Store/serialization/transport/Host visualization round trip: 1 passed.
- Swift headless Markdown adapter and existing desktop Markdown selection regression passed.
- iOS 26.5 Simulator: 2 passed, 0 failed, 0 skipped, verified by `xcresulttool get test-results summary`. Result: `target/qa/Bex-1789390768-28366.xcresult`; screenshots: `target/qa/ios-visualize-{loaded,selected,reopened}.png`. This includes the existing Japanese Markdown table acceptance test.
- Android API 37: visualization acceptance passed (1 test), including selection and reopening; `target/qa-visualize-android-accepted.log`, screenshots `target/qa/android-visualize-{selected,reopened}.png`. Existing seven Store/UI tests passed in the preceding run (`target/qa-visualize-android-complete.log`). The full-run permission test failed in Android Settings; an earlier seven-test run also hit a PixelCopy screenshot timeout. Assertions were retained. Feature verification used explicitly granted LAN permission, so the full nine-test runner is not reported as passing.
- npm lockfile installation reproduced the exact vendored Lucide bundle; Rust formatting, SwiftLint and Android detekt passed.

The shared parser replaces treating the reference as ordinary paragraph text. Native adapters now consume a shared visualization block and the existing authenticated file-service route handles HTML delivery; clients do not duplicate parsing/path rules. No existing desktop renderer was replaced. The additional files implement previously absent mobile rendering and its regression coverage.

Platform API references: [Android local content](https://developer.android.com/develop/ui/views/layout/webapps/load-local-content), [Android WebView](https://developer.android.com/reference/android/webkit/WebView), [WKWebView](https://developer.apple.com/documentation/webkit/wkwebview/).
