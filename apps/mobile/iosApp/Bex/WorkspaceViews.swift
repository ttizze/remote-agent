import AgentCore
import SwiftUI
import UIKit

extension FileEntry: @retroactive Identifiable {
    public var id: String {
        path
    }
}

struct ConversationDirectorySheet: View {
    let model: ConversationModel
    let destination: WorkspaceDestination
    let close: () -> Void

    var body: some View {
        NavigationStack {
            WorkspaceScreen(
                model: model,
                root: destination.directory,
                draft: destination.scope.draft,
                initialFile: destination.filePath.map { FileEntry(
                    name: ($0 as NSString).lastPathComponent,
                    path: $0,
                    directory: false,
                    size: 0
                ) },
                initialLine: destination.fileLine
            ) {
                close()
            }
            .onDisappear {
                guard model.host == destination.scope.host,
                      model.coreDraftKey == destination.scope.draft else { return }
                model.perform(.reviewWorkspace(ReviewWorkspace(cwd: model.cwd)))
            }
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("閉じる", action: close)
                }
            }
        }
    }
}

struct WorkspaceScreen: View {
    let model: ConversationModel
    let root: String
    let draft: DraftKey
    var initialFile: FileEntry?
    var initialLine: UInt32?
    let close: () -> Void

    var body: some View {
        WorkspaceDirectoryScreen(
            files: model.files,
            home: model.hostHomeDirectory,
            directory: root,
            initialFile: initialFile,
            initialLine: initialLine,
            perform: request,
            fileDraft: fileDraft,
            downloadFile: model.download,
            aiEdit: { model.perform(.setDraftText(
                threadId: draft,
                text: "このファイルを編集してください: \($0)\n変更内容: "
            )) },
            close: close
        )
        .background(Color(uiColor: .systemGroupedBackground))
    }

    private func request(_ intent: Intent, completion: @escaping (AgentCore.Snapshot, Result<Outcome, Error>) -> Void) {
        model.requestSnapshot(intent) { snapshot, result in
            model.notice = nil
            completion(snapshot, result)
        }
    }

    private func fileDraft(_ path: String) -> Binding<String> {
        Binding(get: {
            model.files.fileDraft(path: path)?.text ??
                model.files.file().flatMap { $0.path == path ? $0.text : nil } ?? ""
        }, set: { model.perform(.setFileDraft(path: path, text: $0)) })
    }
}

private struct WorkspaceDirectoryScreen: View {
    let files: AgentCore.Files
    let home: String?
    let directory: String
    let initialFile: FileEntry?
    let initialLine: UInt32?
    let perform: SnapshotRequest
    let fileDraft: (String) -> Binding<String>
    let downloadFile: @MainActor (String) async throws -> URL
    let aiEdit: (String) -> Void
    let close: () -> Void
    @State private var owner = UUID().uuidString
    @State private var refreshCompletion: CheckedContinuation<Void, Never>?
    @State private var selected: FileEntry?
    @State private var selectedLine: UInt32?

    private var root: String {
        fileHostPath(path: directory, home: home)
    }

    private var key: FileTreeKey {
        FileTreeKey(owner: owner, root: root)
    }

