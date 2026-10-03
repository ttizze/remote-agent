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
        if defaults || model.isNewThread, let providerOverride {
            return providerOverride
        }
        if defaults {
            return model.snapshot.defaultModel(scope: scope ?? .global)?.model.provider
                ?? preferences.model?.provider ?? .codex
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
        model.accounts.first { $0.provider == provider && model.snapshot.accountIsSelected(
            provider: $0.provider,
            id: $0.id
        ) }
    }

    var body: some View {
        List {
            Section {
                Picker("ハーネス", selection: Binding(get: { provider }, set: selectProvider)) {
                    Text("Codex").tag(ProviderKind.codex)
                    Text("Claude Code").tag(ProviderKind.claude)
                }
                .pickerStyle(.menu)
                .accessibilityIdentifier("model.provider")
                .disabled(disabled || (!defaults && !model.isNewThread))
                VStack(spacing: 8) {
                    NavigationLink {
                        AgentSettingsScreen(model: model, provider: provider, close: close)
                    } label: {
                        HStack {
                            Text("アカウント")
                            Spacer(minLength: 8)
                            AccountIdentityView(account: selectedAccount)
                                .font(.subheadline)
                        }
                    }
                    .accessibilityIdentifier("model.accounts.manage")
                    if let account = selectedAccount {
                        WeeklyUsageView(windows: model.snapshot.accountWeeklyUsage(
                            provider: account.provider, id: account.id
                        ), compact: true)
                            .accessibilityIdentifier("model.account.usage")
                    }
                }
                .padding(.vertical, 4)
                modelSection
            }
            let controls = defaults ? model.snapshot.defaultModelControls(scope: scope ?? .global)
                : model.snapshot.modelQuickControls(threadId: model.coreDraftKey)
            if !defaults || model.snapshot.defaultModel(scope: scope ?? .global)?.model.provider == provider,
               !controls.efforts.isEmpty || controls.toggleFastTo != nil {
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
                    if let tier = controls.fastServiceTier {
                        Picker("速度", selection: Binding<String?>(get: {
                            defaults ? preferences.serviceTier : controls.fast ? tier : "default"
                        }, set: { value in
                            if let scope {
                                model.perform(.selectDefaultServiceTier(scope: scope, serviceTier: value))
                            } else if let value {
                                model.chooseServiceTier(value)
                            }
                        })) {
                            if defaults {
                                Text("自動").tag(String?.none)
                            }
                            Text("通常").tag(Optional("default"))
                            Text("高速").tag(Optional(tier))
                        }
                        .accessibilityIdentifier(defaults ? "model.defaults.speed" : "model.sheet.fast")
                        .accessibilityValue(defaults && preferences.serviceTier == nil ? "自動"
                            : controls.fast ? "高速" : "通常")
                    }
                }.disabled(disabled)
            }
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
            providerOverride = nil
            search = ""
        }
    }

    @ViewBuilder
    private var modelSection: some View {
        HStack(spacing: 8) {
            Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
            TextField("モデルを検索", text: $search)
                .textInputAutocapitalization(.never).autocorrectionDisabled()
                .accessibilityIdentifier("model.search")
        }
        .padding(10)
        .background(.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 8))
        .padding(.vertical, 4)
        let choices = model.snapshot.modelsMatching(provider: provider, query: search)
        ScrollView {
            LazyVStack(spacing: 4) {
                if defaults {
                    Button {
                        if let scope {
                            model.perform(.selectDefaultModel(scope: scope, model: nil))
                        }
                    } label: {
                        ModelChoiceRow(label: "自動", selected: selectedModel == nil)
                    }
                    .accessibilityIdentifier("model.choice.automatic")
                    .accessibilityValue(selectedModel == nil ? "選択中" : "")
                    .disabled(disabled)
                }
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
                        ModelChoiceRow(label: choice.displayName, selected: selectedModel == choice.model)
                    }
                    .accessibilityIdentifier("model.choice." + choice.id)
                    .accessibilityValue(selectedModel == choice.model ? "選択中" : "")
                    .disabled(disabled)
                }
            }
            .buttonStyle(.plain).foregroundStyle(.primary)
        }
        .frame(height: min(CGFloat(max(choices.count + (defaults ? 1 : 0), 1)) * 48, 264))
        ForEach(
            model.snapshot.modelErrorMessages(provider: provider),
            id: \.self
        ) { error in
            Text(accountErrorMessage(message: error)).font(.caption).foregroundStyle(.red)
                .accessibilityIdentifier("model.error")
        }
    }

    private func scopeMenu(_ choices: [ModelScopeChoice], fallback: String, id: String) -> some View {
        Menu {
            ForEach(choices, id: \.id) { choice in
                Button {
                    scope = choice.scope
                    providerOverride = nil
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
        if !defaults, let choice = model.snapshot.modelForProvider(threadId: model.coreDraftKey, provider: provider) {
            model.chooseModel(choice)
        }
    }
}

private struct ModelChoiceRow: View {
    let label: String
    let selected: Bool

    var body: some View {
        HStack {
            Text(label)
            Spacer()
            if selected {
                Image(systemName: "checkmark").foregroundStyle(Color.accentColor)
            }
        }
        .padding(.horizontal, 8).frame(minHeight: 44)
        .background(selected ? Color.accentColor.opacity(0.15) : .clear,
                    in: RoundedRectangle(cornerRadius: 8))
    }
}
