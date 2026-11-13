import AppKit

indirect enum TranscriptPart: Decodable, Equatable {
    struct Code: Decodable, Equatable {
        let label: String
        let source: String
        let blocks: [MessageParagraph]
        let diagram: Diagram?
    }

    struct Diagram: Decodable, Equatable { let svg: String; let png: String; let width: Double; let height: Double }
    struct Table: Decodable, Equatable { let source: String; let csv: String; let rows: [[[TranscriptPart]]] }
    struct Image: Decodable, Equatable { let source: String }
    struct Link: Decodable, Equatable { let url: String }
    case text([MessageParagraph]), code(Code), table(Table), image(Image), link(Link), rule

    init(from decoder: Decoder) throws {
        let value = try decoder.container(keyedBy: Key.self)
        let key = try value.allKeys.first.unwrap()
        switch key.stringValue {
        case "Text": self = try .text(value.decode([MessageParagraph].self, forKey: key))
        case "Code": self = try .code(value.decode(Code.self, forKey: key))
        case "Table": self = try .table(value.decode(Table.self, forKey: key))
        case "Image": self = try .image(value.decode(Image.self, forKey: key))
        case "Link": self = try .link(value.decode(Link.self, forKey: key))
        case "Rule": self = .rule
        default: throw DecodingError.dataCorruptedError(
                forKey: key,
                in: value,
                debugDescription: "Unknown transcript part"
            )
        }
    }

    private struct Key: CodingKey {
        let stringValue: String
        var intValue: Int? {
            nil
        }

        init?(stringValue: String) {
            self.stringValue = stringValue
        }

        init?(intValue _: Int) {
            nil
        }
    }
}

private extension Optional {
    func unwrap() throws -> Wrapped {
        guard let self else { throw CocoaError(.coderReadCorrupt) }
        return self
    }
}

@MainActor final class TranscriptBody: NSView {
    private var parts: [TranscriptPart] = []
    private var blocks: [TranscriptBlock] = []
    private var document = MessageDocument.empty
    private var media: [String: String] = [:]
    var send: (([String: Any]) -> Void)?
    var resized: (() -> Void)?
    override var isFlipped: Bool {
        true
    }

    func update(_ parts: [TranscriptPart], document: MessageDocument, media: [String: String]) {
        let contentChanged = self.parts != parts || self.media != media
        self.document = document
        self.media = media
        if contentChanged {
            // Preserve native selection in unchanged paragraphs during streaming.
            let previous = self.parts
            var next: [TranscriptBlock] = []
            for (index, part) in parts.enumerated() {
                let block = index < previous.count && (previous[index] == part || blocks[index].replace(part))
                    ? blocks[index] : TranscriptBlock.make(part)
                if block.superview !== self {
                    addSubview(block)
                }
                next.append(block)
            }
            for block in blocks where !next.contains(where: { $0 === block }) {
                block.removeFromSuperview()
            }
            blocks = next
            self.parts = parts
        }
        for block in blocks {
            block.send = { [weak self] in self?.send?($0) }
            block.resized = { [weak self] in self?.resized?() }
            block.update(document: document, media: media)
        }
    }

    func preferredWidth(_ maximum: CGFloat) -> CGFloat {
        min(
            maximum,
            blocks.map { ($0 as? NativeParagraph)?.text.attributedString().size().width ?? maximum }.max() ?? 0
        )
    }

    func measure(_ width: CGFloat) -> CGFloat {
        var y: CGFloat = 0
        for (index, block) in blocks.enumerated() {
            if index > 0 {
                y += 12
            }
            let height = block.measure(max(1, width))
            block.frame = NSRect(x: 0, y: y, width: width, height: height)
            y += height
        }
        return y
    }

    override func accessibilityChildren() -> [Any]? {
        blocks
    }
}

@MainActor class TranscriptBlock: NSView {
    var send: (([String: Any]) -> Void)?
    var resized: (() -> Void)?
    var document = MessageDocument.empty
    override var isFlipped: Bool {
        true
    }

    func update(document: MessageDocument, media _: [String: String]) {
        self.document = document
    }

    func measure(_: CGFloat) -> CGFloat {
        0
    }

    func replace(_: TranscriptPart) -> Bool {
        false
    }

    func label(_ key: String) -> String {
        document.labels?[key] ?? ""
    }

    func emit(_ action: String, text: String) {
        send?(["action": action, "text": text, "target": document.target])
    }

