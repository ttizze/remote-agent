import AppKit

struct ConversationButton: Decodable, Equatable {
    let action: String
    let title: String
}

struct NativeConversationRow: Decodable, Equatable {
    let id: String
    let user: Bool
    let gap: Double
    let buttons: [ConversationButton]
    let document: MessageDocument
    let parts: [TranscriptPart]
    let images: [String]
    let media: [String: String]
    let time: String?
    let queued: Bool
    let steer: Bool
}

struct NativeConversationUpdate: Decodable {
    struct History: Decodable {
        let title: String
        let enabled: Bool
    }

    let scope: String
    let reset: Bool
    let order: [String]?
    let changed: [NativeConversationRow]
    let removed: [String]
    let following: Bool
    let inset: Double
    let history: History?
    let notice: String?
    let latest: String
}

@MainActor final class ConversationScrollView: NSScrollView {
    var beginInteraction: (() -> Void)?
    var endInteraction: (() -> Void)?

    override func scrollWheel(with event: NSEvent) {
        beginInteraction?()
        super.scrollWheel(with: event)
        endInteraction?()
    }
}

/// A transcript exposes its message views rather than virtual table cells.
@MainActor final class ConversationTableView: NSTableView {
    override func accessibilityRole() -> NSAccessibility.Role? {
        .list
    }

    override func accessibilityChildren() -> [Any]? {
        let visible = rows(in: visibleRect)
        guard visible.location != NSNotFound else { return [] }
        return (visible.location ..< NSMaxRange(visible)).compactMap {
            view(atColumn: 0, row: $0, makeIfNecessary: false)
        }
    }
}

/// A complete native viewport. NSH never receives row heights or owns its
/// cells: AppKit owns reuse, TextKit layout and the reading-position anchor.
@MainActor final class NativeConversationView: NSView, NSTableViewDelegate {
    let scroll = ConversationScrollView()
    let table = ConversationTableView()
    private let column = NSTableColumn(identifier: .init("message"))
    private let history = NSButton(title: "", target: nil, action: nil)
    private let notice = NSTextField(wrappingLabelWithString: "")
    private let latest = NSButton(title: "↓", target: nil, action: nil)
    private var source: NSTableViewDiffableDataSource<Int, String>!
    private var rows: [String: NativeConversationRow] = [:]
    private var heights: [String: CGFloat] = [:]
    private var invalidated = Set<String>()
    private let measurement = NativeConversationCell(frame: .zero)
    private var order: [String] = []
    private var scope = ""
    private var following = true
    private var inset: CGFloat = 0
    private var applying = false
    private var pending = false
    private var restoring = false
    private var interacting = false
    private var observers: [NSObjectProtocol] = []
    private var readingAnchor: Anchor?
    private var laneWidth: CGFloat = 0
    private var generation: UInt64 = 0
    private var active = true
    var send: (([String: Any]) -> Void)?
    private let selection = NSStackView()
    private var lastVisible: NSRange?
    private var selectedAction: [String: Any]?

    override var isFlipped: Bool {
        true
    }

