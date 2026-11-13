import AppKit
@testable import BexNshNative
import XCTest

final class ConversationTests: XCTestCase {
    @MainActor func testPreparationKeepsNativeStepStateTimingOutputDetailsAndFailure() throws {
        let setup = try JSONDecoder().decode(
            ConversationPreparation.self,
            from: Data(#"{"steps":[{"id":"install","label":"Install dependencies","detail":"installing","tail":"npm install\nready","icon":"running","dim":false,"running":true,"time":"2 seconds"}],"details":[{"label":"Workspace","value":"/repo/worktree"}],"error":"Setup failed"}"#
                .utf8)
        )
        let view = NativeConversationCell(frame: NSRect(x: 0, y: 0, width: 700, height: 400))
        let row = NativeConversationRow(
            id: "setup",
            user: false,
            gap: 16,
            buttons: [],
            document: .empty,
            parts: [.text([.plain("Setting up worktree")])],
            images: [],
            media: [:],
            time: "3 seconds",
            queued: false,
            steer: false,
            timeAlways: true,
            preparation: setup
        )
        view.update(row, width: 700, send: { _ in })
        func fields(_ view: NSView) -> [String] {
            (view as? NSTextField).map { [$0.stringValue] } ?? view.subviews.flatMap(fields)
        }
        let displayed = fields(view)
        for value in ["Install dependencies", "installing", "2 seconds", "3 seconds", "Setup failed"] {
            XCTAssertTrue(displayed.contains(value), value)
        }
        let native = texts(view).map(\.string)
        XCTAssertTrue(native.contains("npm install\nready"))
        XCTAssertTrue(native.contains("Workspace  /repo/worktree"))
        XCTAssertGreaterThan(view.fittingSize.height, 100)
    }

    @MainActor func testResizingTheViewportKeepsVisibleNativeRowsAtTheEnd() async throws {
        let view = NativeConversationView(frame: NSRect(x: 0, y: 0, width: 1360, height: 720))
        let window = NSWindow(contentRect: view.frame, styleMask: .borderless, backing: .buffered, defer: false)
        window.contentView = view
        window.orderFront(nil)
        defer { view.stop(); window.orderOut(nil) }
        try view.update(update(
            scope: "resize",
            text: String(repeating: "visible native message ", count: 20),
            count: 40
        ))
        await settle()
        for size in [NSSize(width: 660, height: 530), NSSize(width: 1360, height: 720)] {
            window.setContentSize(size)
            view.needsLayout = true
            view.layoutSubtreeIfNeeded()
            await settle()
            XCTAssertEqual(view.table.numberOfRows, 40)
            let visible = view.table.rows(in: view.scroll.documentVisibleRect)
            XCTAssertNotEqual(visible.location, NSNotFound)
            XCTAssertGreaterThan(
                visible.length,
                0,
                "\(size) table=\(view.table.frame) bounds=\(view.table.bounds) visible=\(view.scroll.documentVisibleRect) last=\(view.table.rect(ofRow: 39))"
            )
            for index in visible.location ..< NSMaxRange(visible) {
                XCTAssertNotNil(view.table.view(atColumn: 0, row: index, makeIfNecessary: false), "row \(index)")
            }
        }
    }

    @MainActor func testNativeCopyFeedbackResetsWhenItsCellChangesMessages() throws {
        let view = NativeConversationCell(frame: NSRect(x: 0, y: 0, width: 700, height: 200))
        var document = MessageDocument.empty; document.labels = ["copied": "Copied"]
        func row(_ id: String) -> NativeConversationRow {
            NativeConversationRow(
                id: id,
                user: true,
                gap: 16,
                buttons: [.init(action: "copy", title: "Copy message", icon: "copy", iconOnly: true)],
                document: document,
                parts: [.text([paragraph("original **source**")])],
                images: [],
                media: [:],
                time: nil,
                queued: false,
                steer: false
            )
        }
        var actions: [[String: Any]] = []
        view.update(row("first"), width: 700, scope: "thread", send: { actions.append($0) })
        let copy = try XCTUnwrap(controls(view).first { $0.title == "Copy message" })
        copy.performClick(nil)
        XCTAssertEqual(actions.last?["action"] as? String, "copy")
        XCTAssertEqual(copy.accessibilityLabel(), "Copied")
        XCTAssertEqual(copy.imagePosition, .imageOnly)
        view.update(row("second"), width: 700, scope: "thread", send: { actions.append($0) })
        XCTAssertEqual(copy.accessibilityLabel(), "Copy message")
        XCTAssertEqual(copy.image?.accessibilityDescription, "Copy message")
    }