    static func make(_ part: TranscriptPart) -> TranscriptBlock {
        switch part {
        case let .text(blocks): NativeParagraph(blocks)
        case let .code(code): NativeCodeBlock(code)
        case let .table(table): NativeTableBlock(table)
        case let .image(image): NativePicture(image.source)
        case let .link(link): NativePicture(link.url, image: false)
        case .rule: NativeRule()
        }
    }
}

@MainActor final class NativeParagraph: TranscriptBlock {
    let text = MessageTextView()
    private var blocks: [MessageParagraph]
    private var revealed: String?
    init(_ blocks: [MessageParagraph]) {
        self.blocks = blocks
        super.init(frame: .zero)
        addSubview(text)
    }

    @available(*, unavailable) required init?(coder _: NSCoder) {
        fatalError()
    }

    override func replace(_ part: TranscriptPart) -> Bool {
        guard case let .text(blocks) = part else { return false }
        self.blocks = blocks; return true
    }

    override func update(document: MessageDocument, media: [String: String]) {
        super.update(document: document, media: media)
        text.document = document
        let content = MessageAttributes.text(blocks, secondary: document.secondary,
                                             dark: effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua,
                                             answer: document.role == "assistant", cell: document.cell)
        let retained = MessageAttributes.retainedSelection(
            text.selectedRange(),
            previous: text.string,
            next: content.string
        )
        if text.attributedString() != content {
            text.textStorage?.setAttributedString(content)
        }
        text.setSelectedRange(retained)
        text.setAccessibilityIdentifier("message.\(document.role)-text")
        text.action = { [weak self] in self?.send?($0) }
    }

    override func measure(_ width: CGFloat) -> CGFloat {
        guard let container = text.textContainer, let manager = text.layoutManager else { return 0 }
        container.containerSize = NSSize(width: max(1, width), height: .greatestFiniteMagnitude)
        manager.ensureLayout(for: container)
        let height = ceil(manager.usedRect(for: container).maxY)
        text.frame = NSRect(x: 0, y: 0, width: width, height: height)
        if let key = document.findKey, key != revealed {
            var active: NSRange?
            text.attributedString().enumerateAttribute(
                .messageFindActive,
                in: NSRange(location: 0, length: text.string.utf16.count)
            ) { value, range, stop in
                if value as? Bool == true {
                    active = range; stop.pointee = true
                }
            }
            if let active {
                revealed = key
                let rect = manager.boundingRect(
                    forGlyphRange: manager.glyphRange(forCharacterRange: active, actualCharacterRange: nil),
                    in: container
                )
                DispatchQueue.main.async { [weak self] in
                    guard let self, document.findKey == key else { return }
                    text.scrollToVisible(rect.insetBy(dx: -12, dy: -32))
                }
            }
        }
        return height
    }

    override func accessibilityChildren() -> [Any]? {
        [text]
    }
}

@MainActor final class NativeCodeBlock: TranscriptBlock {
    private var code: TranscriptPart.Code
    private let drawing = NSImageView()
    private let mode = NSButton()
    private var showSource = false
    private let text: NativeParagraph
    private let header = NSTextField(labelWithString: "")
    private let copy = NSButton()
    private let wrap = NSButton()
    private let scroll = NSScrollView()
    private var wrapping = true
    init(_ code: TranscriptPart.Code) {
        self.code = code
        text = NativeParagraph(code.blocks)
        super.init(frame: .zero)
        wantsLayer = true; layer?.cornerRadius = 8
        layer?.backgroundColor = NSColor(white: 17.0 / 255, alpha: 1).cgColor
        header.stringValue = code.label
        header.font = .monospacedSystemFont(ofSize: 11, weight: .regular)
        copy.target = self; copy.action = #selector(copyCode)
        wrap.target = self; wrap.action = #selector(toggleWrap)
        for button in [copy, wrap] {
            button.bezelStyle = .inline; button.controlSize = .small
        }
        scroll.drawsBackground = false; scroll.hasHorizontalScroller = true; scroll.documentView = text
        mode.bezelStyle = .inline; mode.target = self; mode.action = #selector(toggleSource)
        drawing.imageScaling = .scaleProportionallyUpOrDown
        for view in [header, copy, wrap, scroll, drawing, mode] {
            addSubview(view)
        }
    }

    @available(*, unavailable) required init?(coder _: NSCoder) {
        fatalError()
    }

