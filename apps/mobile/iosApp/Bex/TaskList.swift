import AgentCore
import SwiftUI

struct ThreadsScreen: View {
    @ObservedObject var model: BexAppViewModel
    @State private var search = ""
    @State private var showingSettings = false

    var body: some View {
        let projects = model.list?.projects ?? []
        let chats = model.list?.threads ?? []
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
                            model.perform(.setProjectExpanded(projectId: project.id, expanded: !project.expanded))
                        } label: {
                            HStack(spacing: 16) {
                                ProjectIcon(
                                    png: project.iconPng,
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
                        Button { model.openNewThread(cwd: project.roots.first?.path ?? "") } label: {
                            Image(systemName: "square.and.pencil").font(.title3)
                                .foregroundColor(.secondary).frame(width: 44, height: 44)
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("\(project.name)で新しいタスク")
                        .accessibilityIdentifier("tasks.new.project.\(project.id)")
                    }
                    .taskListRowStyle()
                    if project.expanded {
                        if project.loading && !project.hasMore {
                            ProgressView().taskListRowStyle()
                        }
                        if let error = project.error {
                            BexNotice(text: error).taskListRowStyle()
                            Button("再試行") {
                                model.perform(.refreshProject(projectId: project.id))
                            }.taskListRowStyle()
                        }
                        let threads = project.threads
                        ForEach(threads, id: \.id) { thread in
                            ThreadListRow(thread: thread, indented: true) { model.openThread(thread.id) }
                        }
                        if project.hasMore {
                            Button {
                                model.expandTaskList(projectId: project.id)
                            } label: {
                                HStack(spacing: 8) {
                                    if project.loading {
                                        ProgressView().controlSize(.mini)
                                    }
                                    Text("もっと見る")
                                }
                            }
                                .padding(.leading, 40)
                                .disabled(project.loading)
                                .accessibilityIdentifier("tasks.project.\(project.id).more")
                                .taskListRowStyle()
                        }
                    }
                }
                .listSectionSeparator(.hidden)
            }

            if model.list?.hasMoreProjects == true {
                Section {
                    Button {
                        model.perform(.expandProjects)
                    } label: {
                        HStack(spacing: 8) {
                            if model.loadingThreads {
                                ProgressView().controlSize(.mini)
                            }
                            Text("もっとプロジェクトを表示")
                        }
                    }
                        .disabled(model.loadingThreads)
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
                ForEach(chats, id: \.id) { thread in
                    ThreadListRow(thread: thread) { model.openThread(thread.id) }
                }
                if model.list?.hasMore == true {
                    Button {
                        model.expandTaskList()
                    } label: {
                        HStack(spacing: 8) {
                            if model.loadingThreads {
                                ProgressView().controlSize(.mini)
                            }
                            Text("もっと見る")
                        }
                    }
                        .disabled(model.loadingThreads)
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
                                .tint(model.isConnecting ? .red : .green)
                                .accessibilityLabel(model.isConnecting ? "接続中" : "読み込み中")
                                .accessibilityIdentifier("connection.progress")
                        } else {
                            Circle().fill(model.isConnected ? Color.blue : Color.red).frame(width: 6, height: 6)
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
                        .accessibilityIdentifier("tasks.settings")
                    Button { model.refreshTaskList() } label: { Label("更新", systemImage: "arrow.clockwise") }
                        .disabled(model.threadLoadState == .loading)
                        .accessibilityIdentifier("tasks.refresh")
                } label: { Image(systemName: "ellipsis") }
                    .accessibilityLabel("その他")
                    .accessibilityIdentifier("tasks.menu")
            }
        }
        .sheet(isPresented: $showingSettings) { SettingsSheet(model: model) }
    }
}

private struct ProjectIcon: View {
    let png: Data?
    let monogram: String
    let colorRGB: UInt32

    var body: some View {
        let color = Color(
            red: Double((colorRGB >> 16) & 255) / 255,
            green: Double((colorRGB >> 8) & 255) / 255,
            blue: Double(colorRGB & 255) / 255
        )
        Group {
            if let png, let image = UIImage(data: png) {
                Image(uiImage: image).resizable().scaledToFit()
            } else {
                Text(monogram)
                    .font(.system(size: 10, weight: .bold, design: .rounded))
                    .foregroundStyle(color)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(color.opacity(0.15))
            }
        }
        .frame(width: 24, height: 24)
        .clipShape(RoundedRectangle(cornerRadius: 6))
        .accessibilityHidden(true)
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

    private var accessibilityID: String {
        "\(thread.id.provider == .codex ? "codex" : "claude"):\(thread.id.id)"
    }

    private var accessibilityPrefix: String {
        indented ? "tasks.project" : "tasks"
    }

    var body: some View {
        Button(action: open) {
            HStack(spacing: 10) {
                Text(thread.title).font(.title3).foregroundColor(.primary).lineLimit(1)
                Spacer()
                if thread.active {
                    ProgressView().controlSize(.small)
                        .accessibilityIdentifier("\(accessibilityPrefix).running.\(accessibilityID)")
                } else if thread.unread {
                    Circle().fill(Color.white).frame(width: 8, height: 8)
                        .accessibilityLabel("完了・未確認")
                        .accessibilityIdentifier("\(accessibilityPrefix).completed.\(accessibilityID)")
                }
                if let status = thread.worktreeStatus {
                    let unmerged = status == .unmerged
                    Image(unmerged ? "GitDiff" : "GitMerge")
                        .resizable()
                        .frame(width: 18, height: 18)
                        .foregroundStyle(unmerged ? .orange : .purple)
                        .accessibilityLabel(unmerged ? "main に未反映の変更あり" : "main にマージ済み")
                        .accessibilityIdentifier(
                            "\(accessibilityPrefix).\(unmerged ? "unmerged" : "merged").\(accessibilityID)"
                        )
                }
            }
            .padding(.leading, indented ? 40 : 0)
            .contentShape(Rectangle())
        }
        .accessibilityIdentifier("\(accessibilityPrefix).row.\(accessibilityID)")
        .accessibilityValue(thread.active ? "実行中" : thread.unread ? "完了・未確認" : "")
        .taskListRowStyle()
    }
}
