import RemoteAgentMobile
import SwiftUI

struct ThreadsScreen: View {
    let state: IosAppViewState
    @ObservedObject var model: BexAppViewModel
    @State private var search = ""
    @State private var expandedProjectIds = Set<String>()

    var body: some View {
        if #available(iOS 26.0, *) {
            taskList.toolbar {
                DefaultToolbarItem(kind: .search, placement: .bottomBar)
                ToolbarSpacer(.flexible, placement: .bottomBar)
            }
        } else {
            taskList
        }
    }

    @ViewBuilder private var taskList: some View {
        let knownProjects = Set(state.projects.map(\.id))
        let groupedThreads = Dictionary(grouping: visibleThreads) { thread in
            thread.projectId.flatMap { knownProjects.contains($0) ? $0 : nil }
        }
        let projects = search.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            ? state.projects : state.projects.filter { groupedThreads[$0.id] != nil }
        let chats = groupedThreads[nil] ?? []
        List {
            if state.threadLoadState == .failed {
                Section {
                    if let error = state.threadLoadError {
                        BexNotice(text: error)
                            .taskListRowStyle()
                    }
                    Button("再試行") { model.refreshTaskList() }
                        .accessibilityIdentifier("tasks.retry")
                        .taskListRowStyle()
                }
                .listSectionSeparator(.hidden)
            }
            if let notice = state.notice {
                Section {
                    BexNotice(text: notice)
                        .taskListRowStyle()
                }
                .listSectionSeparator(.hidden)
            }

            Section {
                Text("プロジェクト")
                    .font(.title2.weight(.bold))
                    .textCase(nil)
                    .foregroundColor(.primary)
                    .accessibilityIdentifier("tasks.projects")
                    .taskListRowStyle()
            }
            .listSectionSeparator(.hidden)

            ForEach(projects.prefix(Int(state.visibleProjectCount)), id: \.id) { project in
                Section {
                    HStack(spacing: 16) {
                        Button {
                            if expandedProjectIds.contains(project.id) {
                                expandedProjectIds.remove(project.id)
                            } else {
                                expandedProjectIds.insert(project.id)
                            }
                        } label: {
                            HStack(spacing: 16) {
                                Image(systemName: "folder")
                                    .font(.title3).frame(width: 24)
                                Text(project.name).font(.title3).lineLimit(1)
                                Spacer(minLength: 0)
                            }
                            .frame(minHeight: 44)
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityIdentifier("tasks.project.\(project.id)")
                        .accessibilityValue(expandedProjectIds.contains(project.id) ? "開いています" : "閉じています")
                        Button { model.openNewThread(cwd: project.roots.first ?? "") } label: {
                            Image(systemName: "square.and.pencil").font(.title3)
                                .foregroundColor(.secondary).frame(width: 44, height: 44)
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("\(project.name)で新しいタスク")
                        .accessibilityIdentifier("tasks.new.project.\(project.id)")
                    }
                    .taskListRowStyle()
                    if expandedProjectIds.contains(project.id) {
                        let threads = groupedThreads[project.id] ?? []
                        ForEach(threads, id: \.id) { thread in
                            ThreadListRow(thread: thread, indented: true) { model.openThread(thread.id) }
                        }
                        if state.moreProjectIds.contains(project.id) {
                            Button("もっと見る") { model.expandTaskList(projectId: project.id) }
                                .padding(.leading, 40)
                                .disabled(state.loadingMoreThreads)
                                .accessibilityIdentifier("tasks.project.\(project.id).more")
                                .taskListRowStyle()
                        }
                    }
                }
                .listSectionSeparator(.hidden)
            }

            if state.hasMoreProjects {
                Section {
                    Button("もっと見る") { model.expandTaskList(projects: true) }
                        .disabled(state.loadingMoreThreads)
                        .accessibilityIdentifier("tasks.projects.more")
                        .taskListRowStyle()
                }
                .listSectionSeparator(.hidden)
            }

            if state.threadLoadState == .ready, state.projects.isEmpty {
                Section {
                    Text("Codexに登録されたプロジェクトはありません")
                        .font(.subheadline)
                        .foregroundColor(.secondary)
                        .taskListRowStyle()
                }
                .listSectionSeparator(.hidden)
            }

            Section {
                if state.threadLoadState == .ready, chats.isEmpty {
                    Text("プロジェクトに属さないチャットはありません")
                        .font(.subheadline)
                        .foregroundColor(.secondary)
                        .accessibilityIdentifier("tasks.empty")
                        .taskListRowStyle()
                } else {
                    ForEach(chats, id: \.id) { thread in
                        ThreadListRow(thread: thread) { model.openThread(thread.id) }
                    }
                    if state.hasMoreChats {
                        Button("もっと見る") { model.expandTaskList() }
                            .disabled(state.loadingMoreThreads)
                            .accessibilityIdentifier("tasks.chats.more")
                            .taskListRowStyle()
                    }
                }
            } header: {
                Text("チャット")
                    .font(.title3.weight(.semibold))
                    .textCase(nil)
                    .foregroundColor(.primary)
            }
            .listSectionSeparator(.hidden)
        }
        .listStyle(.plain)
        .environment(\.defaultMinListRowHeight, 52)
        .background(Color(UIColor.systemBackground))
        .accessibilityIdentifier("tasks.list")
        .searchable(text: $search, placement: .toolbar, prompt: "チャットを検索")
        .refreshable { model.refreshTaskList() }
        .task(id: search) {
            do { try await Task.sleep(nanoseconds: 200_000_000) } catch { return }
            model.searchTaskList(search)
        }
        .navigationBarBackButtonHidden(true)
        .navigationTitle("リモート")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) {
                VStack(spacing: 1) {
                    Text("リモート").font(.headline)
                    HStack(spacing: 5) {
                        if state.isConnecting || state.threadLoadState == .loading {
                            ProgressView().controlSize(.mini)
                                .accessibilityLabel(state.isConnecting ? "接続中" : "読み込み中")
                                .accessibilityIdentifier("connection.progress")
                        } else {
                            Circle().fill(state.isConnected ? Color.green : Color.secondary).frame(width: 6, height: 6)
                        }
                        Image(systemName: "laptopcomputer")
                        Text(state.selectedProfileName ?? "PC Host").lineLimit(1)
                    }
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .accessibilityElement(children: .combine)
                    .accessibilityLabel(
                        "\(state.selectedProfileName ?? "PC Host")、\(state.isConnected ? "接続済み" : "未接続")"
                    )
                }
            }
            ToolbarItem(placement: .bottomBar) {
                Button { model.openNewThread(cwd: "") } label: {
                    Image(systemName: "square.and.pencil")
                }
                .buttonStyle(.borderedProminent)
                .tint(.primary)
                .accessibilityLabel("プロジェクトなしで新しいタスク")
                .accessibilityIdentifier("tasks.new.chat")
            }
            ToolbarItem(placement: .navigationBarLeading) {
                Button { model.showProfiles() } label: { Image(systemName: "line.3.horizontal") }
                    .accessibilityLabel("PC一覧")
                    .accessibilityIdentifier("tasks.hosts")
            }
            ToolbarItem(placement: .navigationBarTrailing) {
                Menu {
                    Button { model.refreshTaskList() } label: { Label("更新", systemImage: "arrow.clockwise") }
                        .disabled(state.threadLoadState == .loading)
                        .accessibilityIdentifier("tasks.refresh")
                    Button { model.showProfiles() } label: { Label("PC一覧", systemImage: "laptopcomputer") }
                } label: { Image(systemName: "ellipsis") }
                    .accessibilityLabel("その他")
                    .accessibilityIdentifier("tasks.menu")
            }
        }
    }

    private var visibleThreads: [IosThreadSummaryView] {
        let query = search.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else { return state.threads }
        return state.threads.filter {
            $0.title.localizedCaseInsensitiveContains(query) ||
                $0.preview.localizedCaseInsensitiveContains(query)
        }
    }
}

