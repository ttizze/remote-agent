@testable import BexNshNative
import XCTest
#if os(macOS)
    import AppKit
    import CoreText
#else
    import UIKit
#endif

final class MessageTests: XCTestCase {
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

    #if os(iOS)
        @MainActor func testFileChipMenuShowsTheSharedActions() throws {
            let url = try XCTUnwrap(Bundle.module.url(forResource: "file-chip", withExtension: "json"))
            let block = try JSONDecoder().decode(MessageParagraph.self, from: Data(contentsOf: url))
            let file = try XCTUnwrap(block.runs.first?.file)
            let menu = MessageTextView().fileMenu(file.menu, link: "/repo/SKILL.md")
            XCTAssertEqual(menu.children.compactMap { ($0 as? UIAction)?.title },
                           ["相対パスをコピー", "フルパスをコピー"])
        }

        @MainActor func testFileChipFontAndIconFollowThePreferredTextSize() throws {
            let url = try XCTUnwrap(Bundle.module.url(forResource: "file-chip", withExtension: "json"))
            let block = try JSONDecoder().decode(MessageParagraph.self, from: Data(contentsOf: url))
            UITraitCollection(preferredContentSizeCategory: .accessibilityExtraExtraExtraLarge).performAsCurrent {
                let content = MessageAttributes.text([block], secondary: false, dark: true)
                let label = (content.string as NSString).range(of: "統合前レビュー").location
                let font = content.attribute(.font, at: label, effectiveRange: nil) as? UIFont
                let expected = UIFontMetrics.default.scaledFont(for: .systemFont(ofSize: 18, weight: .medium))
                XCTAssertEqual(font?.pointSize, expected.pointSize)
                let icon = content.attribute(.attachment, at: 0, effectiveRange: nil) as? NSTextAttachment
                XCTAssertEqual(icon?.bounds.width, expected.pointSize)
            }
        }
    #endif

