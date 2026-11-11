import Foundation
#if os(macOS)
    import AppKit

    typealias MessageFont = NSFont
    typealias MessageColor = NSColor
#else
    import UIKit

    typealias MessageFont = UIFont
    typealias MessageColor = UIColor
#endif

struct MessageRun: Decodable, Equatable {
    let text: String
    let strong: Bool
    let emphasis: Bool
    let strikethrough: Bool
    let code: Bool
    let link: String?
    let darkColor: UInt32?
    let lightColor: UInt32?
    /// The shared chip of the file the run names.
    var file: MessageFile?
    var highlights: [MessageHighlight]? = nil
}

/// Core's UTF-8 byte ranges within one displayed run.
struct MessageHighlight: Decodable, Equatable {
    let start: Int
    let end: Int
    let active: Bool
}

/// A file chip as Core describes it: its glyph (a folder's, or the file's artwork),
/// its full path and size, its menu, and its accent in either look.
struct MessageFile: Decodable, Equatable {
    /// The shared SVG artwork rasterized at build time, as a base64 PNG.
    let icon: String?
    let folder: Bool
    let tooltip: String
    let menu: [MessageMenuItem]
    let darkColor: UInt32
    let lightColor: UInt32
}

/// An item of a chip's menu: an action by its opaque id, or a submenu.
struct MessageMenuItem: Decodable, Equatable {
    let id: String?
    let title: String
    let children: [MessageMenuItem]?
}

extension NSAttributedString.Key {
    /// The chip a range of message text shows, and the link it opens.
    static let messageFile = NSAttributedString.Key("BexMessageFile")
    static let messageFindActive = NSAttributedString.Key("BexMessageFindActive")
    /// The plain text replacing a chip's decorative icon and spacing when selected.
    static let messageDecoration = NSAttributedString.Key("BexMessageDecoration")
}

/// What a chip's text carries: its link and the chip, for its menu.
final class MessageFileLink: NSObject {
    let link: String
    let file: MessageFile

    init(link: String, file: MessageFile) {
        self.link = link
        self.file = file
    }
}

struct MessageParagraph: Decodable, Equatable {
    let header: Int?
    let marker: String?
    let code: Bool
    let quoted: Bool
    let depth: UInt32
    let alignment: String
    let runs: [MessageRun]
    var markerHighlights: [MessageHighlight]? = nil
}

struct MessageDocument: Decodable, Equatable {
    let blocks: [MessageParagraph]
    let secondary: Bool
    /// The text's color at the device's contrast.
    var foreground: UInt32? = nil
    /// The selected occurrence, so layout reveals it once per navigation.
    var findKey: String? = nil
    /// Whose text it is, `user` or `assistant`, as its accessibility identifier names it.
    let role: String
    let target: String
    let sideChat: Bool
    let maxWidth: Double
    /// The selection menu's items in the app's language.
    let menu: MessageMenuTitles
    /// A table's cell, `header` or `body`, whose text is smaller.
    var cell: String? = nil

    static let empty = MessageDocument(blocks: [], secondary: false, role: "assistant", target: "", sideChat: false,
                                       maxWidth: 0, menu: MessageMenuTitles(quote: "", sideChat: ""))
}

/// What a selection's menu calls adding it to the chat and asking about it in
/// a side chat, as Bex's localization names them.
struct MessageMenuTitles: Decodable, Equatable {
    let quote: String
    let sideChat: String
}

@MainActor enum MessageAttributes {
    static func retainedSelection(_ range: NSRange, previous: String, next: String) -> NSRange {
        guard range.length > 0, NSMaxRange(range) <= previous.utf16.count, NSMaxRange(range) <= next.utf16.count,
              (previous as NSString).substring(with: range) == (next as NSString).substring(with: range)
        else { return NSRange(location: 0, length: 0) }
        return range
    }
    /// `answer`: an assistant's text, which T3 Code draws at 80% of the
    /// foreground on the Mac. `cell`: a table's cell (`header` or `body`).
    static func text(_ blocks: [MessageParagraph], secondary: Bool, dark: Bool,
                     answer: Bool = false, cell: String? = nil,
                     foreground: UInt32? = nil) -> NSAttributedString {
        let result = NSMutableAttributedString(string: "")
        for (index, block) in blocks.enumerated() {
            let start = result.length
            let size = Self.size(block, cell: cell)
            if let marker = block.marker {
                append(NSAttributedString(string: marker + " ", attributes: [
                    .font: font(size: size, mono: block.code, weight: .regular, italic: false),
                    .foregroundColor: color(secondary: secondary, answer: answer, foreground: foreground)
                ]), highlights: block.markerHighlights, to: result)
            }
            for (position, run) in block.runs.enumerated() {
                var attributes = attributes(
                    run, size: size, code: block.code, heading: block.header != nil || cell == "header",
                    secondary: secondary, answer: answer, dark: dark, foreground: foreground
                )
                if caption(block.runs, at: position) {
                    // A chip's authored caption reads as text, and opens as the chip does.
                    attributes[.foregroundColor] = color(secondary: secondary, answer: answer, foreground: foreground)
                    attributes[.underlineStyle] = nil
                }
                if let file = run.file, let link = run.link {
                    let font = font(size: size, mono: false, weight: .medium, italic: false)
                    attributes = chip(file, link: link, font: font, base: attributes, dark: dark)
                    if let glyph = glyph(file, font: font) {
                        // A space sets the chip apart from its caption.
                        let gap = position > 0 && caption(block.runs, at: position - 1) ? " " : ""
                        let icon = NSMutableAttributedString(string: gap)
                        icon.append(NSAttributedString(attachment: glyph))
                        icon.append(NSAttributedString(string: " "))
                        icon.addAttributes(attributes, range: NSRange(location: 0, length: icon.length))
                        icon.addAttribute(
                            .messageDecoration,
                            value: gap,
                            range: NSRange(location: 0, length: icon.length)
                        )
                        result.append(icon)
                    }
                }
                append(
                    NSAttributedString(string: run.text, attributes: attributes),
                    highlights: run.highlights,
                    to: result
                )
            }
            let paragraph = style(block, at: index, of: blocks)
            if index + 1 < blocks.count {
                result.append(NSAttributedString(string: "\n", attributes: [
                    .font: font(size: size, mono: block.code, weight: .regular, italic: false)
                ]))
            }
            let range = NSRange(location: start, length: result.length - start)
            result.addAttribute(.paragraphStyle, value: paragraph, range: range)
        }
        return result
    }