extension View {
    func taskListRowStyle() -> some View {
        listRowSeparator(.hidden)
            .listRowInsets(EdgeInsets(top: 0, leading: 20, bottom: 0, trailing: 20))
            .listRowBackground(Color.clear)
    }
}

private struct ThreadListRow: View {
    let thread: IosThreadSummaryView
    var indented = false
    let open: () -> Void

    var body: some View {
        Button(action: open) {
            HStack(spacing: 10) {
                Text(thread.title).font(.title3).foregroundColor(.primary).lineLimit(1)
                Spacer()
                if thread.isActive {
                    ProgressView().controlSize(.small)
                        .accessibilityIdentifier("tasks.running.\(thread.id)")
                } else if thread.hasUnreadCompletion {
                    Circle().fill(Color.white).frame(width: 8, height: 8)
                        .accessibilityLabel("完了・未確認")
                        .accessibilityIdentifier("tasks.completed.\(thread.id)")
                }
            }
            .padding(.leading, indented ? 40 : 0)
            .contentShape(Rectangle())
        }
        .accessibilityIdentifier("tasks.row.\(thread.id)")
        .accessibilityValue(thread.isActive ? "実行中" : thread.hasUnreadCompletion ? "完了・未確認" : "")
        .taskListRowStyle()
    }
}

