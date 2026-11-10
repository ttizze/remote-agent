import Foundation
#if os(macOS)
    import AppKit
#else
    import UIKit
#endif

typealias MessageCallback = @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<UInt8>?, Int) -> Void

/// TextKit owns selection, copying, hit testing and glyph measurement. NSH receives only its intrinsic size and
/// actions.
@MainActor final class MessageTextOwner {
    let context: UnsafeMutableRawPointer?
    var callback: MessageCallback?
    let view = MessageTextContainer()

    init(context: UnsafeMutableRawPointer?, callback: @escaping MessageCallback) {
        self.context = context
        self.callback = callback
        let send: ([String: Any]) -> Void = { [weak self] action in
            guard let self, let data = try? JSONSerialization.data(withJSONObject: action) else { return }
            data.withUnsafeBytes { self.callback?(self.context, $0.bindMemory(to: UInt8.self).baseAddress, data.count) }
        }
        view.send = send
        view.text.action = send
    }

    func update(_ data: Data) {
        do { try view.update(JSONDecoder().decode(MessageDocument.self, from: data)) }
        catch {
            // Keep decoder failures visible rather than silently dropping a message.
            view.text.stringValueForFailure(error.localizedDescription)
        }
    }
}

#if os(macOS)
    final class MessageTextView: NSTextView, NSTextViewDelegate {
        var action: (([String: Any]) -> Void)?
        var document = MessageDocument.empty
        /// The modifier keys of the click being handled: a link opens from
        /// within it, where `NSApp.currentEvent` may be another event.
        private var clickModifiers: NSEvent.ModifierFlags = []
        /// A drag is choosing the selection: only where it ends is reported.
        private var selecting = false
        /// NSH was last told of a selection, not of its end.
        private var reported = false
        private var geometryObservers: [NSKeyValueObservation] = []
        private var lastVisibleRect: NSRect = .zero
        func watchScrollingGeometry() {
            geometryObservers.removeAll()
            lastVisibleRect = .zero
            guard window != nil else { return }
            let redraw: @Sendable () -> Void = { [weak self] in
                MainActor.assumeIsolated {
                    self?.invalidateNewlyVisibleText()
                }
            }
            var ancestor = superview
            while let current = ancestor {
                if let layer = current.layer {
                    // A prepend moves rows and then compensates the scroll offset.
                    // Track both halves, including the deferred bounds commit: a
                    // transiently clipped text view cannot retain its old pixels.
                    geometryObservers.append(layer.observe(\.position, options: [.old, .new]) { _, change in
                        if change.oldValue != change.newValue {
                            redraw()
                        }
                    })
                    geometryObservers.append(layer.observe(\.bounds, options: [.old, .new]) { _, change in
                        if change.oldValue != change.newValue {
                            redraw()
                        }
                    })
                    geometryObservers.append(layer.observe(\.sublayerTransform, options: [.old, .new]) { _, change in
                        if let old = change.oldValue, let new = change.newValue,
                           !CATransform3DEqualToTransform(old, new) {
                            redraw()
                        }
                    })
                }
                ancestor = current.superview
            }
            invalidateNewlyVisibleText()
        }

        private func invalidateNewlyVisibleText() {
            let current = visibleRect
            let previous = lastVisibleRect
            lastVisibleRect = current
            // Retain the overlapping glyph pixels. Layer-only scrolling only
            // needs the exposed strips; moving a fully visible cell needs none.
            for rect in Self.newlyExposedRects(previous: previous, current: current) {
                setNeedsDisplay(rect)
            }
        }

        static func newlyExposedRects(previous: NSRect, current: NSRect) -> [NSRect] {
            guard !current.isEmpty else { return [] }
            let overlap = previous.intersection(current)
            guard !overlap.isEmpty else { return [current] }
            var rects: [NSRect] = []
            if current.minY < overlap.minY {
                rects.append(NSRect(x: current.minX, y: current.minY, width: current.width,
                                    height: overlap.minY - current.minY))
            }
            if overlap.maxY < current.maxY {
                rects.append(NSRect(x: current.minX, y: overlap.maxY, width: current.width,
                                    height: current.maxY - overlap.maxY))
            }
            if current.minX < overlap.minX {
                rects.append(NSRect(x: current.minX, y: overlap.minY, width: overlap.minX - current.minX,
                                    height: overlap.height))
            }
            if overlap.maxX < current.maxX {
                rects.append(NSRect(x: overlap.maxX, y: overlap.minY, width: current.maxX - overlap.maxX,
                                    height: overlap.height))
            }
            return rects
        }

        /// NSH scrolls a container's sublayers before committing its NSView
        /// bounds. TextKit's inherited visibleRect follows those stale bounds
        /// and suppresses glyphs until the trailing scroll commit. Convert the
        /// actual layer clips instead, keeping TextKit on the displayed frame.
        override var visibleRect: NSRect {
            guard let own = layer, let root = window?.contentView?.layer else { return super.visibleRect }
            var rect = bounds.intersection(own.convert(root.bounds, from: root))
            var ancestor = own.superlayer
            while let current = ancestor {
                if current.masksToBounds || current.mask != nil {
                    rect = rect.intersection(own.convert(current.bounds, from: current))
                }
                if current === root {
                    break
                }
                ancestor = current.superlayer
            }
            return rect.isNull ? .zero : rect
        }

        #if DEBUG
            /// Acceptance observes completed native draws, rather than text merely
            /// published to accessibility (which includes offscreen list cells).
            private var paints = RenderProbe.Paints()

            @objc func bexPaintedAtForText(_ expected: String, since: TimeInterval, visiblePrefix: Bool) -> Double {
                guard let manager = layoutManager, let container = textContainer,
                      !visibleRect.isEmpty else { return 0 }
                let glyphs = manager.glyphRange(forBoundingRect: visibleRect, in: container)
                let range = manager.characterRange(forGlyphRange: glyphs, actualGlyphRange: nil)
                let visible = (string as NSString).substring(with: range)
                if visiblePrefix {
                    return paints.firstVisible(prefix: expected, in: visible) ?? 0
                }
                guard visible.contains(expected) else { return 0 }
                return paints.first(for: expected, since: since) ?? 0
            }

            func resetPaints() {
                paints.clear()
            }

            func preparePaints(for content: String) {
                paints.update(previous: string, current: content)
            }

            override func draw(_ dirtyRect: NSRect) {
                super.draw(dirtyRect)
                guard RenderProbe.enabled || RenderProbe.timings,
                      let manager = layoutManager, let container = textContainer else { return }
                let rect = visibleRect.intersection(dirtyRect)
                guard !rect.isEmpty else { return }
                let glyphs = manager.glyphRange(forBoundingRect: rect, in: container)
                let range = manager.characterRange(forGlyphRange: glyphs, actualGlyphRange: nil)
                let painted = RenderProbe.enabled ? (string as NSString).substring(with: range) : ""
                if RenderProbe.enabled {
                    paints.record(painted, at: Date.timeIntervalSinceReferenceDate)
                }
                RenderProbe.record("draw", self, painted: painted)
            }

        #endif

        init() {
            let storage = NSTextStorage()
            let manager = NSLayoutManager()
            let container = NSTextContainer(size: .zero)
            storage.addLayoutManager(manager)
            manager.addTextContainer(container)
            super.init(frame: .zero, textContainer: container)
            wantsLayer = true
            isEditable = false
            isSelectable = true
            drawsBackground = false
            isRichText = true
            isHorizontallyResizable = false
            isVerticallyResizable = false
            textContainerInset = .zero
            textContainer?.lineFragmentPadding = 0
            textContainer?.widthTracksTextView = false
            // Read-only text needs no checking; it otherwise analyzes every message off the main thread.
            isContinuousSpellCheckingEnabled = false
            isGrammarCheckingEnabled = false
            isAutomaticSpellingCorrectionEnabled = false
            isAutomaticTextReplacementEnabled = false
            isAutomaticDataDetectionEnabled = false
            isAutomaticLinkDetectionEnabled = false
            isAutomaticTextCompletionEnabled = false
            enabledTextCheckingTypes = 0
            // Runs carry their own link look: a file chip keeps its accent.
            linkTextAttributes = [.cursor: NSCursor.pointingHand]
            delegate = self
        }

        @available(*, unavailable)
        required init?(coder _: NSCoder) {
            fatalError("init(coder:) is unavailable")
        }

        override func mouseDown(with event: NSEvent) {
            clickModifiers = event.modifierFlags
            selecting = true
            defer {
                clickModifiers = []
                selecting = false
                reportSelection()
            }
            // AppKit tracks the drag in here until the button is released.
            super.mouseDown(with: event)
        }

        func textViewDidChangeSelection(_: Notification) {
            if !selecting {
                reportSelection()
            }
        }

        override func resignFirstResponder() -> Bool {
            let resigned = super.resignFirstResponder()
            if resigned, reported {
                reported = false
                action?(["action": "selection", "text": ""])
            }
            return resigned
        }

        /// Tells NSH what is selected and where, in the window's content from
        /// its top left, for the bar of actions over it; or that nothing is.
        private func reportSelection() {
            let range = selectedRange()
            guard range.length > 0, NSMaxRange(range) <= (textStorage?.length ?? 0),
                  let manager = layoutManager, let container = textContainer,
                  let content = window?.contentView
            else {
                if reported {
                    reported = false
                    action?(["action": "selection", "text": ""])
                }
                return
            }
            let glyphs = manager.glyphRange(forCharacterRange: range, actualCharacterRange: nil)
            let box = manager.boundingRect(forGlyphRange: glyphs, in: container)
                .offsetBy(dx: textContainerOrigin.x, dy: textContainerOrigin.y)
            let frame = content.convert(box, from: self)
            let top = content.isFlipped ? frame.minY : content.bounds.height - frame.maxY
            reported = true
            action?(["action": "selection", "text": (string as NSString).substring(with: range),
                     "target": document.target, "sideChat": document.sideChat,
                     "bounds": [frame.minX, top, frame.width, frame.height]])
        }

        override func menu(for event: NSEvent) -> NSMenu? {
            if let chip = chip(at: convert(event.locationInWindow, from: nil)) {
                return fileMenu(chip.file.menu, link: chip.link)
            }
            guard let menu = super.menu(for: event) else { return nil }
            let range = selectedRange()
            guard range.length > 0, NSMaxRange(range) <= (textStorage?.length ?? 0) else { return menu }
            let selected = (string as NSString).substring(with: range)
            let target = document.target
            let quote = NSMenuItem(title: document.menu.quote, action: #selector(quoteSelection(_:)), keyEquivalent: "")
            quote.target = self
            quote.representedObject = ["action": "quote", "text": selected, "target": target]
            menu.insertItem(quote, at: 0)
            if document.sideChat {
                let side = NSMenuItem(
                    title: document.menu.sideChat,
                    action: #selector(quoteSelection(_:)),
                    keyEquivalent: ""
                )
                side.target = self
                side.representedObject = ["action": "sideChat", "text": selected, "target": target]
                menu.insertItem(side, at: 1)
            }
            menu.insertItem(.separator(), at: document.sideChat ? 2 : 1)
            return menu
        }

        @objc private func quoteSelection(_ sender: NSMenuItem) {
            guard let payload = sender.representedObject as? [String: Any] else { return }
            action?(payload)
        }

        /// The chip under `point`, in the view's coordinates.
        func chip(at point: CGPoint) -> MessageFileLink? {
            guard let manager = layoutManager, let container = textContainer, let storage = textStorage,
                  storage.length > 0 else { return nil }
            var fraction: CGFloat = 0
            let glyph = manager.glyphIndex(for: point, in: container, fractionOfDistanceThroughGlyph: &fraction)
            guard manager.boundingRect(forGlyphRange: NSRange(location: glyph, length: 1), in: container)
                .contains(point) else { return nil }
            let index = manager.characterIndexForGlyph(at: glyph)
            guard index < storage.length else { return nil }
            return storage.attribute(.messageFile, at: index, effectiveRange: nil) as? MessageFileLink
        }

        /// A chip's menu as Core lists it; an item reports its id with the chip's link.
        func fileMenu(_ items: [MessageMenuItem], link: String) -> NSMenu {
            let menu = NSMenu()
            for item in items {
                let entry = NSMenuItem(title: item.title, action: nil, keyEquivalent: "")
                if let children = item.children {
                    entry.submenu = fileMenu(children, link: link)
                } else if let id = item.id {
                    entry.action = #selector(quoteSelection(_:))
                    entry.target = self
                    entry.representedObject = ["action": "file", "text": link, "id": id, "target": document.target]
                }
                menu.addItem(entry)
            }
            return menu
        }

        func textView(_: NSTextView, clickedOnLink link: Any, at index: Int) -> Bool {
            // A chip opens its authored link, which Core resolves again.
            let chip = index < (textStorage?.length ?? 0)
                ? textStorage?.attribute(.messageFile, at: index, effectiveRange: nil) as? MessageFileLink : nil
            let url = chip?.link ?? (link as? URL)?.absoluteString ?? (link as? String) ?? ""
            // Command-click sends a file to the editor and a web page to the in-app
            // browser, as the desktop client's links did.
            action?(["action": "link", "text": url, "target": document.target,
                     "command": clickModifiers.contains(.command)])
            return true
        }

        func stringValueForFailure(_ reason: String) {
            string = reason
        }
    }

    final class MessageTextContainer: NSView {
        let text = MessageTextView()
        var send: (([String: Any]) -> Void)?
        private var measuredSize = CGSize(width: -1, height: -1)
        private var revealedFind: String?
        override var isFlipped: Bool {
            true
        }

        init() {
            super.init(frame: .zero)
            addSubview(text)
        }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            text.watchScrollingGeometry()
        }

        override func viewDidMoveToSuperview() {
            super.viewDidMoveToSuperview()
            text.watchScrollingGeometry()
        }

        @available(*, unavailable)
        required init?(coder _: NSCoder) {
            fatalError("init(coder:) is unavailable")
        }

        func update(_ document: MessageDocument) {
            guard text.document != document else { return }
            if text.document.findKey != document.findKey {
                revealedFind = nil
            }
            #if DEBUG
                RenderProbe.record("update.begin", text)
            #endif
            #if DEBUG
                if document.target != text.document.target || document.role != text.document.role
                    || document.maxWidth != text.document.maxWidth {
                    text.resetPaints()
                }
            #endif
            text.document = document
            text.setAccessibilityIdentifier("message.\(document.role)-text")
            refresh()
            #if DEBUG
                RenderProbe.record("update", text)
            #endif
        }

        override func viewDidChangeEffectiveAppearance() {
            super.viewDidChangeEffectiveAppearance(); refresh()
        }

        private func reportSize(_ size: CGSize) {
            guard measuredSize != size else { return }
            measuredSize = size
            send?(["measuredWidth": size.width, "measuredHeight": size.height])
        }

        private func refresh() {
            let document = text.document
            let content = MessageAttributes.text(document.blocks, secondary: document.secondary,
                                                 dark: effectiveAppearance
                                                     .bestMatch(from: [.darkAqua, .aqua]) == .darkAqua,
                                                 answer: document.role == "assistant", cell: document.cell,
                                                 foreground: document.foreground)
            let range = text.selectedRange()
            let keep = range.length > 0 && NSMaxRange(range) <= content.length && NSMaxRange(range) <= text.string.utf16
                .count
                && (text.string as NSString).substring(with: range) == (content.string as NSString)
                .substring(with: range)
            #if DEBUG
                if RenderProbe.enabled {
                    text.preparePaints(for: content.string)
                }
            #endif
            text.textStorage?.setAttributedString(content)
            // The natural size comes from TextKit's own layout, whose glyphs the
            // wrapped layout below reuses, rather than a second typesetting pass.
            if let container = text.textContainer, let manager = text.layoutManager {
                container.containerSize = CGSize(width: 1_000_000, height: CGFloat.greatestFiniteMagnitude)
                manager.ensureLayout(for: container)
                let size = manager.usedRect(for: container).size
                let width = min(ceil(size.width), document.maxWidth)
                container.containerSize = CGSize(width: max(1, width), height: .greatestFiniteMagnitude)
                manager.ensureLayout(for: container)
                reportSize(CGSize(width: ceil(size.width), height: ceil(manager.usedRect(for: container).height)))
            }
            text.setSelectedRange(keep ? range : NSRange(location: 0, length: 0))
            needsLayout = true
        }

        override func layout() {
            super.layout()
            guard bounds.width > 0, let container = text.textContainer, let manager = text.layoutManager else { return }
            // A recycled cell still has its previous row's frame until the
            // engine applies the size above. Never measure the new document at
            // that stale width or feed its transient height back into the list.
            let width = min(measuredSize.width, text.document.maxWidth)
            container.containerSize = CGSize(width: max(1, width), height: .greatestFiniteMagnitude)
            manager.ensureLayout(for: container)
            let height = ceil(manager.usedRect(for: container).maxY)
            text.frame = CGRect(x: 0, y: 0, width: bounds.width, height: height)
            reportSize(CGSize(width: max(0, measuredSize.width), height: height))
            revealFind(manager, container: container)
            #if DEBUG
                RenderProbe.record("layout", text)
            #endif
        }

        private func revealFind(_ manager: NSLayoutManager, container: NSTextContainer) {
            guard let key = text.document.findKey, key != revealedFind,
                  let storage = text.textStorage else { return }
            var selected: NSRange?
            storage.enumerateAttribute(.messageFindActive, in: NSRange(
                location: 0,
                length: storage.length
            )) { value, range, stop in
                if value as? Bool == true {
                    selected = range; stop.pointee = true
                }
            }
            guard let selected else { return }
            let glyphs = manager.glyphRange(forCharacterRange: selected, actualCharacterRange: nil)
            let bounds = manager.boundingRect(forGlyphRange: glyphs, in: container)
            revealedFind = key
            send?(["findBounds": [bounds.minX, bounds.minY, bounds.width, bounds.height], "findKey": key])
        }
    }
