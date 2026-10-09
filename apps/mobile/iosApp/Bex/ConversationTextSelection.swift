import AgentCore
import SwiftUI
import UIKit

struct ConversationSelectionActions {
    let addToChat: (String) -> Void
    let askInSideChat: ((String) -> Void)?
}

/// Native inline attachments share selection and copy semantics with surrounding prose.
struct AssistantSelectableText: UIViewRepresentable {
    let blocks: [ConversationMarkdownContent.Block]
    let actions: ConversationSelectionActions
    var cwd: String = ""
    var identifier: String = "message.assistant-text"
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.openURL) private var openURL
    @Environment(\.sizeCategory) private var sizeCategory

    func makeCoordinator() -> Coordinator {
        Coordinator(self)
    }

    func makeUIView(context: Context) -> ConversationTextView {
        let view = ConversationTextView(usingTextLayoutManager: false)
        view.scrollsToTop = false
        view.isEditable = false
        view.isSelectable = true
        view.isScrollEnabled = false
        view.backgroundColor = .clear
        view.textContainerInset = .zero
        view.textContainer.lineFragmentPadding = 0
        view.delegate = context.coordinator
        view.accessibilityIdentifier = identifier
        view.linkTextAttributes = [
            .foregroundColor: UIColor.systemBlue,
            .underlineStyle: NSUnderlineStyle.single.rawValue
        ]
        return view
    }

    func updateUIView(_ view: ConversationTextView, context: Context) {
        let coordinator = context.coordinator
        coordinator.parent = self
        guard coordinator.blocks != blocks || coordinator.sizeCategory != sizeCategory || coordinator
            .colorScheme != colorScheme else { return }
        coordinator.blocks = blocks
        coordinator.sizeCategory = sizeCategory
        coordinator.colorScheme = colorScheme
        view.setContent(ConversationAttributedText.attributedText(blocks, dark: colorScheme == .dark,
                                                                  attachmentWidth: view.attachmentWidth))
    }

    func sizeThatFits(_ proposal: ProposedViewSize, uiView: ConversationTextView, context _: Context) -> CGSize? {
        let width = proposal.width ?? ceil(uiView.attributedText.size().width)
        guard width > 0 else { return nil }
        if uiView.attachmentWidth != width {
            uiView.attachmentWidth = width
            uiView.setContent(ConversationAttributedText.attributedText(blocks, dark: colorScheme == .dark,
                                                                        attachmentWidth: width))
        }
        let size = uiView.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude))
        return CGSize(width: width, height: size.height)
    }

    final class Coordinator: NSObject, UITextViewDelegate {
        var parent: AssistantSelectableText
        var blocks: [ConversationMarkdownContent.Block] = []
        var sizeCategory: ContentSizeCategory?
        var colorScheme: ColorScheme?

        init(_ parent: AssistantSelectableText) {
            self.parent = parent
        }

        func textView(_ textView: UITextView, editMenuForTextIn range: NSRange,
                      suggestedActions: [UIMenuElement]) -> UIMenu? {
            guard range.length > 0, NSMaxRange(range) <= (textView.text as NSString).length else { return nil }
            let text = ConversationAttributedText.selectedText(textView.attributedText, range: range)
            let add = UIAction(title: "チャットに追加", image: UIImage(systemName: "bubble")) { [weak self] _ in
                self?.parent.actions.addToChat(text)
            }
            let copy = UIAction(title: "コピー", image: UIImage(systemName: "doc.on.doc")) { _ in
                UIPasteboard.general.string = text
            }
            var actions: [UIMenuElement] = [add, copy]
            if parent.actions.askInSideChat != nil {
                actions.append(UIAction(
                    title: "サイドチャットで質問",
                    image: UIImage(systemName: "bubble.left.and.bubble.right")
                ) { [weak self] _ in
                    self?.parent.actions.askInSideChat?(text)
                })
            }
            return UIMenu(children: actions + suggestedActions.compactMap(Self.removingCopy))
        }

        private static func removingCopy(_ element: UIMenuElement) -> UIMenuElement? {
            if let command = element as? UICommand,
               command.action == #selector(UIResponderStandardEditActions.copy(_:)) {
                return nil
            }
            if let menu = element as? UIMenu {
                let children = menu.children.compactMap(removingCopy)
                return children.isEmpty ? nil : menu.replacingChildren(children)
            }
            return element
        }

        func textView(_: UITextView, menuConfigurationFor textItem: UITextItem,
                      defaultMenu _: UIMenu) -> UITextItem.MenuConfiguration? {
            guard case let .link(url) = textItem.content,
                  let file = markdownFileTarget(source: url.absoluteString, cwd: parent.cwd) else { return nil }
            let open = UIAction(title: "ファイルを開く", image: UIImage(systemName: "doc")) { [weak self] _ in
                self?.parent.openURL(url)
            }
            let copy = UIAction(title: "パスをコピー", image: UIImage(systemName: "doc.on.doc")) { _ in
                UIPasteboard.general.string = file.path
            }
            return UITextItem.MenuConfiguration(menu: UIMenu(title: file.path, children: [open, copy]))
        }

        func textView(_: UITextView, primaryActionFor textItem: UITextItem, defaultAction: UIAction) -> UIAction? {
            guard case let .link(url) = textItem.content else { return defaultAction }
            return UIAction { [weak self] _ in self?.parent.openURL(url) }
        }
    }
}

