import AgentCore
import SwiftUI

struct ThreadsScreen: View {
    let model: AppViewModel
    @State private var search = ""
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        let projects = model.list?.projects ?? []
        let chats = model.list?.threads ?? []
        List {
            if !model.profiles.isEmpty {
                ErrorNotices(notices: model.notices[.projects] ?? [], perform: { model.perform($0) }).taskListRowStyle()
                if model.threadLoadState == .failed {
                    Section {
                        Button("再試行") { model.refreshTaskList() }
                            .accessibilityIdentifier("tasks.retry")
                            .taskListRowStyle()
                    }
                    .listSectionSeparator(.hidden)
                }
                if let notice = model.notice {
                    Section {
                        AppNotice(text: notice)
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

                ForEach(projects, id: \.listKey) { project in
                    let expanded = Binding(get: { project.expanded }, set: {
                        model.perform(.setProjectExpanded(projectId: project.id, expanded: $0), on: project.hostId)
                    })
                    Section {
                        DisclosureGroup(isExpanded: expanded) {
                            if project.loading, !project.hasMore {
                                ProgressView().taskListRowStyle(leading: 40)
                            }
                            if let error = project.error {
                                AppNotice(text: error).taskListRowStyle(leading: 40)
                                Button("再試行") {
                                    model.perform(.refreshProject(projectId: project.id), on: project.hostId)
                                }.taskListRowStyle(leading: 40)
                            }
                            ForEach(project.threads, id: \.listKey) { thread in
                                ThreadListRow(thread: thread, leadingInset: 40) { model.openThread(
                                    thread.id,
                                    on: thread.hostId
                                ) }
                            }
                            if project.hasMore {
                                Button {
                                    model.expandTaskList(projectId: project.id, host: project.hostId)
                                } label: {
                                    HStack(spacing: 8) {
                                        if project.loading {
                                            ProgressView().controlSize(.mini)
                                        }
                                        Text("もっと見る")
                                    }
                                }
                                .disabled(project.loading)
                                .accessibilityIdentifier("tasks.project.\(project.id).more")
                                .taskListRowStyle(leading: 40)
                            }
                        } label: {
                            HStack(spacing: 16) {
                                Button { expanded.wrappedValue.toggle() } label: {
                                    HStack(spacing: 16) {
                                        ProjectIcon(
                                            png: colorScheme == .dark ? project.icon?.darkPng : project.icon?.lightPng,
                                            monogram: project.monogram,
                                            colorRGB: project.iconColor
                                        )
                                        Text(project.name).font(.title3).lineLimit(1)
                                        Spacer(minLength: 0)
                                    }
                                    .frame(minHeight: 44)
                                    .contentShape(Rectangle())
                                }
                                .buttonStyle(.plain)
                                .accessibilityIdentifier("tasks.project.\(project.id)")
                                .accessibilityValue(project.expanded ? "開いています" : "閉じています")
                                Button { model.openNewThread(cwd: project.roots.first?.path ?? "", on: project.hostId)
                                } label: {
                                    Image(systemName: "square.and.pencil").font(.title3)
                                        .foregroundColor(.secondary).frame(width: 44, height: 44)
                                        .contentShape(Rectangle())
                                }
                                .buttonStyle(.borderless)
                                .accessibilityLabel("\(project.name)で新しいタスク")
                                .accessibilityIdentifier("tasks.new.project.\(project.id)")
                            }
                        }
                        .disclosureGroupStyle(ProjectDisclosureStyle())
                        .taskListRowStyle()
                    }
                    .listSectionSeparator(.hidden)
                }

                if model.list?.hasMoreProjects == true {
                    Section {
                        Button {
                            for host in model.listedHostIds {
                                model.perform(.expandProjects, on: host)
                            }
                        } label: {
                            HStack(spacing: 8) {
                                if model.list?.loadingProjects == true {
                                    ProgressView().controlSize(.mini)
                                }
                                Text("もっとプロジェクトを表示")
                            }
                        }
                        .disabled(model.loadingThreads || model.list?.loadingProjects == true)
                        .accessibilityIdentifier("tasks.projects.more")
                        .taskListRowStyle()
                    }
                    .listSectionSeparator(.hidden)
                }

                if model.threadLoadState == .ready, (model.list?.projects ?? []).isEmpty {
                    Section {
                        Text("登録されたプロジェクトはありません")
                            .font(.subheadline)
                            .foregroundColor(.secondary)
                            .taskListRowStyle()
                    }
                    .listSectionSeparator(.hidden)
                }

                Section {
                    if model.threadLoadState == .ready, chats.isEmpty {
                        Text("チャットがありません")
                            .font(.subheadline)
                            .foregroundColor(.secondary)
                            .accessibilityIdentifier("tasks.empty")
                            .taskListRowStyle()
                    }
                    ForEach(chats, id: \.listKey) { thread in
                        ThreadListRow(thread: thread) { model.openThread(thread.id, on: thread.hostId) }
                    }
                    if model.list?.hasMore == true {
                        Button {
                            model.expandTaskList()
                        } label: {
                            HStack(spacing: 8) {
                                if model.list?.loadingChats == true {
                                    ProgressView().controlSize(.mini)
                                }
                                Text("もっと見る")
                            }
                        }
                        .disabled(model.loadingThreads || model.list?.loadingChats == true)
                        .accessibilityIdentifier("tasks.chats.more")
                        .taskListRowStyle()
                    }
                } header: {
                    Text("チャット")
                        .font(.title3.weight(.semibold))
                        .textCase(nil)
                        .foregroundColor(.primary)
                }
                .listSectionSeparator(.hidden)
            }
        }
        // Keep the list identifier from propagating to the connection overlay.
        .accessibilityIdentifier("tasks.list")
        .overlay {
            if model.profiles.isEmpty {
                TaskConnectionEmptyState(connect: model.openPairing)
            }
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
        .searchable(text: $search, placement: .toolbar, prompt: "チャットを検索")
        .refreshable { model.refreshTaskList() }
        .task(id: "\(model.allProfiles):\(model.selectedProfileId ?? ""):\(search)") {
            do { try await Task.sleep(nanoseconds: 200_000_000) } catch { return }
            model.searchTaskList(search)
        }
        .toolbar {
            DefaultToolbarItem(kind: .search, placement: .bottomBar)
            ToolbarSpacer(.flexible, placement: .bottomBar)
            ToolbarItem(placement: .principal) {
                if !model.profiles.isEmpty {
                    TaskConnectionsMenu(profiles: model.profiles, selected: model.selectedProfileId,
                                        all: model.allProfiles, connecting: model.isConnecting,
                                        loading: model.threadLoadState == .loading,
                                        connected: model.isConnected) { host in
                        if let host {
                            model.selectProfile(host)
                        } else {
                            model.selectAllProfiles()
                        }
                    }
                }
            }
            ToolbarItem(placement: .topBarLeading) {
                Image("BrandIcon").resizable().scaledToFit().frame(width: 24, height: 24)
                    .clipShape(RoundedRectangle(cornerRadius: 5))
                    .accessibilityLabel("Bex")
            }
            .sharedBackgroundVisibility(.hidden)
            ToolbarItem(placement: .bottomBar) {
                Button { model.openNewThread(cwd: "") } label: {
                    Image(systemName: "square.and.pencil")
                }
                .buttonStyle(.borderedProminent)
                .tint(.primary)
                .disabled(model.profiles.isEmpty)
                .accessibilityLabel("プロジェクトなしで新しいタスク")
                .accessibilityIdentifier("tasks.new.chat")
            }
            ToolbarItem(placement: .navigationBarTrailing) {
                Menu {
                    Button(action: model.showSettings) { Label("設定", systemImage: "gearshape") }
                        .accessibilityIdentifier("tasks.settings")
                    Button { model.refreshTaskList() } label: { Label("更新", systemImage: "arrow.clockwise") }
                        .disabled(model.threadLoadState == .loading || model.profiles.isEmpty)
                        .accessibilityIdentifier("tasks.refresh")
                } label: { Image(systemName: "ellipsis") }
                    .accessibilityLabel("その他")
                    .accessibilityIdentifier("tasks.menu")
            }
        }
    }
}

private extension ProjectSummary {
    var listKey: String {
        "\(hostId):\(id)"
    }
}

private extension ThreadSummary {
    var listKey: String {
        "\(hostId):\(id.provider.bexKey):\(id.id)"
    }
}

/// Keep the standard group's rows while its label provides separate expand and compose buttons.
private struct ProjectDisclosureStyle: DisclosureGroupStyle {
    func makeBody(configuration: Configuration) -> some View {
        Group {
            configuration.label
            if configuration.isExpanded {
                configuration.content
            }
        }
    }
}

private extension View {
    func taskListRowStyle(leading: CGFloat = 20) -> some View {
        listRowSeparator(.hidden)
            .listRowInsets(EdgeInsets(top: 0, leading: leading, bottom: 0, trailing: 20))
            .listRowBackground(Color.clear)
    }
}

private struct ThreadListRow: View {
    let thread: ThreadSummary
    var leadingInset: CGFloat = 20
    let open: () -> Void

    private var accessibilityID: String {
        "\(thread.id.provider.bexKey):\(thread.id.id)"
    }

    var body: some View {
        Button(action: open) {
            HStack(spacing: 10) {
                Text(thread.title).font(.title3).foregroundColor(.primary).lineLimit(1)
                Spacer()
                if thread.waiting {
                    Image(systemName: TaskActivityAttributes.IconKind.waiting.symbol)
                        .foregroundStyle(.orange)
                        .accessibilityLabel("確認待ち")
                        .accessibilityIdentifier("tasks.waiting.\(accessibilityID)")
                } else if thread.active {
                    RunningIndicator().controlSize(.small)
                        .accessibilityIdentifier("tasks.running.\(accessibilityID)")
                } else if thread.unread {
                    Circle().fill(Color.primary).frame(width: 8, height: 8)
                        .accessibilityLabel("完了・未確認")
                        .accessibilityIdentifier("tasks.completed.\(accessibilityID)")
                }
                if let status = thread.worktreeStatus {
                    let unmerged = status == .unmerged
                    Image(unmerged ? "GitDiff" : "GitMerge")
                        .resizable()
                        .frame(width: 18, height: 18)
                        .foregroundStyle(unmerged ? .orange : .purple)
                        .accessibilityLabel(LocalizedStringKey(unmerged ? "main に未反映の変更あり" : "main にマージ済み"))
                        .accessibilityIdentifier(
                            "tasks.\(unmerged ? "unmerged" : "merged").\(accessibilityID)"
                        )
                }
            }
            .contentShape(Rectangle())
        }
        .accessibilityIdentifier("tasks.row.\(accessibilityID)")
        .accessibilityValue(LocalizedStringKey(
            thread.waiting ? "確認待ち" : thread.active ? "実行中" : thread.unread ? "完了・未確認" : ""
        ))
        .taskListRowStyle(leading: leadingInset)
    }
}

/// On iOS 26.6 devices a running row kept the indicator's layout space while the
/// indicator stayed stopped and invisible, and refreshing the unchanged row did not
/// restart it. A new indicator on each appearance and foreground return starts animating.
private struct RunningIndicator: View {
    @Environment(\.scenePhase) private var scenePhase
    @State private var generation = 0

    var body: some View {
        ProgressView()
            .id(generation)
            .onAppear { generation += 1 }
            .onChange(of: scenePhase) { _, phase in
                if phase == .active {
                    generation += 1
                }
            }
    }
}