struct ModelSettingsSheet: View {
    @ObservedObject var model: BexAppViewModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationView {
            Form {
                Section {
                    NavigationLink {
                        Form { TurnOptionsPicker(model: model, conversation: model.conversation) }
                            .navigationTitle("モデルの詳細設定")
                            .navigationBarTitleDisplayMode(.inline)
                            .toolbar { ToolbarItem(placement: .confirmationAction) { doneButton } }
                    } label: {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(model.currentModel?
                                .displayName ?? (model.selectedModel.isEmpty ? "現在の設定" : model.selectedModel))
                            Text(model.selectedEffort
                                .isEmpty ? (model.currentModel?.defaultReasoningEffort ?? "") : model.selectedEffort)
                                .foregroundColor(.secondary)
                        }
                    }
                    .accessibilityIdentifier("model.details")
                }
                if let current = model.currentModel, !current.reasoningEfforts.isEmpty {
                    Section("推論の強度") {
                        Picker("推論の強度", selection: Binding(
                            get: { model.selectedEffort.isEmpty ? current.defaultReasoningEffort : model.selectedEffort
                            },
                            set: model.chooseEffort
                        )) {
                            ForEach(current.reasoningEfforts, id: \.self) { effort in
                                Text(effort).tag(effort)
                            }
                        }
                        .pickerStyle(.segmented)
                        .accessibilityIdentifier("model.quick.effort")
                    }
                }
            }
            .navigationTitle("モデル設定")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { doneButton } }
        }
        .navigationViewStyle(StackNavigationViewStyle())
    }

    private var doneButton: some View {
        Button("完了") { dismiss() }.accessibilityIdentifier("model.close")
    }
}

private struct TurnOptionsPicker: View {
    @ObservedObject var model: BexAppViewModel
    @ObservedObject var conversation: BexConversationModel

    var body: some View {
        Picker("モデル", selection: Binding(get: { model.selectedModel }, set: model.chooseModel)) {
            Text("現在の設定").tag("")
            if !model.selectedModel.isEmpty, model.currentModel == nil {
                Text(model.selectedModel).tag(model.selectedModel)
            }
            ForEach(model.models, id: \.id) { Text($0.displayName).tag($0.model) }
        }
        .pickerStyle(.menu).accessibilityIdentifier("model.picker")
        if let current = model.currentModel {
            Picker("推論の強度", selection: Binding(get: { model.selectedEffort }, set: model.chooseEffort)) {
                Text("既定 (\(current.defaultReasoningEffort))").tag("")
                ForEach(current.reasoningEfforts, id: \.self) { Text($0).tag($0) }
            }
            .pickerStyle(.menu).accessibilityIdentifier("model.effort")
        }
        if model.loadingModels {
            ProgressView()
        }
        if let error = model.modelError {
            HStack {
                Text(error).font(.caption).foregroundColor(.secondary).lineLimit(2)
                Button("再読込") { model.loadModels() }
            }
        }
        if conversation.thread?.turns.contains(where: \.isInProgress) == true {
            Text("実行中の追加入力には現在の設定が使われます")
                .font(.caption2).foregroundColor(.secondary)
        }
    }
}
