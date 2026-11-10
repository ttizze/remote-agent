import Foundation
#if os(iOS)
    import SwiftUI
#endif

struct ProjectEditorData: Decodable {
    let id: String
    let name: String
    let section: String
    let error: String?
}

#if os(iOS)
/// The phones' project editor; the Mac draws its dialog with NSH.
struct ProjectEditorPresenter: View {
    let editor: ProjectEditorData?
    let send: ([String: String]) -> Void

    var body: some View {
        Color.clear.sheet(isPresented: Binding(
            get: { editor != nil },
            set: {
                if !$0, let editor {
                    send(["action": "cancel", "token": editor.id])
                }
            }
        )) {
            if let editor {
                ProjectEditorForm(editor: editor, send: send).id(editor.id)
            }
        }
    }
}

private struct ProjectEditorForm: View {
    let editor: ProjectEditorData
    let send: ([String: String]) -> Void
    @State private var name: String
    @State private var section: String

    init(editor: ProjectEditorData, send: @escaping ([String: String]) -> Void) {
        self.editor = editor
        self.send = send
        _name = State(initialValue: editor.name)
        _section = State(initialValue: editor.section)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("プロジェクトを編集").font(.title2.weight(.semibold))
            Text("表示名とセクションはこの端末・接続先ごとに保存されます。フォルダや会話履歴は残ります。")
                .font(.callout).foregroundStyle(.secondary)
            Form {
                TextField("表示名", text: $name).accessibilityIdentifier("project.editor.name")
                TextField("セクション", text: $section, prompt: Text("空欄で未分類"))
                    .accessibilityIdentifier("project.editor.section")
            }
            if let error = editor.error {
                Text(verbatim: error).foregroundStyle(.red).accessibilityIdentifier("project.editor.error")
            }
            HStack {
                Spacer()
                Button("キャンセル", role: .cancel) { send(["action": "cancel", "token": editor.id]) }
                    .keyboardShortcut(.cancelAction)
                Button("保存") { send(["action": "save", "token": editor.id, "name": name, "section": section]) }
                    .keyboardShortcut(.defaultAction).accessibilityIdentifier("project.editor.save")
            }
        }
        .padding(24)
        #if os(iOS)
            .presentationDetents([.medium])
        #endif
    }
}

#endif
