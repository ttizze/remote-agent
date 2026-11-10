import NativeSettingsViews
import Foundation
#if os(iOS)
    import SwiftUI
#endif

struct WorktreeEditorData: Decodable {
    struct Editor: Decodable {
        struct Receipt: Decodable, Equatable {
            let id: String
            let succeeded: Bool
        }

        let id: String
        let host: String
        let settings: WorktreeSettingsInputs?
        let pending: Bool
        let error: String?
        let receipt: Receipt?
    }

    struct Row: Decodable, Identifiable {
        struct Thread: Decodable {
            let id: WorktreeSession
            let name: String
            let active: Bool
        }

        var id: String {
            path
        }

        let path: String
        let project: String
        let branch: String
        let threads: [Thread]
        let reason: String?
    }

    let editor: Editor
    let hosts: [SettingsScopeItem]
    let projects: [SettingsScopeItem]
    let connected: Bool
    let rows: [Row]?
}

struct WorktreeSession: Codable, Hashable {
    let provider: String
    let id: String
}

enum WorktreeAction: Encodable {
    case load
    case close
    case host(id: String)
    case save(id: String, settings: WorktreeSettingsInputs)
    case remove(path: String)
    case review(path: String)
    case open(thread: WorktreeSession)
}

private func sendWorktree(_ action: WorktreeAction, token: String, send: ([String: String]) -> Void) {
    guard let data = try? JSONEncoder().encode(action), let event = String(data: data, encoding: .utf8) else { return }
    send(["token": token, "event": event])
}

#if os(iOS)
struct WorktreeEditorPresenter: View {
    let data: WorktreeEditorData?
    let send: ([String: String]) -> Void

    var body: some View {
        Color.clear.sheet(isPresented: Binding(get: { data != nil }, set: {
            if !$0, let data {
                sendWorktree(.close, token: data.editor.id, send: send)
            }
        })) {
            if let data {
                WorktreeEditorForm(data: data, send: send).id(data.editor.id)
            }
        }
    }
}

private struct WorktreeEditorForm: View {
    private struct Submission {
        let id: String
        let inputs: WorktreeSettingsInputs
    }

    let data: WorktreeEditorData
    let send: ([String: String]) -> Void
    @State private var inputs: WorktreeSettingsInputs
    @State private var project: String?
    @State private var submission: Submission?
    @State private var removal: String?

    init(data: WorktreeEditorData, send: @escaping ([String: String]) -> Void) {
        self.data = data
        self.send = send
        _inputs = State(initialValue: data.editor.settings ?? WorktreeSettingsInputs())
    }

    private var busy: Bool {
        data.editor.pending || submission != nil
    }

    private var loaded: Bool {
        data.editor.settings != nil
    }

    private var fieldsEnabled: Bool {
            loaded && data.connected && !busy
    }

    var body: some View {
        NavigationStack {
            Form {
                Section("適用範囲") {
                    SettingsScopeFields(profiles: data.hosts, selectedProfileId: data.editor.host,
                                        disabled: busy, select: { emit(.host(id: $0)) },
                                        project: $project, projects: data.projects)
                }
                WorktreeSettingsFields(
                    inputs: Binding(get: { inputs }, set: { value in edit(value) }),
                    project: project,
                    enabled: fieldsEnabled
                )
                if busy {
                    ProgressView().accessibilityLabel("設定を通信中")
                }
                if !data.connected {
                    Text("作業環境との接続を確認してください。")
                }
                if let error = data.editor.error {
                    Section {
                        Text(verbatim: error).foregroundStyle(.red).accessibilityIdentifier("worktree.error")
                        Button("再読み込み") { emit(.load) }.disabled(busy || !data.connected)
                    }
                }
                managedWorktrees
            }
            .formStyle(.grouped)
            .navigationTitle("ワークツリー")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("完了") { emit(.close) }.disabled(busy).keyboardShortcut(.cancelAction)
                    }
                        ToolbarItem(placement: .confirmationAction) {
                            Button("保存", action: save).disabled(!loaded || busy || !data.connected)
                                .accessibilityIdentifier("worktree.save")
                        }
                }
        }
        .interactiveDismissDisabled(busy)
        .onChange(of: data.editor.settings) { old, new in
            if let new, submission == nil, old == nil || inputs == old {
                inputs = new
            }
        }
        .onChange(of: data.editor.receipt) { _, receipt in
            guard let receipt, let sent = submission, receipt.id == sent.id else { return }
            submission = nil
            guard receipt.succeeded else { return }
                emit(.close)
        }
        .confirmationDialog("作業ディレクトリを削除しますか？", isPresented: Binding(get: { removal != nil }, set: {
            if !$0 {
                removal = nil
            }
        }), titleVisibility: .visible) {
            if let path = removal {
                Button("削除", role: .destructive) { emit(.remove(path: path)); removal = nil }
            }
            Button("キャンセル", role: .cancel) { removal = nil }
        } message: {
            Text("この作業ディレクトリを削除します。ブランチと会話履歴は残ります。次のメッセージ送信時に、元プロジェクトの main から同じ場所に作り直します。")
        }
    }

    private var managedWorktrees: some View {
        Section {
            if let rows = data.rows {
                if rows.isEmpty {
                    Text("Bexで作成したワークツリーはありません。").foregroundStyle(.secondary)
                }
                ForEach(rows) { row in
                    VStack(alignment: .leading, spacing: 8) {
                        HStack {
                            Text(verbatim: "\(row.project) · \(row.branch)").fontWeight(.semibold)
                            Spacer()
                            Button("削除…", role: .destructive) { removal = row.path }
                                .disabled(row.reason != nil || busy || !data.connected)
                        }
                        Text(verbatim: row.path).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                        ForEach(row.threads, id: \.id) { thread in
                            Button { emit(.open(thread: thread.id)) } label: {
                                Label(thread.active ? "実行中 · \(thread.name)" : thread.name, systemImage: "text.bubble")
                            }.disabled(busy || !data.connected)
                        }
                        Button("変更を確認") { emit(.review(path: row.path)) }.disabled(busy || !data.connected)
                        if let reason = row.reason {
                            Text(verbatim: reason).font(.caption).foregroundStyle(.secondary)
                        }
                    }
                    .padding(.vertical, 4)
                }
            } else {
                ProgressView("一覧を読み込み中…")
            }
            Button("ワークツリー一覧を更新") { emit(.load) }.disabled(busy || !data.connected)
        } header: { Text("作成済みのワークツリー") }
    }

    private func emit(_ action: WorktreeAction) {
        sendWorktree(action, token: data.editor.id, send: send)
    }

    private func edit(_ value: WorktreeSettingsInputs) {
        inputs = value
    }

    private func save() {
        guard loaded, !busy, data.connected else { return }
        let id = UUID().uuidString
        submission = Submission(id: id, inputs: inputs)
        emit(.save(id: id, settings: inputs))
    }
}

#endif