    override init(frame: NSRect) {
        super.init(frame: frame)
        table.addTableColumn(column)
        table.headerView = nil
        table.backgroundColor = .clear
        table.intercellSpacing = .zero
        table.columnAutoresizingStyle = .lastColumnOnlyAutoresizingStyle
        table.selectionHighlightStyle = .none
        table.delegate = self
        table.setAccessibilityLabel("会話のメッセージ")
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.drawsBackground = false
        scroll.automaticallyAdjustsContentInsets = false
        scroll.setAccessibilityLabel("この会話")
        scroll.setAccessibilityIdentifier("native.conversation")
        scroll.beginInteraction = { [weak self] in self?.interacting = true; self?.hideSelection() }
        scroll.endInteraction = { [weak self] in
            guard let self else { return }
            didScroll()
            interacting = false
        }
        addSubview(scroll)
        selection.orientation = .horizontal
        selection.spacing = 6
        selection.wantsLayer = true
        selection.layer?.backgroundColor = NSColor.windowBackgroundColor.cgColor
        selection.layer?.cornerRadius = 12
        selection.isHidden = true
        addSubview(selection)
        for button in [history, latest] {
            button.target = self
            button.bezelStyle = .inline
            addSubview(button)
        }
        history.action = #selector(loadHistory)
        latest.action = #selector(goLatest)
        notice.textColor = .secondaryLabelColor
        addSubview(notice)
        source = NSTableViewDiffableDataSource<Int, String>(tableView: table) { [weak self] table, _, _, id in
            guard let self, let row = rows[id] else { return NSView() }
            let cell = table.makeView(withIdentifier: .init("conversation.cell"), owner: nil) as? NativeConversationCell
                ?? NativeConversationCell(frame: .zero)
            cell.identifier = .init("conversation.cell")
            cell.update(row, width: laneWidth, scope: scope, send: actionHandler)
            cell.resized = { [weak self] in self?.resizeRow(id) }
            return cell
        }
        scroll.contentView.postsBoundsChangedNotifications = true
        observers.append(NotificationCenter.default.addObserver(
            forName: NSView.boundsDidChangeNotification, object: scroll.contentView, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.updateLatest() }
        })
        for (notification, interacting) in [
            (NSScrollView.willStartLiveScrollNotification, true),
            (NSScrollView.didEndLiveScrollNotification, false)
        ] {
            observers.append(NotificationCenter.default.addObserver(
                forName: notification,
                object: scroll,
                queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated { self?.interacting = interacting }
            })
        }
        observers.append(NotificationCenter.default.addObserver(
            forName: NSScrollView.didLiveScrollNotification,
            object: scroll,
            queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.didScroll() }
        })
        table.postsFrameChangedNotifications = true
        observers.append(NotificationCenter.default.addObserver(
            forName: NSView.frameDidChangeNotification, object: table, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self, self.active, !self.applying, !self.restoring, !self.interacting else { return }
                self.restore(self.readingAnchor)
            }
        })
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) {
        fatalError("init(coder:) is unavailable")
    }

    func stop() {
        active = false
        send = nil
        for observer in observers {
            NotificationCenter.default.removeObserver(observer)
        }
        observers.removeAll()
        table.delegate = nil
        table.dataSource = nil
        rows.removeAll()
        heights.removeAll()
        generation &+= 1
    }

    func update(_ update: NativeConversationUpdate) {
        guard active else { return }
        let reset = update.reset || scope != update.scope
        if reset {
            generation &+= 1
            rows.removeAll()
            heights.removeAll()
            invalidated.removeAll()
            scope = update.scope
            readingAnchor = nil
            hideSelection()
            table.isHidden = true
        }
        for id in update.removed {
            rows.removeValue(forKey: id)
            heights.removeValue(forKey: id)
            invalidated.remove(id)
        }
        for row in update.changed {
            rows[row.id] = row
            heights.removeValue(forKey: row.id)
            invalidated.insert(row.id)
        }
        if let order = update.order {
            self.order = order
        }
        following = update.following
        inset = CGFloat(update.inset)
        history.title = update.history?.title ?? ""
        history.isEnabled = update.history?.enabled ?? false
        history.isHidden = update.history == nil
        notice.stringValue = update.notice ?? ""
        notice.isHidden = notice.stringValue.isEmpty
        latest.setAccessibilityLabel(update.latest)
        needsLayout = true
        layoutSubtreeIfNeeded()
        apply()
    }

    func fail(_ reason: String) {
        notice.stringValue = reason
        notice.isHidden = false
        needsLayout = true
    }

    private struct Anchor {
        let id: String
        let displacement: CGFloat
    }

    private func anchor() -> Anchor? {
        let visible = table.rows(in: scroll.documentVisibleRect)
        guard visible.location != NSNotFound, visible.length > 0,
              let id = source.itemIdentifier(forRow: visible.location) else { return nil }
        return Anchor(id: id, displacement: table.rect(ofRow: visible.location).minY - scroll.documentVisibleRect.minY)
    }

    private func restore(_ anchor: Anchor?) {
        restoring = true
        defer { restoring = false }
        if following {
            scroll.contentView.scroll(to: NSPoint(
                x: 0,
                y: max(0, table.bounds.height - scroll.contentView.bounds.height + scroll.contentInsets.bottom)
            ))
        } else if let anchor, let index = source.row(forItemIdentifier: anchor.id) {
            scroll.contentView.scroll(to: NSPoint(x: 0, y: table.rect(ofRow: index).minY - anchor.displacement))
        }
        scroll.reflectScrolledClipView(scroll.contentView)
        updateLatest()
    }

    private func apply() {
        guard active else { return }
        guard !applying else { pending = true; return }
        let anchor = following ? nil : anchor()
        readingAnchor = anchor
        let generation = generation
        if source.snapshot().itemIdentifiers == order {
            refreshVisible()
            table.layoutSubtreeIfNeeded()
            restore(anchor)
            table.isHidden = false
            return
        }
        applying = true
        var snapshot = NSDiffableDataSourceSnapshot<Int, String>()
        snapshot.appendSections([0])
        snapshot.appendItems(order)
        source.apply(snapshot, animatingDifferences: false) { [weak self] in
            MainActor.assumeIsolated {
                guard let self, self.active else { return }
                self.applying = false
                self.refreshVisible()
                self.table.layoutSubtreeIfNeeded()
                if self.generation == generation {
                    self.restore(anchor)
                    self.table.isHidden = false
                }
                if self.pending {
                    self.pending = false
                    self.apply()
                }
            }
        }
    }

    private func refreshVisible() {
        var resized = IndexSet(invalidated.compactMap { source.row(forItemIdentifier: $0) })
        invalidated.removeAll()
        table.enumerateAvailableRowViews { [weak self] _, index in
            guard let self, let id = source.itemIdentifier(forRow: index), let row = rows[id],
                  let cell = table.view(atColumn: 0, row: index, makeIfNecessary: false) as? NativeConversationCell
            else { return }
            if cell.update(row, width: laneWidth, scope: scope, send: actionHandler) {
                resized.insert(index)
            }
        }
        if !resized.isEmpty {
            table.noteHeightOfRows(withIndexesChanged: resized)
        }
    }

    override func layout() {
        super.layout()
        let anchor = following ? nil : anchor()
        var top: CGFloat = 0
        if !notice.isHidden {
            notice.frame = NSRect(x: 24, y: top, width: max(0, bounds.width - 48), height: 40)
            top += 40
        }
        if !history.isHidden {
            history.frame = NSRect(x: 24, y: top, width: max(0, bounds.width - 48), height: 32)
            top += 32
        }
        scroll.frame = NSRect(x: 0, y: top, width: bounds.width, height: max(0, bounds.height - top))
        scroll.contentInsets = NSEdgeInsets(top: 0, left: 0, bottom: inset + 16, right: 0)
        let width = scroll.contentSize.width
        column.width = width
        let lane = max(40, min(700, width - 48))
        if lane != laneWidth {
            laneWidth = lane
            heights.removeAll()
            refreshVisible()
            table.noteHeightOfRows(withIndexesChanged: IndexSet(integersIn: 0 ..< table.numberOfRows))
            table.layoutSubtreeIfNeeded()
            restore(anchor)
        }
        latest.frame = NSRect(x: (bounds.width - 44) / 2, y: bounds.height - inset - 50, width: 44, height: 44)
        updateLatest()
    }

    private func updateLatest() {
        let visible = table.rows(in:scroll.documentVisibleRect)
        if lastVisible != visible {
            lastVisible = visible
            send?(["action":"visible", "scope":scope, "start":visible.location, "end":NSMaxRange(visible)])
        }
        let remaining = table.bounds.height - scroll.documentVisibleRect.maxY
        latest.isHidden = following || remaining < 48
    }

    private func didScroll() {
        updateLatest()
        guard active, !restoring else { return }
        readingAnchor = anchor()
        send?(["action": "scroll", "scope": scope, "offset": scroll.documentVisibleRect.minY,
               "viewport": scroll.documentVisibleRect.height,
               "content": table.bounds.height + scroll.contentInsets.bottom])
    }

    private var actionHandler: ([String: Any]) -> Void {
        { [weak self] action in
            guard let self else { return }
            if action["action"] as? String == "selection" {
                showSelection(action)
            } else {
                send?(action)
            }
        }
    }

    private func resizeRow(_ id: String) {
        guard active, let row = rows[id], let index = source.row(forItemIdentifier: id) else { return }
        let anchor = following ? nil : anchor()
        heights.removeValue(forKey: id)
        if let cell = table.view(atColumn: 0, row: index, makeIfNecessary: false) as? NativeConversationCell {
            cell.update(row, width: laneWidth, scope: scope, send: actionHandler)
        }
        table.noteHeightOfRows(withIndexesChanged: IndexSet(integer: index))
        table.layoutSubtreeIfNeeded(); restore(anchor)
    }

    private func hideSelection() {
        selection.isHidden = true; selectedAction = nil
    }

    private func showSelection(_ action: [String: Any]) {
        guard let text = action["text"] as? String, !text.isEmpty,
              let values = action["bounds"] as? [Double], values.count == 4,
              let content = window?.contentView else {
            if let event = NSApp.currentEvent, event.type == .leftMouseDown,
               selection.frame.contains(convert(event.locationInWindow, from: nil)) {
                return
            }
            hideSelection(); return
        }
        selectedAction = action
        selection.arrangedSubviews.forEach { selection.removeArrangedSubview($0); $0.removeFromSuperview() }
        let titles = action["menu"] as? [String: String] ?? [:]
        var items = ["quote", "explain"]
        if action["sideChat"] as? Bool == true {
            items.append("sideChat")
        }
        for id in items {
            guard let title = titles[id], !title.isEmpty else { continue }
            let button = NSButton(title: title, target: self, action: #selector(selectionPressed(_:)))
            button.identifier = .init(id); button.bezelStyle = .inline; selection.addArrangedSubview(button)
        }
        let width = selection.fittingSize.width + 16
        let y = content.isFlipped ? values[1] : content.bounds.height - values[1] - values[3]
        let selected = convert(NSRect(x: values[0], y: y, width: values[2], height: values[3]), from: content)
        selection.frame = NSRect(
            x: max(8, min(selected.minX, bounds.width - width - 8)),
            y: max(8, selected.minY - 34),
            width: width,
            height: 30
        )
        selection.isHidden = false
    }

    @objc private func selectionPressed(_ sender: NSButton) {
        guard var action = selectedAction, let id = sender.identifier?.rawValue else { return }
        action["action"] = id; hideSelection(); send?(action)
    }

    @objc private func loadHistory() {
        send?(["action": "history", "scope": scope])
    }

    @objc private func goLatest() {
        send?(["action": "latest", "scope": scope])
    }

    func command(_ value: [String: Any]) {
        guard value["name"] as? String == "reveal", let args = value["args"] as? [String: Any],
              let row = args["row"] as? Int, order.indices.contains(row) else { return }
        following = false
        restoring = true
        table.scrollRowToVisible(row)
        readingAnchor = anchor()
        restoring = false
    }

    func tableView(_: NSTableView, heightOfRow index: Int) -> CGFloat {
        // A new snapshot can arrive while AppKit still measures the old
        // table. Its delegate must always return a positive row height.
        guard let id = source.itemIdentifier(forRow: index), let row = rows[id] else { return table.rowHeight }
        if let height = heights[id] {
            return height
        }
        measurement.update(row, width: laneWidth, scope: scope, send: { _ in })
        let height = ceil(measurement.fittingSize.height)
        heights[id] = height
        return height
    }

    func tableView(_: NSTableView, shouldSelectRow _: Int) -> Bool {
        false
    }
}

