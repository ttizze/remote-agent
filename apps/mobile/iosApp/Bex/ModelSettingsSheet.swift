import AgentCore
import SwiftUI

private let modelSettingsBackground = Color(red: 0.09, green: 0.094, blue: 0.098)

struct ModelSettingsSheet: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject var model: BexAppViewModel

    var body: some View {
        NavigationStack {
            ModelSettingsScreen(model: model, close: { dismiss() })
        }
        .presentationDetents([.height(640), .large])
        .presentationDragIndicator(.visible)
        .presentationBackground(modelSettingsBackground)
    }
}

struct ModelSettingsScreen: View {
    @ObservedObject var model: BexAppViewModel
    @State private var scope: ModelDefaultsScope?
    let close: () -> Void
    @State private var search = ""
    @State private var providerOverride: ProviderKind?
    @State private var loadingModels = false

    init(model: BexAppViewModel, scope: ModelDefaultsScope? = nil, close: @escaping () -> Void) {
        self.model = model
        _scope = State(initialValue: scope)
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
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                ModelSettingRow(title: "エージェント") {
                    Picker("エージェント", selection: Binding(get: { provider }, set: selectProvider)) {
                        Text("Codex").tag(ProviderKind.codex)
                        Text("Claude Code").tag(ProviderKind.claude)
                    }
                    .labelsHidden().pickerStyle(.menu)
                    .accessibilityIdentifier("model.provider")
                    .disabled(disabled || (!defaults && !model.isNewThread))
                }
                Divider()
                VStack(spacing: 4) {
                    NavigationLink {
                        AgentSettingsScreen(model: model, provider: provider, close: close)
                            .toolbar(.visible, for: .navigationBar)
                    } label: {
                        ModelSettingRow(title: "アカウント") {
                            AccountIdentityView(account: selectedAccount)
                                .font(.system(size: 12))
                            Image(systemName: "chevron.right").font(.caption)
                                .foregroundStyle(.secondary)
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
                .padding(.bottom, 12)
                Divider()
                modelSection
                let controls = defaults ? model.snapshot.defaultModelControls(scope: scope ?? .global)
                    : model.snapshot.modelQuickControls(threadId: model.coreDraftKey)
                if !defaults || model.snapshot.defaultModel(scope: scope ?? .global)?.model.provider == provider,
                   !controls.efforts.isEmpty || controls.toggleFastTo != nil {
                    Divider()
                    if !controls.efforts.isEmpty {
                        ModelSettingRow(title: "思考の深さ") {
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
                            .labelsHidden().pickerStyle(.menu).disabled(disabled)
                        }
                    }
                    if let tier = controls.fastServiceTier {
                        ModelSettingRow(title: "速度") {
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
                            .labelsHidden().pickerStyle(.menu).disabled(disabled)
                        }
                    }
                }
                if let scope, model.snapshot.hasModelDefaultsOverride(scope: scope) {
                    Divider()
                    Button("共通設定を使う") { model.perform(.inheritModelDefaults(scope: scope)) }
                        .frame(minHeight: 44)
                        .accessibilityIdentifier("model.defaults.inherit")
                        .disabled(disabled)
                }
            }
            .padding(.horizontal, 20)
            .padding(.bottom, 16)
        }
        .font(.system(size: 15)).tint(.primary)
        .buttonStyle(.plain)
        .background(modelSettingsBackground)
        .safeAreaInset(edge: .top, spacing: 0) {
            VStack(spacing: 0) {
                HStack {
                    Text("モデル").fontWeight(.medium)
                    Spacer()
                    Button(action: close) { Image(systemName: "xmark") }
                        .frame(width: 44, height: 44)
                        .accessibilityLabel("閉じる").accessibilityIdentifier("model.close")
                }
                .font(.system(size: 15)).padding(.leading, 20).padding(.trailing, 8)
                if let scope {
                    SettingsScopeBar {
                        ModelDefaultsScopeMenu(choices: model.snapshot.modelProjectScopeChoices(scope: scope),
                                               selected: scope, fallback: "すべてのプロジェクト",
                                               id: "settings.scope.projects", select: selectScope)
                    } environment: {
                        ModelDefaultsScopeMenu(choices: model.snapshot.modelEnvironmentScopeChoices(scope: scope),
                                               selected: scope, fallback: "この環境",
                                               id: "settings.scope.environment", select: selectScope)
                    }
                    .disabled(model.store == nil || model.sending)
                }
            }
            .background(modelSettingsBackground)
        }
        .toolbar(.hidden, for: .navigationBar)
        .onAppear {
            model.perform(.listAccounts(ListAccounts()))
            loadingModels = true
            model.perform(.loadModels(LoadModels())) { _ in loadingModels = false }
        }
        .onChange(of: model.selectedProfileId) { _ in
            if defaults {
                scope = .global
            }
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
        .overlay(RoundedRectangle(cornerRadius: 6).stroke(.secondary.opacity(0.3)))
        .padding(.top, 12).padding(.bottom, 8)
        let choices = model.snapshot.modelsMatching(provider: provider, query: search)
        ScrollView {
            LazyVStack(spacing: 0) {
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
            .foregroundStyle(.primary)
        }
        .scrollClipDisabled()
        .frame(height: min(CGFloat(max(choices.count + (defaults ? 1 : 0), 1)) * 44, 264))
        .padding(.bottom, 8)
        ForEach(
            model.snapshot.modelErrorMessages(provider: provider),
            id: \.self
        ) { error in
            Text(accountErrorMessage(message: error)).font(.caption).foregroundStyle(.red)
                .accessibilityIdentifier("model.error")
        }
    }

    private func selectScope(_ scope: ModelDefaultsScope) {
        self.scope = scope
        providerOverride = nil
        search = ""
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
        .frame(minHeight: 44)
        .background {
            RoundedRectangle(cornerRadius: 6)
                .fill(selected ? Color(red: 0.15, green: 0.17, blue: 0.2) : .clear)
                .padding(.horizontal, -8)
        }
    }
}

private struct ModelSettingRow<Content: View>: View {
    let title: String
    @ViewBuilder let content: () -> Content

    var body: some View {
        HStack(spacing: 8) {
            Text(title).fixedSize()
            Spacer(minLength: 8)
            content()
        }.frame(minHeight: 44)
    }
}

private struct ModelDefaultsScopeMenu: View {
    let choices: [ModelScopeChoice]
    let selected: ModelDefaultsScope
    let fallback: String
    let id: String
    let select: (ModelDefaultsScope) -> Void

    var body: some View {
        Menu {
            ForEach(choices, id: \.id) { choice in
                Button { select(choice.scope) } label: {
                    if choice.scope == selected {
                        Label(choice.label, systemImage: "checkmark")
                    } else {
                        Text(choice.label)
                    }
                }
                .accessibilityIdentifier(id + "." + choice.id)
            }
        } label: {
            HStack(spacing: 4) {
                Text(choices.first { $0.scope == selected }?.label ?? fallback).lineLimit(1)
                Image(systemName: "chevron.down")
            }
        }
        .accessibilityIdentifier(id)
    }
}