#else
    final class MessageTextView: UITextView, UITextViewDelegate {
        var action: (([String: Any]) -> Void)?
        var document = MessageDocument.empty

        init() {
            super.init(frame: .zero, textContainer: nil)
            isEditable = false
            isSelectable = true
            isScrollEnabled = false
            backgroundColor = .clear
            textContainerInset = .zero
            textContainer.lineFragmentPadding = 0
            delegate = self
        }

        @available(*, unavailable)
        required init?(coder _: NSCoder) {
            fatalError("init(coder:) is unavailable")
        }

        func textView(_ textView: UITextView, editMenuForTextIn range: NSRange,
                      suggestedActions: [UIMenuElement]) -> UIMenu? {
            guard range.length > 0, NSMaxRange(range) <= textView.attributedText.length else { return nil }
            let selected = (textView.text as NSString).substring(with: range)
            let target = document.target
            var actions: [UIMenuElement] = [UIAction(title: document.menu.quote, image: UIImage(systemName: "bubble")) {
                [weak self] _ in self?.action?(["action": "quote", "text": selected, "target": target])
            }]
            if document.sideChat {
                actions.append(UIAction(
                    title: document.menu.sideChat,
                    image: UIImage(systemName: "bubble.left.and.bubble.right")
                ) {
                    [weak self] _ in self?.action?(["action": "sideChat", "text": selected, "target": target])
                })
            }
            return UIMenu(children: actions + suggestedActions)
        }

        func textView(_: UITextView, primaryActionFor textItem: UITextItem, defaultAction: UIAction) -> UIAction? {
            guard case let .link(url) = textItem.content else { return defaultAction }
            let target = document.target
            return UIAction { [weak self] _ in
                self?.action?(["action": "link", "text": url.absoluteString, "target": target])
            }
        }

        func stringValueForFailure(_ reason: String) {
            text = reason
        }
    }

    final class MessageTextContainer: UIView {
        let text = MessageTextView()
        var send: (([String: Any]) -> Void)?
        private var measuredSize = CGSize(width: -1, height: -1)

        init() {
            super.init(frame: .zero)
            addSubview(text)
            registerForTraitChanges([UITraitUserInterfaceStyle.self, UITraitPreferredContentSizeCategory.self]) {
                (view: MessageTextContainer, _: UITraitCollection) in view.refresh()
            }
        }

        @available(*, unavailable)
        required init?(coder _: NSCoder) {
            fatalError("init(coder:) is unavailable")
        }

        func update(_ document: MessageDocument) {
            guard text.document != document else { return }
            text.document = document
            text.accessibilityIdentifier = "message.\(document.role)-text"
            refresh()
        }

        private func reportSize(_ size: CGSize) {
            guard measuredSize != size else { return }
            measuredSize = size
            send?(["measuredWidth": size.width, "measuredHeight": size.height])
        }

        private func refresh() {
            let document = text.document
            let content = MessageAttributes.text(document.blocks, secondary: document.secondary,
                                                 dark: traitCollection.userInterfaceStyle == .dark,
                                                 foreground: document.foreground)
            let range = text.selectedRange
            let keep = range.length > 0 && NSMaxRange(range) <= content.length && NSMaxRange(range) <= text
                .attributedText.length
                && (text.text as NSString).substring(with: range) == (content.string as NSString).substring(with: range)
            text.attributedText = content
            let size = content.size()
            reportSize(CGSize(width: ceil(size.width), height: ceil(size.height)))
            text.selectedRange = keep ? range : NSRange(location: 0, length: 0)
            setNeedsLayout()
        }

        override func layoutSubviews() {
            super.layoutSubviews()
            guard bounds.width > 0 else { return }
            let height = ceil(text.sizeThatFits(CGSize(width: bounds.width, height: .greatestFiniteMagnitude)).height)
            text.frame = CGRect(x: 0, y: 0, width: bounds.width, height: height)
            reportSize(CGSize(width: max(0, measuredSize.width), height: height))
        }
    }
