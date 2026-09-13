import AgentCore
import SwiftUI

struct WorktreeSettingsHost: Identifiable {
    let id: String
    let name: String
}

struct WorktreeSettingsSheet: View {
    let connected: Bool
    let request: SnapshotRequest
    let settings: WorktreeSettings?
    let host: WorktreeSettingsHost
    @Environment(\.dismiss) private var dismiss
    @State private var createOnNewSession = false
    @State private var copyOnCreate = false
    @State private var copyPaths = ""
    @State private var directory = ""
    @State private var loaded = false
    @State private var busy = false
    @State private var error: String?

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Text(host.name).font(.headline)
                    Text("このMacに保存し、Mac・iPhoneからの新規セッションに適用します。")
                        .font(.footnote).foregroundColor(.secondary)
                }
                Section {
                    Toggle("新規セッションをワークツリーで開始", isOn: $createOnNewSession)
                        .accessibilityIdentifier("worktree.create")
                    TextField("接続先Mac上の絶対パス（空欄で既定）", text: $directory)
                        .textInputAutocapitalization(.never).autocorrectionDisabled()
                        .accessibilityLabel("ワークツリーの保存先")
                        .accessibilityIdentifier("worktree.directory")
                } header: { Text("作成と保存先") }
                    footer: { Text("指定フォルダ内にセッションごとのフォルダを作ります。空欄ならリポジトリのGit管理領域に保存します。既存のワークツリーは移動しません。") }
                    .disabled(!loaded || busy || !connected)
                Section {
                    Toggle("作成時にファイルをコピー", isOn: $copyOnCreate)
                        .accessibilityIdentifier("worktree.copy")
                    TextEditor(text: $copyPaths)
                        .frame(minHeight: 88)
                        .textInputAutocapitalization(.never).autocorrectionDisabled()
                        .accessibilityLabel("コピー対象")
                        .accessibilityIdentifier("worktree.paths")
                } header: { Text("コピー対象") }
                    footer: { Text("リポジトリからの相対パスを1行に1つ指定します（例: .env、config/local）。存在しないパスはスキップします。") }
                    .disabled(!loaded || busy || !connected)
                if busy {
                    ProgressView().accessibilityLabel("設定を通信中")
                }
                if !connected {
                    Text("接続先Macとの接続を確認してください。")
                }
                if let error {
                    Section {
                        Text(error).foregroundColor(.red).accessibilityIdentifier("worktree.error")
                        if !loaded {
                            Button("再読み込み", action: load).disabled(busy || !connected)
                        }
                    }
                }
            }
            .navigationTitle("ワークツリー設定")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("キャンセル") { dismiss() }.disabled(busy)
                        .accessibilityIdentifier("worktree.cancel")
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("保存", action: save).disabled(!loaded || busy || !connected)
                        .accessibilityIdentifier("worktree.save")
                }
            }
        }
        .interactiveDismissDisabled(busy)
        .onAppear(perform: load)
    }

    private func load() {
        busy = true; error = nil
        request(.readWorktreeSettings(ReadWorktreeSettings())) { snapshot, result in
            busy = false
            if case let .failure(failure) = result {
                error = failure.localizedDescription; return
            }
            guard let settings = snapshot.worktreeSettings() else { error = "設定の応答が無効です。"; return }
            createOnNewSession = settings.createOnNewSession
            copyOnCreate = settings.copyOnCreate
            copyPaths = settings.copyPaths.joined(separator: "\n")
            directory = settings.worktreeDirectory
            loaded = true
        }
    }

    private func save() {
        busy = true; error = nil
        let paths = copyPaths.components(separatedBy: .newlines)
            .map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
        request(.updateWorktreeSettings(UpdateWorktreeSettings(settings: WorktreeSettings(
            createOnNewSession: createOnNewSession, copyOnCreate: copyOnCreate,
            copyPaths: paths, worktreeDirectory: directory.trimmingCharacters(in: .whitespacesAndNewlines),
            extra: settings?.extra ?? [:]
        )))) { snapshot, result in
            busy = false
            if case let .failure(failure) = result {
                error = failure.localizedDescription; return
            }
            guard snapshot.worktreeSettings() != nil else { error = "設定を保存できませんでした。"; return }
            dismiss()
        }
    }
}