    /// Copy and quotation use labels, without the icons drawn beside them.
    static func selectedText(_ content: NSAttributedString, range: NSRange) -> String {
        let selected = content.attributedSubstring(from: range)
        let result = NSMutableString(string: selected.string)
        var replacements: [(NSRange, String)] = []
        selected.enumerateAttribute(.messageDecoration, in: NSRange(
            location: 0,
            length: selected.length
        )) { value, range, _ in
            if let text = value as? String {
                replacements.append((range, text))
            }
        }
        for (range, text) in replacements.reversed() {
            result.replaceCharacters(in: range, with: text)
        }
        return result as String
    }

    /// The paragraph style of the block at `index`.
    private static func style(_ block: MessageParagraph, at index: Int,
                              of blocks: [MessageParagraph]) -> NSParagraphStyle {
        let paragraph = NSMutableParagraphStyle()
        paragraph.alignment = block.alignment == "center" ? .center : block.alignment == "right" ? .right : .left
        let last = index + 1 == blocks.count
        #if os(macOS)
            // Items of one list: a numbered list next to a bulleted one is
            // another list, a paragraph's spacing apart.
            let numbered = { (marker: String?) in marker.map { $0.hasSuffix(".") } }
            let item = block.marker != nil && !last
                && numbered(block.marker) == numbered(blocks[index + 1].marker)
            paragraph.paragraphSpacing = last || block.code ? 0 : item ? 4 : 10.4
            paragraph.paragraphSpacingBefore = block.header != nil && index > 0 ? 9.6 : 0
        #else
            paragraph.lineSpacing = block.code ? 0 : 5
            paragraph.paragraphSpacing = last ? 0 : 14
        #endif
        let indent = CGFloat(max(0, Int(block.depth) - 1)) * 22 + (block.quoted ? 12 : 0)
        paragraph.firstLineHeadIndent = indent
        paragraph.headIndent = indent + (block.depth > 0 ? 22 : 0)
        return paragraph
    }

    /// A block's text size: on the Mac T3 Code's `.chat-markdown` (14 pt
    /// text, 12 pt in a table's cells, 13 pt code, headings from 20 pt).
    private static func size(_ block: MessageParagraph, cell: String?) -> CGFloat {
        #if os(macOS)
            let text: CGFloat = cell == nil ? 14 : 12
            let (base, headings): (CGFloat, [CGFloat]) = (block.code ? 13 : text, [20, 18, 16, 14, 14, 14])
        #else
            let (base, headings): (CGFloat, [CGFloat]) = (block.code ? 15 : 18, [26, 23, 21, 19, 18, 18])
        #endif
        return block.header.map { headings[min(5, max(0, $0 - 1))] } ?? base
    }

    /// Whether the run at `position` is part of the caption a chip's author
    /// wrote before it: a link to the same target, up to the chip.
    private static func caption(_ runs: [MessageRun], at position: Int) -> Bool {
        guard runs[position].file == nil, let link = runs[position].link else { return false }
        return runs[(position + 1)...].first { $0.file != nil || $0.link != link }
            .map { $0.file != nil && $0.link == link } ?? false
    }

    /// A chip's text as the desktop client's: the file's accent at medium
    /// weight, its path and size as its tooltip.
    private static func chip(_ file: MessageFile, link: String, font: MessageFont,
                             base: [NSAttributedString.Key: Any], dark: Bool) -> [NSAttributedString.Key: Any] {
        var result = base
        result[.font] = font
        result[.foregroundColor] = rgb(dark ? file.darkColor : file.lightColor)
        result[.underlineStyle] = nil
        result[.backgroundColor] = nil
        result[.messageFile] = MessageFileLink(link: link, file: file)
        // A path no URL holds still opens: the text view reports the string.
        result[.link] = result[.link] ?? link
        #if os(macOS)
            result[.toolTip] = file.tooltip
        #endif
        return result
    }