#endif

@_cdecl("bex_nsh_message_text_create")
@MainActor public func messageTextCreate(
    _ context: UnsafeMutableRawPointer?,
    _ callback: @escaping @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<UInt8>?, Int) -> Void
) -> UnsafeMutableRawPointer {
    Unmanaged.passRetained(MessageTextOwner(context: context, callback: callback)).toOpaque()
}

@_cdecl("bex_nsh_message_text_view")
@MainActor public func messageTextView(_ handle: UnsafeMutableRawPointer) -> UnsafeMutableRawPointer {
    Unmanaged.passUnretained(Unmanaged<MessageTextOwner>.fromOpaque(handle).takeUnretainedValue().view).toOpaque()
}

@_cdecl("bex_nsh_message_text_update")
@MainActor public func messageTextUpdate(
    _ handle: UnsafeMutableRawPointer,
    _ bytes: UnsafePointer<UInt8>,
    _ count: Int
) {
    Unmanaged<MessageTextOwner>.fromOpaque(handle).takeUnretainedValue().update(Data(bytes: bytes, count: count))
}

@_cdecl("bex_nsh_message_text_release")
@MainActor public func messageTextRelease(_ handle: UnsafeMutableRawPointer) {
    let owner = Unmanaged<MessageTextOwner>.fromOpaque(handle).takeRetainedValue()
    owner.callback = nil
    owner.view.send = nil
    owner.view.text.action = nil
}
