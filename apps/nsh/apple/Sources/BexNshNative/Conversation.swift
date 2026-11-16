import AppKit
import NativeUI
import SwiftUI

struct ConversationButton: Decodable, Equatable {
    let action: String
    let title: String
    var icon: String? = nil
    var iconOnly: Bool? = nil
    var disclosure: Bool? = nil
    var menuItems: [MessageMenuItem]? = nil
    var enabled: Bool? = nil
    var accessibilityLabel: String? = nil
    var header: Bool? = nil
    var shimmer: MessageShimmer? = nil
}

struct NativeConversationRow: Decodable, Equatable {
    var attemptFold: Bool? = nil
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
    var trailing: Bool? = nil
    var collapsible: Bool? = nil
    var top: Double? = nil
    var agentStatus: String? = nil
    var icon: String? = nil
    var separator: Bool? = nil
    var plan: Bool? = nil
    var planTitle: String? = nil
    var error: ConversationErrorData? = nil
    var preparation: NativePreparation? = nil
}

struct ConversationErrorData: Decodable, Equatable {
    let title: String
    let message: String
    let details: String?
    let reconnecting: Bool
    let usageLimit: Bool
}

struct NativeConversationUpdate: Decodable {
    struct History: Decodable {
        let title: String
        let enabled: Bool
    }