private enum ConversationAttributedText {
    static func attributedText(_ blocks: [ConversationMarkdownContent.Block],
                               dark: Bool, attachmentWidth: CGFloat) -> NSAttributedString {
        let text = NSMutableAttributedString(string: "")
        for (index, block) in blocks.enumerated() {
            if index > 0 {
                text.append(NSAttributedString(string: "\n"))
            }
            let start = text.length
            let style = block.style
            let font = font(for: style)
            if let marker = style.marker {
                text.append(NSAttributedString(
                    string: marker + " ",
                    attributes: [.font: font, .foregroundColor: UIColor.label]
                ))
            }
            for run in block.runs {
                if let file = run.file, let link = run.link {
                    text.append(fileChip(file, link: link, font: font, maxWidth: attachmentWidth))
                } else {
                    text.append(NSAttributedString(
                        string: run.text,
                        attributes: attributes(for: run, font: font, dark: dark)
                    ))
                }
            }
            let paragraph = NSMutableParagraphStyle()
            switch style.alignment {
            case .center: paragraph.alignment = .center
            case .right: paragraph.alignment = .right
            case .left: paragraph.alignment = .left
            }
            paragraph.lineSpacing = style.code ? 0 : 5
            paragraph.paragraphSpacing = index + 1 == blocks.count ? 0 : 14
            let indent = CGFloat(max(0, Int(style.listDepth) - 1)) * 22 + (style.quoted ? 12 : 0)
            paragraph.headIndent = indent + (style.listDepth > 0 ? 22 : 0)
            paragraph.firstLineHeadIndent = indent
            let paragraphRange = NSRange(location: start, length: text.length - start)
            if style.quoted {
                text.addAttribute(ConversationTextView.quote, value: true, range: paragraphRange)
            }
            if style.rule {
                text.addAttributes([ConversationTextView.rule: true, .foregroundColor: UIColor.clear,
                                    copySource: "---"], range: paragraphRange)
            }
            text.addAttribute(
                .paragraphStyle,
                value: paragraph,
                range: NSRange(location: start, length: text.length - start)
            )
        }
        return text
    }

    private static func attributes(for run: MarkdownRun, font: UIFont, dark: Bool) -> [NSAttributedString.Key: Any] {
        var runFont = font
        if run.code {
            runFont = UIFont.monospacedSystemFont(
                ofSize: font.pointSize,
                weight: .regular
            )
        }
        var traits = runFont.fontDescriptor.symbolicTraits
        if run.strong {
            traits.insert(.traitBold)
        }
        if run.emphasis {
            traits.insert(.traitItalic)
        }
        if let descriptor = runFont.fontDescriptor.withSymbolicTraits(traits) {
            runFont = UIFont(descriptor: descriptor, size: runFont.pointSize)
        }
        var attributes: [NSAttributedString.Key: Any] = [.font: runFont, .foregroundColor: UIColor.label]
        if let color = dark ? run.darkColor : run.lightColor {
            attributes[.foregroundColor] = UIColor(red: CGFloat((color >> 16) & 255) / 255,
                                                   green: CGFloat((color >> 8) & 255) / 255,
                                                   blue: CGFloat(color & 255) / 255, alpha: 1)
        }
        if let link = run.link.flatMap(URL.init(string:)) {
            attributes[.link] = link
        }
        if run.strikethrough {
            attributes[.strikethroughStyle] = NSUnderlineStyle.single.rawValue
        }
        if run.code {
            attributes[.backgroundColor] = UIColor.secondarySystemBackground
        }
        return attributes
    }

    private static let copySource = NSAttributedString.Key("bex.markdown-copy")

