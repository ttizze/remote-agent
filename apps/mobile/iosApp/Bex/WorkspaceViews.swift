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
    var opensDiff = false
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationView {
            WorkspaceDirectoryScreen(model: model, root: root, directory: root, opensDiff: opensDiff) { dismiss() }
        }
        .navigationViewStyle(StackNavigationViewStyle())
    }
}

private struct WorkspaceDirectoryScreen: View {
    @ObservedObject var model: BexAppViewModel
    let root: String
    let directory: String
    var opensDiff = false
    let close: () -> Void
    @State private var destinationPath: String?
    @State private var path = ""
    @State private var entries: [WorkspaceEntry] = []
    @State private var error: String?
    @State private var busy = false
    @State private var selected: WorkspaceEntry?
    @State private var diff: [String] = []
    @State private var showingDiff = false
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
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                Button("閉じる", action: close).accessibilityIdentifier("files.close")
            }
            ToolbarItem(placement: .primaryAction) {
                Button("差分") {
                    loadDiff()
                }.accessibilityIdentifier("files.diff")
            }
        }
        .onAppear {
            if path.isEmpty {
                load(directory); if opensDiff {
                    loadDiff()
                }
            }
        }
        .sheet(item: $selected) { entry in
            FileEditorSheet(model: model, entry: entry, download: download) { path in
                model.draft = "このファイルを編集してください: \(path)\n変更内容: "
                selected = nil
                close()
            }
        }
        .sheet(isPresented: $showingDiff) {
            NavigationView {
                ScrollView([.horizontal, .vertical]) {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(Array(diff.enumerated()), id: \.offset) { _, line in
                            Text(line.isEmpty ? " " : line).font(.caption.monospaced())
                                .foregroundColor(line.hasPrefix("+") ? .green : line.hasPrefix("-") ? .red : .primary)
                                .textSelection(.enabled)
                        }
                    }.padding()
                }
                .navigationTitle("作業中の差分")
                .toolbar {
                    Button("閉じる") { showingDiff = false }.accessibilityIdentifier("files.diff.close")
                }
            }
        }
        .sheet(item: $sharedFile, onDismiss: cleanupDownload) { item in
            FileShareSheet(url: item.url)
        }
    }

    private func loadDiff() {
        busy = true; error = nil
        model.workspace("host/workspace/review", ["cwd": root]) { result, message in
            busy = false; error = message
            if let value = result?["diff"] as? String {
                diff = value.components(separatedBy: "\n"); showingDiff = true
            }
        }
    }

    private func load(_ directory: String) {
        busy = true; error = nil
        model.workspace("host/file/list", ["path": directory]) { result, message in
            busy = false; error = message
            guard let result else { return }
            path = result["path"] as? String ?? directory
            entries = (result["entries"] as? [[String: Any]] ?? []).compactMap {
                guard let name = $0["name"] as? String, let path = $0["path"] as? String else { return nil }
                return WorkspaceEntry(name: name, path: path, directory: $0["directory"] as? Bool ?? false)
            }
            if result["truncated"] as? Bool == true {
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

private struct FileEditorSheet: View {
    @ObservedObject var model: BexAppViewModel
    let entry: WorkspaceEntry
    let download: (String) -> Void
    let aiEdit: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var text = ""
    @State private var revision = ""
    @State private var savedText = ""
    @State private var error: String?
    @State private var busy = false
    @State private var initialized = false
    @State private var confirmReload = false

    private var draftKey: String {
        "bex.editor.v4:\(model.state.selectedProfileId ?? ""):\(entry.path)"
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
                TextEditor(text: $text).font(.body.monospaced())
                    .textInputAutocapitalization(.never).disableAutocorrection(true)
                    .accessibilityIdentifier("file.editor")
                    .disabled(revision.isEmpty)
                    .onChange(of: text) { value in
                        guard initialized else { return }
                        if value == savedText {
                            UserDefaults.standard.removeObject(forKey: draftKey)
                        } else {
                            UserDefaults.standard.set(["text": value, "revision": revision], forKey: draftKey)
                        }
                    }
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
                        let submitted = text
                        model.workspace(
                            "host/file/write",
                            ["path": entry.path, "revision": revision, "text": submitted]
                        ) { result, message in
                            busy = false; error = message
                            if let result, let nextRevision = result["revision"] as? String {
                                revision = nextRevision; savedText = result["text"] as? String ?? submitted
                                if text ==
                                    submitted {
                                    text = savedText; UserDefaults.standard.removeObject(forKey: draftKey)
                                } else {
                                    UserDefaults.standard.set(["text": text, "revision": revision], forKey: draftKey)
                                }
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
                    UserDefaults.standard.removeObject(forKey: draftKey); load(restoreDraft: false)
                }
            }
        }
    }

    private func load(restoreDraft: Bool) {
        busy = true; error = nil
        model.workspace("host/file/read", ["path": entry.path]) { result, message in
            busy = false; error = message
            guard let result, let value = result["text"] as? String,
                  let version = result["revision"] as? String else { return }
            initialized = false
            text = value; savedText = value; revision = version
            if restoreDraft, let draft = UserDefaults.standard.dictionary(forKey: draftKey) as? [String: String],
               let cached = draft["text"] {
                text = cached
                if let original = draft["revision"] {
                    revision = original
                }
                if revision != version {
                    error = "ホストのファイルが変更されています。下書きは保持しました。再読込すると下書きを破棄します。"
                }
            }
            initialized = true
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
