import AppKit
@testable import BexNshNative
import CoreText
import XCTest

final class MessageTests: XCTestCase {
    private struct Fixture: Decodable {
        let blocks: [MessageParagraph]
        let secondary: Bool
        let target: String
        let sideChat: Bool
    }

    @MainActor func testConversationActionsUseAvailableSystemSymbols() {
        for name in [
            "copy",
            "agent",
            "terminal", "eye", "zap", "alert", "stop", "compact", "tools", "thinking", "globe", "tool",
            "edit",
            "fork",
            "more",
            "wrap",
            "check",
            "chevronDown",
            "chevronRight",
            "quote",
            "explain",
            "sideChat"
        ] {
            let image = MessageAttributes.icon(name, size: 12)
            XCTAssertNotNil(image, name)
        }
    }

    @MainActor func testWebLinkMenuReportsExplicitDestinationAndOriginalUrl() throws {
        let url = "https://example.com/path?q=1#section"
        let json = """
        {"text":"Read this", "strong":false, "emphasis":false, "strikethrough":false,
         "code":false, "link":"\(url)", "webMenu":[
           {"id":"systemBrowser", "title":"Open in default browser"},
           {"id":"inAppBrowser", "title":"Open in Bex"},
           {"id":"copyLink", "title":"Copy link"}]}
        """
        let run = try JSONDecoder().decode(MessageRun.self, from: Data(json.utf8))
        let block = MessageParagraph(header: nil, marker: nil, code: false, quoted: false,
                                     depth: 0, alignment: "left", runs: [run])
        let text = MessageAttributes.text([block], secondary: false, dark: true)
        let link = try XCTUnwrap(text.attribute(.messageWebLink, at: 0, effectiveRange: nil) as? MessageWebLink)
        let view = MessageTextView()
        var actions: [[String: Any]] = []
        view.action = { actions.append($0) }
        let menu = view.fileMenu(link.menu, link: link.link, action: "webLink")
        XCTAssertEqual(menu.items.map(\.title), ["Open in default browser", "Open in Bex", "Copy link"])
        for (index, id) in ["systemBrowser", "inAppBrowser", "copyLink"].enumerated() {
            let item = menu.items[index]
            _ = item.target?.perform(item.action, with: item)
            XCTAssertEqual(actions.last?["action"] as? String, "webLink")
            XCTAssertEqual(actions.last?["id"] as? String, id)
            XCTAssertEqual(actions.last?["text"] as? String, url)
        }
    }

    @MainActor func testFileChipKeepsItsIconAccentAndLinkWithoutCodeStyling() throws {
        let url = try XCTUnwrap(Bundle.module.url(forResource: "file-chip", withExtension: "json"))
        let block = try JSONDecoder().decode(MessageParagraph.self, from: Data(contentsOf: url))
        for dark in [true, false] {
            let content = MessageAttributes.text([block], secondary: false, dark: dark)
            let label = (content.string as NSString).range(of: "統合前レビュー").location
            let attachment = try XCTUnwrap(content.attribute(
                .attachment,
                at: 0,
                effectiveRange: nil
            ) as? NSTextAttachment)
            XCTAssertNotNil(attachment.image, "the file's PNG is drawn on both Apple platforms")
            let font = try XCTUnwrap(content.attribute(.font, at: label, effectiveRange: nil) as? MessageFont)
            #if os(macOS)
                XCTAssertFalse(font.isFixedPitch)
            #else
                XCTAssertFalse(font.fontDescriptor.symbolicTraits.contains(.traitMonoSpace))
            #endif
            XCTAssertNil(content.attribute(.backgroundColor, at: label, effectiveRange: nil))
            XCTAssertNil(content.attribute(.underlineStyle, at: label, effectiveRange: nil))
            let color = try XCTUnwrap(content.attribute(
                .foregroundColor,
                at: label,
                effectiveRange: nil
            ) as? MessageColor)
            let expected = dark ? MessageColor(red: 1, green: 0, blue: 0, alpha: 1)
                : MessageColor(red: 0, green: 0, blue: 1, alpha: 1)
            XCTAssertEqual(color, expected)
            let link = try XCTUnwrap(content.attribute(.link, at: label, effectiveRange: nil) as? URL)
            XCTAssertEqual(link.relativeString, "/repo/SKILL.md")
            XCTAssertEqual(MessageAttributes.selectedText(content, range: NSRange(location: 0, length: content.length)),
                           "統合前レビュー")
        }
    }

    @MainActor func testFileChipMenuShowsTheSharedActions() throws {
        let url = try XCTUnwrap(Bundle.module.url(forResource: "file-chip", withExtension: "json"))
        let block = try JSONDecoder().decode(MessageParagraph.self, from: Data(contentsOf: url))
        let file = try XCTUnwrap(block.runs.first?.file)
        let menu = MessageTextView().fileMenu(file.menu, link: "/repo/SKILL.md")
        XCTAssertEqual(menu.items.map(\.title),
                       ["相対パスをコピー", "フルパスをコピー"])
    }