    var body: some View {
        let tree = files.fileTree(key: key)
        ScrollViewReader { proxy in
            List {
                if let error = tree.error {
                    AppNotice(text: error)
                }
                if tree.truncated {
                    Text("ほかにも一致する項目があります。検索語を絞り込んでください。")
                        .font(.caption).foregroundStyle(.secondary)
                }
                ForEach(tree.rows, id: \.path) { row in
                    Button { send(.click(path: row.path, additive: false, range: false)) } label: {
                        fileRow(row)
                    }
                    .foregroundStyle(.primary)
                    .listRowInsets(EdgeInsets(top: 0, leading: 16 + CGFloat(row.depth) * 18, bottom: 0, trailing: 16))
                    .listRowBackground(row.selected ? Color.primary.opacity(0.08) : nil)
                    .accessibilityLabel(row.path)
                    .accessibilityIdentifier("file.\(row.label)")
                    .accessibilityAddTraits(row.selected ? .isSelected : [])
                    .id(row.path)
                }
            }
            .listStyle(.plain)
            .accessibilityIdentifier("files.list")
            .overlay {
                if tree.rows.isEmpty {
                    emptyState(pending: tree.pending, error: tree.error, query: tree.query)
                }
            }
            .scrollDismissesKeyboard(.interactively)
            .refreshable {
                await withCheckedContinuation { continuation in
                    refreshCompletion?.resume()
                    refreshCompletion = continuation
                    perform(.fileTree(key: key, action: .refresh)) { next, _ in
                        if !next.files().fileTree(key: key).refreshing {
                            finishRefresh()
                        }
                    }
                }
            }
            .onChange(of: tree.refreshing) {
                if !tree.refreshing {
                    finishRefresh()
                }
            }
            .onChange(of: tree.revealId) {
                if let path = tree.focusedPath {
                    proxy.scrollTo(path)
                }
            }
        }
        .navigationTitle("ファイル")
        .navigationBarTitleDisplayMode(.inline)
        .searchable(text: Binding(get: { tree.query }, set: { send(.query(text: $0)) }), prompt: "ファイルを検索")
        .onAppear {
            send(.open(surface: .mobile, locale: Locale.current.identifier, workspaceRoot: root))
            if let initialFile {
                selectedLine = initialLine
                selected = initialFile
            }
        }
        .onDisappear { send(.close); finishRefresh() }
        .onChange(of: tree.openFile) { _, request in
            guard let request else { return }
            selectedLine = nil
            let path = fileTreePath(root: root, relative: request.path)
            selected = FileEntry(name: request.path.components(separatedBy: "/").last ?? request.path,
                                 path: path, directory: false, size: 0)
        }
        .sheet(item: $selected) { entry in
            WorkspaceFileSheet(files: files, text: fileDraft(entry.path), perform: perform,
                               entry: entry, line: selectedLine, download: downloadFile) { path in
                aiEdit(path)
                selected = nil
                close()
            }
        }
    }

    @ViewBuilder private func emptyState(pending: Bool, error: ErrorText?, query: String) -> some View {
        if pending {
            ProgressView()
        } else if error != nil {
            ContentUnavailableView {
                Label { AppText("ファイルを表示できません") } icon: { Image(systemName: "exclamationmark.triangle") }
            } actions: {
                Button("再試行") { send(.refresh) }
            }
        } else if !query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            ContentUnavailableView.search(text: query)
        } else {
            ContentUnavailableView {
                Label { AppText("ファイルが見つかりません") } icon: { Image(systemName: "folder") }
            } description: {
                AppText("作業スペースは空です。")
            }
        }
    }

    private func fileRow(_ row: FileTreeRow) -> some View {
        HStack(spacing: 8) {
            Group {
                if row.kind == .directory {
                    Image(systemName: row.expanded ? "chevron.down" : "chevron.right")
                        .font(.system(size: 12)).foregroundStyle(.secondary)
                } else {
                    Color.clear
                }
            }.frame(width: 12)
            if row.kind == .directory {
                Image(systemName: "folder").font(.system(size: 17)).foregroundStyle(.secondary)
                    .frame(width: 17, height: 17)
            } else {
                Image(uiImage: mobileFileImage(row.path)).resizable().scaledToFit().frame(width: 17, height: 17)
            }
            Text(row.label).font(.system(size: 14, weight: row.selected ? .bold : .medium))
                .foregroundStyle(row.selected ? Color.primary : row
                    .ignored ? Color(uiColor: .tertiaryLabel) : .secondary)
                .lineLimit(1).frame(maxWidth: .infinity, alignment: .leading)
            if row.kind == .directory, row.expanded, row.loading {
                ProgressView().controlSize(.small)
            }
        }
        .contentShape(Rectangle())
    }

    private func finishRefresh() {
        refreshCompletion?.resume()
        refreshCompletion = nil
    }

    private func send(_ action: FileTreeAction) {
        perform(.fileTree(key: key, action: action)) { _, _ in }
    }
}