    @MainActor func testNativeCodeAndTableKeepAccessibleIconActionsAndWrappingState() throws {
        var document = MessageDocument.empty
        document.labels = ["copyCode": "Copy code", "copyTable": "Copy table", "wrap": "Wrap", "unwrap": "Unwrap"]
        let body = TranscriptBody()
        body.update([.code(.init(label: "swift", source: "let x = 1", blocks: [paragraph("let x = 1")], diagram: nil)),
                     .table(.init(source: "| a |", csv: "a", rows: [[[.text([paragraph("a")])]]]))], document: document,
                    media: [:])
        let buttons = controls(body)
        for title in ["Copy code", "Copy table", "Unwrap"] {
            let button = try XCTUnwrap(buttons.first { $0.accessibilityLabel() == title }, title)
            XCTAssertEqual(button.imagePosition, .imageOnly)
            XCTAssertNotNil(button.image)
        }
        let wrap = try XCTUnwrap(buttons.first { $0.title == "Unwrap" })
        wrap.performClick(nil)
        XCTAssertEqual(wrap.accessibilityLabel(), "Wrap")
        XCTAssertEqual(wrap.image?.accessibilityDescription, "Wrap")
    }

    @MainActor func testStreamingReusesTheNativeTextAndPreservesOnlyUnchangedSelection() throws {
        let document = MessageDocument.empty
        let body = TranscriptBody()
        body.update([.text([paragraph("日本語 Alpha")])], document: document, media: [:])
        let text = try XCTUnwrap((body.subviews.first as? NativeParagraph)?.text)
        let range = (text.string as NSString).range(of: "Alpha")
        text.setSelectedRange(range)
        body.update([.text([paragraph("日本語 Alpha Bravo")])], document: document, media: [:])
        XCTAssertTrue((body.subviews.first as? NativeParagraph)?.text === text)
        XCTAssertEqual(text.selectedRange(), range)
        body.update([.text([paragraph("日本語 Other Bravo")])], document: document, media: [:])
        XCTAssertEqual(text.selectedRange().length, 0)
    }

    @MainActor func testNativeViewportClearsRowsAndSelectionWhenTheConversationChanges() async throws {
        let view = NativeConversationView(frame: NSRect(x: 0, y: 0, width: 800, height: 400))
        try view.update(update(scope: "one", text: "Original conversation"))
        await settle()
        XCTAssertEqual(view.table.numberOfRows, 1)
        let first = try XCTUnwrap(view.table.view(atColumn: 0, row: 0, makeIfNecessary: true))
        XCTAssertTrue(texts(first).contains { $0.string == "Original conversation" })
        let selected = try XCTUnwrap(texts(first).first)
        selected.setSelectedRange((selected.string as NSString).range(of: "conversation"))
        try view.update(update(scope: "two", text: "Original conversation"))
        await settle()
        let identical = try XCTUnwrap(view.table.view(atColumn: 0, row: 0, makeIfNecessary: true))
        XCTAssertTrue(texts(identical).allSatisfy { $0.selectedRange().length == 0 })
        try view.update(update(scope: "three", text: "Different conversation"))
        await settle()
        let next = try XCTUnwrap(view.table.view(atColumn: 0, row: 0, makeIfNecessary: true))
        XCTAssertTrue(texts(next).contains { $0.string == "Different conversation" })
        XCTAssertFalse(texts(next).contains { $0.string.contains("Original") || $0.selectedRange().length > 0 })
        view.stop()
    }