    override func replace(_ part: TranscriptPart) -> Bool {
        guard case let .code(code) = part else { return false }
        self.code = code; _ = text.replace(.text(code.blocks)); return true
    }

    override func update(document: MessageDocument, media: [String: String]) {
        super.update(document: document, media: media)
        copy.title = label("copyCode"); wrap.title = label(wrapping ? "unwrap" : "wrap")
        for (button, key) in [(copy, "copy"), (wrap, "wrap")] {
            button.isBordered = false; button.setAccessibilityLabel(button.title); button.toolTip = button.title
            button.image = MessageAttributes.icon(key, size: 12); button.imagePosition = .imageOnly
        }
        if document.findKey != nil,
           code.blocks
           .contains(where: { $0.runs.contains(where: { $0.highlights?.contains(where: \.active) == true }) }) {
            showSource = true
        }
        mode.title = label(showSource ? "diagram" : "source")
        header.stringValue = code.label
        drawing.image = code.diagram.flatMap { Data(base64Encoded: $0.png) }.flatMap(NSImage.init(data:))
        mode.isHidden = code.diagram == nil
        drawing.isHidden = code.diagram == nil || showSource
        scroll.isHidden = code.diagram != nil && !showSource
        wrap.isHidden = scroll.isHidden
        text.send = { [weak self] in self?.send?($0) }
        text.update(document: document, media: media)
    }

    override func measure(_ width: CGFloat) -> CGFloat {
        let available = max(1, width - 30)
        copy.setFrameSize(NSSize(width: 24, height: 24)); wrap.setFrameSize(NSSize(width: 24, height: 24))
        copy.frame.origin = NSPoint(x: max(0, width - copy.frame.width - 7), y: 5)
        wrap.frame.origin = NSPoint(x: max(0, copy.frame.minX - wrap.frame.width - 6), y: 5)
        header.frame = NSRect(x: 14, y: 8, width: max(0, wrap.frame.minX - 20), height: 16)
        mode.sizeToFit(); mode.frame.origin = NSPoint(x: max(14, wrap.frame.minX - mode.frame.width - 6), y: 5)
        if !drawing.isHidden, let size = drawing.image?.size, size.width > 0 {
            let height = min(size.height, available * size.height / size.width)
            drawing.frame = NSRect(x: 15, y: 35, width: available, height: height)
            return drawing.frame.maxY + 15
        }
        let contentWidth = wrapping ? available : max(available, ceil(text.text.attributedString().size().width))
        let height = text.measure(contentWidth)
        text.frame = NSRect(x: 0, y: 0, width: contentWidth, height: height)
        scroll.hasHorizontalScroller = !wrapping
        scroll.frame = NSRect(x: 15, y: 35, width: available, height: height + (wrapping ? 0 : 16))
        return scroll.frame.maxY + 15
    }

    @objc private func copyCode() {
        NSPasteboard.general.clearContents(); NSPasteboard.general.setString(code.source, forType: .string)
    }

    @objc private func toggleSource() {
        showSource.toggle(); update(document: document, media: [:]); resized?()
    }

    @objc private func toggleWrap() {
        wrapping.toggle(); wrap.title = label(wrapping ? "unwrap" : "wrap"); resized?()
    }
}

@MainActor final class NativeTableBlock: TranscriptBlock {
    private let table: TranscriptPart.Table
    private let scroll = NSScrollView()
    private let content = FlippedTranscriptCanvas()
    private var cells: [[TranscriptBody]] = []
    private let copy = NSButton()
    init(_ table: TranscriptPart.Table) {
        self.table = table
        super.init(frame: .zero)
        scroll.documentView = content; scroll.hasHorizontalScroller = true; scroll.drawsBackground = false
        for row in table.rows {
            cells.append(row.map { _ in let cell = TranscriptBody(); content.addSubview(cell); return cell })
        }
        copy.target = self; copy.action = #selector(showCopyMenu); copy.isBordered = false
        addSubview(scroll); addSubview(copy)
    }

    @available(*, unavailable) required init?(coder _: NSCoder) {
        fatalError()
    }

    override func update(document: MessageDocument, media: [String: String]) {
        super.update(document: document, media: media)
        copy.title = label("copyTable"); copy.setAccessibilityLabel(copy.title); copy.toolTip = copy.title
        copy.image = MessageAttributes.icon("copy", size: 12); copy.imagePosition = .imageOnly
        for (r, row) in cells.enumerated() {
            for (c, cell) in row.enumerated() {
                cell.send = { [weak self] in self?.send?($0) }
                var cellDocument = document; cellDocument.cell = r == 0 ? "header" : "body"
                cell.update(table.rows[r][c], document: cellDocument, media: media)
            }
        }
    }