@MainActor final class NativeConversationCell: NSTableCellView {
    private let body = TranscriptBody()
    private let metadata = NSTextField(labelWithString: "")
    private var buttons: [NSButton] = []
    private var row: NativeConversationRow?
    private var scope = ""
    private var lane: CGFloat = 0
    private var measured = NSSize.zero
    private var send: (([String: Any]) -> Void)?
    var resized: (() -> Void)?
    override var isFlipped: Bool {
        true
    }

    override var fittingSize: NSSize {
        measured
    }

    override init(frame: NSRect) {
        super.init(frame: frame)
        addSubview(body); addSubview(metadata)
        setAccessibilityElement(true); setAccessibilityRole(.group)
        metadata.font = .systemFont(ofSize: 11)
        metadata.textColor = .secondaryLabelColor
        body.resized = { [weak self] in self?.resized?() }
    }

    @available(*, unavailable) required init?(coder _: NSCoder) {
        fatalError()
    }

    override func accessibilityChildren() -> [Any]? {
        [body, metadata] + buttons
    }

    @discardableResult
    func update(
        _ row: NativeConversationRow,
        width: CGFloat,
        scope: String = "",
        send: @escaping ([String: Any]) -> Void
    ) -> Bool {
        let changed = self.row != row || self.scope != scope || lane != width
        self.send = send; self.scope = scope
        body.send = { [weak self] action in
            guard let self, let row = self.row else { return }
            var value = action; value["scope"] = self.scope; value["row"] = row.id
            self.send?(value)
        }
        let parts = row.parts + row.images.map { TranscriptPart.image(.init(source: $0)) }
        body.update(parts, document: row.document, media: row.media)
        metadata.stringValue = [
            row.time,
            row.queued ? row.document.labels?["queued"] : nil,
            row.steer ? row.document.labels?["steer"] : nil
        ].compactMap(\.self).joined(separator: " · ")
        if self.row?.buttons != row.buttons {
            buttons.forEach { $0.removeFromSuperview() }
            buttons = row.buttons.map { action in
                let button = NSButton(title: action.title, target: self, action: #selector(pressed(_:)))
                button.identifier = .init(action.action); button.bezelStyle = .inline; button.controlSize = .small
                button
                    .setAccessibilityLabel(action.title.hasPrefix("▸ ") || action.title
                        .hasPrefix("▾ ") ? String(action.title.dropFirst(2)) : action.title)
                addSubview(button); return button
            }
        }
        self.row = row; lane = width
        measure()
        return changed
    }

    override func layout() {
        super.layout(); measure()
    }

    private func measure() {
        let user = row?.user == true
        let width = max(40, user ? lane * 0.85 : lane)
        let inset: CGFloat = user ? 14 : 0
        let x = max(24, (bounds.width - lane) / 2) + (user ? lane - width : 0)
        let height = body.measure(max(1, width - inset * 2))
        body.frame = NSRect(x: x + inset, y: 10, width: width - inset * 2, height: height)
        body.wantsLayer = true; body.layer?.cornerRadius = user ? 12 : 0
        body.layer?.backgroundColor = user ? NSColor.controlBackgroundColor.cgColor : nil
        var bx = x
        var y = body.frame.maxY + 6
        for button in buttons {
            button.sizeToFit()
            if bx + button.frame.width > x + width {
                bx = x; y += 24
            }
            button.frame.origin = NSPoint(x: bx, y: y); bx += button.frame.width + 10
        }
        metadata.sizeToFit(); metadata.frame.origin = NSPoint(x: x, y: y + (buttons.isEmpty ? 0 : 24))
        measured = NSSize(width: lane + 48, height: ceil(metadata.frame.maxY + max(8, row?.gap ?? 0)))
    }

    @objc private func pressed(_ button: NSButton) {
        guard let row, let action = button.identifier?.rawValue else { return }
        send?(["action": action, "row": row.id, "scope": scope])
    }
}

@MainActor private final class NativeConversationOwner {
    let context: UnsafeMutableRawPointer?
    var callback: MessageCallback?
    let view = NativeConversationView(frame: .zero)

