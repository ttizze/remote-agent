import AgentCore
import SwiftUI

struct SettingsSheet: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject var model: BexAppViewModel

    var body: some View {
        NavigationStack {
            List {
                Section {
                    NavigationLink {
                        ModelSettingsScreen(model: model, defaults: true, close: { dismiss() })
                    } label: { Label("モデル", systemImage: "slider.horizontal.3") }
                        .accessibilityIdentifier("settings.models")
                    NavigationLink {
                        AgentSettingsScreen(model: model, close: { dismiss() })
                    } label: { Label("エージェント", systemImage: "bubble.left.and.bubble.right") }
                        .accessibilityIdentifier("settings.agents")
                        .disabled(!model.isConnected)
                    Button {
                        dismiss()
                        model.showProfiles()
                    } label: {
                        HStack {
                            Label("端末と接続", systemImage: "network")
                            Spacer()
                            Image(systemName: "chevron.right").font(.caption.weight(.semibold))
                                .foregroundStyle(.tertiary)
                        }.foregroundStyle(.primary)
                    }
                    .accessibilityIdentifier("settings.connections")
                    NavigationLink {
                        WorktreeSettingsScreen(connected: model.isConnected, request: model.requestSnapshot,
                                               environmentName: model.selectedProfileName ?? "作業環境")
                    } label: { Label("ワークツリー", systemImage: "arrow.triangle.branch") }
                        .accessibilityIdentifier("settings.worktrees")
                        .disabled(!model.isConnected)
                }
            }
            .safeAreaInset(edge: .top, spacing: 0) {
                SettingsScopeBar(projects: "すべてのプロジェクト", environment: model.selectedProfileName ?? "未選択")
            }
            .navigationTitle("設定")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("完了") { dismiss() }.accessibilityIdentifier("settings.close")
                }
            }
        }
    }
}

struct WorktreeSettingsScreen: View {
    let connected: Bool
    let request: SnapshotRequest
    let environmentName: String
    @Environment(\.dismiss) private var dismiss
    @State private var createOnNewSession = false
    @State private var copyOnCreate = false
    @State private var copyPaths = ""
    @State private var directory = ""
    @State private var loaded = false
    @State private var busy = false
    @State private var error: String?

    var body: some View {
        Form {
            Section {
                Text("この環境に保存し、新しい会話に適用します。")
                    .font(.footnote).foregroundColor(.secondary)
            }
            Section {
                Toggle("新規セッションをワークツリーで開始", isOn: $createOnNewSession)
                    .accessibilityIdentifier("worktree.create")
                TextField("この環境の絶対パス（空欄で既定）", text: $directory)
                    .textInputAutocapitalization(.never).autocorrectionDisabled()
                    .accessibilityLabel("ワークツリーの保存先")
                    .accessibilityIdentifier("worktree.directory")
            } header: { Text("作成と保存先") }
                footer: { Text("保存先に「セッション名/リポジトリ名」の構成で作ります。空欄なら元のリポジトリ内の .worktree に保存します。既存のワークツリーは移動しません。") }
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
                Text("作業環境との接続を確認してください。")
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
        .safeAreaInset(edge: .top, spacing: 0) {
            SettingsScopeBar(projects: "すべてのプロジェクト", environment: environmentName)
        }
        .navigationTitle("ワークツリー")
        .navigationBarTitleDisplayMode(.inline)
        .navigationBarBackButtonHidden(true)
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
            copyPaths: paths, worktreeDirectory: directory.trimmingCharacters(in: .whitespacesAndNewlines)
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

struct SettingsScopeBar: View {
    let projects: String
    let environment: String

    var body: some View {
        HStack(spacing: 8) {
            Text("設定の適用先").foregroundStyle(.secondary)
            Text("\(projects) ／ \(environment)")
        }
        .font(.caption)
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 20).padding(.vertical, 12)
        .background(Color(UIColor.secondarySystemGroupedBackground))
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("settings.scope")
    }
}