    override func measure(_ width: CGFloat) -> CGFloat {
        let count = max(1, cells.map(\.count).max() ?? 1)
        let column = min(384, max(160, width / CGFloat(count)))
        var y: CGFloat = 0
        for row in cells {
            let height = max(30, row.map { $0.measure(column - 24) + 16 }.max() ?? 30)
            for (c, cell) in row.enumerated() {
                cell.frame = NSRect(x: CGFloat(c) * column + 12, y: y + 8, width: column - 24, height: height - 16)
            }
            y += height
        }
        content.frame = NSRect(x: 0, y: 0, width: column * CGFloat(count), height: y)
        scroll.frame = NSRect(x: 0, y: 0, width: width, height: y + 16)
        copy.frame = NSRect(x: max(0, width - 24), y: scroll.frame.maxY + 2, width: 24, height: 24)
        return copy.frame.maxY
    }

    @objc private func showCopyMenu() {
        let menu = NSMenu()
        for (index, key) in ["copyMarkdown", "copyCsv"].enumerated() {
            let item = NSMenuItem(title: label(key), action: #selector(copyTable(_:)), keyEquivalent: "")
            item.target = self; item.tag = index; menu.addItem(item)
        }
        menu.popUp(positioning: nil, at: NSPoint(x: copy.frame.minX, y: copy.frame.maxY), in: self)
    }

    @objc private func copyTable(_ sender: NSMenuItem) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(sender.tag == 1 ? table.csv : table.source, forType: .string)
    }
}

@MainActor final class NativePicture: TranscriptBlock {
    private let source: String
    private let button = NSButton()
    private let image: Bool
    private let error = NSTextField(wrappingLabelWithString: "")
    init(_ source: String, image: Bool = true) {
        self.source = source; self.image = image
        super.init(frame: .zero)
        button.bezelStyle = .inline; button.target = self; button.action = #selector(open)
        button.imageScaling = .scaleProportionallyUpOrDown
        error.font = .systemFont(ofSize: 12); error.textColor = .secondaryLabelColor
        addSubview(button); addSubview(error)
    }

    @available(*, unavailable) required init?(coder _: NSCoder) {
        fatalError()
    }

    override func update(document: MessageDocument, media: [String: String]) {
        super.update(document: document, media: media)
        button.image = media[source].flatMap { Data(base64Encoded: $0) }.flatMap(NSImage.init(data:))
        let reason = document.mediaErrors?[source]
        error.stringValue = reason ?? ""; error.isHidden = reason == nil
        button
            .title = label(reason != nil ? "retry" : image && button
                .image == nil ? "loadingImage" : image ? "openImage" : "openFile")
        button.setAccessibilityLabel(button.title)
        button.imagePosition = button.image != nil ? .imageOnly : .noImage
    }

    override func measure(_ width: CGFloat) -> CGFloat {
        let size = button.image?.size ?? .zero
        let height = size.width > 0 ? min(300, size.height * min(1, width / size.width)) : 30
        let messageHeight: CGFloat = error.isHidden ? 0 : ceil(error.cell?.cellSize(forBounds: NSRect(
            x: 0,
            y: 0,
            width: width,
            height: .greatestFiniteMagnitude
        )).height ?? 20)
        error.frame = NSRect(x: 0, y: 0, width: width, height: messageHeight)
        button.frame = NSRect(x: 0, y: messageHeight, width: width, height: height)
        return height + messageHeight
    }

    @objc private func open() {
        emit(document.mediaErrors?[source] != nil ? "retryImage" : image ? "image" : "link", text: source)
    }
}

@MainActor final class NativeRule: TranscriptBlock {
    private let rule = NSBox()
    override init(frame: NSRect) {
        super.init(frame: frame); rule.boxType = .separator; addSubview(rule)
    }

    @available(*, unavailable) required init?(coder _: NSCoder) {
        fatalError()
    }

    override func measure(_ width: CGFloat) -> CGFloat {
        rule.frame = NSRect(
            x: 0,
            y: 4,
            width: width,
            height: 1
        ); return 9
    }
}

@MainActor private final class FlippedTranscriptCanvas: NSView { override var isFlipped: Bool {
    true
} }