    /// A chip's glyph at the text's size: the system folder, or the file's icon.
    private static func glyph(_ file: MessageFile, font: MessageFont) -> NSTextAttachment? {
        let data = file.icon.flatMap { Data(base64Encoded: $0) }
        #if os(macOS)
            let image = file.folder ? NSImage(systemSymbolName: "folder", accessibilityDescription: nil)
                : data.flatMap { NSImage(data: $0) }
        #else
            let image = file.folder ? UIImage(systemName: "folder") : data.flatMap { UIImage(data: $0) }
        #endif
        guard let image else { return nil }
        let attachment = NSTextAttachment()
        attachment.image = image
        let size = font.pointSize
        attachment.bounds = CGRect(x: 0, y: (font.capHeight - size) / 2, width: size, height: size)
        return attachment
    }

    private static func append(_ text: NSAttributedString, highlights: [MessageHighlight]?,
                               to result: NSMutableAttributedString) {
        guard let highlights, !highlights.isEmpty else { result.append(text); return }
        let value = NSMutableAttributedString(attributedString: text)
        let string = value.string
        let bytes = Array(string.utf8)
        for highlight in highlights {
            guard highlight.start >= 0, highlight.end > highlight.start,
                  highlight.end <= bytes.count,
                  let prefix = String(bytes: bytes[..<highlight.start], encoding: .utf8),
                  let found = String(bytes: bytes[highlight.start ..< highlight.end], encoding: .utf8) else { continue }
            let range = NSRange(location: prefix.utf16.count, length: found.utf16.count)
            value.addAttributes([.backgroundColor: rgb(highlight.active ? 0xF97316 : 0xF59E0B),
                                 .foregroundColor: rgb(0x000000)], range: range)
            if highlight.active {
                value.addAttribute(.messageFindActive, value: true, range: range)
            }
        }
        result.append(value)
    }

    private static func rgb(_ rgb: UInt32) -> MessageColor {
        MessageColor(red: CGFloat((rgb >> 16) & 255) / 255, green: CGFloat((rgb >> 8) & 255) / 255,
                     blue: CGFloat(rgb & 255) / 255, alpha: 1)
    }

    private static func color(secondary: Bool, answer: Bool = false, foreground: UInt32? = nil) -> MessageColor {
        if let foreground {
            return rgb(foreground)
        }
        #if os(macOS)
            // T3 Code's answer: the foreground at 90%.
            return secondary ? .secondaryLabelColor : answer ? NSColor(white: 0.83, alpha: 1) : .labelColor
        #else
            return secondary ? .secondaryLabel : .label
        #endif
    }

    private static func attributes(_ run: MessageRun, size: CGFloat, code: Bool, heading: Bool,
                                   secondary: Bool, answer: Bool, dark: Bool,
                                   foreground: UInt32?) -> [NSAttributedString.Key: Any] {
        var result: [NSAttributedString.Key: Any] = [
            .font: font(
                size: size,
                mono: code || run.code,
                weight: heading || run.strong ? .semibold : .regular,
                italic: run.emphasis
            ),
            .foregroundColor: color(secondary: secondary, answer: answer, foreground: foreground)
        ]
        if let color = dark ? run.darkColor : run.lightColor {
            result[.foregroundColor] = rgb(color)
        }
        if let url = run.link.flatMap(URL.init(string:)) {
            result[.link] = url
            #if os(macOS)
                // The text view draws links as their runs say, so a chip
                // keeps its accent; other links are underlined in the link
                // color, as AppKit draws them.
                result[.foregroundColor] = NSColor.linkColor
                result[.underlineStyle] = NSUnderlineStyle.single.rawValue
            #else
                result[.foregroundColor] = UIColor.link
            #endif
        }
        if run.strikethrough {
            result[.strikethroughStyle] = NSUnderlineStyle.single.rawValue
        }
        if run.code {
            #if os(macOS)
                result[.backgroundColor] = NSColor.quaternaryLabelColor
            #else
                result[.backgroundColor] = UIColor.secondarySystemBackground
            #endif
        }
        return result
    }

    private static func font(size: CGFloat, mono: Bool, weight: MessageFont.Weight, italic: Bool) -> MessageFont {
        let font = mono ? MessageFont.monospacedSystemFont(ofSize: size, weight: weight)
            : MessageFont.systemFont(ofSize: size, weight: weight)
        #if os(macOS)
            return italic ? NSFontManager.shared.convert(font, toHaveTrait: .italicFontMask) : font
        #else
            let base = italic ? font.fontDescriptor.withSymbolicTraits(.traitItalic)
                .map { UIFont(descriptor: $0, size: size) } ?? font : font
            return UIFontMetrics.default.scaledFont(for: base)
        #endif
    }
}
