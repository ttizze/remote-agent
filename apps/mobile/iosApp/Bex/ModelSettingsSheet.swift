import AgentCore
import SwiftUI

struct ModelSettingsSheet: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject var model: BexAppViewModel

    var body: some View {
        NavigationStack {
            ModelSettingsScreen(model: model, close: { dismiss() })
        }
    }
}

struct ModelSettingsScreen: View {
    @ObservedObject var model: BexAppViewModel
    @State private var scope: ModelDefaultsScope?
    private let conversation: Bool
    let close: () -> Void
    @State private var search = ""
    @State private var providerOverride: ProviderKind?
    @State private var loadingModels = false

    init(model: BexAppViewModel, scope: ModelDefaultsScope? = nil, close: @escaping () -> Void) {
        self.model = model
        _scope = State(initialValue: scope)
        conversation = scope == nil
        self.close = close
    }

    private var defaults: Bool {
        scope != nil
    }

    private var preferences: ModelDefaults {
        model.snapshot.modelDefaults(scope: scope ?? .global)
    }

    private var provider: ProviderKind {
        if defaults {
            return model.snapshot.defaultModel(scope: scope ?? .global)?.model.provider
                ?? preferences.model?.provider ?? .codex
        }
        if model.isNewThread, let providerOverride {
            return providerOverride
        }
        return model.snapshot.modelProviderForDraft(threadId: model.coreDraftKey)
    }

    private var selectedModel: ModelRef? {
        defaults ? preferences.model : model.selectedModel
    }

    private var disabled: Bool {
        defaults ? model.store == nil : !model.isConnected || model.sending
    }

    private var selectedAccount: Account? {
        model.accounts.first { $0.provider == provider && model.snapshot.accountIsSelected(id: $0.id) }
    }