    private static func fileChip(_ file: MarkdownFileReference, link: String, font: UIFont,
                                 maxWidth: CGFloat) -> NSAttributedString {
        let labelFont = UIFont.systemFont(ofSize: font.pointSize * 0.86, weight: .medium)
        let label = file.label as NSString
        let labelWidth = min(label.size(withAttributes: [.font: labelFont]).width, font.pointSize * 16)
        let height = ceil(font.pointSize * 1.42)
        let iconSize = font.pointSize * 0.9
        let width = max(iconSize + 18, min(ceil(labelWidth + iconSize + 17), maxWidth))
        let color = UIColor.systemTeal
        let image = UIGraphicsImageRenderer(size: CGSize(width: width, height: height)).image { _ in
            let rect = CGRect(x: 0.5, y: 0.5, width: width - 1, height: height - 1)
            let outline = UIBezierPath(roundedRect: rect, cornerRadius: font.pointSize * 0.5)
            color.withAlphaComponent(0.10).setFill(); outline.fill()
            color.withAlphaComponent(0.35).setStroke(); outline.lineWidth = 1; outline.stroke()
            let symbol = switch file.kind {
            case .markdown: "doc.richtext"
            case .code: "chevron.left.forwardslash.chevron.right"
            case .image: "photo"
            case .document: "doc.text"
            case .folder: "folder"
            case .file: "doc"
            }
            UIImage(systemName: symbol)?.withTintColor(color).draw(
                in: CGRect(x: 6, y: (height - iconSize) / 2, width: iconSize, height: iconSize)
            )
            let paragraph = NSMutableParagraphStyle()
            paragraph.lineBreakMode = .byTruncatingMiddle
            label.draw(in: CGRect(x: iconSize + 10, y: (height - labelFont.lineHeight) / 2,
                                  width: width - iconSize - 17, height: labelFont.lineHeight),
                       withAttributes: [.font: labelFont, .foregroundColor: color, .paragraphStyle: paragraph])
        }
        let attachment = NSTextAttachment()
        attachment.image = image
        attachment.bounds = CGRect(x: 0, y: font.descender - 2, width: width, height: height)
        let text = NSMutableAttributedString(attachment: attachment)
        text.addAttributes([copySource: "[\(file.label)](<\(link)>)",
                            .font: font], range: NSRange(location: 0, length: text.length))
        if let url = URL(string: link) {
            text.addAttribute(.link, value: url, range: NSRange(location: 0, length: text.length))
        }
        return text
    }

    static func selectedText(_ content: NSAttributedString, range: NSRange) -> String {
        let selected = content.attributedSubstring(from: range)
        let result = NSMutableString(string: selected.string)
        var replacements: [(NSRange, String)] = []
        selected.enumerateAttribute(copySource, in: NSRange(location: 0, length: selected.length)) { value, range, _ in
            if let source = value as? String {
                replacements.append((range, source))
            }
        }
        for (range, source) in replacements.reversed() {
            result.replaceCharacters(in: range, with: source)
        }
        return result as String
    }

    private static func font(for style: MarkdownStyle) -> UIFont {
        let headingSizes: [CGFloat] = [26, 23, 21, 19, 18, 18]
        let size: CGFloat = style.code ? 15 : style.header.map { headingSizes[Int($0) - 1] } ?? 18
        let base = style.code ? UIFont.monospacedSystemFont(ofSize: size, weight: .regular)
            : UIFont.systemFont(ofSize: size, weight: style.header == nil ? .regular : .semibold)
        return UIFontMetrics.default.scaledFont(for: base)
    }
}

/// Decorations stay in the same native text view as selectable prose.
final class ConversationTextView: UITextView {
    var attachmentWidth: CGFloat = 300

    func setContent(_ content: NSAttributedString) {
        let range = selectedRange
        let old = text as NSString
        let next = content.string as NSString
        let keepSelection = range.length > 0 && NSMaxRange(range) <= old.length && NSMaxRange(range) <= next.length
            && ConversationAttributedText.selectedText(attributedText, range: range) == ConversationAttributedText
            .selectedText(
                content,
                range: range
            )
        textStorage.setAttributedString(content)
        selectedRange = keepSelection ? range : NSRange(location: 0, length: 0)
        invalidateIntrinsicContentSize()
    }

    static let quote = NSAttributedString.Key("bex.quote")
    static let rule = NSAttributedString.Key("bex.rule")

    override func draw(_ rect: CGRect) {
        super.draw(rect)
        let range = NSRange(location: 0, length: textStorage.length)
        for attribute in [Self.quote, Self.rule] {
            textStorage.enumerateAttribute(attribute, in: range) { value, range, _ in
                guard value as? Bool == true else { return }
                let glyphs = layoutManager.glyphRange(forCharacterRange: range, actualCharacterRange: nil)
                let bounds = layoutManager.boundingRect(forGlyphRange: glyphs, in: textContainer)
                let line = UIBezierPath()
                if attribute == Self.quote {
                    let lineX = max(1, bounds.minX - 10) + textContainerInset.left
                    line.move(to: CGPoint(x: lineX, y: bounds.minY + textContainerInset.top))
                    line.addLine(to: CGPoint(x: lineX, y: bounds.maxY + textContainerInset.top))
                    line.lineWidth = 2
                } else {
                    line.move(to: CGPoint(x: 0, y: bounds.midY + textContainerInset.top))
                    line.addLine(to: CGPoint(x: self.bounds.width, y: bounds.midY + textContainerInset.top))
                    line.lineWidth = 0.5
                }
                UIColor.separator.setStroke()
                line.stroke()
            }
        }
    }
}
