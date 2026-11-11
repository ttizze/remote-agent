#if os(macOS)
    import AppKit
    @testable import BexNshNative
    import XCTest

    @MainActor final class TerminalTests: XCTestCase {
        @MainActor private final class Events: NSObject {
            var quoted: [String] = []
            var input: [UInt8] = []
            var inputReceived: XCTestExpectation?
            var menus = 0
            var menuReceived: XCTestExpectation?

            @objc func menuBegan(_ note: Notification) {
                menus += 1
                menuReceived?.fulfill()
                if let menu = note.object as? NSMenu {
                    DispatchQueue.main.async { menu.cancelTracking() }
                }
            }
        }

        private func terminal(_ events: Events) throws -> (UnsafeMutableRawPointer, HostTerminalView) {
            _ = NSApplication.shared
            let handle = terminalCreate(Unmanaged.passUnretained(events)
                .toOpaque()) { context, kind, bytes, count, _, _ in
                    guard let context, let bytes, count > 0 else { return }
                    MainActor.assumeIsolated {
                        let events = Unmanaged<Events>.fromOpaque(context).takeUnretainedValue()
                        let data = Array(UnsafeBufferPointer(start: bytes, count: count))
                        if kind == 4 {
                            events.quoted.append(String(decoding: data, as: UTF8.self))
                        }
                        if kind == 1 {
                            events.input += data
                            events.inputReceived?.fulfill()
                        }
                    }
                }
            let view = try XCTUnwrap(Unmanaged<NSView>.fromOpaque(terminalView(handle))
                .takeUnretainedValue() as? HostTerminalView)
            view.frame = NSRect(x: 0, y: 0, width: 640, height: 240)
            terminalFeed(handle, nil, 0, 40, 6)
            return (handle, view)
        }

        private func feed(_ text: String, into handle: UnsafeMutableRawPointer) {
            let bytes = Array(text.utf8)
            bytes.withUnsafeBufferPointer { terminalFeed(handle, $0.baseAddress, $0.count, 0, 0) }
        }

        private func contextMenu(_ view: HostTerminalView) throws -> NSMenu {
            let event = try XCTUnwrap(NSEvent.mouseEvent(
                with: .rightMouseDown, location: .zero, modifierFlags: [], timestamp: 0,
                windowNumber: 0, context: nil, eventNumber: 0, clickCount: 1, pressure: 0
            ))
            return try XCTUnwrap(view.menu(for: event))
        }

        func testTerminalMenusFollowSelectionAndInputAvailability() throws {
            let events = Events()
            let (handle, view) = try terminal(events)
            defer { terminalRelease(handle) }
            let labels = Array(#"{"quote":"チャットに追加","copy":"コピー","paste":"貼り付け","selectAll":"すべて選択","bottom":"最新へ"}"#
                .utf8)
            labels.withUnsafeBufferPointer { terminalMenu(handle, $0.baseAddress, $0.count) }
            let empty = try contextMenu(view)
            XCTAssertEqual(empty.items.map(\.title), ["チャットに追加", "コピー", "貼り付け", "すべて選択", "最新へ"])
            XCTAssertEqual(empty.items.map(\.isEnabled), [false, false, false, true, true])
            view.selectAll()
            XCTAssertEqual(try contextMenu(view).items.map(\.isEnabled), [false, false, false, true, true])
            feed("first 日本語\r\nsecond", into: handle)
            empty.performActionForItem(at: 3)
            XCTAssertTrue(try XCTUnwrap(view.getSelection()).contains("first 日本語\nsecond"))
            XCTAssertEqual(view.selectionMenu().items.map(\.title), ["チャットに追加", "コピー"])
            XCTAssertEqual(try contextMenu(view).items.map(\.isEnabled), [true, true, false, true, true])
            terminalInput(handle, true)
            XCTAssertTrue(try contextMenu(view).items[2].isEnabled)
            try contextMenu(view).performActionForItem(at: 0)
            XCTAssertTrue(events.input.isEmpty, "selection and menus never write to the PTY")
        }

        func testTerminalCopiesSnapshotAndOnlyQuotesAnUnchangedSelection() throws {
            let events = Events()
            let (handle, view) = try terminal(events)
            defer { terminalRelease(handle) }
            feed("selected 日本語", into: handle)
            view.selectAll()
            let selected = try XCTUnwrap(view.getSelection())
            let menu = view.selectionMenu()
            menu.performActionForItem(at: 0)
            XCTAssertEqual(events.quoted, ["selected 日本語"])
            XCTAssertNil(view.getSelection())
            menu.performActionForItem(at: 1)
            XCTAssertEqual(NSPasteboard.general.string(forType: .string), selected)
            feed("\r\nnew output", into: handle)
            view.selectAll()
            menu.performActionForItem(at: 0)
            XCTAssertEqual(events.quoted, ["selected 日本語"], "a stale menu cannot quote changed output")
        }

        func testTerminalPasteUsesHostInputAndCannotWriteAfterDisconnection() throws {
            let events = Events()
            let (handle, view) = try terminal(events)
            defer { terminalRelease(handle) }
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString("echo 日本語", forType: .string)
            view.paste(self)
            XCTAssertTrue(events.input.isEmpty)
            let input = expectation(description: "SwiftTerm delivers pasted input on the main queue")
            events.inputReceived = input
            terminalInput(handle, true)
            view.paste(self)
            wait(for: [input], timeout: 1)
            events.inputReceived = nil
            XCTAssertEqual(String(decoding: events.input, as: UTF8.self), "echo 日本語")
            events.input = []
            feed("\u{1b}[6n", into: handle)
            let replies = expectation(description: "queued terminal replies have been delivered")
            DispatchQueue.main.async { replies.fulfill() }
            wait(for: [replies], timeout: 1)
            XCTAssertTrue(events.input.isEmpty, "the Host answers terminal queries even while input is enabled")
            terminalInput(handle, false)
            view.paste(self)
            XCTAssertTrue(events.input.isEmpty)
        }

        func testOnlyANewSelectionOpensTheSelectionMenu() throws {
            let events = Events()
            let (handle, view) = try terminal(events)
            defer { terminalRelease(handle) }
            let window = NSWindow(contentRect: view.frame, styleMask: [], backing: .buffered, defer: false)
            window.isReleasedWhenClosed = false
            window.contentView = view
            XCTAssertTrue(window.makeFirstResponder(view))
            window.orderFront(nil)
            defer { window.close() }
            feed("selected text", into: handle)
            NotificationCenter.default.addObserver(
                events, selector: #selector(Events.menuBegan(_:)),
                name: NSMenu.didBeginTrackingNotification, object: nil
            )
            defer { NotificationCenter.default.removeObserver(events) }
            func click(_ type: NSEvent.EventType, count: Int, x: CGFloat = 10) throws -> NSEvent {
                try XCTUnwrap(NSEvent.mouseEvent(
                    with: type, location: view.convert(NSPoint(x: x, y: view.bounds.maxY - 5), to: nil),
                    modifierFlags: [], timestamp: 0, windowNumber: window.windowNumber,
                    context: nil, eventNumber: 0, clickCount: count, pressure: 0
                ))
            }
            try view.mouseDown(with: click(.leftMouseDown, count: 2))
            try view.mouseUp(with: click(.leftMouseUp, count: 2))
            XCTAssertEqual(events.menus, 0, "the next click can still select a line")
            let lineMenu = expectation(description: "the final line selection opens its native menu")
            events.menuReceived = lineMenu
            try view.mouseDown(with: click(.leftMouseDown, count: 3))
            try view.mouseUp(with: click(.leftMouseUp, count: 3))
            let line = try XCTUnwrap(view.getSelection())
            XCTAssertTrue(line.contains("selected text"))
            // Menu tracking begins asynchronously after the multi-click timer;
            // its notification, rather than a fixed extra frame, completes the action.
            wait(for: [lineMenu], timeout: NSEvent.doubleClickInterval + 1)
            events.menuReceived = nil
            XCTAssertEqual(events.menus, 1, "only the final line selection opens a menu")
            feed("\u{1b}[?1000h", into: handle)
            try view.mouseDown(with: click(.leftMouseDown, count: 1))
            feed("\r\nbackground output", into: handle)
            try view.mouseUp(with: click(.leftMouseUp, count: 1))
            RunLoop.main.run(until: Date().addingTimeInterval(NSEvent.doubleClickInterval + 0.05))
            XCTAssertEqual(
                events.menus,
                1,
                "a mouse-reporting click preserves the old selection without reopening its menu"
            )
            feed("\u{1b}[?1000l", into: handle)
            let repeatedMenu = expectation(description: "a repeated selection opens its native menu")
            events.menuReceived = repeatedMenu
            try view.mouseDown(with: click(.leftMouseDown, count: 1, x: 1))
            try view.mouseDragged(with: click(.leftMouseDragged, count: 1, x: 220))
            try view.mouseUp(with: click(.leftMouseUp, count: 1, x: 220))
            XCTAssertEqual(view.getSelection(), line, "the same text can be selected again")
            wait(for: [repeatedMenu], timeout: NSEvent.doubleClickInterval + 1)
            events.menuReceived = nil
            XCTAssertEqual(events.menus, 2, "reselecting identical text opens its actions again")
        }

        /// The Mac screen's text is read through SwiftTerm's internal terminal,
        /// which an upgrade could rename or replace.
        func testMacTerminalReadsItsVisibleRowsAsAccessibilityText() {
            let handle = terminalCreate(nil) { _, _, _, _, _, _ in }
            defer { terminalRelease(handle) }
            let view = Unmanaged<NSView>.fromOpaque(terminalView(handle)).takeUnretainedValue()
            view.frame = NSRect(x: 0, y: 0, width: 640, height: 240)
            let bytes = Array("first 日本語\r\nsecond\r\n".utf8)
            bytes.withUnsafeBufferPointer { terminalFeed(handle, $0.baseAddress, $0.count, 40, 6) }
            XCTAssertTrue(view.isAccessibilityElement())
            XCTAssertEqual(view.accessibilityRole(), .textArea)
            let deadline = Date().addingTimeInterval(5)
            while view.accessibilityValue() as? String != "first 日本語\nsecond", Date() < deadline {
                RunLoop.main.run(until: Date().addingTimeInterval(0.05))
            }
            XCTAssertEqual(view.accessibilityValue() as? String, "first 日本語\nsecond")
        }
    }
#endif