    struct Opening: Decodable { let loading: Bool; let text: String; let retry: String? }
    var opening: Opening? = nil
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
    var lane: Double? = nil
    var padding: Double? = nil
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
    override func hitTest(_ point: NSPoint) -> NSView? {
        guard let hit = super.hitTest(point) else { return nil }
        guard hit === self else { return hit }
        let local = convert(point, from: superview)
        let index = row(at: local)
        guard index >= 0, let cell = view(atColumn: 0, row: index, makeIfNecessary: false) else { return hit }
        // Rows are not selectable: their text and controls receive the pointer
        // directly instead of NSTableView's row-selection tracking loop.
        return cell.hitTest(convert(local, to: cell.superview)) ?? hit
    }

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
    private let opening = NSTextField(wrappingLabelWithString: "")
    private let retryOpen = NSButton()
    private let loading = NSProgressIndicator()
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
    private var maximumWidth: CGFloat = 736
    private var padding: CGFloat = 24
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
        selection.layer?.backgroundColor = NSColor(white: 20.0 / 255, alpha: 1).cgColor
        selection.layer?.cornerRadius = 12
        selection.isHidden = true
        addSubview(selection)
        opening.textColor = .secondaryLabelColor
        loading.style = .spinning; loading.controlSize = .small; loading.isDisplayedWhenStopped = false
        retryOpen.target = self; retryOpen.action = #selector(openAgain)
        for child in [opening, retryOpen, loading] {
            addSubview(child)
        }
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
            MainActor.assumeIsolated {
                guard let self else { return }
                if !self.applying, !self.restoring {
                    self.hideSelection()
                }
                self.updateLatest()
            }
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
        maximumWidth = CGFloat(update.lane ?? 736)
        padding = CGFloat(update.padding ?? 24)
        inset = CGFloat(update.inset)
        history.title = update.history?.title ?? ""
        history.isEnabled = update.history?.enabled ?? false
        history.isHidden = update.history == nil
        notice.stringValue = update.notice ?? ""
        notice.isHidden = notice.stringValue.isEmpty
        opening.stringValue = update.opening?.text ?? ""; opening.isHidden = update.opening == nil
        if update.opening?.loading == true {
            loading.startAnimation(nil)
        } else {
            loading.stopAnimation(nil)
        }
        retryOpen.title = update.opening?.retry ?? ""; retryOpen.isHidden = update.opening?.retry == nil
        latest.setAccessibilityLabel(update.latest)
        latest.isEnabled = !update.latest.isEmpty
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
            table.tile()
        }
    }

    override func layout() {
        super.layout()
        let anchor = following ? nil : anchor()
        var top: CGFloat = 0
        if !opening.isHidden {
            opening.frame = NSRect(x: 48, y: 16, width: max(0, bounds.width - 72), height: 40)
            loading.frame = NSRect(x: 24, y: 24, width: 16, height: 16)
            top = 64
            if !retryOpen.isHidden {
                retryOpen.frame = NSRect(x: 48, y: 56, width: 100, height: 28); top = 96
            }
        }
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
        let lane = max(40, min(maximumWidth, width - padding * 2))
        if lane != laneWidth {
            laneWidth = lane
            heights.removeAll()
            refreshVisible()
            table.noteHeightOfRows(withIndexesChanged: IndexSet(integersIn: 0 ..< table.numberOfRows))
            table.tile()
            table.layoutSubtreeIfNeeded()
            restore(anchor)
        }
        latest.frame = NSRect(x: (bounds.width - 44) / 2, y: bounds.height - inset - 50, width: 44, height: 44)
        updateLatest()
    }

    private func updateLatest() {
        let visible = table.rows(in: scroll.documentVisibleRect)
        if lastVisible != visible, visible.location != NSNotFound {
            lastVisible = visible
            send?(["action": "visible", "scope": scope, "start": visible.location, "end": NSMaxRange(visible)])
        }
        let remaining = table.bounds.height - scroll.documentVisibleRect.maxY
        latest.isHidden = !latest.isEnabled || following || remaining < 48
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
                if let name = action["action"] as? String,
                   ["fold", "group", "item", "setupDetails", "browse"].contains(name) {
                    following = false; readingAnchor = anchor()
                }
                send?(action)
            }
        }
    }

    private func resizeRow(_ id: String) {
        guard active, let row = rows[id], let index = source.row(forItemIdentifier: id) else { return }
        let anchor = following ? nil : anchor()
        if let cell = table.view(atColumn: 0, row: index, makeIfNecessary: false) as? NativeConversationCell {
            cell.update(row, width: laneWidth, scope: scope, send: actionHandler)
            heights[id] = cell.fittingSize.height
        } else {
            heights.removeValue(forKey: id)
        }
        table.noteHeightOfRows(withIndexesChanged: IndexSet(integer: index))
        table.tile()
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
            button.identifier = .init(id); button.isBordered = false; button.font = .systemFont(ofSize: 12)
            button.image = MessageAttributes.icon(id, size: 14, label: title); button.imagePosition = .imageLeading
            button.setAccessibilityLabel(title); selection.addArrangedSubview(button)
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

    @objc private func openAgain() {
        send?(["action": "retryOpen", "scope": scope])
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

/// AppKit owns viewport reuse; iPhone's SwiftUI components own every row's appearance.
@MainActor final class NativeConversationCell: NSTableCellView {
    private let host = NSHostingView(rootView: AnyView(EmptyView()))
    private var row: NativeConversationRow?
    private var scope = ""
    private var lane: CGFloat = 1
    private var measured = NSSize(width: 1, height: 1)
    var resized: (() -> Void)?
    override var isFlipped: Bool {
        true
    }

    override var fittingSize: NSSize {
        measured
    }

    override init(frame: NSRect) {
        super.init(frame: frame)
        host.sizingOptions = .intrinsicContentSize
        addSubview(host)
    }

    @available(*, unavailable) required init?(coder _: NSCoder) {
        fatalError()
    }

    @discardableResult func update(_ row: NativeConversationRow, width: CGFloat, scope: String = "",
                                   send: @escaping ([String: Any]) -> Void) -> Bool {
        guard self.row != row || self.scope != scope || lane != max(1, width) else { return false }
        self.row = row; self.scope = scope; lane = max(1, width)
        let content = MacConversationRow(row: row) { action in
            var value = action; value["row"] = row.id; value["scope"] = scope; send(value)
        }
        host.rootView = AnyView(content.accessibilityElement(children: .contain).id(scope + ":" + row.id)
            .padding(.top, row.top ?? 0).padding(.bottom, row.gap)
            .frame(width: lane, alignment: .leading).fixedSize(horizontal: false, vertical: true)
            .onGeometryChange(for: CGFloat.self, of: { $0.size.height }) { [weak self] height in
                guard let self, abs(measured.height - ceil(height)) > 0.5 else { return }
                measured.height = max(1, ceil(height))
                DispatchQueue.main.async { [weak self] in self?.resized?() }
            })
        measured = NSSize(width: lane, height: max(1, ceil(host.fittingSize.height)))
        needsLayout = true
        return true
    }

    override func layout() {
        super.layout()
        host.frame = NSRect(x: max(0, (bounds.width - lane) / 2), y: 0,
                            width: lane, height: measured.height)
    }
}

private struct MacConversationRow: View {
    let row: NativeConversationRow
    let send: ([String: Any]) -> Void
    private var headers: [ConversationButton] {
        row.buttons.filter { $0.header == true }
    }

    private var footer: [ConversationButton] {
        row.buttons.filter { $0.header != true }
    }

    var body: some View {
        Group {
            if let preparation = row.preparation {
                MacPreparation(value: preparation, summary: row.parts, document: row.document,
                               elapsed: row.time, actions: footer, send: send)
            } else if let error = row.error {
                VStack(alignment: .leading, spacing: 10) {
                    ConversationError(context: label("thisResponse"), title: error.title,
                                      reconnecting: error.reconnecting, usageLimit: error.usageLimit) {
                        TranscriptContent(parts: [.text([.plain(error.message)])], document: row.document,
                                          media: row.media, send: send)
                    } details: {
                        if let details = error.details, !details.isEmpty {
                            DisclosureGroup(label("showDetails")) {
                                TranscriptContent(parts: [.text([.plain(details)])], document: row.document,
                                                  media: row.media, send: send)
                            }.font(.caption)
                        }
                    }
                    ConversationActions { ForEach(footer, id: \.action) { action($0) } }
                }
            } else if let status = row.agentStatus, let agent = headers.first {
                ConversationSubagent(name: agent.title, status: status, elapsed: row.time,
                                     enabled: agent.enabled ?? true, open: { emit(agent.action) }) {
                    text
                }.accessibilityIdentifier("subagent." + row.id)
            } else if row.plan == true {
                ConversationPlan(title: row.planTitle ?? "", badge: label("plan")) { text } actions: {
                    ForEach(headers, id: \.action) { action($0) }
                }
            } else if row.separator == true, row.icon == "compact" {
                ConversationDivider(label: row.parts.compactMap { part in
                    if case let .text(blocks) = part {
                        return blocks.flatMap(\.runs).map(\.text).joined()
                    }
                    return nil
                }.joined(separator: "\n"))
            } else if let work = headers.first, work.action == "item" || work.action == "group" {
                ConversationWork(
                    title: work.title,
                    symbol: row.attemptFold == true ? nil : MessageAttributes.symbol(row.icon) ?? "chevron.left.forwardslash.chevron.right",
                    expanded: Binding(get: { work.icon == "chevronDown" }, set: { _ in emit(work.action) }),
                    active: work.shimmer != nil,
                    accessibilityLabel: work.accessibilityLabel
                ) {
                    text
                }
            } else if row.user || !footer.isEmpty || headers.isEmpty && row.icon == nil {
                message
            } else {
                HStack(alignment: .top, spacing: 8) {
                    if let symbol = MessageAttributes.symbol(row.icon) {
                        Image(systemName: symbol).font(.system(size: 14)).foregroundStyle(.secondary)
                            .frame(width: 16, height: 20).accessibilityHidden(true)
                    }
                    VStack(alignment: .leading, spacing: 10) {
                        ForEach(headers, id: \.action) { action($0) }
                        if !row.parts.isEmpty { text }
                    }
                }
            }
        }.frame(maxWidth: .infinity, alignment: .leading).buttonStyle(.plain)
    }

    private var text: some View {
        TranscriptContent(parts: row.parts + (row.user ? [] : row.images.map { .image(.init(source: $0)) }),
                          document: row.document, media: row.media, send: send,
                          identifier: row.user ? "conversation.user-body" : "conversation.assistant-body")
    }

    private var message: some View {
        ConversationMessage(user: row.user, hasText: !row.parts.isEmpty || !row.user && !row.images.isEmpty,
                            hasImages: !row.images.isEmpty, collapsible: row.collapsible ?? false,
                            searching: row.document.searching ?? false, persistentActions: row.trailing ?? false,
                            expandTitle: label("expand"), collapseTitle: label("collapse"), identifier: row.id,
                            expand: { emit("browse") }) {
            if row.queued {
                ConversationInputMarker(title: label("queued"), description: label("queuedDescription"), steer: false)
            }
            if row.steer {
                ConversationInputMarker(title: label("steer"), description: label("steerDescription"), steer: true)
            }
        } images: {
            if row.user, !row.images.isEmpty {
                ConversationImages {
                    ForEach(row.images, id: \.self) { source in
                        TranscriptContent(parts: [.image(.init(source: source))], document: row.document,
                                          media: row.media, send: send).frame(width: 80, height: 80).clipped()
                    }
                }
            }
        } content: {
            VStack(alignment: .leading, spacing: 10) {
                ForEach(headers, id: \.action) { action($0) }
                text
            }
        } actions: {
            if !footer.isEmpty || row.time != nil {
                ConversationActions {
                    if row.user {
                        time
                    }
                    ForEach(footer, id: \.action) { action($0) }
                    if !row.user {
                        time
                    }
                }
            }
        } notices: { EmptyView() }
    }

    @ViewBuilder private var time: some View {
        if let time = row.time {
            Text(verbatim: time).font(.system(size: 12))
                .accessibilityIdentifier(row.user ? "conversation.user-time" : "conversation.assistant-time")
        }
    }

    private func label(_ key: String) -> String {
        row.document.labels?[key] ?? ""
    }

    private func emit(_ action: String) {
        send(["action": action])
    }

    private func action(_ value: ConversationButton) -> some View {
        ConversationAction(value: value, icon: row.icon, copiedTitle: label("copied"), send: emit)
            .accessibilityIdentifier("conversation." + (row.user ? "user-" : "assistant-") + value.action)
    }
}

private struct ConversationAction: View {
    let value: ConversationButton
    var icon: String?
    let copiedTitle: String
    let send: (String) -> Void

    var body: some View {
        Group {
            if let items = value.menuItems {
                Menu {
                    ForEach(items, id: \.id) { item in
                        if let id = item.id {
                            Button(item.title) { send(id) }.disabled(item.enabled == false)
                        }
                    }
                } label: { Image(systemName: "ellipsis") }
                    .accessibilityLabel(Text(verbatim: value.accessibilityLabel ?? value.title))
                    .menuStyle(.borderlessButton).menuIndicator(.hidden).fixedSize()
            } else if value.action == "copy" {
                ConversationCopyButton(title: value.accessibilityLabel ?? value.title,
                                       copiedTitle: copiedTitle) { send(value.action) }
            } else {
                Button { send(value.action) } label: { content }
                    .accessibilityLabel(Text(verbatim: value.accessibilityLabel ?? value.title))
            }
        }
        .disabled(value.enabled == false)
        .help(value.title)
    }

    @ViewBuilder private var content: some View {
        if value.disclosure == true || value.shimmer != nil {
            HStack(spacing: 8) {
                if let symbol = MessageAttributes.symbol(icon) {
                    Image(systemName: symbol).foregroundStyle(.secondary)
                }
                ConversationActivityHeader(summary: value.title, canCollapse: value.disclosure == true,
                                           isInProgress: value.shimmer != nil,
                                           expanded: value.icon == "chevronDown")
            }
        } else if value.iconOnly == true, let symbol = MessageAttributes.symbol(value.icon) {
            Image(systemName: symbol).rotationEffect(.degrees(value.action == "fork" ? 90 : 0))
        } else {
            Text(verbatim: value.title)
        }
    }
}

/// Native selection and text layout stay in TextKit; row decorations come from NativeUI.
struct TranscriptContent: NSViewRepresentable {
    let parts: [TranscriptPart]
    let document: MessageDocument
    let media: [String: String]
    let send: ([String: Any]) -> Void
    var identifier: String?
    @State private var layoutRevision = 0
    func makeNSView(context _: Context) -> TranscriptBody {
        TranscriptBody()
    }

    func updateNSView(_ view: TranscriptBody, context _: Context) {
        _ = layoutRevision
        view.setAccessibilityIdentifier(identifier)
        view.resized = { layoutRevision &+= 1 }
        view.send = send; view.update(parts, document: document, media: media)
    }

    func sizeThatFits(_ proposal: ProposedViewSize, nsView: TranscriptBody, context _: Context) -> CGSize? {
        let width = max(1, proposal.width ?? nsView.preferredWidth(10000))
        return NSSize(width: width, height: nsView.measure(width))
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