    @MainActor func testNativeSelectionOffersTheCapturedTextAndTarget() async throws {
        let view = NativeConversationView(frame: NSRect(x: 0, y: 0, width: 800, height: 400))
        let window = NSWindow(contentRect: view.frame, styleMask: .borderless, backing: .buffered, defer: false)
        window.contentView = view
        var actions: [[String: Any]] = []
        view.send = { actions.append($0) }
        try view.update(update(scope: "selected-conversation", text: "Needle Alpha Bravo"))
        await settle()
        let cell = try XCTUnwrap(view.table.view(atColumn: 0, row: 0, makeIfNecessary: true))
        let text = try XCTUnwrap(texts(cell).first)
        window.makeFirstResponder(text)
        text.setSelectedRange((text.string as NSString).range(of: "Alpha"))
        let toolbar = try XCTUnwrap(view.subviews.compactMap { $0 as? NSStackView }.first)
        XCTAssertFalse(toolbar.isHidden)
        let quote = try XCTUnwrap(toolbar.arrangedSubviews.compactMap { $0 as? NSButton }.first { $0.title == "Quote" })
        quote.performClick(nil)
        let action = try XCTUnwrap(actions.last { $0["action"] as? String == "quote" })
        XCTAssertEqual(action["text"] as? String, "Alpha")
        XCTAssertEqual(action["target"] as? String, "selected-conversation")
        XCTAssertTrue(toolbar.isHidden)
        // Scrolling dismisses the actions while preserving the actual text
        // selection. A publication refresh must not manufacture a new selection.
        text.setSelectedRange((text.string as NSString).range(of: "Bravo"))
        XCTAssertFalse(toolbar.isHidden)
        view.scroll.beginInteraction?(); view.scroll.endInteraction?()
        XCTAssertTrue(toolbar.isHidden)
        view.update(NativeConversationUpdate(
            scope: "selected-conversation",
            reset: false,
            order: nil,
            changed: [],
            removed: [],
            following: false,
            inset: 0,
            history: nil,
            notice: nil,
            latest: "Latest"
        ))
        await settle()
        XCTAssertEqual(text.selectedRange(), (text.string as NSString).range(of: "Bravo"))
        XCTAssertTrue(toolbar.isHidden)
        view.stop()
    }

    private func paragraph(_ text: String) -> MessageParagraph {
        MessageParagraph(header: nil, marker: nil, code: false, quoted: false, depth: 0, alignment: "left",
                         runs: [MessageRun(text: text, strong: false, emphasis: false, strikethrough: false,
                                           code: false, link: nil, darkColor: nil, lightColor: nil)])
    }

    private func update(scope: String, text: String, count: Int = 1) throws -> NativeConversationUpdate {
        let row: [String: Any] = [
            "id": "same-row-id", "user": false, "gap": 16, "buttons": [], "images": [], "media": [:],
            "queued": false, "steer": false,
            "parts": [["Text": [["header": NSNull(), "marker": NSNull(), "code": false, "quoted": false,
                                 "depth": 0, "alignment": "left", "runs": [["text": text, "strong": false,
                                                                            "emphasis": false, "strikethrough": false,
                                                                            "code": false]]]]]],
            "document": ["secondary": false, "role": "assistant", "target": scope,
                         "sideChat": true, "menu": ["quote": "Quote", "sideChat": "Side chat"]]
        ]
        let rows = (0 ..< count).map { index in
            var next = row; next["id"] = "same-row-id-\(index)"; return next
        }
        let data = try JSONSerialization.data(withJSONObject: [
            "scope": scope,
            "reset": true,
            "order": rows.map { $0["id"]! },
            "changed": rows,
            "removed": [],
            "following": true,
            "inset": 0,
            "latest": "Latest"
        ])
        return try JSONDecoder().decode(NativeConversationUpdate.self, from: data)
    }

    @MainActor private func texts(_ view: NSView) -> [NSTextView] {
        (view as? NSTextView).map { [$0] } ?? view.subviews.flatMap(texts)
    }

    @MainActor private func controls(_ view: NSView) -> [NSButton] {
        (view as? NSButton).map { [$0] } ?? view.subviews.flatMap(controls)
    }

    @MainActor private func settle() async {
        await withCheckedContinuation { continuation in DispatchQueue.main.async { continuation.resume() } }
    }
}
