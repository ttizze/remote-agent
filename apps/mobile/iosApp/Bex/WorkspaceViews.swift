import AgentCore
import SwiftUI
import UIKit

extension FileEntry: @retroactive Identifiable {
    public var id: String {
        path
    }
}

/// Own directory navigation independently of the conversation's navigation stack.
private struct WorkspaceNavigation: UIViewControllerRepresentable {
    let root: String
    let content: (String, @escaping (String) -> Void) -> WorkspaceDirectoryScreen

    func makeCoordinator() -> Coordinator {
        Coordinator(content: content)
    }

    func makeUIViewController(context: Context) -> UINavigationController {
        context.coordinator.open(root)
        return context.coordinator.navigation
    }

    func updateUIViewController(_ controller: UINavigationController, context: Context) {
        let coordinator = context.coordinator
        coordinator.content = content
        for case let page as UIHostingController<WorkspaceDirectoryScreen> in controller.viewControllers {
            page.rootView = coordinator.screen(page.rootView.directory)
        }
    }

    final class Coordinator: NSObject, UINavigationControllerDelegate {
        let navigation = UINavigationController()
        var content: (String, @escaping (String) -> Void) -> WorkspaceDirectoryScreen

        init(content: @escaping (String, @escaping (String) -> Void) -> WorkspaceDirectoryScreen) {
            self.content = content
            super.init()
            navigation.delegate = self
        }

        func navigationController(_ navigationController: UINavigationController,
                                  willShow viewController: UIViewController, animated: Bool) {
            navigationController.setNavigationBarHidden(
                viewController === navigationController.viewControllers.first, animated: animated
            )
        }

        func screen(_ directory: String) -> WorkspaceDirectoryScreen {
            content(directory) { [weak self] in self?.open($0) }
        }

        func open(_ directory: String) {
            let page = UIHostingController(rootView: screen(directory))
            page.edgesForExtendedLayout = []
            page.title = URL(fileURLWithPath: directory).lastPathComponent
            page.navigationItem.largeTitleDisplayMode = .never
            navigation.pushViewController(page, animated: !navigation.viewControllers.isEmpty)
        }
    }
}

struct WorkspaceScreen: View {
    @ObservedObject var model: BexAppViewModel
    let root: String
    @Binding var showingDiff: Bool
    let close: () -> Void
    @State private var diffSelection: TurnDiffOption?

    var body: some View {
        VStack(spacing: 0) {
            Picker("表示", selection: $showingDiff) {
                Text("変更済み").tag(true).accessibilityIdentifier("files.diff")
                Text("すべてのファイル").tag(false).accessibilityIdentifier("files.all")
            }
            .pickerStyle(.segmented).padding(.horizontal).padding(.bottom)
            Divider()
            if showingDiff {
                HStack {
                    Menu(diffSelection?.label ?? "Workspace changes") {
                        Button("Workspace changes") { diffSelection = nil }
                        ForEach(model.snapshot.turnDiffOptions(), id: \.label) { option in
                            Button(option.label) { diffSelection = option }
                        }
                    }
                    Spacer()
                }.padding()
                WorkspaceDiffScreen { complete in
                    let intent = diffSelection.map { Intent.readTurnDiff(
                        fromTurnCount: $0.fromTurnCount,
                        toTurnCount: $0.toTurnCount,
                        ignoreWhitespace: false
                    ) } ?? .reviewWorkspace(cwd: root)
                    request(intent) { snapshot, result in
                        if case let .failure(failure) = result {
                            complete(.failure(failure))
                        } else if let review = snapshot.review() {
                            Task {
                                let files = await Task.detached(priority: .userInitiated) { review.diffFiles() }.value
                                complete(.success(files))
                            }
                        } else {
                            complete(.failure(NSError(domain: "BexWorkspace", code: 1,
                                                      userInfo: [
                                                          NSLocalizedDescriptionKey: "差分を取得できませんでした。再試行してください。"
                                                      ])))
                        }
                    }
                }
                .id(diffSelection.map { "\($0.fromTurnCount):\($0.toTurnCount)" } ?? "workspace")
            } else {
                WorkspaceNavigation(root: root) { directory, openDirectory in
                    WorkspaceDirectoryScreen(snapshot: model.snapshot, directory: directory,
                                             perform: request, fileDraft: fileDraft,
                                             downloadFile: model.download,
                                             aiEdit: { model.draft = "このファイルを編集してください: \($0)\n変更内容: " },
                                             close: close, openDirectory: openDirectory)
                }
            }
        }
        .background(T3.color("canvas"))
    }

    private func request(_ intent: Intent, completion: @escaping (AgentCore.Snapshot, Result<Outcome, Error>) -> Void) {
        model.requestSnapshot(intent) { snapshot, result in
            model.notice = nil
            completion(snapshot, result)
        }
    }

    private func fileDraft(_ path: String) -> Binding<String> {
        Binding(get: {
            model.snapshot.fileDraft(path: path) ??
                model.snapshot.file().flatMap { $0.path == path ? $0.text : nil } ?? ""
        }, set: { model.perform(.editFile(path: path, text: $0)) })
    }
}

private struct WorkspaceDirectoryScreen: View {
    let snapshot: AgentCore.Snapshot
    let directory: String
    let perform: SnapshotRequest
    let fileDraft: (String) -> Binding<String>
    let downloadFile: @MainActor (String) async throws -> URL
    let aiEdit: (String) -> Void
    let close: () -> Void
    let openDirectory: (String) -> Void
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
                Button("開く") { openDirectory(path) }
                    .accessibilityIdentifier("files.open-path")
            }.padding(.horizontal).padding(.vertical, 8)
            List {
                Button("親ディレクトリ") { openDirectory((directory as NSString).deletingLastPathComponent) }
                    .disabled(directory == "/")
                ForEach(entries) { entry in
                    HStack {
                        if entry.directory {
                            Button { openDirectory(entry.path) } label: {
                                Label(entry.name, systemImage: "folder")
                            }
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
            .listStyle(.plain)
        }
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
        path = directory
        busy = true; error = nil
        perform(.listFiles(path: directory)) { snapshot, result in
            busy = false
            if case let .failure(failure) = result {
                error = failure.localizedDescription; return
            }
            guard let result = snapshot.directory(), result.path == directory else {
                error = snapshot.error()
                return
            }
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
                        Text("+\(additions)").foregroundColor(T3.color("successForeground"))
                    }
                    if let deletions = file.deletions {
                        Text("−\(deletions)").foregroundColor(T3.color("errorForeground"))
                    }
                }.font(.subheadline).padding(12).contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("diff.file.\(file.path)")
            .background(T3.color("mobileGroupedCard"))
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
                    .background(row.kind == "+" ? T3.color("successForeground").opacity(0.18) :
                        row.kind == "-" ? T3.color("errorForeground").opacity(0.18) :
                        row.kind == "@" ? Color.secondary.opacity(0.12) : Color.clear)
                }
            }
        }
        .background(T3.color("surface"))
        .clipShape(RoundedRectangle(cornerRadius: 12))
    }
}