    init(context: UnsafeMutableRawPointer?, callback: @escaping MessageCallback) {
        self.context = context
        self.callback = callback
        view.send = { [weak self] action in
            guard let self, let data = try? JSONSerialization.data(withJSONObject: action) else { return }
            data.withUnsafeBytes { self.callback?(self.context, $0.bindMemory(to: UInt8.self).baseAddress, data.count) }
        }
    }
}

@_cdecl("bex_nsh_conversation_create")
@MainActor public func conversationCreate(
    _ context: UnsafeMutableRawPointer?,
    _ callback: @escaping @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<UInt8>?, Int) -> Void
) -> UnsafeMutableRawPointer {
    Unmanaged.passRetained(NativeConversationOwner(context: context, callback: callback)).toOpaque()
}

@_cdecl("bex_nsh_conversation_view")
@MainActor public func conversationView(_ handle: UnsafeMutableRawPointer) -> UnsafeMutableRawPointer {
    Unmanaged.passUnretained(Unmanaged<NativeConversationOwner>.fromOpaque(handle).takeUnretainedValue().view)
        .toOpaque()
}

@_cdecl("bex_nsh_conversation_update")
@MainActor public func conversationUpdate(
    _ handle: UnsafeMutableRawPointer,
    _ bytes: UnsafePointer<UInt8>,
    _ count: Int
) {
    let owner = Unmanaged<NativeConversationOwner>.fromOpaque(handle).takeUnretainedValue()
    do { try owner.view.update(JSONDecoder().decode(
        NativeConversationUpdate.self,
        from: Data(bytes: bytes, count: count)
    )) } catch { owner.view.fail(error.localizedDescription) }
}

@_cdecl("bex_nsh_conversation_command")
@MainActor public func conversationCommand(
    _ handle: UnsafeMutableRawPointer,
    _ bytes: UnsafePointer<UInt8>,
    _ count: Int
) {
    guard let command = try? JSONSerialization.jsonObject(with: Data(bytes: bytes, count: count)) as? [String: Any]
    else { return }
    Unmanaged<NativeConversationOwner>.fromOpaque(handle).takeUnretainedValue().view.command(command)
}

@_cdecl("bex_nsh_conversation_release")
@MainActor public func conversationRelease(_ handle: UnsafeMutableRawPointer) {
    let owner = Unmanaged<NativeConversationOwner>.fromOpaque(handle).takeRetainedValue()
    owner.callback = nil
    owner.view.stop()
}
