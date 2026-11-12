#if os(macOS) && DEBUG
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
        override func accessibilityRole() -> NSAccessibility.Role? { .list }

        override func accessibilityChildren() -> [Any]? {
            let visible = rows(in: visibleRect)
            guard visible.location != NSNotFound else { return [] }
            return (visible.location..<NSMaxRange(visible)).compactMap {
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

        override var isFlipped: Bool { true }

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
            scroll.beginInteraction = { [weak self] in self?.interacting = true }
            scroll.endInteraction = { [weak self] in
                guard let self else { return }
                self.didScroll()
                self.interacting = false
            }
            addSubview(scroll)
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
                guard let self, let row = self.rows[id] else { return NSView() }
                let cell = table.makeView(withIdentifier: .init("conversation.cell"), owner: nil) as? NativeConversationCell
                    ?? NativeConversationCell(frame: .zero)
                cell.identifier = .init("conversation.cell")
                cell.update(row, width: self.laneWidth, scope: self.scope, send: self.actionHandler)
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
                observers.append(NotificationCenter.default.addObserver(forName: notification, object: scroll, queue: .main) { [weak self] _ in
                    MainActor.assumeIsolated { self?.interacting = interacting }
                })
            }
            observers.append(NotificationCenter.default.addObserver(forName: NSScrollView.didLiveScrollNotification, object: scroll, queue: .main) { [weak self] _ in
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
        required init?(coder _: NSCoder) { fatalError("init(coder:) is unavailable") }

        func stop() {
            active = false
            send = nil
            for observer in observers { NotificationCenter.default.removeObserver(observer) }
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
            if let order = update.order { self.order = order }
            following = update.following
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
                scroll.contentView.scroll(to: NSPoint(x: 0, y: max(0, table.bounds.height - scroll.contentView.bounds.height + scroll.contentInsets.bottom)))
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
                guard let self, let id = self.source.itemIdentifier(forRow: index), let row = self.rows[id],
                      let cell = self.table.view(atColumn: 0, row: index, makeIfNecessary: false) as? NativeConversationCell
                else { return }
                if cell.update(row, width: self.laneWidth, scope: self.scope, send: self.actionHandler) { resized.insert(index) }
            }
            if !resized.isEmpty { table.noteHeightOfRows(withIndexesChanged: resized) }
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
            scroll.contentInsets = NSEdgeInsets(top: 0, left: 0, bottom: 16, right: 0)
            let width = scroll.contentSize.width
            column.width = width
            let lane = max(40, min(700, width - 48))
            if lane != laneWidth {
                laneWidth = lane
                heights.removeAll()
                refreshVisible()
                table.noteHeightOfRows(withIndexesChanged: IndexSet(integersIn: 0..<table.numberOfRows))
                table.layoutSubtreeIfNeeded()
                restore(anchor)
            }
            latest.frame = NSRect(x: (bounds.width - 44) / 2, y: bounds.height - 50, width: 44, height: 44)
            updateLatest()
        }

        private func updateLatest() {
            let remaining = table.bounds.height - scroll.documentVisibleRect.maxY
            latest.isHidden = following || remaining < 48
        }

        private func didScroll() {
            updateLatest()
            guard active, !restoring else { return }
            readingAnchor = anchor()
            send?(["action": "scroll", "scope": scope, "offset": scroll.documentVisibleRect.minY,
                   "viewport": scroll.documentVisibleRect.height, "content": table.bounds.height + scroll.contentInsets.bottom])
        }

        private var actionHandler: ([String: Any]) -> Void {
            { [weak self] action in self?.send?(action) }
        }
        @objc private func loadHistory() { send?(["action": "history", "scope": scope]) }
        @objc private func goLatest() { send?(["action": "latest", "scope": scope]) }

        func command(_ value: [String: Any]) {
            guard value["name"] as? String == "reveal", let args = value["args"] as? [String: Any],
                  let row = args["row"] as? Int, order.indices.contains(row) else { return }
            restoring = true
            table.scrollRowToVisible(row)
            readingAnchor = anchor()
            restoring = false
        }

        func tableView(_: NSTableView, heightOfRow index: Int) -> CGFloat {
            // A new snapshot can arrive while AppKit still measures the old
            // table. Its delegate must always return a positive row height.
            guard let id = source.itemIdentifier(forRow: index), let row = rows[id] else { return table.rowHeight }
            if let height = heights[id] { return height }
            measurement.update(row, width: laneWidth, scope: scope, send: { _ in })
            let height = ceil(measurement.fittingSize.height)
            heights[id] = height
            return height
        }

        func tableView(_: NSTableView, shouldSelectRow _: Int) -> Bool { false }
    }

    @MainActor final class NativeConversationCell: NSTableCellView {
        let text = MessageTextView()
        private let stack = NSStackView()
        private let actions = NSStackView()
        private var row: NativeConversationRow?
        private var scope = ""
        private var lane: CGFloat = 0
        private var textHeight: NSLayoutConstraint!
        private var laneConstraint: NSLayoutConstraint!
        private var alignment: NSLayoutConstraint!
        private var bottom: NSLayoutConstraint!
        private var send: (([String: Any]) -> Void)?

        override init(frame: NSRect) {
            super.init(frame: frame)
            setAccessibilityElement(true)
            setAccessibilityRole(.group)
            text.nativeScrolling = true
            text.translatesAutoresizingMaskIntoConstraints = false
            stack.orientation = .vertical
            stack.alignment = .leading
            stack.distribution = .fill
            stack.setHuggingPriority(.required, for: .vertical)
            stack.spacing = 8
            stack.translatesAutoresizingMaskIntoConstraints = false
            actions.orientation = .horizontal
            actions.setHuggingPriority(.required, for: .vertical)
            actions.spacing = 12
            stack.addArrangedSubview(text)
            stack.addArrangedSubview(actions)
            addSubview(stack)
            textHeight = text.heightAnchor.constraint(equalToConstant: 20)
            laneConstraint = stack.widthAnchor.constraint(equalToConstant: 400)
            alignment = stack.centerXAnchor.constraint(equalTo: centerXAnchor)
            bottom = stack.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -16)
            NSLayoutConstraint.activate([
                stack.topAnchor.constraint(equalTo: topAnchor, constant: 8),
                alignment, laneConstraint, bottom,
                text.widthAnchor.constraint(equalTo: stack.widthAnchor), textHeight
            ])
        }

        @available(*, unavailable)
        required init?(coder _: NSCoder) { fatalError("init(coder:) is unavailable") }

        override func accessibilityChildren() -> [Any]? {
            (text.isHidden ? [] : [text]) + actions.arrangedSubviews
        }

        @discardableResult
        func update(_ row: NativeConversationRow, width: CGFloat, scope: String = "", send: @escaping ([String: Any]) -> Void) -> Bool {
            self.send = send
            let sameScope = self.scope == scope
            self.scope = scope
            text.action = { [weak self] value in
                guard let self else { return }
                var value = value
                value["scope"] = self.scope
                self.send?(value)
            }
            guard !sameScope || self.row != row || lane != width else { return false }
            let same = sameScope && self.row?.id == row.id
            if !same { text.resetPaints() }
            let content = MessageAttributes.text(row.document.blocks, secondary: row.document.secondary,
                                                 dark: effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua,
                                                 answer: !row.user)
            let selection = same ? text.selectedRange() : NSRange(location: 0, length: 0)
            let retained = MessageAttributes.retainedSelection(selection, previous: text.string, next: content.string)
            text.document = row.document
            text.setAccessibilityIdentifier("message.\(row.document.role)-text")
            text.preparePaints(for: content.string)
            text.textStorage?.setAttributedString(content)
            text.setSelectedRange(retained)
            lane = width
            let contentWidth = row.user ? width * 0.85 : width
            laneConstraint.constant = contentWidth
            alignment.constant = row.user ? (width - contentWidth) / 2 : 0
            stack.alignment = row.user ? .trailing : .leading
            text.wantsLayer = true
            text.layer?.cornerRadius = row.user ? 12 : 0
            text.layer?.backgroundColor = row.user ? NSColor(calibratedWhite: 20 / 255, alpha: 1).cgColor : nil
            text.textContainerInset = row.user ? NSSize(width: 14, height: 10) : .zero
            bottom.constant = -max(8, row.gap)
            text.textContainer?.containerSize = NSSize(width: max(1, contentWidth - text.textContainerInset.width * 2), height: .greatestFiniteMagnitude)
            if let manager = text.layoutManager, let container = text.textContainer {
                manager.ensureLayout(for: container)
                textHeight.constant = max(0, ceil(manager.usedRect(for: container).maxY) + text.textContainerInset.height * 2)
            }
            text.isHidden = content.length == 0
            if self.row?.buttons != row.buttons || !same {
                for view in actions.arrangedSubviews { actions.removeArrangedSubview(view); view.removeFromSuperview() }
                for action in row.buttons {
                    let button = NSButton(title: action.title, target: self, action: #selector(pressed(_:)))
                    button.identifier = .init(action.action)
                    button.bezelStyle = .inline
                    button.controlSize = .small
                    button.lineBreakMode = .byTruncatingTail
                    button.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
                    button.setAccessibilityLabel(action.title.hasPrefix("▸ ") || action.title.hasPrefix("▾ ")
                        ? String(action.title.dropFirst(2)) : action.title)
                    actions.addArrangedSubview(button)
                }
            }
            actions.isHidden = row.buttons.isEmpty
            self.row = row
            return true
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
        Unmanaged.passUnretained(Unmanaged<NativeConversationOwner>.fromOpaque(handle).takeUnretainedValue().view).toOpaque()
    }

    @_cdecl("bex_nsh_conversation_update")
    @MainActor public func conversationUpdate(_ handle: UnsafeMutableRawPointer, _ bytes: UnsafePointer<UInt8>, _ count: Int) {
        let owner = Unmanaged<NativeConversationOwner>.fromOpaque(handle).takeUnretainedValue()
        do { owner.view.update(try JSONDecoder().decode(NativeConversationUpdate.self, from: Data(bytes: bytes, count: count))) }
        catch { owner.view.fail(error.localizedDescription) }
    }

    @_cdecl("bex_nsh_conversation_command")
    @MainActor public func conversationCommand(_ handle: UnsafeMutableRawPointer, _ bytes: UnsafePointer<UInt8>, _ count: Int) {
        guard let command = try? JSONSerialization.jsonObject(with: Data(bytes: bytes, count: count)) as? [String: Any] else { return }
        Unmanaged<NativeConversationOwner>.fromOpaque(handle).takeUnretainedValue().view.command(command)
    }

    @_cdecl("bex_nsh_conversation_release")
    @MainActor public func conversationRelease(_ handle: UnsafeMutableRawPointer) {
        let owner = Unmanaged<NativeConversationOwner>.fromOpaque(handle).takeRetainedValue()
        owner.callback = nil
        owner.view.stop()
    }
#endif
