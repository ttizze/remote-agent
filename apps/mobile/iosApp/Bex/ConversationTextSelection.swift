#if !BEX_NSH_CONVERSATION
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
        var home: String?
        var saveFile: ((String) -> Void)?
        var identifier: String = "message.assistant-text"
        @Environment(\.colorScheme) private var colorScheme
        @Environment(\.openURL) private var openURL
        @Environment(\.locale) private var locale
        @Environment(\.sizeCategory) private var sizeCategory

        func makeCoordinator() -> Coordinator {
            Coordinator(self)
        }

        func makeUIView(context: Context) -> ConversationTextView {
            let view = ConversationTextView()
            view.delegate = context.coordinator
            view.accessibilityIdentifier = identifier
            view.update(blocks: blocks, dark: colorScheme == .dark, sizeCategory: sizeCategory)
            return view
        }

        func updateUIView(_ view: ConversationTextView, context: Context) {
            context.coordinator.parent = self
            view.update(blocks: blocks, dark: colorScheme == .dark, sizeCategory: sizeCategory)
        }

        func sizeThatFits(_ proposal: ProposedViewSize, uiView: ConversationTextView, context _: Context) -> CGSize? {
            uiView.measure(width: proposal.width)
        }

        final class Coordinator: NSObject, UITextViewDelegate {
            var parent: AssistantSelectableText
            init(_ parent: AssistantSelectableText) {
                self.parent = parent
            }

            func textView(_ textView: UITextView, editMenuForTextIn range: NSRange,
                          suggestedActions: [UIMenuElement]) -> UIMenu? {
                guard range.length > 0, NSMaxRange(range) <= (textView.text as NSString).length else { return nil }
                let text = ConversationAttributedText.selectedText(textView.attributedText, range: range)
                let language = parent.locale.appLanguage
                var actions: [UIMenuElement] = [UIAction(
                    title: localizedUiText(language: language, source: "チャットに追加"),
                    image: UIImage(systemName: "bubble")
                ) { [weak self] _ in
                    self?.parent.actions.addToChat(text)
                }]
                if parent.actions.askInSideChat != nil {
                    actions.append(UIAction(
                        title: localizedUiText(language: language, source: "サイドチャットで質問"),
                        image: UIImage(systemName: "bubble.left.and.bubble.right")
                    ) { [weak self] _ in
                        self?.parent.actions.askInSideChat?(text)
                    })
                }
                // Keep the system Copy on the first page beside the chat actions.
                let system = splitCopyCommand(suggestedActions)
                let copy: [UIMenuElement] = system.copy.map { [$0] } ?? []
                return UIMenu(children: copy + actions + system.others)
            }

            func textView(_: UITextView, menuConfigurationFor textItem: UITextItem,
                          defaultMenu _: UIMenu) -> UITextItem.MenuConfiguration? {
                guard case let .link(url) = textItem.content else { return nil }
                let items = mobileFileMenu(
                    language: parent.locale.appLanguage,
                    source: url.absoluteString,
                    cwd: parent.cwd,
                    home: parent.home
                )
                guard !items.isEmpty else { return nil }
                let actions = items.map { item in
                    UIAction(title: item.label) { [weak self] _ in
                        switch item.action {
                        case .copyFullPath, .copyRelativePath:
                            UIPasteboard.general.string = item.path
                        case .open: self?.parent.openURL(url)
                        case .saveOrShare: self?.parent.saveFile?(item.path)
                        }
                    }
                }
                return UITextItem.MenuConfiguration(menu: UIMenu(title: items[0].path, children: actions))
            }

            func textView(_: UITextView, primaryActionFor textItem: UITextItem, defaultAction: UIAction) -> UIAction? {
                guard case let .link(url) = textItem.content else { return defaultAction }
                return UIAction { [weak self] _ in self?.parent.openURL(url) }
            }
        }
    }

    enum ConversationAttributedText {
        static func attributedText(_ blocks: [ConversationMarkdownContent.Block],
                                   dark: Bool) -> NSAttributedString {
            let text = NSMutableAttributedString(string: "")
            for (index, block) in blocks.enumerated() {
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
                        text.append(fileChip(file, link: link, font: font, dark: dark))
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
                // TextKit needs the paragraph's font and style on its terminating newline too.
                // Keep the separator outside decoration/copy ranges so copying retains line breaks.
                if index + 1 < blocks.count {
                    text.append(NSAttributedString(string: "\n", attributes: [.font: font]))
                }
                text.addAttribute(
                    .paragraphStyle,
                    value: paragraph,
                    range: NSRange(location: start, length: text.length - start)
                )
            }
            return text
        }

        private static func attributes(for run: MarkdownRun, font: UIFont,
                                       dark: Bool) -> [NSAttributedString.Key: Any] {
            var runFont = font
            if run.code {
                runFont = UIFont.monospacedSystemFont(
                    ofSize: font.pointSize,
                    weight: .regular
                )
            }
            if run.strong || run.emphasis {
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

        private static let copySource = NSAttributedString.Key("agent.markdown-copy")

        private static func fileChip(_ file: MarkdownFileReference, link: String, font: UIFont,
                                     dark: Bool) -> NSAttributedString {
            let attachment = FileChipAttachment(label: file.label, path: file.path, font: font,
                                                color: mobileFileAccent(dark: dark))
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
            selected.enumerateAttribute(copySource, in: NSRange(
                location: 0,
                length: selected.length
            )) { value, range, _ in
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

    /// A file chip as wide as its label, truncated to the line TextKit lays it out on,
    /// so the attributed text does not depend on the view's width.
    private final class FileChipAttachment: NSTextAttachment {
        private let label: String
        private let path: String
        private let font: UIFont
        private let color: UIColor
        private var rendered: UIImage?

        init(label: String, path: String, font: UIFont, color: UIColor) {
            self.label = label
            self.path = path
            self.font = font
            self.color = color
            super.init(data: nil, ofType: nil)
        }

        @available(*, unavailable)
        required init?(coder _: NSCoder) {
            fatalError("init(coder:) has not been implemented")
        }

        private var labelFont: UIFont {
            .systemFont(ofSize: font.pointSize, weight: .medium)
        }

        private var height: CGFloat {
            ceil(labelFont.lineHeight + 4)
        }

        override func attachmentBounds(for _: NSTextContainer?, proposedLineFragment lineFrag: CGRect,
                                       glyphPosition _: CGPoint, characterIndex _: Int) -> CGRect {
            let iconSize = font.pointSize
            let labelWidth = (label as NSString).size(withAttributes: [.font: labelFont]).width
            let width = max(iconSize + 10, min(ceil(labelWidth + iconSize + 14), lineFrag.width))
            return CGRect(x: 0, y: font.descender - 2, width: width, height: height)
        }

        override func image(forBounds imageBounds: CGRect, textContainer _: NSTextContainer?,
                            characterIndex _: Int) -> UIImage? {
            if let rendered, rendered.size == imageBounds.size {
                return rendered
            }
            let size = imageBounds.size
            let iconSize = font.pointSize
            let labelFont = labelFont
            let image = UIGraphicsImageRenderer(size: size).image { _ in
                mobileFileImage(path).draw(in: CGRect(x: 5, y: (size.height - iconSize) / 2,
                                                      width: iconSize, height: iconSize))
                let paragraph = NSMutableParagraphStyle()
                paragraph.lineBreakMode = .byTruncatingTail
                (label as NSString).draw(
                    in: CGRect(x: iconSize + 9, y: (size.height - labelFont.lineHeight) / 2,
                               width: size.width - iconSize - 14, height: labelFont.lineHeight),
                    withAttributes: [.font: labelFont, .foregroundColor: color, .paragraphStyle: paragraph]
                )
            }
            rendered = image
            return image
        }
    }

#endif
