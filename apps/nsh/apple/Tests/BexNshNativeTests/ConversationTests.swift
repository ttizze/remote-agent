#if os(macOS) && DEBUG
    import AppKit
    @testable import BexNshNative
    import XCTest

    @MainActor final class ConversationTests: XCTestCase {
        private func row(_ id: String, _ content: String) -> NativeConversationRow {
            let run = MessageRun(text: content, strong: false, emphasis: false, strikethrough: false,
                                 code: false, link: nil, darkColor: nil, lightColor: nil)
            let block = MessageParagraph(header: nil, marker: nil, code: false, quoted: false, depth: 0,
                                         alignment: "left", runs: [run])
            return NativeConversationRow(id: id, user: false, gap: 16, buttons: [], document: MessageDocument(
                blocks: [block], secondary: false, role: "assistant", target: "", sideChat: false,
                maxWidth: 700, menu: MessageMenuTitles(quote: "Quote", sideChat: "Side chat")
            ))
        }

        func testStreamingPreservesTheSelectedTextAndNativeTextView() {
            let cell = NativeConversationCell(frame: NSRect(x: 0, y: 0, width: 600, height: 120))
            cell.update(row("same", "Reading this paragraph"), width: 550, send: { _ in })
            let text = cell.text
            text.setSelectedRange(NSRange(location: 8, length: 4))
            cell.update(row("same", "Reading this paragraph\nMore arrives"), width: 550, send: { _ in })
            XCTAssertTrue(cell.text === text)
            XCTAssertEqual(text.selectedRange(), NSRange(location: 8, length: 4))
            XCTAssertEqual((text.string as NSString).substring(with: text.selectedRange()), "this")
            cell.update(row("other", "Reading this paragraph"), width: 550, send: { _ in })
            XCTAssertEqual(text.selectedRange().length, 0)
            text.setSelectedRange(NSRange(location: 8, length: 4))
            cell.update(row("other", "Reading this paragraph"), width: 550, scope: "another", send: { _ in })
            XCTAssertEqual(text.selectedRange().length, 0)
        }

        func testTextKitUsesNativeScrollingGeometryAndResizesWithoutNSHMeasurements() {
            let cell = NativeConversationCell(frame: NSRect(x: 0, y: 0, width: 600, height: 100))
            let content = String(repeating: "A long paragraph keeps its complete selectable text. ", count: 100)
            let message = row("long", content)
            cell.update(message, width: 550, send: { _ in XCTFail("row measurement must stay inside AppKit") })
            let wide = cell.fittingSize.height
            cell.update(message, width: 200, send: { _ in XCTFail("row measurement must stay inside AppKit") })
            XCTAssertTrue(cell.text.nativeScrolling)
            XCTAssertEqual(cell.text.string, content)
            XCTAssertGreaterThan(cell.fittingSize.height, wide)
        }

        func testNativeTableAcceptsHistoryPrependAndAnIndependentConversation() {
            _ = NSApplication.shared
            let view = NativeConversationView(frame: NSRect(x: 0, y: 0, width: 700, height: 500))
            let window = NSWindow(contentRect: view.frame, styleMask: [.titled], backing: .buffered, defer: false)
            window.isReleasedWhenClosed = false
            window.contentView = view
            defer { view.stop(); window.close() }
            var events: [[String: Any]] = []
            view.send = { events.append($0) }
            let messages = (0..<100).map { row("\($0)", String(repeating: "Message \($0). ", count: 40)) }
            apply(view, scope: "first", order: messages.map(\.id), changed: messages, reset: true, following: true)
            XCTAssertEqual(view.scroll.verticalScroller?.doubleValue ?? 0, 1, accuracy: 0.001)
            view.scroll.endInteraction?()
            let position = events.last!
            XCTAssertEqual((position["content"] as! CGFloat) - (position["offset"] as! CGFloat)
                - (position["viewport"] as! CGFloat), 0, accuracy: 0.5)
            events.removeAll()
            apply(view, scope: "first", order: messages.map(\.id), changed: messages, reset: true)
            XCTAssertEqual(view.table.numberOfRows, 100)
            let children = view.table.accessibilityChildren() as? [NativeConversationCell]
            XCTAssertFalse(children?.isEmpty ?? true)
            XCTAssertTrue(children?.contains { !$0.text.string.isEmpty } ?? false)
            view.command(["name": "reveal", "args": ["row": 99]])
            let previousHeight = view.table.rect(ofRow: 0).height
            let grown = row("0", String(repeating: "An offscreen answer continues streaming. ", count: 300))
            apply(view, scope: "first", order: messages.map(\.id), changed: [grown], reset: false)
            XCTAssertGreaterThan(view.table.rect(ofRow: 0).height, previousHeight)
            view.command(["name": "reveal", "args": ["row": 0]])
            let grownCell = view.table.view(atColumn: 0, row: 0, makeIfNecessary: true) as! NativeConversationCell
            XCTAssertEqual(view.table.rect(ofRow: 0).height, grownCell.fittingSize.height, accuracy: 1)
            view.command(["name": "reveal", "args": ["row": 50]])
            let before = view.scroll.documentVisibleRect.minY - view.table.rect(ofRow: 50).minY
            let earlier = row("earlier", "Earlier history")
            apply(view, scope: "first", order: [earlier.id] + messages.map(\.id), changed: [earlier], reset: false)
            XCTAssertEqual(view.table.numberOfRows, 101)
            XCTAssertEqual(view.scroll.documentVisibleRect.minY - view.table.rect(ofRow: 51).minY, before, accuracy: 1)
            XCTAssertFalse(events.contains { $0["action"] as? String == "scroll" }, "layout changes are not user scrolling")
            apply(view, scope: "second", order: ["new"], changed: [row("new", "Another conversation")], reset: true)
            XCTAssertEqual(view.table.numberOfRows, 1)
            let cell = view.table.view(atColumn: 0, row: 0, makeIfNecessary: true) as? NativeConversationCell
            XCTAssertEqual(cell?.text.string, "Another conversation")
        }

        private func apply(_ view: NativeConversationView, scope: String, order: [String],
                           changed: [NativeConversationRow], reset: Bool, following: Bool = false) {
            view.update(NativeConversationUpdate(scope: scope, reset: reset, order: order, changed: changed,
                                                 removed: [], following: following, inset: 60, history: nil,
                                                 notice: nil, latest: "Latest"))
            let deadline = Date().addingTimeInterval(3)
            while view.table.numberOfRows != order.count || view.table.isHidden, Date() < deadline {
                RunLoop.current.run(until: Date().addingTimeInterval(0.005))
            }
            view.layoutSubtreeIfNeeded()
        }
    }
#endif
