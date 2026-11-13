import AppKit
@testable import BexNshNative
import XCTest

final class ConversationTests: XCTestCase {
    @MainActor func testNativeCodeAndTableKeepAccessibleIconActionsAndWrappingState() throws {
        var document = MessageDocument.empty
        document.labels = ["copyCode":"Copy code", "copyTable":"Copy table", "wrap":"Wrap", "unwrap":"Unwrap"]
        let body = TranscriptBody()
        body.update([.code(.init(label:"swift", source:"let x = 1", blocks:[paragraph("let x = 1")], diagram:nil)),
                     .table(.init(source:"| a |", csv:"a", rows:[[[.text([paragraph("a")])]]]))], document:document, media:[:])
        let buttons = controls(body)
        for title in ["Copy code","Copy table","Unwrap"] {
            let button = try XCTUnwrap(buttons.first{$0.accessibilityLabel() == title},title)
            XCTAssertEqual(button.imagePosition,.imageOnly)
            XCTAssertNotNil(button.image)
        }
        let wrap = try XCTUnwrap(buttons.first{$0.title == "Unwrap"})
        wrap.performClick(nil)
        XCTAssertEqual(wrap.accessibilityLabel(),"Wrap")
        XCTAssertEqual(wrap.image?.accessibilityDescription,"Wrap")
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
        try view.update(update(scope: "two", text: "Different conversation"))
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
        view.stop()
    }

    private func paragraph(_ text: String) -> MessageParagraph {
        MessageParagraph(header: nil, marker: nil, code: false, quoted: false, depth: 0, alignment: "left",
                         runs: [MessageRun(text: text, strong: false, emphasis: false, strikethrough: false,
                                           code: false, link: nil, darkColor: nil, lightColor: nil)])
    }

    private func update(scope: String, text: String) throws -> NativeConversationUpdate {
        let row: [String: Any] = [
            "id": "same-row-id", "user": false, "gap": 16, "buttons": [], "images": [], "media": [:],
            "queued": false, "steer": false,
            "parts": [["Text": [["header": NSNull(), "marker": NSNull(), "code": false, "quoted": false,
                                 "depth": 0, "alignment": "left", "runs": [["text": text, "strong": false,
                                                                            "emphasis": false, "strikethrough": false,
                                                                            "code": false]]]]]],
            "document": ["blocks": [], "secondary": false, "role": "assistant", "target": scope,
                         "sideChat": true, "maxWidth": 700, "menu": ["quote": "Quote", "sideChat": "Side chat"]]
        ]
        let data = try JSONSerialization.data(withJSONObject: ["scope": scope, "reset": true, "order": ["same-row-id"],
                                                               "changed": [row], "removed": [], "following": true,
                                                               "inset": 0, "latest": "Latest"])
        return try JSONDecoder().decode(NativeConversationUpdate.self, from: data)
    }

    @MainActor private func texts(_ view: NSView) -> [NSTextView] {
        (view as? NSTextView).map { [$0] } ?? view.subviews.flatMap(texts)
    }

    @MainActor private func controls(_ view: NSView) -> [NSButton] {
        (view as? NSButton).map{[$0]} ?? view.subviews.flatMap(controls)
    }

    @MainActor private func settle() async {
        await withCheckedContinuation { continuation in DispatchQueue.main.async { continuation.resume() } }
    }
}
