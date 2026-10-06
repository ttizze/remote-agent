import AgentCore
import SwiftUI

struct ThreadsScreen: View {
    @ObservedObject var model: BexAppViewModel
    @State private var search = ""
    @State private var collapsed: Set<ShelfKind> = []
    @State private var settledLimit: UInt32 = 10
    @State private var showArchive = false
    @State private var showingSettings = false
    @State private var addingProject = false
    @State private var projectPath = ""

    var body: some View {
        TimelineView(.periodic(from: .now, by: 1)) { clock in
            let now = Int64(clock.date.timeIntervalSince1970 * 1000)
            let shelves = (try? model.snapshot.shelves(nowMillis: now, settledLimit: settledLimit)) ?? []
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    HStack(spacing: 8) {
                        Image(systemName: "magnifyingglass").foregroundStyle(AppTheme.color("textMuted"))
                        TextField("Search threads", text: $search).font(AppTheme.font(14))
                            .onChange(of: search) { _, query in model.perform(.search(query: query)) }
                        Menu {
                            Button("All projects") { model.perform(.filterProject(projectId: nil)) }
                            ForEach(model.snapshot.projects(), id: \.id) { project in
                                Button(project.name) { model.perform(.filterProject(projectId: project.id)) }
                            }
                        } label: { Image(systemName: "folder") }
                        Button { addingProject = true } label: { Image(systemName: "folder.badge.plus") }
                        Button { model.openNewThread() } label: { Image(systemName: "square.and.pencil") }
                    }
                    .padding(10).background(
                        AppTheme.color("sidebarControlSurface"),
                        in: RoundedRectangle(cornerRadius: 8)
                    )
                    .padding(.vertical, 10)
                    if let id = model.snapshot.selectedProjectId() {
                        HStack {
                            Text("Project: " +
                                (model.snapshot.projects().first { $0.id == id }?.name ?? "Chats")); Button("Clear") {
                                    model.perform(.filterProject(projectId: nil))
                                }
                        }.font(AppTheme.font(12))
                    }
                    if let error = model.notice {
                        BexNotice(text: error).padding(.vertical, 8)
                    }
                    if model.isConnecting {
                        ProgressView("Connecting…").padding(.vertical)
                    }
                    ForEach(shelves, id: \.kind) { shelf in
                        Button {
                            if collapsed.remove(shelf.kind) == nil {
                                collapsed.insert(shelf.kind)
                            }
                        } label: {
                            HStack {
                                Image(systemName: collapsed.contains(shelf.kind) ? "chevron.right" : "chevron.down")
                                    .font(.system(size: 10))
                                Text(shelf.title).font(AppTheme.font(13, weight: .medium))
                                Text("\(shelf.total)").font(AppTheme.font(11))
                                    .foregroundStyle(AppTheme.color("textMuted"))
                                Spacer()
                            }.foregroundStyle(AppTheme.color("sidebarForeground")).frame(height: 34)
                        }.buttonStyle(.plain)
                        if !collapsed.contains(shelf.kind) {
                            ForEach(shelf.rows, id: \.id) { row in ThreadCard(model: model, row: row) }
                            if shelf.hasMore {
                                Button("Load 25 more") { settledLimit += 25 }.font(AppTheme.font(13)).padding(
                                    .vertical,
                                    12
                                )
                            }
                        }
                    }
                    DisclosureGroup("Archived", isExpanded: $showArchive) {
                        ForEach((try? model.snapshot.archivedThreads(nowMillis: now)) ?? [], id: \.id) { row in
                            ThreadCard(
                                model: model,
                                row: row
                            )
                        }
                    }.font(AppTheme.font(13)).padding(.vertical, 12)
                }.padding(.horizontal, 20).padding(.bottom, 28)
            }
        }
        .background(AppTheme.color("sidebar")).foregroundStyle(AppTheme.color("text"))
        .navigationTitle(model.selectedProfileName ?? "Bex").navigationBarTitleDisplayMode(.inline)
        .toolbar { Button { showingSettings = true } label: { Image(systemName: "gearshape") } }
        .sheet(isPresented: $showingSettings) { SettingsSheet(model: model) }
        .alert("Add project", isPresented: $addingProject) {
            TextField("Absolute path on Host", text: $projectPath).textInputAutocapitalization(.never)
            Button("Add") { model.perform(.registerProject(path: projectPath)); projectPath = "" }
            Button("Cancel", role: .cancel) {}
        }
        .onAppear { search = model.snapshot.searchQuery(); model.recordListViewUpdate() }
    }
}

private struct ThreadCard: View {
    let model: BexAppViewModel
    let row: ThreadRow
    var body: some View {
        Button { model.openThread(row.id) } label: {
            HStack(alignment: .top, spacing: 10) {
                let icon = row.providerKind == .claude ? "claude" : "openai"
                Image(icon).resizable().scaledToFit().frame(width: 16, height: 16)
                VStack(alignment: .leading, spacing: 5) {
                    Text(row.title).font(AppTheme.font(14, weight: row.unread ? .bold : .medium)).lineLimit(1)
                    if !row.slim {
                        Text(row.preview).font(AppTheme.font(12)).foregroundStyle(AppTheme.color("textMuted"))
                            .lineLimit(1)
                        HStack(spacing: 6) {
                            Text(row.status).foregroundStyle(AppTheme.status(row.tone))
                            if let duration = row
                                .durationMs {
                                Text("\(duration / 1000)s").monospacedDigit()
                                    .foregroundStyle(AppTheme.color("textMuted"))
                            }
                            if let wake = row.wakeLabel {
                                Text(wake).foregroundStyle(AppTheme.color("textMuted"))
                            }
                            Spacer()
                            Text(model.snapshot.projects().first { $0.id == row.projectId }?.name ?? "")
                                .foregroundStyle(AppTheme.color("textMuted"))
                        }.font(AppTheme.font(11))
                        if let branch = row
                            .branch {
                            Text(branch).font(.system(size: 11, design: .monospaced))
                                .foregroundStyle(AppTheme.color("textMuted")).lineLimit(1)
                        }
                    }
                }
                Spacer(minLength: 0)
                if row.unread {
                    Circle().fill(AppTheme.color("accent")).frame(width: 5, height: 5)
                }
            }
            .foregroundStyle(AppTheme.color("sidebarForeground")).padding(.horizontal, 12).padding(
                .vertical,
                row.slim ? 8 : 12
            )
            .frame(maxWidth: .infinity, minHeight: row.slim ? 36 : 82, alignment: .leading)
            .background(
                AppTheme.color(row.selected ? "mobileSelected" : "surface"),
                in: RoundedRectangle(cornerRadius: 10)
            )
        }
        .buttonStyle(.plain).padding(.bottom, 1)
        .contextMenu { ThreadActions(
            model: model,
            id: row.id,
            pinned: row.pinned,
            archived: row.archived,
            settled: row.settled,
            snoozed: row.snoozed
        ) }
    }
}
