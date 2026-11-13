import AppKit

typealias MessageCallback = @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<UInt8>?, Int) -> Void

final class MessageTextView: NSTextView, NSTextViewDelegate {
    var action: (([String: Any]) -> Void)?
    var document = MessageDocument.empty
    private var clickModifiers: NSEvent.ModifierFlags = []
    private var selecting = false
    private var reported = false
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

    /// Reports the native selection and its box in the window's content from
    /// its top left, for the bar of actions over it; or that nothing is.
    private func reportSelection() {
        let range = selectedRange()
        guard let storage = textStorage, range.length > 0, NSMaxRange(range) <= storage.length,
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
        action?(["action": "selection", "text": MessageAttributes.selectedText(storage, range: range),
                 "target": document.target, "sideChat": document.sideChat,
                 "menu": [
                     "quote": document.menu.quote,
                     "explain": document.menu.explain ?? "",
                     "sideChat": document.menu.sideChat
                 ],
                 "bounds": [Double(frame.minX), Double(top), Double(frame.width), Double(frame.height)]])
    }

    override func menu(for event: NSEvent) -> NSMenu? {
        let point = convert(event.locationInWindow, from: nil)
        if let chip = chip(at: point) {
            return fileMenu(chip.file.menu, link: chip.link)
        }
        if let link = attribute(.messageWebLink, at: point) as? MessageWebLink {
            return fileMenu(link.menu, link: link.link, action: "webLink")
        }
        guard let menu = super.menu(for: event) else { return nil }
        let range = selectedRange()
        guard let storage = textStorage, range.length > 0, NSMaxRange(range) <= storage.length else { return menu }
        let selected = MessageAttributes.selectedText(storage, range: range)
        let target = document.target
        let quote = NSMenuItem(title: document.menu.quote, action: #selector(quoteSelection(_:)), keyEquivalent: "")
        quote.target = self
        quote.representedObject = ["action": "quote", "text": selected, "target": target]
        menu.insertItem(quote, at: 0)
        if let title = document.menu.explain, !title.isEmpty {
            let explain = NSMenuItem(title: title, action: #selector(quoteSelection(_:)), keyEquivalent: "")
            explain.target = self; explain.representedObject = ["action": "explain", "text": selected, "target": target]
            menu.insertItem(explain, at: 1)
        }
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

    override func copy(_: Any?) {
        let range = selectedRange()
        guard let storage = textStorage, range.length > 0, NSMaxRange(range) <= storage.length else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(MessageAttributes.selectedText(storage, range: range), forType: .string)
    }

    @objc private func quoteSelection(_ sender: NSMenuItem) {
        guard let payload = sender.representedObject as? [String: Any] else { return }
        action?(payload)
    }

    /// The chip under `point`, in the view's coordinates.
    func chip(at point: CGPoint) -> MessageFileLink? {
        attribute(.messageFile, at: point) as? MessageFileLink
    }

    private func attribute(_ key: NSAttributedString.Key, at point: CGPoint) -> Any? {
        guard let manager = layoutManager, let container = textContainer, let storage = textStorage,
              storage.length > 0 else { return nil }
        var fraction: CGFloat = 0
        let glyph = manager.glyphIndex(for: point, in: container, fractionOfDistanceThroughGlyph: &fraction)
        guard manager.boundingRect(forGlyphRange: NSRange(location: glyph, length: 1), in: container)
            .contains(point) else { return nil }
        let index = manager.characterIndexForGlyph(at: glyph)
        guard index < storage.length else { return nil }
        return storage.attribute(key, at: index, effectiveRange: nil)
    }

    /// A chip's menu as Core lists it; an item reports its id with the chip's link.
    func fileMenu(_ items: [MessageMenuItem], link: String, action: String = "file") -> NSMenu {
        let menu = NSMenu()
        for item in items {
            let entry = NSMenuItem(title: item.title, action: nil, keyEquivalent: "")
            if let children = item.children {
                entry.submenu = fileMenu(children, link: link, action: action)
            } else if let id = item.id {
                entry.action = #selector(quoteSelection(_:))
                entry.target = self
                entry.representedObject = ["action": action, "text": link, "id": id, "target": document.target]
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
        // Core applies the web preference and Command's external override;
        // files retain their separate editor shortcut.
        action?(["action": "link", "text": url, "target": document.target,
                 "command": clickModifiers.contains(.command)])
        return true
    }
}
