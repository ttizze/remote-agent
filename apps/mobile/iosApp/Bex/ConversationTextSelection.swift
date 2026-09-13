import SwiftUI
import UIKit

struct ConversationSelectionActions {
    let addToChat: (String) -> Void
    let askInSideChat: ((String) -> Void)?
}

/// Assistant prose stays selectable in place; user bubbles have their own long-press menu.
struct AssistantSelectableText: UIViewRepresentable {
    let blocks: [ConversationMarkdown.Block]
    let actions: ConversationSelectionActions
    @Environment(\.openURL) private var openURL
    @Environment(\.sizeCategory) private var sizeCategory

    func makeCoordinator() -> Coordinator {
        Coordinator(self)
    }

    func makeUIView(context: Context) -> UITextView {
        let view = UITextView()
        view.isEditable = false
        view.isSelectable = true
        view.isScrollEnabled = false
        view.backgroundColor = .clear
        view.textContainerInset = .zero
        view.textContainer.lineFragmentPadding = 0
        view.delegate = context.coordinator
        view.accessibilityIdentifier = "message.assistant-text"
        view.linkTextAttributes = [.foregroundColor: UIColor.label, .underlineStyle: NSUnderlineStyle.single.rawValue]
        return view
    }

    func updateUIView(_ view: UITextView, context: Context) {
        let coordinator = context.coordinator
        coordinator.parent = self
        guard coordinator.blocks != blocks || coordinator.sizeCategory != sizeCategory else { return }
        coordinator.blocks = blocks
        coordinator.sizeCategory = sizeCategory
        let content = Self.attributedText(blocks)
        let range = view.selectedRange
        let old = view.text as NSString
        let next = content.string as NSString
        let keepSelection = range.length > 0 && NSMaxRange(range) <= old.length && NSMaxRange(range) <= next.length
            && old.substring(with: range) == next.substring(with: range)
        view.textStorage.setAttributedString(content)
        view.selectedRange = keepSelection ? range : NSRange(location: 0, length: 0)
        view.invalidateIntrinsicContentSize()
    }

    func sizeThatFits(_ proposal: ProposedViewSize, uiView: UITextView, context _: Context) -> CGSize? {
        let width = proposal.width ?? ceil(uiView.attributedText.size().width)
        guard width > 0 else { return nil }
        return uiView.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude))
    }

    private static func attributedText(_ blocks: [ConversationMarkdown.Block]) -> NSAttributedString {
        let text = NSMutableAttributedString(string: "")
        for (index, block) in blocks.enumerated() {
            if index > 0 {
                text.append(NSAttributedString(string: "\n"))
            }
            let start = text.length
            let style = block.style
            let size: CGFloat = style.code ? 15 : style.header == 1 ? 25 : style.header == nil ? 18 : 21
            let base = style.code ? UIFont.monospacedSystemFont(ofSize: size, weight: .regular)
                : UIFont.systemFont(ofSize: size, weight: style.header == nil ? .regular : .semibold)
            let font = UIFontMetrics.default.scaledFont(for: base)
            if let marker = style.marker {
                text.append(NSAttributedString(string: marker + " ", attributes: [.font: font]))
            }
            for run in block.content.runs {
                text.append(NSAttributedString(
                    string: String(block.content[run.range].characters),
                    attributes: attributes(for: run, font: font)
                ))
            }
            let paragraph = NSMutableParagraphStyle()
            paragraph.lineSpacing = style.code ? 0 : 5
            paragraph.paragraphSpacing = index + 1 == blocks.count ? 0 : 14
            paragraph.headIndent = style.marker == nil ? (style.quoted ? 12 : 0) : 20
            paragraph.firstLineHeadIndent = style.quoted ? 12 : 0
            text.addAttribute(
                .paragraphStyle,
                value: paragraph,
                range: NSRange(location: start, length: text.length - start)
            )
        }
        return text
    }

    private static func attributes(for run: AttributedString.Runs.Run, font: UIFont) -> [NSAttributedString.Key: Any] {
        var runFont = font
        let intent = run.inlinePresentationIntent ?? []
        if intent.contains(.code) {
            runFont = UIFont.monospacedSystemFont(
                ofSize: font.pointSize,
                weight: .regular
            )
        }
        var traits = runFont.fontDescriptor.symbolicTraits
        if intent.contains(.stronglyEmphasized) {
            traits.insert(.traitBold)
        }
        if intent.contains(.emphasized) {
            traits.insert(.traitItalic)
        }
        if let descriptor = runFont.fontDescriptor.withSymbolicTraits(traits) {
            runFont = UIFont(descriptor: descriptor, size: runFont.pointSize)
        }
        var attributes: [NSAttributedString.Key: Any] = [.font: runFont, .foregroundColor: UIColor.label]
        if let link = run.link {
            attributes[.link] = link
        }
        if intent
            .contains(.strikethrough) {
            attributes[.strikethroughStyle] = NSUnderlineStyle.single.rawValue
        }
        if intent.contains(.code) {
            attributes[.backgroundColor] = UIColor.secondarySystemBackground
        }
        return attributes
    }

    final class Coordinator: NSObject, UITextViewDelegate {
        var parent: AssistantSelectableText
        var blocks: [ConversationMarkdown.Block] = []
        var sizeCategory: ContentSizeCategory?

        init(_ parent: AssistantSelectableText) {
            self.parent = parent
        }

        func textView(_ textView: UITextView, editMenuForTextIn range: NSRange,
                      suggestedActions: [UIMenuElement]) -> UIMenu? {
            guard range.length > 0, NSMaxRange(range) <= (textView.text as NSString).length else { return nil }
            let text = (textView.text as NSString).substring(with: range)
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

        func textView(_: UITextView, primaryActionFor textItem: UITextItem, defaultAction: UIAction) -> UIAction? {
            guard case let .link(url) = textItem.content else { return defaultAction }
            return UIAction { [weak self] _ in self?.parent.openURL(url) }
        }
    }
}
