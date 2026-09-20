import AgentCore
import SwiftUI

struct ThreadsScreen: View {
    @ObservedObject var model: BexAppViewModel
    @State private var search = ""
    @State private var expandedProjectIds = Set<String>()
    @State private var worktreeHost: WorktreeSettingsHost?
    @State private var showingSettings = false

    var body: some View {
        let groupedThreads = Dictionary(grouping: model.list?.threads ?? [], by: \.projectId)
        let projects = model.list?.projects ?? []
        let chats = groupedThreads[nil] ?? []
        List {
            if let notice = model.list?.notice {
                BexNotice(text: notice).taskListRowStyle()
            }
            if model.threadLoadState == .failed {
                Section {
                    if let error = model.notice {
                        BexNotice(text: error)
                            .taskListRowStyle()
                    }
                    Button("再試行") { model.refreshTaskList() }
                        .accessibilityIdentifier("tasks.retry")
                        .taskListRowStyle()
                }
                .listSectionSeparator(.hidden)
            }
            if let notice = model.notice {
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

            ForEach(projects, id: \.id) { project in
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
                        Button { model.openNewThread(cwd: project.roots.first?.path ?? "") } label: {
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
                        if (model.list?.moreProjectIds ?? []).contains(project.id) {
                            Button("もっと見る") { model.expandTaskList(projectId: project.id) }
                                .padding(.leading, 40)
                                .disabled(model.loadingThreads)
                                .accessibilityIdentifier("tasks.project.\(project.id).more")
                                .taskListRowStyle()
                        }
                    }
                }
                .listSectionSeparator(.hidden)
            }

            if model.list?.hasMoreProjects == true {
                Section {
                    Button("もっと見る") { model.expandTaskList(projects: true) }
                        .disabled(model.loadingThreads)
                        .accessibilityIdentifier("tasks.projects.more")
                        .taskListRowStyle()
                }
                .listSectionSeparator(.hidden)
            }

            if model.threadLoadState == .ready, (model.list?.projects ?? []).isEmpty {
                Section {
                    Text("Codexに登録されたプロジェクトはありません")
                        .font(.subheadline)
                        .foregroundColor(.secondary)
                        .taskListRowStyle()
                }
                .listSectionSeparator(.hidden)
            }

            Section {
                if model.threadLoadState == .ready, chats.isEmpty {
                    Text("プロジェクトに属さないチャットはありません")
                        .font(.subheadline)
                        .foregroundColor(.secondary)
                        .accessibilityIdentifier("tasks.empty")
                        .taskListRowStyle()
                } else {
                    ForEach(chats, id: \.id) { thread in
                        ThreadListRow(thread: thread) { model.openThread(thread.id) }
                    }
                    if model.list?.hasMoreChats == true {
                        Button("もっと見る") { model.expandTaskList() }
                            .disabled(model.loadingThreads)
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
        .onChange(of: model.threadLoadState) { _, state in
            if state == .ready {
                model.recordListViewUpdate()
            }
        }
        .onAppear {
            if model.threadLoadState == .ready {
                model.recordListViewUpdate()
            }
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
            DefaultToolbarItem(kind: .search, placement: .bottomBar)
            ToolbarSpacer(.flexible, placement: .bottomBar)
            ToolbarItem(placement: .principal) {
                VStack(spacing: 1) {
                    Text("リモート").font(.headline)
                    HStack(spacing: 5) {
                        if model.isConnecting || model.threadLoadState == .loading {
                            ProgressView().controlSize(.mini)
                                .accessibilityLabel(model.isConnecting ? "接続中" : "読み込み中")
                                .accessibilityIdentifier("connection.progress")
                        } else {
                            Circle().fill(model.isConnected ? Color.green : Color.secondary).frame(width: 6, height: 6)
                        }
                        Image(systemName: "laptopcomputer")
                        Text(model.selectedProfileName ?? "PC Host").lineLimit(1)
                    }
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .accessibilityElement(children: .combine)
                    .accessibilityLabel(
                        "\(model.selectedProfileName ?? "PC Host")、\(model.isConnected ? "接続済み" : "未接続")"
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
                    Button { showingSettings = true } label: { Label("設定", systemImage: "gearshape") }
                        .disabled(!model.isConnected)
                        .accessibilityIdentifier("tasks.settings")
                    Button { model.refreshTaskList() } label: { Label("更新", systemImage: "arrow.clockwise") }
                        .disabled(model.threadLoadState == .loading)
                        .accessibilityIdentifier("tasks.refresh")
                    Button {
                        if let id = model.selectedProfileId {
                            worktreeHost = WorktreeSettingsHost(id: id, name: model.selectedProfileName ?? "PC Host")
                        }
                    } label: { Label("ワークツリー設定", systemImage: "arrow.triangle.branch") }
                        .disabled(!model.isConnected)
                        .accessibilityIdentifier("tasks.worktree-settings")
                    Button { model.showProfiles() } label: { Label("PC一覧", systemImage: "laptopcomputer") }
                } label: { Image(systemName: "ellipsis") }
                    .accessibilityLabel("その他")
                    .accessibilityIdentifier("tasks.menu")
            }
        }
        .sheet(isPresented: $showingSettings) { AppSettingsSheet(model: model) }
        .sheet(item: $worktreeHost) { host in WorktreeSettingsSheet(
            connected: model.isConnected && model.selectedProfileId == host.id,
            request: model.requestSnapshot,
            settings: model.snapshot.worktreeSettings(),
            host: host
        ) }
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
    let thread: ThreadSummary
    var indented = false
    let open: () -> Void

    var body: some View {
        Button(action: open) {
            HStack(spacing: 10) {
                Text(thread.title).font(.title3).foregroundColor(.primary).lineLimit(1)
                Spacer()
                if thread.active {
                    ProgressView().controlSize(.small)
                        .accessibilityIdentifier("tasks.running.\(thread.id)")
                } else if thread.unread {
                    Circle().fill(Color.white).frame(width: 8, height: 8)
                        .accessibilityLabel("完了・未確認")
                        .accessibilityIdentifier("tasks.completed.\(thread.id)")
                }
                if thread.worktreeMerged {
                    Image("GitMerge")
                        .resizable()
                        .frame(width: 18, height: 18)
                        .foregroundStyle(.purple)
                        .accessibilityLabel("main にマージ済み")
                        .accessibilityIdentifier("tasks.merged.\(thread.id)")
                }
            }
            .padding(.leading, indented ? 40 : 0)
            .contentShape(Rectangle())
        }
        .accessibilityIdentifier("tasks.row.\(thread.id)")
        .accessibilityValue(thread.active ? "実行中" : thread.unread ? "完了・未確認" : "")
        .taskListRowStyle()
    }
}