    var body: some View {
        List {
            if !defaults, model.isNewThread {
                Section {
                    Picker("エージェント", selection: Binding(get: { provider }, set: selectProvider)) {
                        Text("Codex").tag(ProviderKind.codex)
                        Text("Claude Code").tag(ProviderKind.claude)
                    }
                    .pickerStyle(.segmented)
                    .accessibilityIdentifier("model.provider")
                    .disabled(!model.isConnected || model.sending)
                }
            }
            Section {
                HStack {
                    VStack(alignment: .leading, spacing: 6) {
                        Text("使用中のアカウント").font(.caption).foregroundStyle(.secondary)
                        AccountIdentityView(account: selectedAccount)
                    }
                    Spacer(minLength: 8)
                    NavigationLink {
                        AgentSettingsScreen(model: model, provider: provider, close: close)
                    } label: { Text("管理") }
                        .fixedSize()
                        .accessibilityIdentifier("model.accounts.manage")
                }
                if let account = selectedAccount {
                    AccountUsageView(usage: account.usage)
                        .accessibilityIdentifier("model.account.usage")
                }
            }
            let controls = defaults ? model.snapshot.defaultModelControls(scope: scope ?? .global)
                : model.snapshot.modelQuickControls(threadId: model.coreDraftKey)
            if !controls.efforts.isEmpty || controls.toggleFastTo != nil {
                Section {
                    if !controls.efforts.isEmpty {
                        Picker("思考の深さ", selection: Binding<String?>(get: {
                            defaults ? preferences.effort : controls.effort
                        }, set: { value in
                            if let scope {
                                model.perform(.selectDefaultEffort(scope: scope, effort: value))
                            } else if let value {
                                model.chooseEffort(value)
                            }
                        })) {
                            if defaults {
                                Text("自動").tag(String?.none)
                            }
                            ForEach(controls.efforts, id: \.self) { Text($0).tag(Optional($0)) }
                        }
                        .accessibilityIdentifier("model.sheet.effort")
                        .accessibilityValue(defaults ? preferences.effort ?? "自動" : controls.effort)
                    }
                    if defaults, let tier = controls.fastServiceTier {
                        Picker("速度", selection: Binding<String?>(get: {
                            preferences.serviceTier
                        }, set: { value in
                            if let scope {
                                model.perform(.selectDefaultServiceTier(scope: scope, serviceTier: value))
                            }
                        })) {
                            Text("自動").tag(String?.none)
                            Text("通常").tag(Optional("default"))
                            Text("高速").tag(Optional(tier))
                        }
                        .accessibilityIdentifier("model.defaults.speed")
                        .accessibilityValue(preferences.serviceTier == nil ? "自動"
                            : controls.fast ? "高速" : "通常")
                    } else if let next = controls.toggleFastTo {
                        Toggle(
                            "Fast",
                            isOn: Binding(get: { controls.fast }, set: { _ in model.chooseServiceTier(next) })
                        )
                        .accessibilityIdentifier("model.sheet.fast")
                    }
                }.disabled(disabled)
            }
            modelSection
            if let scope, model.snapshot.hasModelDefaultsOverride(scope: scope) {
                Section {
                    Button("共通設定を使う") { model.perform(.inheritModelDefaults(scope: scope)) }
                        .accessibilityIdentifier("model.defaults.inherit")
                        .disabled(disabled)
                }
            }
        }
        .contentMargins(.top, 12, for: .scrollContent)
        .safeAreaInset(edge: .top, spacing: 0) {
            SettingsScopeBar {
                scopeMenu(model.snapshot.modelProjectScopeChoices(scope: scope, conversation: conversation),
                          fallback: "すべてのプロジェクト", id: "settings.scope.projects")
            } environment: {
                scopeMenu(model.snapshot.modelEnvironmentScopeChoices(scope: scope),
                          fallback: "この環境", id: "settings.scope.environment")
            }
        }
        .navigationTitle(defaults || model.isNewThread ? "モデル" : provider == .codex ? "Codex" : "Claude Code")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                Button("完了", action: close).accessibilityIdentifier("model.close")
            }
        }
        .onAppear {
            model.perform(.listAccounts(ListAccounts()))
            loadingModels = true
            model.perform(.loadModels(LoadModels())) { _ in loadingModels = false }
        }
        .onChange(of: model.selectedProfileId) { _ in
            scope = conversation ? nil : .global
            search = ""
        }
    }

    private var modelSection: some View {
        Section {
            TextField("モデルを検索", text: $search)
                .textInputAutocapitalization(.never).autocorrectionDisabled()
                .accessibilityIdentifier("model.search")
            if defaults {
                Button {
                    if let scope {
                        model.perform(.selectDefaultModel(scope: scope, model: nil))
                    }
                } label: {
                    HStack {
                        Text("自動").foregroundStyle(.primary)
                        Spacer()
                        if selectedModel == nil {
                            Image(systemName: "checkmark")
                        }
                    }
                }
                .accessibilityIdentifier("model.choice.automatic")
                .accessibilityValue(selectedModel == nil ? "選択中" : "")
                .disabled(disabled)
            }
            let choices = model.snapshot.modelsMatching(provider: defaults ? nil : provider, query: search)
            if choices.isEmpty {
                Text(loadingModels ? "モデルを読み込み中…" : "利用可能なモデルがありません")
                    .foregroundStyle(.secondary)
            }
            ForEach(choices, id: \.model) { choice in
                Button {
                    if let scope {
                        model.perform(.selectDefaultModel(scope: scope, model: choice.model))
                    } else {
                        model.chooseModel(choice.model)
                    }
                } label: {
                    HStack {
                        Text(choice.displayName).foregroundStyle(.primary)
                        Spacer()
                        if selectedModel == choice.model {
                            Image(systemName: "checkmark")
                        }
                    }
                }
                .accessibilityIdentifier("model.choice." + choice.id)
                .accessibilityValue(selectedModel == choice.model ? "選択中" : "")
                .disabled(disabled)
            }
            ForEach(
                model.snapshot.modelErrorMessages(provider: defaults ? selectedModel?.provider : provider),
                id: \.self
            ) { error in
                Text(accountErrorMessage(message: error)).font(.caption).foregroundStyle(.red)
                    .accessibilityIdentifier("model.error")
            }
        } header: {
            if defaults {
                Text("新しい会話")
            }
        } footer: {
            if defaults {
                Text("選んだ適用先の初期値をこのiPhoneに保存します。既存の会話には影響しません。")
            }
        }
    }

    private func scopeMenu(_ choices: [ModelScopeChoice], fallback: String, id: String) -> some View {
        Menu {
            ForEach(choices, id: \.id) { choice in
                Button {
                    scope = choice.scope
                    search = ""
                } label: {
                    if choice.scope == scope {
                        Label(choice.label, systemImage: "checkmark")
                    } else {
                        Text(choice.label)
                    }
                }
                .accessibilityIdentifier(id + "." + choice.id)
            }
        } label: {
            HStack(spacing: 4) {
                Text(choices.first { $0.scope == scope }?.label ?? fallback).lineLimit(1)
                Image(systemName: "chevron.down")
            }
        }
        .accessibilityIdentifier(id)
        .disabled(model.store == nil || model.sending)
    }

    private func selectProvider(_ provider: ProviderKind) {
        providerOverride = provider
        search = ""
        if let choice = model.snapshot.modelForProvider(threadId: model.coreDraftKey, provider: provider) {
            model.chooseModel(choice)
        }
    }
}
