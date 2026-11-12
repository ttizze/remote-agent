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
            LanguageSettings(language: model.language, select: model.selectLanguage)
            FollowUpSettings(preferences: model.composerPreferences, select: model.selectComposerPreferences)
                .disabled(model.profiles.isEmpty)
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
    let language: UiLanguage
    let select: (UiLanguage) -> Void

    var body: some View {
        Section("表示言語") {
            Picker("言語", selection: Binding(get: { language }, set: select)) {
                Text(verbatim: "English").tag(UiLanguage.english)
                Text(verbatim: "日本語").tag(UiLanguage.japanese)
            }
            .accessibilityIdentifier("settings.language")
        }
    }
}

/// What a message sent while the agent works does, as the Mac's general settings choose.
struct FollowUpSettings: View {
    let preferences: ComposerPreferences
    let select: (ComposerPreferences) -> Void

    var body: some View {
        Section {
            Picker("フォローアップの動作", selection: Binding(get: { preferences.followUp }, set: { value in
                var next = preferences
                next.followUp = value
                select(next)
            })) {
                Text("キュー").tag(FollowUp.queue)
                Text("追加の指示").tag(FollowUp.steer)
            }
            .accessibilityIdentifier("settings.followUp")
        } footer: {
            Text("エージェントの実行中に送るメッセージを、キューに追加するか実行中の作業に追加で指示するかを選択します。")
        }
    }
}

struct WorktreeSettingsScreen: View {
    let model: AppViewModel
    @Environment(\.dismiss) private var dismiss
    @State private var inputs = WorktreeSettingsInputs()
    @State private var project: String?
    @State private var loaded = false
    @State private var busy = false
    @State private var error: String?

    var body: some View {
        Form {
            Section("適用範囲") {
                SettingsScopeFields(
                    profiles: model.profiles.map { SettingsScopeItem(id: $0.id, name: $0.name) },
                    selectedProfileId: model.selectedProfileId,
                    disabled: busy || model.settings.accountLogin() != nil,
                    select: model.activateProfile,
                    project: $project,
                    projects: ((model.list?.projects ?? []) + (model.list?.hiddenProjects ?? []))
                        .map { SettingsScopeItem(id: $0.id, name: $0.name) }
                )
            }
            WorktreeSettingsFields(inputs: $inputs, project: project,
                                   enabled: loaded && !busy && model.isConnected)
            if busy {
                ProgressView().accessibilityLabel("設定を通信中")
            }
            if !model.isConnected {
                Text("作業環境との接続を確認してください。")
            }
            if let error {
                Section {
                    ErrorMessage(source: error).foregroundColor(.red).accessibilityIdentifier("worktree.error")
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

    private func load() {
        guard model.isConnected else { return }
        busy = true; error = nil
        model.requestSnapshot(.readWorktreeSettings(ReadWorktreeSettings())) { snapshot, result in
            busy = false
            if case let .failure(failure) = result {
                error = failure.localizedDescription; return
            }
            guard let settings = snapshot.files(paneId: "")?.worktreeSettings() else { error = "設定の応答が無効です。"; return }
            inputs = WorktreeSettingsInputs(
                createOnNewSession: settings.createOnNewSession, copyOnCreate: settings.copyOnCreate,
                copyPaths: settings.copyPaths, worktreeDirectory: settings.worktreeDirectory,
                deleteUnchanged: settings.deleteUnchanged, projectDeleteUnchanged: settings.projectDeleteUnchanged
            )
            loaded = true
        }
    }

    private func save() {
        busy = true; error = nil
        let settings = normalizeWorktreeSettings(settings: WorktreeSettings(
            createOnNewSession: inputs.createOnNewSession, copyOnCreate: inputs.copyOnCreate,
            copyPaths: inputs.copyPaths, worktreeDirectory: inputs.worktreeDirectory,
            deleteUnchanged: inputs.deleteUnchanged, projectDeleteUnchanged: inputs.projectDeleteUnchanged
        ))
        model.requestSnapshot(.updateWorktreeSettings(UpdateWorktreeSettings(settings: settings))) { snapshot, result in
            busy = false
            if case let .failure(failure) = result {
                error = failure.localizedDescription; return
            }
            guard snapshot.files(paneId: "")?.worktreeSettings() != nil else { error = "設定を保存できませんでした。"; return }
            dismiss()
        }
    }
}
