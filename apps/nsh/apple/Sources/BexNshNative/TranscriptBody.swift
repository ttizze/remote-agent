import AppKit
import NativeUI
import SwiftUI

indirect enum TranscriptPart: Decodable, Equatable {
    struct Code: Decodable, Equatable {
        let label: String
        let source: String
        let blocks: [MessageParagraph]
        let diagram: Diagram?
    }

    struct Diagram: Decodable, Equatable { let png: String; let width: Double; let height: Double }
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
    var send: (([String: Any]) -> Void)?
    var resized: (() -> Void)?
    override var isFlipped: Bool {
        true
    }

    func clear() {
        blocks.forEach { $0.removeFromSuperview() }
        blocks.removeAll(); parts.removeAll()
    }

    func update(_ parts: [TranscriptPart], document: MessageDocument, media: [String: String]) {
        let contentChanged = self.parts != parts
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
                y += 14
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
        if text.selectedRange() != retained {
            text.setSelectedRange(retained)
        }
        text.setAccessibilityIdentifier("message.\(document.role)-text")
        text.action = { [weak self] in self?.send?($0) }
    }

    override func measure(_ width: CGFloat) -> CGFloat {
        guard let container = text.textContainer, let manager = text.layoutManager else { return 0 }
        container.containerSize = NSSize(width: max(1, width), height: .greatestFiniteMagnitude)
        manager.ensureLayout(for: container)
        let height = ceil(manager.usedRect(for: container).maxY)
        text.frame = NSRect(x: 0, y: 0, width: width, height: height)
        text.updateShimmer()
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

@MainActor class HostedTranscriptBlock: TranscriptBlock {
    private let host = NSHostingView(rootView: AnyView(EmptyView()))
    private var content = AnyView(EmptyView())
    private var width: CGFloat = 1
    private var height: CGFloat = 0
    override init(frame: NSRect) {
        super.init(frame: frame); host.sizingOptions = .intrinsicContentSize; addSubview(host)
    }

    @available(*, unavailable) required init?(coder _: NSCoder) {
        fatalError()
    }

    func show(_ content: some View) {
        self.content = AnyView(content); render()
    }

    private func render() {
        host.rootView = AnyView(content.accessibilityElement(children: .contain).frame(
            width: width,
            alignment: .leading
        )
        .fixedSize(horizontal: false, vertical: true)
        .onGeometryChange(for: CGFloat.self, of: { $0.size.height }) { [weak self] next in
            guard let self, abs(next - height) > 0.5 else { return }
            height = next
            DispatchQueue.main.async { [weak self] in self?.resized?() }
        })
    }

    override func measure(_ width: CGFloat) -> CGFloat {
        if self.width != width {
            self.width = width; render()
        }
        height = ceil(host.fittingSize.height)
        host.frame = NSRect(x: 0, y: 0, width: width, height: height)
        return height
    }
}

@MainActor final class NativeCodeBlock: HostedTranscriptBlock {
    private var code: TranscriptPart.Code
    init(_ code: TranscriptPart.Code) {
        self.code = code; super.init(frame: .zero)
    }

    @available(*, unavailable) required init?(coder _: NSCoder) {
        fatalError()
    }

    override func replace(_ part: TranscriptPart) -> Bool {
        guard case let .code(code) = part else { return false }
        self.code = code; return true
    }

    override func update(document: MessageDocument, media: [String: String]) {
        super.update(document: document, media: media)
        show(MacCodeBlock(code: code, document: document, media: media) { [weak self] in self?.send?($0) })
    }
}

private struct MacCodeBlock: View {
    let code: TranscriptPart.Code
    let document: MessageDocument
    let media: [String: String]
    let send: ([String: Any]) -> Void
    @State private var wrapped = true
    @State private var source = false
    @State private var large = false
    private func label(_ key: String) -> String {
        document.labels?[key] ?? ""
    }

    private var showingSource: Bool {
        source || code.diagram == nil || document.findKey != nil
    }

    var body: some View {
        ConversationCode {
            HStack(spacing: 12) {
                Text(verbatim: code.label).lineLimit(1).truncationMode(.middle)
                Spacer(minLength: 0)
                if code.diagram != nil {
                    Button(label(showingSource ? "diagram" : "source")) { source.toggle() }
                    if !showingSource {
                        Button(label(large ? "shrink" : "enlarge")) { large.toggle() }
                    }
                }
                if showingSource {
                    Button { wrapped.toggle() } label: {
                        Image(systemName: wrapped ? "text.alignleft" : "arrow.right.to.line")
                    }.accessibilityLabel(Text(verbatim: label(wrapped ? "unwrap" : "wrap")))
                }
                ConversationCopyButton(title: label(code.diagram == nil ? "copyCode" : "copyDiagram"),
                                       copiedTitle: label("copied")) {
                    NSPasteboard.general.clearContents(); NSPasteboard.general.setString(code.source, forType: .string)
                }.font(.system(size: 16))
            }.buttonStyle(.plain)
        } content: {
            if showingSource {
                if wrapped {
                    text.padding(12)
                } else {
                    ScrollView(.horizontal) {
                        text.fixedSize(horizontal: true, vertical: false).padding(12)
                    }
                }
            } else if let diagram = code.diagram, let data = Data(base64Encoded: diagram.png),
                      let image = NSImage(data: data) {
                Image(nsImage: image).resizable().aspectRatio(contentMode: .fit)
                    .frame(maxHeight: large ? 640 : 300).padding(12)
            }
        }
    }

    private var text: some View {
        TranscriptContent(parts: [.text(code.blocks)], document: document, media: media, send: send)
    }
}

@MainActor final class NativeTableBlock: HostedTranscriptBlock {
    private var table: TranscriptPart.Table
    init(_ table: TranscriptPart.Table) {
        self.table = table; super.init(frame: .zero)
    }

    @available(*, unavailable) required init?(coder _: NSCoder) {
        fatalError()
    }

    override func replace(_ part: TranscriptPart) -> Bool {
        guard case let .table(table) = part else { return false }
        self.table = table; return true
    }

    override func update(document: MessageDocument, media: [String: String]) {
        super.update(document: document, media: media)
        show(MacTableBlock(table: table, document: document, media: media) { [weak self] in self?.send?($0) })
    }
}

private struct MacTableBlock: View {
    let table: TranscriptPart.Table
    let document: MessageDocument
    let media: [String: String]
    let send: ([String: Any]) -> Void
    @ScaledMetric(relativeTo: .body) private var columnWidth = 160.0
    private func label(_ key: String) -> String {
        document.labels?[key] ?? ""
    }

    var body: some View {
        ConversationTable(rowLengths: table.rows.map(\.count), columnWidth: columnWidth) {
            Menu {
                Button(label("copyMarkdown")) { copy(table.source) }
                Button(label("copyCsv")) { copy(table.csv) }
            } label: { Image(systemName: "doc.on.doc").foregroundStyle(.secondary) }
                .accessibilityLabel(Text(verbatim: label("copyTable")))
                .menuStyle(.borderlessButton).menuIndicator(.hidden).fixedSize()
        } cell: { row, column in
            TranscriptContent(parts: table.rows[row][column], document: cellDocument(row), media: media, send: send)
        }
    }

    private func cellDocument(_ row: Int) -> MessageDocument {
        var value = document; value.cell = row == 0 ? "header" : "body"; return value
    }

    private func copy(_ text: String) {
        NSPasteboard.general.clearContents(); NSPasteboard.general.setString(text, forType: .string)
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
        button.imagePosition = button.image != nil ? .imageOnly : .noImage
        button.image?.accessibilityDescription = button.title
        button.setAccessibilityLabel(button.title)
        button.menu = nil
        if image, button.image != nil {
            let menu = NSMenu()
            let save = NSMenuItem(title: label("saveImage"), action: #selector(saveImage), keyEquivalent: "")
            save.target = self; menu.addItem(save); button.menu = menu
        }
    }

    override func measure(_ width: CGFloat) -> CGFloat {
        let size = button.image?.size ?? .zero
        let height = size.width > 0 ? min(480, size.height * min(1, width / size.width)) : 30
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

    @objc private func saveImage() {
        emit("saveImage", text: source)
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
