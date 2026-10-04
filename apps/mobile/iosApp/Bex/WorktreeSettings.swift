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
                        ModelSettingsScreen(model: model, scope: .global, close: { dismiss() })
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
                        WorktreeSettingsScreen(model: model).id(model.selectedProfileId)
                    } label: { Label("ワークツリー", systemImage: "arrow.triangle.branch") }
                        .accessibilityIdentifier("settings.worktrees")
                        .disabled(!model.isConnected)
                }
            }
            .safeAreaInset(edge: .top, spacing: 0) {
                SettingsScopeBar { Text("すべてのプロジェクト") } environment: {
                    EnvironmentScopeMenu(model: model)
                }
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
    @ObservedObject var model: BexAppViewModel
    @Environment(\.dismiss) private var dismiss
    @State private var createOnNewSession = false
    @State private var copyOnCreate = false
    @State private var deleteMerged = false
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
                .disabled(!loaded || busy || !model.isConnected)
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
                .disabled(!loaded || busy || !model.isConnected)
            Section {
                Toggle("マージ済みを自動削除", isOn: $deleteMerged)
                    .accessibilityIdentifier("worktree.deleteMerged")
            } header: { Text("自動削除") }
                footer: { Text("main に取り込まれた作業場所を毎分確認します。実行中・回答待ち・ターミナル使用中・ローカル変更ありの場合は保留し、後で再確認します。ブランチと会話履歴は残ります。") }
                .disabled(!loaded || busy || !model.isConnected)
            if busy {
                ProgressView().accessibilityLabel("設定を通信中")
            }
            if !model.isConnected {
                Text("作業環境との接続を確認してください。")
            }
            if let error {
                Section {
                    Text(error).foregroundColor(.red).accessibilityIdentifier("worktree.error")
                    if !loaded {
                        Button("再読み込み", action: load).disabled(busy || !model.isConnected)
                    }
                }
            }
        }
        .safeAreaInset(edge: .top, spacing: 0) {
            SettingsScopeBar { Text("すべてのプロジェクト") } environment: {
                EnvironmentScopeMenu(model: model).disabled(busy)
            }
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
                Button("保存", action: save).disabled(!loaded || busy || !model.isConnected)
                    .accessibilityIdentifier("worktree.save")
            }
        }
        .interactiveDismissDisabled(busy)
        .onAppear(perform: load)
        .onChange(of: model.isConnected) { connected in
            if connected {
                load()
            }
        }
    }

    private func load() {
        guard model.isConnected else { return }
        busy = true; error = nil
        model.requestSnapshot(.readWorktreeSettings(ReadWorktreeSettings())) { snapshot, result in
            busy = false
            if case let .failure(failure) = result {
                error = failure.localizedDescription; return
            }
            guard let settings = snapshot.worktreeSettings() else { error = "設定の応答が無効です。"; return }
            createOnNewSession = settings.createOnNewSession
            copyOnCreate = settings.copyOnCreate
            deleteMerged = settings.deleteMerged
            copyPaths = settings.copyPaths.joined(separator: "\n")
            directory = settings.worktreeDirectory
            loaded = true
        }
    }

    private func save() {
        busy = true; error = nil
        let paths = copyPaths.components(separatedBy: .newlines)
            .map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
        model.requestSnapshot(.updateWorktreeSettings(UpdateWorktreeSettings(settings: WorktreeSettings(
            createOnNewSession: createOnNewSession, copyOnCreate: copyOnCreate,
            copyPaths: paths, worktreeDirectory: directory.trimmingCharacters(in: .whitespacesAndNewlines),
            deleteMerged: deleteMerged
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

struct SettingsScopeBar<Projects: View, Environment: View>: View {
    @ViewBuilder let projects: () -> Projects
    @ViewBuilder let environment: () -> Environment

    var body: some View {
        HStack(spacing: 8) {
            Text("設定の適用先").foregroundStyle(.secondary)
            environment()
            Text("／").foregroundStyle(.secondary)
            projects()
        }
        .font(.caption)
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 20).padding(.vertical, 12)
        .background(Color(UIColor.secondarySystemGroupedBackground))
    }
}

struct EnvironmentScopeMenu: View {
    @ObservedObject var model: BexAppViewModel

    var body: some View {
        Menu {
            ForEach(model.profiles) { profile in
                Button { model.selectProfile(profile.id) } label: {
                    if profile.id == model.selectedProfileId {
                        Label(profile.name, systemImage: "checkmark")
                    } else {
                        Text(profile.name)
                    }
                }
            }
        } label: {
            HStack(spacing: 4) {
                Text(model.selectedProfileName ?? "環境を選択").lineLimit(1)
                Image(systemName: "chevron.down")
            }
        }
        .accessibilityIdentifier("settings.scope.environment")
        .disabled(model.snapshot.accountLogin() != nil)
    }
}
