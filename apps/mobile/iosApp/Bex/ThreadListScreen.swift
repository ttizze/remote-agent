import AgentCore
import SwiftUI

/// Home: pinned and active cards, unsent tasks, then the Working, Snoozed and
/// Settled shelves.
struct ThreadListScreen: View {
    @ObservedObject var model: BexAppViewModel
    /// Shown as the split view's sidebar.
    var sidebar = false
    let openSettings: () -> Void
    let newTask: () -> Void
    /// Shows the new task on the draft core already opened.
    let showNewTaskDraft: () -> Void
    @State private var query = ""
    @AppStorage("threads.shelf.working") private var workingExpanded = false
    @AppStorage("threads.shelf.snoozed") private var snoozedExpanded = false
    @AppStorage("threads.shelf.settled") private var settledExpanded = true
    @State private var settledLimit: UInt32 = 10
    @State private var renaming: ThreadRow?
    @State private var title = ""
    /// The row whose swipe actions are open; opening another closes it.
    @State private var openSwipeRow: String?
    @State private var arranging = false
    @State private var customSnooze: String?
    /// When the soonest snoozed thread woke, between the minute ticks.
    @State private var wokeAt = Date.distantPast

    var body: some View {
        TimelineView(.everyMinute) { clock in
            let now = Int64(max(clock.date, wokeAt).timeIntervalSince1970 * 1000)
            let list = model.snapshot.threadList(nowMs: now, options: options)
            content(list, now: now)
                .task(id: list.nextSnoozeWakeAtMs) {
                    guard let wake = list.nextSnoozeWakeAtMs else { return }
                    let delay = max(0, Double(wake - Int64(Date().timeIntervalSince1970 * 1000)) / 1000)
                    do { try await Task.sleep(for: .seconds(delay + 0.05)) } catch { return }
                    wokeAt = Date()
                }
        }
        .background((sidebar ? AppTheme.color("mobileDrawer") : AppTheme.screen).ignoresSafeArea())
        .navigationBarTitleDisplayMode(.inline)
        .toolbar { toolbar }
        .searchable(text: $query, placement: sidebar ? .sidebar : .toolbar, prompt: "Search")
        .onChange(of: query) { _, value in
            settledLimit = 10
            model.perform(.search(query: value))
        }
        .onAppear {
            query = model.snapshot.searchQuery()
            model.recordListViewUpdate()
        }
        .alert("Rename thread", isPresented: Binding(get: { renaming != nil }, set: {
            if !$0 {
                renaming = nil
            }
        })) {
            TextField("Title", text: $title)
            Button("Save") {
                if let row = renaming {
                    Haptics.light()
                    model.perform(.thread(threadId: row.id, action: .rename(title: title)))
                }
            }
            Button("Cancel", role: .cancel) {}
        }
        .fullScreenCover(isPresented: $arranging) { ThreadArrangementSheet(model: model) }
        .sheet(item: Binding(
            get: { customSnooze.map(IdentifiedString.init) },
            set: { customSnooze = $0?.value }
        )) { thread in
            CustomSnoozeSheet { until in
                Haptics.light()
                model.perform(.thread(threadId: thread.value, action: .snooze(until: until)))
            }
        }
    }

    private var options: ThreadListOptions {
        ThreadListOptions(
            workingShelfEnabled: false,
            workingShelfExpanded: workingExpanded,
            snoozedShelfExpanded: snoozedExpanded,
            settledShelfExpanded: settledExpanded,
            settledLimit: settledLimit,
            shelfPreferencesLoading: false,
            timestampFormat: .locale
        )
    }

