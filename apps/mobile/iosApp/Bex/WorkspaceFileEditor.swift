import AgentCore
import SwiftUI

struct FileEditorSheet: View {
    let snapshot: AgentCore.Snapshot
    @Binding var text: String
    let perform: SnapshotRequest
    let entry: FileEntry
    let download: (String) -> Void
    let aiEdit: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var error: String?
    @State private var busy = false
    @State private var initialized = false
    @State private var confirmReload = false
    private var file: FileContent? {
        snapshot.file().flatMap { $0.path == entry.path ? $0 : nil }
    }

    private var revision: String {
        file?.revision ?? ""
    }

    private var savedText: String {
        file?.text ?? ""
    }

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading) {
                Text(entry.path).font(.caption).foregroundColor(.secondary).textSelection(.enabled).padding(.horizontal)
                if let error {
                    BexNotice(text: error).padding(.horizontal)
                }
                if busy {
                    ProgressView().padding()
                }
                BufferedTextInput(value: $text) { TextEditor(text: $0) }.font(.body.monospaced())
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
                        perform(.saveFile(path: entry.path)) { _, result in
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
        perform(.readFile(path: entry.path, discardDraft: !restoreDraft)) { _, result in
            busy = false
            if case let .failure(failure) = result {
                error = failure.localizedDescription; return
            }
            initialized = true
        }
    }
}
