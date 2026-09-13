import AgentCore
import SwiftUI
import UIKit

extension FileEntry: @retroactive Identifiable {
    public var id: String {
        path
    }
}

struct WorkspaceSheet: View {
    @ObservedObject var model: BexAppViewModel
    let root: String
    @Binding var showingDiff: Bool
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text("作業中の変更").font(.headline)
                Spacer()
                Button("閉じる") { dismiss() }.accessibilityIdentifier("files.close")
            }.padding()
            Picker("表示", selection: $showingDiff) {
                Text("変更済み").tag(true).accessibilityIdentifier("files.diff")
                Text("すべてのファイル").tag(false).accessibilityIdentifier("files.all")
            }
            .pickerStyle(.segmented).padding(.horizontal).padding(.bottom)
            Divider()
            if showingDiff {
                WorkspaceDiffScreen { complete in
                    model.perform(.reviewWorkspace(ReviewWorkspace(cwd: root))) { result in
                        if case let .failure(failure) = result {
                            let error = model.snapshot.error() ?? failure.localizedDescription
                            if model.notice == error {
                                model.notice = nil
                            }
                            complete(.failure(NSError(domain: "BexWorkspace", code: 1,
                                                      userInfo: [NSLocalizedDescriptionKey: error])))
                        } else if let review = model.snapshot.review() {
                            complete(.success(review.diffFiles()))
                        } else {
                            complete(.failure(NSError(domain: "BexWorkspace", code: 1,
                                                      userInfo: [
                                                          NSLocalizedDescriptionKey: "差分を取得できませんでした。再試行してください。"
                                                      ])))
                        }
                    }
                }
            } else {
                NavigationView {
                    WorkspaceDirectoryScreen(snapshot: model.snapshot, root: root, directory: root,
                                             perform: model.requestSnapshot, fileDraft: fileDraft,
                                             downloadFile: model.download,
                                             aiEdit: { model.draft = "このファイルを編集してください: \($0)\n変更内容: " },
                                             close: { dismiss() })
                }.navigationViewStyle(StackNavigationViewStyle())
            }
        }
        .background(Color(uiColor: .systemGroupedBackground))
    }

    private func fileDraft(_ path: String) -> Binding<String> {
        Binding(get: {
            model.snapshot.fileDraft(path: path)?.text ??
                model.snapshot.file().flatMap { $0.path == path ? $0.text : nil } ?? ""
        }, set: { model.perform(.setFileDraft(path: path, text: $0)) })
    }
}

private struct WorkspaceDirectoryScreen: View {
    let snapshot: AgentCore.Snapshot
    let root: String
    let directory: String
    let perform: SnapshotRequest
    let fileDraft: (String) -> Binding<String>
    let downloadFile: @MainActor (String) async throws -> URL
    let aiEdit: (String) -> Void
    let close: () -> Void
    @State private var destinationPath: String?
    @State private var path = ""
    @State private var entries: [FileEntry] = []
    @State private var error: String?
    @State private var busy = false
    @State private var selected: FileEntry?
    @State private var sharedFile: SharedFile?

    var body: some View {
        VStack(spacing: 0) {
            if let error {
                BexNotice(text: error).padding()
            }
            if busy {
                ProgressView().padding()
            }
            HStack {
                TextField("絶対パス", text: $path)
                    .textInputAutocapitalization(.never).disableAutocorrection(true)
                    .textFieldStyle(.roundedBorder)
                Button("開く") { destinationPath = path }
                    .accessibilityIdentifier("files.open-path")
            }.padding()
            List {
                NavigationLink("親ディレクトリ") {
                    WorkspaceDirectoryScreen(
                        snapshot: snapshot,
                        root: root,
                        directory: (path as NSString).deletingLastPathComponent,
                        perform: perform, fileDraft: fileDraft, downloadFile: downloadFile, aiEdit: aiEdit, close: close
                    )
                }
                ForEach(entries) { entry in
                    HStack {
                        if entry.directory {
                            NavigationLink {
                                WorkspaceDirectoryScreen(snapshot: snapshot, root: root, directory: entry.path,
                                                         perform: perform, fileDraft: fileDraft,
                                                         downloadFile: downloadFile, aiEdit: aiEdit,
                                                         close: close)
                            } label: { Label(entry.name, systemImage: "folder") }
                                .accessibilityIdentifier("file.\(entry.name)")
                        } else {
                            Button { selected = entry } label: { Label(entry.name, systemImage: "doc") }
                                .buttonStyle(.borderless)
                                .accessibilityIdentifier("file.\(entry.name)")
                        }
                        Spacer()
                        if !entry.directory {
                            Button { download(entry.path) } label: { Image(systemName: "square.and.arrow.down") }
                                .buttonStyle(.borderless).accessibilityLabel("\(entry.name)をダウンロード")
                        }
                    }
                }
            }
        }
        .background(
            NavigationLink(isActive: Binding(
                get: { destinationPath != nil },
                set: {
                    if !$0 {
                        destinationPath = nil
                    }
                }
            )) {
                if let destinationPath {
                    WorkspaceDirectoryScreen(snapshot: snapshot, root: root, directory: destinationPath,
                                             perform: perform, fileDraft: fileDraft, downloadFile: downloadFile,
                                             aiEdit: aiEdit, close: close)
                }
            } label: { EmptyView() }
        )
        .navigationTitle(directory == root ? "ファイル" : URL(fileURLWithPath: directory).lastPathComponent)
        .navigationBarTitleDisplayMode(.inline)
        .onAppear {
            if path.isEmpty {
                load(directory)
            }
        }
        .sheet(item: $selected) { entry in
            FileEditorSheet(
                snapshot: snapshot,
                text: fileDraft(entry.path),
                perform: perform,
                entry: entry,
                download: download
            ) { path in
                aiEdit(path)
                selected = nil
                close()
            }
        }
        .sheet(item: $sharedFile, onDismiss: cleanupDownload) { item in
            FileShareSheet(url: item.url)
        }
    }