    @MainActor func testCoreHighlightByteRangesPreserveUnicodeAndStyles() throws {
        let data = Data(#"{"blocks":[{"header":null,"marker":null,"code":false,"quoted":false,"depth":0,"alignment":"left","runs":[{"text":"😀é ","strong":false,"emphasis":false,"strikethrough":false,"code":false,"link":null,"darkColor":null,"lightColor":null,"highlights":[{"start":4,"end":6,"active":true}]},{"text":"bold","strong":true,"emphasis":false,"strikethrough":false,"code":false,"link":null,"darkColor":null,"lightColor":null,"highlights":[{"start":0,"end":4,"active":false}]}]}],"secondary":false,"role":"assistant","target":"","sideChat":false,"maxWidth":500,"menu":{"quote":"","sideChat":""}}"#
            .utf8)
        let document = try JSONDecoder().decode(Fixture.self, from: data)
        let content = MessageAttributes.text(document.blocks, secondary: false, dark: true)
        XCTAssertEqual(content.string, "😀é bold")
        let accented = (content.string as NSString).range(of: "é").location
        XCTAssertNotNil(content.attribute(.backgroundColor, at: accented, effectiveRange: nil))
        XCTAssertEqual(content.attribute(.messageFindActive, at: accented, effectiveRange: nil) as? Bool, true)
        XCTAssertNil(content.attribute(.messageFindActive, at: 0, effectiveRange: nil))
        let bold = (content.string as NSString).range(of: "bold").location
        XCTAssertNotNil(content.attribute(.backgroundColor, at: bold, effectiveRange: nil))
        XCTAssertNil(content.attribute(.messageFindActive, at: bold, effectiveRange: nil))
        let font = try XCTUnwrap(content.attribute(.font, at: bold, effectiveRange: nil) as? NSFont)
        XCTAssertTrue(NSFontManager.shared.traits(of: font).contains(.boldFontMask))
    }

    @MainActor func testItemsOfOneListSitCloserThanTwoLists() {
        let item = { (marker: String, text: String) in
            MessageParagraph(header: nil, marker: marker, code: false, quoted: false, depth: 1,
                             alignment: "left",
                             runs: [MessageRun(text: text, strong: false, emphasis: false, strikethrough: false,
                                               code: false, link: nil, darkColor: nil, lightColor: nil)])
        }
        let blocks = [item("1.", "first"), item("2.", "second"), item("•", "bullet")]
        let text = MessageAttributes.text(blocks, secondary: false, dark: true)
        let spacing = { (word: String) -> CGFloat in
            let at = (text.string as NSString).range(of: word).location
            let style = text.attribute(.paragraphStyle, at: at, effectiveRange: nil) as? NSParagraphStyle
            return style?.paragraphSpacing ?? -1
        }
        // T3 Code's `li` 0.25rem apart; its lists 0.65rem.
        XCTAssertEqual(spacing("first"), 4)
        XCTAssertEqual(spacing("second"), 10.4, accuracy: 0.01)
    }

    @MainActor func testTableCellsUseT3CodesSmallerTextTheirHeaderSemibold() throws {
        let run = MessageRun(text: "構成", strong: false, emphasis: false, strikethrough: false, code: false,
                             link: nil, darkColor: nil, lightColor: nil)
        let block = MessageParagraph(header: nil, marker: nil, code: false, quoted: false,
                                     depth: 0, alignment: "left", runs: [run])
        for (cell, bold) in [("header", true), ("body", false)] {
            let text = MessageAttributes.text([block], secondary: false, dark: true, answer: true, cell: cell)
            let font = try XCTUnwrap(text.attribute(.font, at: 0, effectiveRange: nil) as? NSFont)
            // `.chat-markdown table`: 0.75rem, its `thead th` weight 600.
            XCTAssertEqual(font.pointSize, 12, cell)
            let traits = font.fontDescriptor.object(forKey: .traits) as? [NSFontDescriptor.TraitKey: Any]
            let weight = traits?[.weight] as? CGFloat ?? 0
            XCTAssertEqual(weight >= NSFont.Weight.semibold.rawValue, bold, cell)
        }
    }

    @MainActor func testNativeTextPreservesTheSharedContentAndOpaqueActionTarget() throws {
        let url = try XCTUnwrap(Bundle.module.url(forResource: "message", withExtension: "json"))
        let document = try JSONDecoder().decode(Fixture.self, from: Data(contentsOf: url))
        let attributed = MessageAttributes.text(document.blocks, secondary: document.secondary, dark: true)
        XCTAssertEqual(attributed.string, "日本語 👋 ファイル")
        XCTAssertEqual(document.target, "opaque-native-menu-target")
        XCTAssertTrue(document.sideChat)
        XCTAssertEqual(document.blocks[0].header, 2)
        let position = (attributed.string as NSString).range(of: "ファイル").location
        let link = attributed.attribute(.link, at: position, effectiveRange: nil) as? URL
        XCTAssertEqual(link?.absoluteString, "file:///fixture/%E6%97%A5%E6%9C%AC%E8%AA%9E.txt")
        #if os(macOS)
            let font = try XCTUnwrap(attributed.attribute(.font, at: 0, effectiveRange: nil) as? NSFont)
            // T3 Code's `h2`: 1.125rem.
            XCTAssertEqual(font.pointSize, 18)
            XCTAssertTrue(NSFontManager.shared.traits(of: font).contains(.boldFontMask))
        #endif
    }
}
