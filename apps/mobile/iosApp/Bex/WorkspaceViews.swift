import AgentCore
import SwiftUI
import UIKit

private struct WorkspaceEntry: Identifiable {
    var id: String {
        path
    }

    let name: String
    let path: String
    let directory: Bool
}

private struct SharedFile: Identifiable {
    let id = UUID()
    let url: URL
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
                WorkspaceDiffScreen(model: model, root: root)
            } else {
                NavigationView {
                    WorkspaceDirectoryScreen(model: model, root: root, directory: root) { dismiss() }
                }.navigationViewStyle(StackNavigationViewStyle())
            }
        }
        .background(Color(uiColor: .systemGroupedBackground))
    }
}

private struct WorkspaceDirectoryScreen: View {
    @ObservedObject var model: BexAppViewModel
    let root: String
    let directory: String
    let close: () -> Void
    @State private var destinationPath: String?
    @State private var path = ""
    @State private var entries: [WorkspaceEntry] = []
    @State private var error: String?
    @State private var busy = false
    @State private var selected: WorkspaceEntry?
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
                        model: model,
                        root: root,
                        directory: (path as NSString).deletingLastPathComponent,
                        close: close
                    )
                }
                ForEach(entries) { entry in
                    HStack {
                        if entry.directory {
                            NavigationLink {
                                WorkspaceDirectoryScreen(model: model, root: root, directory: entry.path, close: close)
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
                    WorkspaceDirectoryScreen(model: model, root: root, directory: destinationPath, close: close)
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
            FileEditorSheet(model: model, entry: entry, download: download) { path in
                model.draft = "このファイルを編集してください: \(path)\n変更内容: "
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
        model.perform(.listFiles(ListFiles(path: directory))) { result in
            busy = false
            if case let .failure(failure) = result {
                error = failure.localizedDescription; return
            }
            guard let result = model.snapshot.directory(), result.path == directory else { return }
            path = result.path
            entries = result.entries.map { WorkspaceEntry(name: $0.name, path: $0.path, directory: $0.directory) }
            if result.truncated {
                error = "先頭 2,000 件を表示しています。パスを指定して開けます。"
            }
        }
    }

    private func download(_ path: String) {
        busy = true; error = nil
        model.download(path) { url, message in
            busy = false; error = message
            if let url {
                selected = nil; sharedFile = SharedFile(url: url)
            }
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
    @ObservedObject var model: BexAppViewModel
    let root: String
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
        model.perform(.reviewWorkspace(ReviewWorkspace(cwd: root))) { result in
            busy = false
            if case let .failure(failure) = result {
                error = model.snapshot.error() ?? failure.localizedDescription
                if model.notice == error {
                    model.notice = nil
                }
                return
            }
            guard let review = model.snapshot.review() else {
                error = "差分を取得できませんでした。再試行してください。"; return
            }
            files = review.diffFiles()
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

private struct FileEditorSheet: View {
    @ObservedObject var model: BexAppViewModel
    let entry: WorkspaceEntry
    let download: (String) -> Void
    let aiEdit: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var error: String?
    @State private var busy = false
    @State private var initialized = false
    @State private var confirmReload = false
    private var file: FileContent? {
        model.snapshot.file().flatMap { $0.path == entry.path ? $0 : nil }
    }

    private var text: String {
        model.snapshot.fileDraft(path: entry.path)?.text ?? file?.text ?? ""
    }

    private var revision: String {
        model.snapshot.fileDraft(path: entry.path)?.revision ?? file?.revision ?? ""
    }

    private var savedText: String {
        file?.text ?? ""
    }

    var body: some View {
        NavigationView {
            VStack(alignment: .leading) {
                Text(entry.path).font(.caption).foregroundColor(.secondary).textSelection(.enabled).padding(.horizontal)
                if let error {
                    BexNotice(text: error).padding(.horizontal)
                }
                if busy {
                    ProgressView().padding()
                }
                TextEditor(text: Binding(
                    get: { text },
                    set: { model.perform(.setFileDraft(path: entry.path, text: $0)) }
                )).font(.body.monospaced())
                    .textInputAutocapitalization(.never).disableAutocorrection(true)
                    .accessibilityIdentifier("file.editor")
                    .disabled(revision.isEmpty)
                HStack {
                    Button("再読込") {
                        if text != savedText {
                            confirmReload = true
                        } else {
                            load(restoreDraft: false)
                        }
                    }
                    Button("ダウンロード") { download(entry.path) }
                    Button("AIで編集") { aiEdit(entry.path) }.accessibilityIdentifier("file.ai-edit")
                }.padding()
            }
            .navigationTitle(entry.name)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("閉じる") { dismiss() }.accessibilityIdentifier("file.close")
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("保存") {
                        busy = true; error = nil
                        model.perform(.saveFile(SaveFile(path: entry.path))) { result in
                            busy = false
                            if case let .failure(failure) = result {
                                error = failure.localizedDescription
                            }
                        }
                    }.disabled(busy || revision.isEmpty || text == savedText).accessibilityIdentifier("file.save")
                }
            }
            .onAppear {
                if !initialized {
                    load(restoreDraft: true)
                }
            }
            .confirmationDialog("保存していない編集を破棄して再読込しますか？", isPresented: $confirmReload, titleVisibility: .visible) {
                Button("編集を破棄して再読込", role: .destructive) {
                    load(restoreDraft: false)
                }
            }
        }
    }

    private func load(restoreDraft: Bool) {
        busy = true; error = nil
        model.perform(.readFile(ReadFile(path: entry.path, discardDraft: !restoreDraft))) { result in
            busy = false
            if case let .failure(failure) = result {
                error = failure.localizedDescription; return
            }
            initialized = true
            if let file, revision != file.revision {
                error = "ホストのファイルが変更されています。下書きは保持しました。再読込すると下書きを破棄します。"
            }
        }
    }
}

private struct FileShareSheet: UIViewControllerRepresentable {
    let url: URL
    func makeUIViewController(context _: Context) -> UIActivityViewController {
        let controller = UIActivityViewController(activityItems: [url], applicationActivities: nil)
        controller.completionWithItemsHandler = { _, _, _, _ in
            try? FileManager.default.removeItem(at: url.deletingLastPathComponent())
        }
        return controller
    }

    func updateUIViewController(_: UIActivityViewController, context _: Context) {}
}