    private func load(_ directory: String) {
        busy = true; error = nil
        perform(.listFiles(ListFiles(path: directory))) { snapshot, result in
            busy = false
            if case let .failure(failure) = result {
                error = failure.localizedDescription; return
            }
            guard let result = snapshot.directory(), result.path == directory else { return }
            path = result.path
            entries = result.entries
            if result.truncated {
                error = "先頭 2,000 件を表示しています。パスを指定して開けます。"
            }
        }
    }

    private func download(_ path: String) {
        busy = true; error = nil
        Task {
            defer { busy = false }
            do {
                let url = try await downloadFile(path)
                selected = nil; sharedFile = SharedFile(url: url)
            } catch { self.error = error.localizedDescription }
        }
    }

    private func cleanupDownload() {
        if let directory = sharedFile?.url
            .deletingLastPathComponent() {
            try? FileManager.default.removeItem(at: directory)
        }
        sharedFile = nil
    }
}

private struct WorkspaceDiffScreen: View {
    let load: (@escaping (Result<[WorkspaceDiffFile], Error>) -> Void) -> Void
    @State private var files: [WorkspaceDiffFile] = []
    @State private var error: String?
    @State private var busy = true

    var body: some View {
        Group {
            if busy {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if let error {
                VStack {
                    BexNotice(text: error).padding()
                    Button("再試行", action: loadDiff).accessibilityIdentifier("files.diff.retry")
                }.frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if files.isEmpty {
                Text("変更はありません").foregroundColor(.secondary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    LazyVStack(spacing: 16) {
                        ForEach(files, id: \.path) { file in
                            WorkspaceDiffCard(file: file)
                        }
                    }.padding(12)
                }
            }
        }
        .onAppear(perform: loadDiff)
    }

    private func loadDiff() {
        busy = true; error = nil
        load { result in
            busy = false
            switch result {
            case let .success(result): files = result
            case let .failure(failure): error = failure.localizedDescription
            }
        }
    }
}

private struct WorkspaceDiffCard: View {
    let file: WorkspaceDiffFile
    @State private var expanded = true

    var body: some View {
        LazyVStack(spacing: 0) {
            Button { expanded.toggle() } label: {
                HStack(alignment: .top, spacing: 8) {
                    Image(systemName: expanded ? "chevron.down" : "chevron.right")
                    Text(file.path).fontWeight(.semibold).frame(maxWidth: .infinity, alignment: .leading)
                    if let additions = file.additions {
                        Text("+\(additions)").foregroundColor(.green)
                    }
                    if let deletions = file.deletions {
                        Text("−\(deletions)").foregroundColor(.red)
                    }
                }.font(.subheadline).padding(12).contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("diff.file.\(file.path)")
            .background(Color(uiColor: .secondarySystemGroupedBackground))
            if expanded {
                if file.rows.isEmpty {
                    Text("テキスト差分はありません").font(.caption).foregroundColor(.secondary).padding()
                }
                ForEach(file.rows.indices, id: \.self) { index in
                    let row = file.rows[index]
                    HStack(alignment: .top, spacing: 0) {
                        if row.kind == "+" || row.kind == "-" || row.kind == " " {
                            Text((row.new ?? row.old).map(String.init) ?? "")
                                .foregroundColor(.secondary).frame(width: 40, alignment: .trailing).padding(
                                    .trailing,
                                    8
                                )
                        }
                        Text(row.text.isEmpty ? " " : row.text)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .fixedSize(horizontal: false, vertical: true)
                            .textSelection(.enabled)
                    }
                    .font(.system(.footnote, design: .monospaced))
                    .padding(.vertical, 3).padding(.horizontal, 8)
                    .background(row.kind == "+" ? Color.green.opacity(0.18) :
                        row.kind == "-" ? Color.red.opacity(0.18) :
                        row.kind == "@" ? Color.secondary.opacity(0.12) : Color.clear)
                }
            }
        }
        .background(Color(uiColor: .systemBackground))
        .clipShape(RoundedRectangle(cornerRadius: 12))
    }
}
