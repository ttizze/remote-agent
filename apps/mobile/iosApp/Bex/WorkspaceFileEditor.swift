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
                    NoticeText(text: error).padding(.horizontal)
                }
                if busy {
                    ProgressView().padding()
                }
                BufferedTextInput(value: $text, edit: { value, acknowledged in
                    perform(.editFile(path: entry.path, text: value)) { _, result in
                        acknowledged((try? result.get()) != nil)
                    }
                }, content: { TextEditor(text: $0) }).id(entry.path).font(.body.monospaced())
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

/// Keep native editing ahead of Store publication; every edit still dispatches synchronously.
struct BufferedTextInput<Content: View>: View {
    @Binding private var value: String
    @State private var text: String
    let content: (Binding<String>) -> Content
    let edit: (String, @escaping (Bool) -> Void) -> Void
    @State private var edits = DraftRevision()

    init(
        value: Binding<String>,
        edit: @escaping (String, @escaping (Bool) -> Void) -> Void,
        @ViewBuilder content: @escaping (Binding<String>) -> Content
    ) {
        _value = value
        _text = State(initialValue: value.wrappedValue)
        self.content = content
        self.edit = edit
    }

    var body: some View {
        let input = Binding(get: { text }, set: {
            guard text != $0 else { return }
            text = $0
            let revision = edits.edit($0).revision
            edit($0) { _ in
                if edits.acknowledge(revision), text != value {
                    text = value
                }
            }
        })
        content(input).onChange(of: value) { _, _ in
            if edits.pending == nil, text != value {
                text = value
            }
        }
    }
}
