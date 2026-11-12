#if !BEX_NSH_CONVERSATION
    import SwiftUI
    import UIKit

    /// One native text view keeps a single selection range across a whole message.
    /// Like T3, it is as tall as the message and never scrolls itself: the conversation owns scrolling,
    /// and UIKit tiles drawing to the visible part of a long message.
    final class ConversationTextView: UITextView {
        static let quote = NSAttributedString.Key("agent.quote")
        static let rule = NSAttributedString.Key("agent.rule")

        private var content: ConversationTextContent?
        /// UITextView does not retain storage that it did not create.
        private let storage = NSTextStorage()

        #if AGENT_DEV_DIAGNOSTICS
            private let diagnosticID = UUID()
            private var diagnosticWidth: CGFloat?
            private var diagnosticProbe: Task<Void, Never>?
            private var diagnosticInput: ConversationTextDiagnosticInput?
            private var diagnosticHadIssue = false
        #endif

        init() {
            // Decorations draw with the text, so they share its tiles instead of a message-sized backing store.
            let layoutManager = DecoratingLayoutManager()
            storage.addLayoutManager(layoutManager)
            #if AGENT_DEV_DIAGNOSTICS
                let container = ConversationTextContainer(size: CGSize(
                    width: 0,
                    height: CGFloat.greatestFiniteMagnitude
                ))
            #else
                let container = NSTextContainer(size: CGSize(width: 0, height: CGFloat.greatestFiniteMagnitude))
            #endif
            container.widthTracksTextView = true
            container.lineFragmentPadding = 0
            layoutManager.addTextContainer(container)
            super.init(frame: .zero, textContainer: container)
            #if AGENT_DEV_DIAGNOSTICS
                layoutManager.didDraw = { [weak self] in self?.scheduleDiagnostic() }
            #endif
            isScrollEnabled = false
            scrollsToTop = false
            isEditable = false
            isSelectable = true
            backgroundColor = .clear
            contentInsetAdjustmentBehavior = .never
            textContainerInset = .zero
            linkTextAttributes = [
                .foregroundColor: UIColor.systemBlue,
                .underlineStyle: NSUnderlineStyle.single.rawValue
            ]
        }

        @available(*, unavailable)
        required init?(coder _: NSCoder) {
            fatalError("init(coder:) has not been implemented")
        }

        func update(blocks: [ConversationMarkdownContent.Block], dark: Bool, sizeCategory: ContentSizeCategory) {
            if let content, content.blocks == blocks, content.dark == dark, content.sizeCategory == sizeCategory {
                return
            }
            content = ConversationTextContent(blocks: blocks, dark: dark, sizeCategory: sizeCategory)
            setContent(ConversationAttributedText.attributedText(blocks, dark: dark))
            invalidateIntrinsicContentSize()
        }

        /// UIKit measures the view that draws the message, so the size always fits what it shows.
        /// Reopened conversations share these sizes, keyed by what the view shows.
        func measure(width proposal: CGFloat?) -> CGSize {
            guard let content, !content.blocks.isEmpty else { return .zero }
            let proposedWidth = proposal.flatMap { $0.isFinite ? $0 : nil }
            let width = proposedWidth ?? .greatestFiniteMagnitude
            guard width > 0 else { return .zero }
            #if AGENT_DEV_DIAGNOSTICS
                diagnosticWidth = width
            #endif
            return ConversationTextSizes.size(content, width: width) {
                let measured = sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude))
                return CGSize(width: proposedWidth ?? measured.width, height: measured.height)
            }
        }

        #if AGENT_DEV_DIAGNOSTICS
            override func layoutSubviews() {
                super.layoutSubviews()
                scheduleDiagnostic()
            }

            private func scheduleDiagnostic() {
                guard window != nil, diagnosticProbe == nil else { return }
                // Coalesce streamed updates without generating glyphs or changing the live layout.
                diagnosticProbe = Task { @MainActor [weak self] in
                    try? await Task.sleep(for: .milliseconds(250))
                    guard let self else { return }
                    diagnosticProbe = nil
                    guard let window, let content, bounds.width > 0, bounds.height > 0 else { return }
                    // Offscreen tails may legitimately have no glyphs yet; do not lay out copies for them.
                    let tail = CGRect(x: bounds.minX, y: bounds.maxY - 1, width: bounds.width, height: 1)
                    guard convert(tail, to: window).intersects(window.bounds) else { return }
                    let input = ConversationTextDiagnosticInput(
                        content: ObjectIdentifier(content), size: bounds.size,
                        container: textContainer.size, offset: contentOffset, proposedWidth: diagnosticWidth,
                        firstUnlaid: layoutManager.firstUnlaidCharacterIndex()
                    )
                    guard input != diagnosticInput else { return }
                    diagnosticInput = input
                    let metrics = textLayoutDiagnostic()
                    let recovered = diagnosticHadIssue && !metrics.incomplete
                    diagnosticHadIssue = metrics.incomplete
                    await ConversationTextDiagnosticLog.shared.record(
                        metrics, recovered: recovered, snapshot: ConversationTextDiagnosticLog.includesNormalLayouts
                    )
                }
            }

            /// Reads the existing layout; the independent reference layout never touches the displayed text.
            func textLayoutDiagnostic() -> ConversationTextDiagnostic {
                let existing = layoutManager.glyphRange(
                    forBoundingRectWithoutAdditionalLayout: CGRect(origin: .zero, size: textContainer.size),
                    in: textContainer
                )
                let characters = layoutManager.characterRange(forGlyphRange: existing, actualGlyphRange: nil)
                let visible = (textStorage.string as NSString).rangeOfCharacter(
                    from: CharacterSet.whitespacesAndNewlines.inverted, options: .backwards
                )
                let requiredEnd = visible.location == NSNotFound ? 0 : NSMaxRange(visible)
                let placedEnd = NSMaxRange(characters)
                let measured = content.flatMap { content in
                    diagnosticWidth.flatMap { ConversationTextSizes.cached(content, width: $0) }
                }
                let tail = min(placedEnd, requiredEnd)
                let lastGlyph = tail > 0 ? layoutManager.glyphRange(
                    forCharacterRange: NSRange(location: tail - 1, length: 1), actualCharacterRange: nil
                ).location : nil
                let lastLineBottom = lastGlyph.map {
                    layoutManager.lineFragmentUsedRect(
                        forGlyphAt: $0,
                        effectiveRange: nil,
                        withoutAdditionalLayout: true
                    )
                    .maxY
                }
                var metrics = ConversationTextDiagnostic(
                    view: diagnosticID, characters: textStorage.length, requiredEnd: requiredEnd,
                    placedEnd: placedEnd, firstUnlaid: layoutManager.firstUnlaidCharacterIndex(),
                    frame: bounds.size, container: textContainer.size, contentSize: contentSize,
                    offset: contentOffset, measured: measured, scale: traitCollection.displayScale,
                    category: traitCollection.preferredContentSizeCategory.rawValue,
                    lastLineBottom: lastLineBottom, referenceHeight: nil
                )
                if metrics.incomplete {
                    metrics.referenceHeight = ConversationTextDiagnostic.referenceHeight(
                        textStorage,
                        width: bounds.width
                    )
                }
                return metrics
            }
        #endif

        /// Every copy path, including the edit menu and a hardware keyboard, copies the message's source text.
        override func copy(_: Any?) {
            guard selectedRange.length > 0 else { return }
            UIPasteboard.general.string = ConversationAttributedText.selectedText(attributedText, range: selectedRange)
        }

        private func setContent(_ content: NSAttributedString) {
            let range = selectedRange
            let keepSelection = range.length > 0 && NSMaxRange(range) <= textStorage.length
                && NSMaxRange(range) <= content.length
                && ConversationAttributedText.selectedText(attributedText, range: range) == ConversationAttributedText
                .selectedText(
                    content,
                    range: range
                )
            // Preserve TextKit's layout for completed paragraphs while the tail streams.
            // Compare attributes too: completing Markdown can restyle existing text.
            let prefix = Self.unchangedPrefix(textStorage, content)
            textStorage.replaceCharacters(
                in: NSRange(location: prefix, length: textStorage.length - prefix),
                with: content.attributedSubstring(from: NSRange(location: prefix, length: content.length - prefix))
            )
            selectedRange = keepSelection ? range : NSRange(location: 0, length: 0)
        }

        private static func unchangedPrefix(_ previous: NSAttributedString, _ next: NSAttributedString) -> Int {
            let limit = min(previous.length, next.length)
            let previousText = previous.string as NSString
            let nextText = next.string as NSString
            var offset = 0
            while offset < limit {
                var oldRange = NSRange()
                var newRange = NSRange()
                let oldAttributes = previous.attributes(at: offset, effectiveRange: &oldRange)
                let newAttributes = next.attributes(at: offset, effectiveRange: &newRange)
                guard NSDictionary(dictionary: oldAttributes).isEqual(to: newAttributes) else { break }
                let end = min(NSMaxRange(oldRange), NSMaxRange(newRange), limit)
                let range = NSRange(location: offset, length: end - offset)
                let oldText = previousText.substring(with: range)
                let newText = nextText.substring(with: range)
                let count = oldText.commonPrefix(with: newText, options: .literal).utf16.count
                offset += count
                if count < range.length {
                    break
                }
            }
            return offset
        }
    }

    #if AGENT_DEV_DIAGNOSTICS
        /// The conversation owns vertical scrolling. A message lays out all its lines even when SwiftUI
        /// rounds its display height down by a pixel. UITextView otherwise overwrites the initial height
        /// when scrolling is disabled, even with heightTracksTextView disabled.
        private final class ConversationTextContainer: NSTextContainer {
            override var size: CGSize {
                get { super.size }
                set {
                    let available = CGSize(width: newValue.width, height: CGFloat.greatestFiniteMagnitude)
                    if super.size != available {
                        super.size = available
                    }
                }
            }
        }
    #endif

    /// The message a text view shows, compared by value. Its hash is computed once
    /// because every size proposal looks it up.
    private final class ConversationTextContent: NSObject {
        let blocks: [ConversationMarkdownContent.Block]
        let dark: Bool
        let sizeCategory: ContentSizeCategory
        private let digest: Int

        init(blocks: [ConversationMarkdownContent.Block], dark: Bool, sizeCategory: ContentSizeCategory) {
            self.blocks = blocks
            self.dark = dark
            self.sizeCategory = sizeCategory
            var hasher = Hasher()
            hasher.combine(blocks)
            hasher.combine(dark)
            hasher.combine(sizeCategory)
            digest = hasher.finalize()
        }

        override var hash: Int {
            digest
        }

        override func isEqual(_ object: Any?) -> Bool {
            guard let other = object as? ConversationTextContent else { return false }
            return other === self || other.digest == digest && other.dark == dark
                && other.sizeCategory == sizeCategory && other.blocks == blocks
        }
    }

    /// Sizes UIKit measured for a message at one width, shared by every view showing the same content.
    /// Opening a conversation again does not remeasure its messages.
    @MainActor
    private enum ConversationTextSizes {
        private final class Key: NSObject {
            let content: ConversationTextContent
            let width: CGFloat

            init(_ content: ConversationTextContent, width: CGFloat) {
                self.content = content
                self.width = width
            }

            override var hash: Int {
                content.hash ^ width.hashValue
            }

            override func isEqual(_ object: Any?) -> Bool {
                guard let other = object as? Key else { return false }
                return other.width == width && other.content.isEqual(content)
            }
        }

        /// Bounded and released under memory pressure, so a resized window does not
        /// keep a size for every proposed width.
        private static let cache: NSCache<Key, NSValue> = {
            let cache = NSCache<Key, NSValue>()
            cache.countLimit = 1000
            return cache
        }()

        static func size(_ content: ConversationTextContent, width: CGFloat, measure: () -> CGSize) -> CGSize {
            let key = Key(content, width: width)
            if let cached = cache.object(forKey: key) {
                return cached.cgSizeValue
            }
            let size = measure()
            cache.setObject(NSValue(cgSize: size), forKey: key)
            return size
        }

        #if AGENT_DEV_DIAGNOSTICS
            static func cached(_ content: ConversationTextContent, width: CGFloat) -> CGSize? {
                cache.object(forKey: Key(content, width: width))?.cgSizeValue
            }
        #endif
    }

    /// Draws quote bars and rules beneath the glyphs of each drawn tile.
    private final class DecoratingLayoutManager: NSLayoutManager {
        #if AGENT_DEV_DIAGNOSTICS
            var didDraw: (() -> Void)?

            override func drawGlyphs(forGlyphRange glyphsToShow: NSRange, at origin: CGPoint) {
                super.drawGlyphs(forGlyphRange: glyphsToShow, at: origin)
                didDraw?()
            }
        #endif

        override func drawBackground(forGlyphRange glyphsToShow: NSRange, at origin: CGPoint) {
            super.drawBackground(forGlyphRange: glyphsToShow, at: origin)
            guard let textStorage, let container = textContainers.first else { return }
            let characters = characterRange(forGlyphRange: glyphsToShow, actualGlyphRange: nil)
            for attribute in [ConversationTextView.quote, ConversationTextView.rule] {
                textStorage.enumerateAttribute(attribute, in: characters) { value, range, _ in
                    guard value as? Bool == true else { return }
                    let glyphs = glyphRange(forCharacterRange: range, actualCharacterRange: nil)
                    let bounds = boundingRect(forGlyphRange: glyphs, in: container)
                    let line = UIBezierPath()
                    if attribute == ConversationTextView.quote {
                        let lineX = max(1, bounds.minX - 10) + origin.x
                        line.move(to: CGPoint(x: lineX, y: bounds.minY + origin.y))
                        line.addLine(to: CGPoint(x: lineX, y: bounds.maxY + origin.y))
                        line.lineWidth = 2
                    } else {
                        line.move(to: CGPoint(x: origin.x, y: bounds.midY + origin.y))
                        line.addLine(to: CGPoint(x: origin.x + container.size.width, y: bounds.midY + origin.y))
                        line.lineWidth = 0.5
                    }
                    UIColor.separator.setStroke()
                    line.stroke()
                }
            }
        }
    }

#endif