    #if os(macOS)
        @MainActor func testCoreHighlightByteRangesPreserveUnicodeAndStyles() throws {
            let data = Data(#"{"blocks":[{"header":null,"marker":null,"code":false,"quoted":false,"depth":0,"alignment":"left","runs":[{"text":"😀é ","strong":false,"emphasis":false,"strikethrough":false,"code":false,"link":null,"darkColor":null,"lightColor":null,"highlights":[{"start":4,"end":6,"active":true}]},{"text":"bold","strong":true,"emphasis":false,"strikethrough":false,"code":false,"link":null,"darkColor":null,"lightColor":null,"highlights":[{"start":0,"end":4,"active":false}]}]}],"secondary":false,"role":"assistant","target":"","sideChat":false,"maxWidth":500,"menu":{"quote":"","sideChat":""}}"#
                .utf8)
            let document = try JSONDecoder().decode(MessageDocument.self, from: data)
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

        @MainActor func testFindReportsTheBoundsOfAnOffscreenUnwrappedMatch() throws {
            let prefix = String(repeating: "0123456789", count: 60)
            let needle = "code-overflow-target"
            var run = MessageRun(text: prefix + needle, strong: false, emphasis: false,
                                 strikethrough: false, code: false, link: nil, darkColor: nil, lightColor: nil)
            run.highlights = [MessageHighlight(start: prefix.utf8.count,
                                               end: (prefix + needle).utf8.count, active: true)]
            let block = MessageParagraph(header: nil, marker: nil, code: true, quoted: false,
                                         depth: 0, alignment: "left", runs: [run])
            var document = MessageDocument(blocks: [block], secondary: false, role: "assistant",
                                           target: "fixture", sideChat: false, maxWidth: 1_000_000,
                                           menu: MessageMenuTitles(quote: "", sideChat: ""))
            document.findKey = "overflow"
            let view = MessageTextContainer()
            var reports: [[CGFloat]] = []
            view.send = {
                if let key = $0["findKey"] as? String, key.hasPrefix("overflow"),
                   let bounds = $0["findBounds"] as? [CGFloat] {
                    reports.append(bounds)
                }
            }
            view.update(document)
            view.frame = NSRect(x: 0, y: 0, width: 200, height: 100)
            view.layout()
            let bounds = try XCTUnwrap(reports.last)
            XCTAssertEqual(bounds.count, 4)
            XCTAssertGreaterThan(bounds[0], 200, "the match starts beyond the visible code viewport")
            XCTAssertGreaterThan(bounds[2], 0)
            XCTAssertGreaterThan(bounds[3], 0)
            view.layout()
            XCTAssertEqual(reports.count, 1, "one geometry report per selected occurrence")
            document.findKey = "overflow-next"
            view.update(document)
            view.layout()
            XCTAssertEqual(reports.count, 2, "navigating to the same match requests fresh geometry")
        }

        @MainActor func testTextKitVisibilityFollowsLayerScrollingBeforeAppKitBoundsCommit() {
            let window = NSWindow(contentRect: CGRect(x: 0, y: 0, width: 400, height: 200),
                                  styleMask: [], backing: .buffered, defer: false)
            window.isReleasedWhenClosed = false
            defer { window.close() }
            let clip = NSView(frame: CGRect(x: 0, y: 0, width: 400, height: 200))
            clip.wantsLayer = true
            clip.layer?.masksToBounds = true
            window.contentView = clip
            let content = NSView(frame: CGRect(x: 0, y: 0, width: 400, height: 1000))
            content.wantsLayer = true
            clip.addSubview(content)
            let text = MessageTextView()
            text.frame = CGRect(x: 0, y: 600, width: 400, height: 40)
            content.addSubview(text)
            window.orderFront(nil)
            window.displayIfNeeded()
            text.watchScrollingGeometry()
            text.layer?.displayIfNeeded()
            XCTAssertEqual(text.layer?.needsDisplay(), false)
            XCTAssertTrue(text.visibleRect.isEmpty)
            let committed = content.bounds
            content.layer?.sublayerTransform = CATransform3DMakeTranslation(0, -600, 0)
            XCTAssertEqual(content.bounds, committed)
            XCTAssertEqual(text.visibleRect.height, 40, "glyphs must draw before the deferred NSView bounds commit")
            XCTAssertEqual(
                text.layer?.needsDisplay(),
                true,
                "layer scrolling must invalidate newly visible native glyphs"
            )
            text.layer?.displayIfNeeded()
            content.layer?.sublayerTransform = CATransform3DMakeTranslation(0, -590, 0)
            XCTAssertEqual(text.visibleRect.height, 40)
            XCTAssertEqual(text.layer?.needsDisplay(), false, "moving a fully painted visible cell needs no redraw")
            content.layer?.sublayerTransform = CATransform3DMakeTranslation(0, -200, 0)
            XCTAssertTrue(text.visibleRect.isEmpty)
            XCTAssertEqual(text.layer?.needsDisplay(), false, "an offscreen cell must not be invalidated")
        }

        @MainActor func testPrependingRowsAndCommittingBoundsRedrawRetainedText() {
            let window = NSWindow(contentRect: CGRect(x: 0, y: 0, width: 400, height: 200),
                                  styleMask: [], backing: .buffered, defer: false)
            window.isReleasedWhenClosed = false
            defer { window.close() }
            let clip = NSView(frame: CGRect(x: 0, y: 0, width: 400, height: 200))
            clip.wantsLayer = true
            clip.layer?.masksToBounds = true
            window.contentView = clip
            let content = NSView(frame: CGRect(x: 0, y: 0, width: 400, height: 2000))
            content.wantsLayer = true
            clip.addSubview(content)
            let row = NSView(frame: CGRect(x: 0, y: 0, width: 400, height: 100))
            row.wantsLayer = true
            content.addSubview(row)
            let text = MessageTextView()
            text.frame = CGRect(x: 0, y: 0, width: 400, height: 40)
            row.addSubview(text)
            window.orderFront(nil)
            window.displayIfNeeded()
            text.watchScrollingGeometry()
            text.layer?.displayIfNeeded()
            let visible = text.visibleRect
            XCTAssertFalse(visible.isEmpty)

            // The list prepends rows, then keeps this row at its screen position.
            row.frame.origin.y += 1000
            content.layer?.sublayerTransform = CATransform3DMakeTranslation(0, -1000, 0)
            XCTAssertEqual(text.visibleRect, visible)
            XCTAssertEqual(text.layer?.needsDisplay(), true, "the row must regain its glyph pixels")
            text.layer?.displayIfNeeded()

            // AppKit catches up later without moving any pixels on screen.
            content.setBoundsOrigin(CGPoint(x: 0, y: 1000))
            content.layer?.sublayerTransform = CATransform3DIdentity
            XCTAssertEqual(text.visibleRect, visible)
            XCTAssertEqual(text.layer?.needsDisplay(), true, "the bounds rebase must not leave text blank")
        }

        @MainActor func testExposureKeepsOverlappingPixelsForBothScrollDirectionsAndResize() {
            let before = NSRect(x: 0, y: 0, width: 400, height: 200)
            let after = NSRect(x: 0, y: 60, width: 400, height: 200)
            XCTAssertEqual(MessageTextView.newlyExposedRects(previous: before, current: after),
                           [NSRect(x: 0, y: 200, width: 400, height: 60)])
            XCTAssertEqual(MessageTextView.newlyExposedRects(previous: after, current: before),
                           [NSRect(x: 0, y: 0, width: 400, height: 60)])
            XCTAssertEqual(MessageTextView.newlyExposedRects(previous: before, current: before), [])
            XCTAssertEqual(MessageTextView.newlyExposedRects(previous: before, current: .zero), [])
            XCTAssertEqual(MessageTextView.newlyExposedRects(previous: .zero, current: before), [before])
            let outside = NSRect(x: 0, y: 300, width: 400, height: 200)
            XCTAssertEqual(MessageTextView.newlyExposedRects(previous: before, current: outside), [outside])
            let inside = NSRect(x: 20, y: 20, width: 360, height: 160)
            XCTAssertEqual(MessageTextView.newlyExposedRects(previous: inside, current: before), [
                NSRect(x: 0, y: 0, width: 400, height: 20),
                NSRect(x: 0, y: 180, width: 400, height: 20),
                NSRect(x: 0, y: 20, width: 20, height: 160),
                NSRect(x: 380, y: 20, width: 20, height: 160)
            ])
        }

        #if DEBUG
            @MainActor func testVisiblePaintMatchesTheActualParagraphInsteadOfAnotherPrefix() {
                var paints = RenderProbe.Paints()
                paints.record("Render turn 49, paragraph 2. Older content.", at: 10)
                paints.record("Render turn 51, paragraph 2. Newly visible content.", at: 20)
                XCTAssertEqual(
                    paints
                        .firstVisible(prefix: "Render turn",
                                      in: "clipped line\nRender turn 51, paragraph 2. Newly visible"),
                    20
                )
                XCTAssertNil(paints.firstVisible(prefix: "Render turn", in: "Render turn 50, paragraph 2. Not painted"))
                XCTAssertNil(paints.firstVisible(prefix: "Render turn", in: "unrelated content"))
            }

            @MainActor func testStreamProbeRetainsFirstPaintAcrossAppendsAndClearsReplacedContent() {
                var paints = RenderProbe.Paints()
                paints.record("update 0", at: 10)
                paints.update(previous: "update 0", current: "update 0\nupdate 1")
                paints.record("update 0\nupdate 1", at: 20)
                XCTAssertEqual(paints.first(for: "update 0"), 10)
                XCTAssertEqual(paints.first(for: "update 1"), 20)
                // A later refresh of the same text must not replace its first draw.
                paints.update(previous: "update 0\nupdate 1", current: "update 0\nupdate 1")
                paints.record("update 0\nupdate 1", at: 30)
                XCTAssertEqual(paints.first(for: "update 1"), 20)
                XCTAssertEqual(paints.first(for: "update 0", since: 25), 30)
                XCTAssertEqual(paints.first(for: "update 1", since: 25), 30)
                paints.update(previous: "update 0\nupdate 1", current: "replacement")
                XCTAssertNil(paints.first(for: "update 0"))
                paints.record("replacement", at: 40)
                paints.clear()
                XCTAssertNil(paints.first(for: "replacement"))
            }
        #endif

        @MainActor func testTextKitOwnsTheVisibleTextAndReflowsAtTheNativeWidth() throws {
            let url = try XCTUnwrap(Bundle.module.url(forResource: "message", withExtension: "json"))
            let document = try JSONDecoder().decode(MessageDocument.self, from: Data(contentsOf: url))
            let view = MessageTextContainer()
            var heights: [CGFloat] = []
            view.send = {
                if let height = $0["measuredHeight"] as? CGFloat {
                    heights.append(height)
                }
            }
            view.update(document)
            XCTAssertEqual(view.text.string, "日本語 👋 ファイル")
            XCTAssertEqual(view.text.accessibilityIdentifier(), "message.user-text")
            XCTAssertNotNil(view.text.textStorage)
            XCTAssertNotNil(view.text.layoutManager)
            view.frame = CGRect(x: 0, y: 0, width: 400, height: 100)
            view.layout()
            let wide = try XCTUnwrap(heights.last)
            view.frame.size.width = 70
            // NSH supplies the new width with the document. The old native
            // frame can belong to a different row while a cell is recycled.
            view.update(MessageDocument(blocks: document.blocks, secondary: document.secondary,
                                        role: document.role, target: document.target,
                                        sideChat: document.sideChat, maxWidth: 70, menu: document.menu))
            view.layout()
            XCTAssertGreaterThan(try XCTUnwrap(heights.last), wide)
        }

        @MainActor func testFileChipsShowTheirGlyphAccentTooltipAndMenuAndReportTheirLink() throws {
            let json = """
            {"text": "main.rs · L4", "strong": false, "emphasis": false, "strikethrough": false, "code": true,
             "link": "src/main.rs:4", "darkColor": null, "lightColor": null,
             "file": {"icon": null, "folder": true, "tooltip": "/repo/src/main.rs:4 · 2 KB",
                      "menu": [{"id": "copy", "title": "フルパスをコピー"},
                               {"title": "エディターで開く", "children": [{"id": "code", "title": "VS Code"}]}],
                      "darkColor": 16711680, "lightColor": 255}}
            """
            let run = try JSONDecoder().decode(MessageRun.self, from: Data(json.utf8))
            let paragraph = MessageParagraph(header: nil, marker: nil, code: false, quoted: false, depth: 0,
                                             alignment: "left", runs: [run])
            let text = MessageAttributes.text([paragraph], secondary: false, dark: true)
            XCTAssertTrue(text.string.hasSuffix(" main.rs · L4"))
            XCTAssertNotNil(text.attribute(.attachment, at: 0, effectiveRange: nil), "the glyph leads the chip")
            let label = (text.string as NSString).range(of: "main.rs").location
            let font = try XCTUnwrap(text.attribute(.font, at: label, effectiveRange: nil) as? NSFont)
            XCTAssertFalse(font.isFixedPitch, "a chip drops its code span's font")
            XCTAssertEqual(text.attribute(.toolTip, at: label, effectiveRange: nil) as? String,
                           "/repo/src/main.rs:4 · 2 KB")
            let color = try XCTUnwrap(text.attribute(.foregroundColor, at: label, effectiveRange: nil) as? NSColor)
            XCTAssertEqual(color.redComponent, 1)
            XCTAssertNil(text.attribute(.underlineStyle, at: label, effectiveRange: nil))

            let view = MessageTextContainer()
            var actions: [[String: Any]] = []
            view.text.action = { actions.append($0) }
            view.update(MessageDocument(blocks: [paragraph], secondary: false, role: "assistant", target: "t",
                                        sideChat: false, maxWidth: 400,
                                        menu: MessageMenuTitles(quote: "Add to chat", sideChat: "Ask in side chat")))
            _ = view.text.textView(view.text, clickedOnLink: "src/main.rs:4", at: label)
            XCTAssertEqual(actions.last?["text"] as? String, "src/main.rs:4")
            XCTAssertEqual(actions.last?["command"] as? Bool, false)
            let menu = try view.text.fileMenu(XCTUnwrap(run.file).menu, link: "src/main.rs:4")
            XCTAssertEqual(menu.items.map(\.title), ["フルパスをコピー", "エディターで開く"])
            XCTAssertEqual(menu.items[1].submenu?.items.first?.title, "VS Code")
            let item = try XCTUnwrap(menu.items[1].submenu?.items.first)
            _ = item.target?.perform(item.action, with: item)
            XCTAssertEqual(actions.last?["action"] as? String, "file")
            XCTAssertEqual(actions.last?["id"] as? String, "code")
        }

        @MainActor func testMessagesKeepNativeLineHeightsAndBaselinesAcrossScripts() throws {
            for role in ["assistant", "user"] {
                let text = String(repeating: "lib.rs · L695 [groups] 日本語 ไทย 🧑🏽‍💻 and more words ", count: 3)
                let run = MessageRun(text: text, strong: false, emphasis: false, strikethrough: false, code: false,
                                     link: nil, darkColor: nil, lightColor: nil)
                let block = MessageParagraph(header: nil, marker: nil, code: false, quoted: false,
                                             depth: 0, alignment: "left", runs: [run])
                let view = MessageTextContainer()
                var height: CGFloat?
                view.send = {
                    if let measured = $0["measuredHeight"] as? CGFloat {
                        height = measured
                    }
                }
                view.update(MessageDocument(blocks: [block], secondary: false, role: role, target: "t",
                                            sideChat: false, maxWidth: 200,
                                            menu: MessageMenuTitles(quote: "", sideChat: "")))
                // The OS chooses room for each script, including fallback fonts.
                let reference = NSLayoutManager()
                let storage = NSTextStorage(string: text, attributes: [.font: NSFont.systemFont(ofSize: 14)])
                let container = NSTextContainer(size: CGSize(width: 200, height: CGFloat.greatestFiniteMagnitude))
                container.lineFragmentPadding = 0
                storage.addLayoutManager(reference)
                reference.addTextContainer(container)
                reference.ensureLayout(for: container)
                XCTAssertEqual(height, ceil(reference.usedRect(for: container).height), role)
                let actual = try XCTUnwrap(view.text.layoutManager)
                XCTAssertEqual(actual.numberOfGlyphs, reference.numberOfGlyphs)
                var lines = 0
                actual.enumerateLineFragments(
                    forGlyphRange: NSRange(location: 0, length: actual.numberOfGlyphs)
                ) { _, used, _, glyphs, _ in
                    let first = glyphs.location
                    let expected = reference.lineFragmentUsedRect(forGlyphAt: first, effectiveRange: nil)
                    XCTAssertEqual(used.height, expected.height, accuracy: 0.01, role)
                    XCTAssertEqual(actual.location(forGlyphAt: first).y,
                                   reference.location(forGlyphAt: first).y, accuracy: 0.01, role)
                    lines += 1
                }
                XCTAssertGreaterThan(lines, 2, "the multilingual message wraps")
            }
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

        @MainActor func testInlineCodeGlyphsFitInsideTheNativeBackground() throws {
            for strong in [false, true] {
                let code = "session-25VpGP"
                let block = MessageParagraph(header: nil, marker: "•", code: false, quoted: false,
                                             depth: 1, alignment: "left", runs: [
                                                 MessageRun(text: "変更がある worktree: ", strong: strong,
                                                            emphasis: false, strikethrough: false, code: false,
                                                            link: nil, darkColor: nil, lightColor: nil),
                                                 MessageRun(text: code, strong: false, emphasis: false,
                                                            strikethrough: false, code: true, link: nil,
                                                            darkColor: nil, lightColor: nil)
                                             ])
                let view = MessageTextContainer()
                view.update(MessageDocument(blocks: [block], secondary: false, role: "assistant", target: "t",
                                            sideChat: false, maxWidth: 600,
                                            menu: MessageMenuTitles(quote: "", sideChat: "")))
                let manager = try XCTUnwrap(view.text.layoutManager)
                let container = try XCTUnwrap(view.text.textContainer)
                let storage = try XCTUnwrap(view.text.textStorage)
                let characters = (view.text.string as NSString).range(of: code)
                let glyphs = manager.glyphRange(forCharacterRange: characters, actualCharacterRange: nil)
                var background = NSRect.null
                manager.enumerateEnclosingRects(forGlyphRange: glyphs,
                                                withinSelectedGlyphRange: NSRange(location: NSNotFound, length: 0),
                                                in: container) { rect, _ in background = background.union(rect) }
                var ink = NSRect.null
                for index in glyphs.location ..< NSMaxRange(glyphs) {
                    var glyph = manager.cgGlyph(at: index)
                    let character = manager.characterIndexForGlyph(at: index)
                    let font = try XCTUnwrap(storage.attribute(.font, at: character, effectiveRange: nil) as? NSFont)
                    let bounds = CTFontGetBoundingRectsForGlyphs(font, .default, &glyph, nil, 1)
                    let line = manager.lineFragmentRect(forGlyphAt: index, effectiveRange: nil)
                    let origin = manager.location(forGlyphAt: index)
                    ink = ink.union(NSRect(x: line.minX + origin.x + bounds.minX,
                                           y: line.minY + origin.y - bounds.maxY,
                                           width: bounds.width, height: bounds.height))
                }
                XCTAssertGreaterThan(ink.minY, background.minY)
                XCTAssertLessThan(ink.maxY, background.maxY)
            }
        }

        @MainActor func testInlineCodeBackgroundsStayInsideTheirFirstAndWrappedLines() throws {
            let code = "hasPrefix(\"# 日本語 🧑🏽‍💻\") source source source source"
            let runs = [
                MessageRun(
                    text: "保持契約の lib.rs · L695 [groups] ไทย ",
                    strong: false,
                    emphasis: false,
                    strikethrough: false,
                    code: false,
                    link: nil,
                    darkColor: nil,
                    lightColor: nil
                ),
                MessageRun(text: code, strong: false, emphasis: false, strikethrough: false,
                           code: true, link: nil, darkColor: nil, lightColor: nil)
            ]
            let block = MessageParagraph(header: nil, marker: nil, code: false, quoted: false,
                                         depth: 0, alignment: "left", runs: runs)
            let view = MessageTextContainer()
            view.update(MessageDocument(blocks: [block], secondary: false, role: "assistant", target: "t",
                                        sideChat: false, maxWidth: 220,
                                        menu: MessageMenuTitles(quote: "", sideChat: "")))
            view.frame = NSRect(x: 0, y: 0, width: 220, height: 200)
            view.layout()
            let manager = try XCTUnwrap(view.text.layoutManager)
            let container = try XCTUnwrap(view.text.textContainer)
            let characters = (view.text.string as NSString).range(of: code)
            let glyphs = manager.glyphRange(forCharacterRange: characters, actualCharacterRange: nil)
            var backgrounds: [NSRect] = []
            manager.enumerateEnclosingRects(forGlyphRange: glyphs,
                                            withinSelectedGlyphRange: NSRange(location: NSNotFound, length: 0),
                                            in: container) { rect, _ in
                backgrounds.append(rect.offsetBy(dx: view.text.textContainerOrigin.x,
                                                 dy: view.text.textContainerOrigin.y))
            }
            XCTAssertGreaterThan(backgrounds.count, 1, "the code spans wrapped native lines")
            for rect in backgrounds {
                XCTAssertGreaterThanOrEqual(rect.minY, 0, "the native background must not be shifted above the view")
                XCTAssertLessThanOrEqual(rect.maxY, view.text.bounds.height)
            }
            view.text.setSelectedRange(characters)
            XCTAssertEqual((view.text.string as NSString).substring(with: view.text.selectedRange()), code)
        }

        @MainActor func testRecycledTextReportsOnlyTheHeightAtItsNewColumnsWidth() throws {
            let run = MessageRun(text: String(repeating: "A long response retains its reading position. ", count: 80),
                                 strong: false, emphasis: false, strikethrough: false, code: false,
                                 link: nil, darkColor: nil, lightColor: nil)
            let block = MessageParagraph(header: nil, marker: nil, code: false, quoted: false,
                                         depth: 0, alignment: "left", runs: [run])
            let document = MessageDocument(blocks: [block], secondary: false, role: "assistant",
                                           target: "fixture", sideChat: false, maxWidth: 740,
                                           menu: MessageMenuTitles(quote: "", sideChat: ""))
            let view = MessageTextContainer()
            // A short user bubble occupied the same recycled native view.
            view.frame = CGRect(x: 0, y: 0, width: 167, height: 17)
            var heights: [CGFloat] = []
            view.send = {
                if let height = $0["measuredHeight"] as? CGFloat {
                    heights.append(height)
                }
            }
            view.update(document)
            let first = try XCTUnwrap(heights.first)
            view.layout()
            view.frame.size.width = 740
            view.layout()
            XCTAssertEqual(heights, [first], "a recycled frame must not publish a second, incorrect row height")
            XCTAssertEqual(view.text.textContainer?.containerSize.width, 740)
            XCTAssertGreaterThan(first, 17)
        }
    #endif

    @MainActor func testNativeTextPreservesTheSharedContentAndOpaqueActionTarget() throws {
        let url = try XCTUnwrap(Bundle.module.url(forResource: "message", withExtension: "json"))
        let document = try JSONDecoder().decode(MessageDocument.self, from: Data(contentsOf: url))
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
