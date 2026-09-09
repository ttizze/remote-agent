import RemoteAgentMobile
import SwiftUI

struct ThreadsScreen: View {
    let state: IosAppViewState
    @ObservedObject var model: BexAppViewModel
    @State private var search = ""
    @State private var expandedProjectIds = Set<String>()
    @State private var worktreeHost: WorktreeSettingsHost?

    var body: some View {
        let knownProjects = Set(state.projects.map(\.id))
        let groupedThreads = Dictionary(grouping: visibleThreads) { thread in
            thread.projectId.flatMap { knownProjects.contains($0) ? $0 : nil }
        }
        let projects = search.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            ? state.projects : state.projects.filter { groupedThreads[$0.id] != nil }
        let chats = groupedThreads[nil] ?? []
        List {
            if let error = state.view.threadList as? LoadPhaseFailed {
                Section {
                    BexNotice(text: error.message)
                        .taskListRowStyle()
                    Button("再試行") { model.controller.navigation.refreshTaskList() }
                        .accessibilityIdentifier("tasks.retry")
                        .taskListRowStyle()
                }
                .listSectionSeparator(.hidden)
            }
            if let notice = state.view.notice {
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

            ForEach(projects.prefix(Int(state.view.visibleProjectCount)), id: \.id) { project in
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
                        Button { model.controller.navigation.openNewThread(cwd: project.roots.first?.path ?? "")
                        } label: {
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
                            ThreadListRow(thread: thread, indented: true) {
                                model.controller.navigation.openThread(threadId: thread.id)
                            }
                        }
                        if state.view.moreProjectIds.contains(project.id) {
                            Button("もっと見る") { model.controller.navigation.expandTaskList(
                                projects: false,
                                projectId: project.id
                            ) }
                            .padding(.leading, 40)
                            .disabled(state.view.loadingMoreThreads)
                            .accessibilityIdentifier("tasks.project.\(project.id).more")
                            .taskListRowStyle()
                        }
                    }
                }
                .listSectionSeparator(.hidden)
            }

            if state.view.hasMoreProjects {
                Section {
                    Button("もっと見る") { model.controller.navigation.expandTaskList(projects: true, projectId: nil) }
                        .disabled(state.view.loadingMoreThreads)
                        .accessibilityIdentifier("tasks.projects.more")
                        .taskListRowStyle()
                }
                .listSectionSeparator(.hidden)
            }

            if state.view.threadList is LoadPhaseReady, state.projects.isEmpty {
                Section {
                    Text("Codexに登録されたプロジェクトはありません")
                        .font(.subheadline)
                        .foregroundColor(.secondary)
                        .taskListRowStyle()
                }
                .listSectionSeparator(.hidden)
            }

            Section {
                if state.view.threadList is LoadPhaseReady, chats.isEmpty {
                    Text("プロジェクトに属さないチャットはありません")
                        .font(.subheadline)
                        .foregroundColor(.secondary)
                        .accessibilityIdentifier("tasks.empty")
                        .taskListRowStyle()
                } else {
                    ForEach(chats, id: \.id) { thread in
                        ThreadListRow(thread: thread) { model.controller.navigation.openThread(threadId: thread.id) }
                    }
                    if state.view.hasMoreChats {
                        Button("もっと見る") { model.controller.navigation.expandTaskList(projects: false, projectId: nil) }
                            .disabled(state.view.loadingMoreThreads)
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
        .refreshable { model.controller.navigation.refreshTaskList() }
        .task(id: search) {
            do { try await Task.sleep(nanoseconds: 200_000_000) } catch { return }
            model.controller.navigation.searchTaskList(term: search)
        }
        .navigationBarBackButtonHidden(true)
        .navigationTitle("リモート")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            DefaultToolbarItem(kind: .search, placement: .bottomBar)
            ToolbarSpacer(.flexible, placement: .bottomBar)
            ToolbarItem(placement: .principal) {
                VStack(spacing: 1) {
                    Text("リモート").font(.headline)
                    HStack(spacing: 5) {
                        if state.isConnecting || state.view.threadList is LoadPhaseLoading {
                            ProgressView().controlSize(.mini)
                                .accessibilityLabel(state.isConnecting ? "接続中" : "読み込み中")
                                .accessibilityIdentifier("connection.progress")
                        } else {
                            Circle().fill(state.isConnected ? Color.green : Color.secondary).frame(width: 6, height: 6)
                        }
                        Image(systemName: "laptopcomputer")
                        Text(state.selectedProfile?.name ?? "PC Host").lineLimit(1)
                    }
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .accessibilityElement(children: .combine)
                    .accessibilityLabel(
                        "\(state.selectedProfile?.name ?? "PC Host")、\(state.isConnected ? "接続済み" : "未接続")"
                    )
                }
            }
            ToolbarItem(placement: .bottomBar) {
                Button { model.controller.navigation.openNewThread(cwd: "") } label: {
                    Image(systemName: "square.and.pencil")
                }
                .buttonStyle(.borderedProminent)
                .tint(.primary)
                .accessibilityLabel("プロジェクトなしで新しいタスク")
                .accessibilityIdentifier("tasks.new.chat")
            }
            ToolbarItem(placement: .navigationBarLeading) {
                Button { model.controller.hosts.showProfiles() } label: { Image(systemName: "line.3.horizontal") }
                    .accessibilityLabel("PC一覧")
                    .accessibilityIdentifier("tasks.hosts")
            }
            ToolbarItem(placement: .navigationBarTrailing) {
                Menu {
                    Button { model.controller.navigation.refreshTaskList() } label: { Label(
                        "更新",
                        systemImage: "arrow.clockwise"
                    ) }
                    .disabled(state.view.threadList is LoadPhaseLoading)
                    .accessibilityIdentifier("tasks.refresh")
                    Button {
                        if let id = state.selectedProfileId {
                            worktreeHost = WorktreeSettingsHost(id: id, name: state.selectedProfile?.name ?? "PC Host")
                        }
                    } label: { Label("ワークツリー設定", systemImage: "arrow.triangle.branch") }
                        .disabled(!state.isConnected)
                        .accessibilityIdentifier("tasks.worktree-settings")
                    Button { model.controller.hosts.showProfiles() } label: { Label(
                        "PC一覧",
                        systemImage: "laptopcomputer"
                    ) }
                } label: { Image(systemName: "ellipsis") }
                    .accessibilityLabel("その他")
                    .accessibilityIdentifier("tasks.menu")
            }
        }
        .sheet(item: $worktreeHost) { host in WorktreeSettingsSheet(model: model, host: host) }
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