    @ViewBuilder
    private func content(_ list: ThreadListView, now: Int64) -> some View {
        if !list.hasThreads, let empty = list.empty {
            ThreadListEmptyState(empty: empty, addEnvironment: model.isConnected ? nil : { model.openPairing() })
        } else {
            List {
                ForEach(list.items, id: \.key) { item in
                    itemView(item, now: now)
                        .listRowInsets(EdgeInsets())
                        .listRowSeparator(.hidden)
                        .listRowBackground(Color.clear)
                }
                if list.hiddenSettledCount > 0 {
                    ShowMoreSettled(count: list.hiddenSettledCount) { settledLimit += 25 }
                        .listRowInsets(EdgeInsets())
                        .listRowSeparator(.hidden)
                        .listRowBackground(Color.clear)
                }
                if list.items.isEmpty, let empty = list.empty {
                    EmptyStateText(title: empty.title, detail: empty.detail)
                        .listRowSeparator(.hidden)
                        .listRowBackground(Color.clear)
                }
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
            .scrollIndicators(.hidden)
            .scrollDismissesKeyboard(.interactively)
            .onScrollPhaseChange { _, phase in
                if phase == .interacting {
                    openSwipeRow = nil
                }
            }
            .environment(\.defaultMinListRowHeight, 0)
        }
    }

    @ViewBuilder
    private func itemView(_ item: ThreadListItem, now _: Int64) -> some View {
        switch item {
        case let .thread(row):
            ThreadSwipeable(
                id: row.key, openRow: $openSwipeRow, primary: primarySwipe(row), secondary: secondarySwipe(row),
                compact: row.variant == .slim,
                background: sidebar ? AppTheme.color("mobileDrawer") : AppTheme.screen
            ) { close in
                ThreadListRowView(row: row, icon: ProjectIconImages.image(model.snapshot, row.projectId),
                                  sidebar: sidebar, open: {
                                      close()
                                      model.openThread(row.id)
                                  })
                                  .contextMenu { ThreadMenuItems(items: row.menu) { run($0, row: row) } }
            }
            // A row that changes shelf or action starts closed.
            .id("\(row.key):\(row.variant):\(row.swipePrimary.label)")
        case let .pendingTask(task):
            let actions = pendingTaskActions(kind: task.kind, projectId: task.projectId)
            PendingTaskRowView(task: task, icon: ProjectIconImages.image(model.snapshot, task.projectId),
                               sidebar: sidebar, status: actions.status, isDraft: actions.isDraft)
                .onTapGesture {
                    switch actions.open {
                    case let .openThread(threadId): model.openThread(threadId)
                    case .newThread:
                        model.perform(actions.open)
                        showNewTaskDraft()
                    default: break
                    }
                }
                .contextMenu {
                    Button("Delete", systemImage: "trash", role: .destructive) { model.perform(actions.discard) }
                }
        case let .workingShelf(shelf):
            ShelfHeaderView(label: "Working", shelf: shelf, sidebar: sidebar) { workingExpanded.toggle() }
        case let .snoozedShelf(shelf):
            ShelfHeaderView(label: "Snoozed", shelf: shelf, sidebar: sidebar) { snoozedExpanded.toggle() }
        case let .settledShelf(shelf):
            ShelfHeaderView(label: "Settled", shelf: shelf, sidebar: sidebar) { settledExpanded.toggle() }
        }
    }

    private func primarySwipe(_ row: ThreadRow) -> SwipeActionSpec {
        let button = row.swipePrimary
        return SwipeActionSpec(
            symbol: button.action.symbol, label: button.label, accessibilityLabel: button.accessibilityLabel
        ) {
            let action: ThreadAction? = switch button.action {
            case .settle: .settle
            case .unsettle: .unsettle
            case .unsnooze: .unsnooze
            case .snooze: nil
            }
            if let action {
                Haptics.light()
                model.perform(.thread(threadId: row.id, action: action))
            }
        }
    }

    /// Snooze offers its wake times as a menu.
    private func secondarySwipe(_ row: ThreadRow) -> SwipeMenuSpec? {
        guard let button = row.swipeSecondary, !row.snoozeOptions.isEmpty else { return nil }
        return SwipeMenuSpec(
            symbol: button.action.symbol, label: button.label, accessibilityLabel: button.accessibilityLabel,
            title: "Snooze until", choices: row.snoozeOptions
        ) { run($0, row: row) }
    }

    private func run(_ action: ThreadMenuAction, row: ThreadRow) {
        switch action {
        case let .thread(value):
            if case .delete = value, model.selectedThreadId == row.id {
                model.showThreadList()
            }
            Haptics.light()
            model.perform(.thread(threadId: row.id, action: value))
        case let .filterProject(projectId): model.perform(.filterProject(projectId: projectId))
        case let .newThreadOnBranch(projectId, branch, worktreePath):
            model.perform(.newThreadOnBranch(projectId: projectId, branch: branch, worktreePath: worktreePath))
            showNewTaskDraft()
        case .customSnooze: customSnooze = row.id
        case .startRename:
            title = row.title
            renaming = row
        case .openProjectSettings: openSettings()
        case .arrange: arranging = true
        case let .move(direction):
            Haptics.light()
            model.perform(.moveThread(
                threadId: row.id, section: row.pinned ? .pinned : .active,
                destination: direction == .up ? .up : .down
            ))
        case .copyPath, .copyBranch, .copyThreadId: copy(action)
        }
    }

    private func copy(_ action: ThreadMenuAction) {
        switch action {
        case let .copyPath(path): Haptics.copy(path ?? "")
        case let .copyBranch(branch): Haptics.copy(branch)
        case let .copyThreadId(threadId): Haptics.copy(threadId)
        default: break
        }
    }

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        ToolbarItem(placement: .principal) {
            ConnectionTitle(model: model, open: openSettings)
        }
        ToolbarItem(placement: .topBarTrailing) {
            Button(action: openSettings) {
                Image(systemName: sidebar ? "gearshape" : "ellipsis")
            }
            .accessibilityLabel("Open settings")
        }
        if sidebar {
            ToolbarItem(placement: .topBarTrailing) { ThreadFilterMenu(model: model) }
        } else {
            ToolbarItem(placement: .bottomBar) { ThreadFilterMenu(model: model) }
            ToolbarSpacer(.flexible, placement: .bottomBar)
            DefaultToolbarItem(kind: .search, placement: .bottomBar)
            ToolbarSpacer(.flexible, placement: .bottomBar)
            ToolbarItem(placement: .bottomBar) {
                Button(action: newTask) { Image(systemName: "square.and.pencil") }
                    .accessibilityLabel("New task")
            }
        }
    }
}

struct IdentifiedString: Identifiable {
    let value: String
    var id: String {
        value
    }
}

extension ThreadListItem {
    /// The list identity: the row's key, or the shelf's.
    var key: String {
        switch self {
        case let .thread(row): row.key
        case let .pendingTask(task): task.key
        case .workingShelf: "working-shelf"
        case .snoozedShelf: "snoozed-shelf"
        case .settledShelf: "settled-shelf"
        }
    }
}

extension SwipeAction {
    var symbol: String {
        switch self {
        case .settle: "checkmark"
        case .unsettle: "arrow.uturn.backward"
        case .snooze, .unsnooze: "clock"
        }
    }
}
