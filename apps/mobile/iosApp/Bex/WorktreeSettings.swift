import RemoteAgentMobile
import SwiftUI

struct WorktreeSettingsHost: Identifiable {
    let id: String
    let name: String
}

struct WorktreeSettingsSheet: View {
    @ObservedObject var model: BexAppViewModel
    let host: WorktreeSettingsHost
    @Environment(\.dismiss) private var dismiss
    @State private var createOnNewSession = false
    @State private var copyOnCreate = false
    @State private var copyPaths = ""
    @State private var directory = ""
    @State private var loaded = false
    @State private var busy = false
    @State private var error: String?

    private var connected: Bool {
        model.state.isConnected && model.state.selectedProfileId == host.id
    }

    var body: some View {
        NavigationView {
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
                            Button("再読み込み") { Task { await load() } }.disabled(busy || !connected)
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
                    Button("保存") { Task { await save() } }.disabled(!loaded || busy || !connected)
                        .accessibilityIdentifier("worktree.save")
                }
            }
        }
        .navigationViewStyle(StackNavigationViewStyle())
        .interactiveDismissDisabled(busy)
        .task { await load() }
    }

    private func requestSettings(update: HostWorktreeSettings? = nil) async -> (HostWorktreeSettings?, String?) {
        await withCheckedContinuation { continuation in
            model.workspace.worktreeSettings(hostIdentity: host.id, update: update) { result, error in
                continuation.resume(returning: (result, error))
            }
        }
    }

    private func load() async {
        busy = true
        error = nil
        defer { busy = false }
        let (settings, failure) = await requestSettings()
        guard let settings, failure == nil else {
            error = failure ?? "設定の応答が無効です。"
            return
        }
        createOnNewSession = settings.createOnNewSession
        copyOnCreate = settings.copyOnCreate
        copyPaths = settings.copyPaths.joined(separator: "\n")
        directory = settings.worktreeDirectory
        loaded = true
    }

    private func save() async {
        busy = true
        error = nil
        defer { busy = false }
        let paths = copyPaths.components(separatedBy: .newlines)
            .map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
        let (result, failure) = await requestSettings(update: HostWorktreeSettings(
            createOnNewSession: createOnNewSession, copyOnCreate: copyOnCreate,
            copyPaths: paths, worktreeDirectory: directory.trimmingCharacters(in: .whitespacesAndNewlines)
        ))
        if result != nil, failure == nil {
            dismiss()
        } else {
            error = failure ?? "設定を保存できませんでした。"
        }
    }
}
