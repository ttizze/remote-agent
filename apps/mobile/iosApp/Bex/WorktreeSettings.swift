import AgentCore
import SwiftUI

struct SettingsScreen: View {
    let model: AppViewModel

    var body: some View {
        List {
            Section("接続") {
                NavigationLink {
                    ConnectionsScreen(model: model)
                } label: {
                    LabeledContent {
                        Text("\(model.profiles.count)")
                    } label: {
                        Label("環境", systemImage: "desktopcomputer")
                    }
                }
                .accessibilityIdentifier("settings.connections")
                NavigationLink {
                    ProviderSettingsScreen(model: model)
                } label: { Label("プロバイダー", systemImage: "cpu") }
                    .accessibilityIdentifier("settings.providers")
                    .disabled(model.profiles.isEmpty)
            }
            LanguageSettings()
            Section("プロジェクトと会話") {
                NavigationLink {
                    WorktreeSettingsScreen(model: model).id(model.selectedProfileId)
                } label: { Label("ワークツリー", systemImage: "arrow.triangle.branch") }
                    .accessibilityIdentifier("settings.worktrees")
                    .disabled(!model.isConnected)
            }
            Section { PrivacyPolicyButton() }
        }
        .tint(.blue)
        .navigationTitle("設定")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                Button("完了", action: model.showThreadList).accessibilityIdentifier("settings.close")
            }
        }
    }
}

struct LanguageSettings: View {
    @AppStorage("bex.language") private var language = "ja"

    var body: some View {
        Section("表示言語") {
            Picker("言語", selection: $language) {
                Text(verbatim: "English").tag("en")
                Text(verbatim: "日本語").tag("ja")
            }
            .accessibilityIdentifier("settings.language")
        }
    }
}

struct WorktreeSettingsScreen: View {
    let model: AppViewModel
    @Environment(\.dismiss) private var dismiss
    @State private var createOnNewSession = false
    @State private var copyOnCreate = false
    @State private var deleteUnchanged = false
    @State private var projectDeleteUnchanged: [String: Bool] = [:]
    @State private var project: String?
    @State private var copyPaths = ""
    @State private var directory = ""
    @State private var loaded = false
    @State private var busy = false
    @State private var error: String?

    var body: some View {
        Form {
            Section("適用範囲") {
                SettingsScopeFields(profiles: model.profiles, selectedProfileId: model.selectedProfileId,
                                    disabled: busy || model.settings.accountLogin() != nil,
                                    select: model.activateProfile,
                                    project: $project,
                                    projects: (model.list?.projects ?? []) + (model.list?.hiddenProjects ?? []))
            }
            Section {
                Text(LocalizedStringKey(project == nil ? "この環境に保存し、新しい会話に適用します。" : "プロジェクトごとに変えられるのは自動削除だけです。"))
                    .font(.footnote).foregroundColor(.secondary)
            }
            if let project {
                projectCleanup(project)
            } else {
                sharedSettings
            }
            if busy {
                ProgressView().accessibilityLabel("設定を通信中")
            }
            if !model.isConnected {
                Text("作業環境との接続を確認してください。")
            }
            if let error {
                Section {
                    AppText(error).foregroundColor(.red).accessibilityIdentifier("worktree.error")
                    if !loaded {
                        Button("再読み込み", action: load).disabled(busy || !model.isConnected)
                    }
                }
            }
        }
        .navigationTitle("ワークツリー")
        .navigationBarTitleDisplayMode(.inline)
        .navigationBarBackButtonHidden(busy)
        .toolbar {
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

    @ViewBuilder private var sharedSettings: some View {
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
            Toggle("取り込み済みを自動削除", isOn: $deleteUnchanged)
                .accessibilityIdentifier("worktree.deleteUnchanged")
        } header: { Text("自動削除") }
            footer: { Text("既定ブランチより先のコミットがない作業場所を1時間ごとに削除します。実行中・回答待ち・ターミナル使用中・ローカル変更ありの場合は保留します。ブランチと会話履歴は残ります。") }
            .disabled(!loaded || busy || !model.isConnected)
    }

    private func projectCleanup(_ project: String) -> some View {
        Section {
            Picker("取り込み済みを自動削除", selection: Binding<Bool?>(
                get: { projectDeleteUnchanged[project] },
                set: { projectDeleteUnchanged[project] = $0 }
            )) {
                Text(LocalizedStringKey(deleteUnchanged ? "すべてのプロジェクトと同じ（オン）" : "すべてのプロジェクトと同じ（オフ）"))
                    .tag(Bool?.none)
                Text("オン").tag(Optional(true))
                Text("オフ").tag(Optional(false))
            }
            .accessibilityIdentifier("worktree.projectDeleteUnchanged")
        } header: { Text("自動削除") }
            footer: { Text("このプロジェクトの作業場所に適用します。") }
            .disabled(!loaded || busy || !model.isConnected)
    }

    private func load() {
        guard model.isConnected else { return }
        busy = true; error = nil
        model.requestSnapshot(.readWorktreeSettings(ReadWorktreeSettings())) { snapshot, result in
            busy = false
            if case let .failure(failure) = result {
                error = failure.localizedDescription; return
            }
            guard let settings = snapshot.files().worktreeSettings() else { error = "設定の応答が無効です。"; return }
            createOnNewSession = settings.createOnNewSession
            copyOnCreate = settings.copyOnCreate
            deleteUnchanged = settings.deleteUnchanged
            projectDeleteUnchanged = settings.projectDeleteUnchanged
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
            deleteUnchanged: deleteUnchanged, projectDeleteUnchanged: projectDeleteUnchanged
        )))) { snapshot, result in
            busy = false
            if case let .failure(failure) = result {
                error = failure.localizedDescription; return
            }
            guard snapshot.files().worktreeSettings() != nil else { error = "設定を保存できませんでした。"; return }
            dismiss()
        }
    }
}

struct SettingsScopeFields: View {
    let profiles: [HostProfile]
    let selectedProfileId: String?
    let disabled: Bool
    let select: (String) -> Void
    /// Screens with project-specific settings pass the project being edited.
    var project: Binding<String?>?
    var projects: [ProjectSummary] = []

    var body: some View {
        Picker("環境", selection: Binding<String?>(get: { selectedProfileId }, set: {
            if let id = $0 {
                select(id)
            }
        })) {
            if selectedProfileId == nil {
                Text("環境を選択").tag(String?.none)
            }
            ForEach(profiles) { profile in
                Text(profile.name).tag(Optional(profile.id))
            }
        }
        .pickerStyle(.menu)
        .accessibilityIdentifier("settings.scope.environment")
        .disabled(disabled)
        if let project {
            Picker("プロジェクト", selection: project) {
                Text("すべてのプロジェクト").tag(String?.none)
                ForEach(projects, id: \.id) { project in
                    Text(project.name).tag(Optional(project.id))
                }
            }
            .pickerStyle(.menu)
            .accessibilityIdentifier("settings.scope.project")
            .disabled(disabled)
        } else {
            LabeledContent("プロジェクト", value: "すべてのプロジェクト")
        }
    }
}
