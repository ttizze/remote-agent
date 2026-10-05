import AgentCore
import SwiftUI

private let modelSettingsBackground = Color(red: 0.08, green: 0.09, blue: 0.11)

struct ModelSettingsSheet: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject var model: BexAppViewModel

    var body: some View {
        NavigationStack {
            ModelSettingsScreen(model: model, close: { dismiss() })
        }
        .presentationDetents([.height(520), .large])
        .presentationDragIndicator(.visible)
        .presentationBackground(modelSettingsBackground)
        .presentationCornerRadius(24)
    }
}

struct ModelSettingsScreen: View {
    @ObservedObject var model: BexAppViewModel
    @State private var scope: ModelDefaultsScope?
    let close: () -> Void
    @State private var search = ""
    @State private var providerOverride: String?
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

    private var provider: String {
        if defaults {
            let preferred = providerOverride ?? model.snapshot.defaultModel(scope: scope ?? .global)?.model.instanceId
                ?? preferences.model?.instanceId
            return model.snapshot.providerSelection(key: .local(key: "model-defaults"), preferred: preferred) ?? ""
        }
        return model.snapshot.providerSelection(key: model.coreDraftKey,
                                                preferred: model.isNewThread ? providerOverride : nil) ?? ""
    }

    private var selectedModel: ModelRef? {
        defaults ? preferences.model : model.selectedModel
    }

    private var disabled: Bool {
        provider.isEmpty || (defaults ? model.store == nil : !model.isConnected || model.sending)
    }

    private var selectedAccount: Account? {
        model.accounts.first { $0.instanceId == provider && model.snapshot.accountIsSelected(
            provider: $0.instanceId,
            id: $0.id
        ) }
    }

    var body: some View {
        HStack(alignment: .top, spacing: 16) {
            ModelAgentRail(
                instances: model.snapshot.providerInstances(),
                provider: provider,
                disabled: disabled || (!defaults && !model.isNewThread),
                select: selectProvider
            )
            Divider().padding(.vertical, 8)
            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
                    let identity = selectedAccount?.email ?? selectedAccount?.id ?? "未選択"
                    NavigationLink {
                        AgentSettingsScreen(model: model, provider: provider, close: close)
                            .toolbar(.visible, for: .navigationBar)
                    } label: {
                        HStack {
                            Text(identity)
                                .lineLimit(1).font(.system(size: 13)).foregroundStyle(.secondary)
                            Spacer(minLength: 8)
                            Image(systemName: "chevron.right").font(.caption).foregroundStyle(.secondary)
                        }
                        .padding(.horizontal, 12)
                        .frame(minHeight: 44).contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("アカウント " + identity)
                    .accessibilityIdentifier("model.accounts.manage")
                    if let account = selectedAccount {
                        WeeklyUsageView(windows: model.snapshot.accountWeeklyUsage(
                            provider: account.instanceId, id: account.id
                        ), compact: true)
                            .accessibilityIdentifier("model.account.usage").padding(.horizontal, 12)
                            .padding(.bottom, 12)
                    }
                    Divider()
                    modelSection
                    optionControls
                    if let scope, model.snapshot.hasModelDefaultsOverride(scope: scope) {
                        Divider()
                        Button("共通設定を使う") { model.perform(.inheritModelDefaults(scope: scope)) }
                            .buttonStyle(.plain).frame(minHeight: 44).padding(.horizontal, 12)
                            .accessibilityIdentifier("model.defaults.inherit").disabled(disabled)
                    }
                }
            }
        }
        .padding(.horizontal, 32).padding(.vertical, 12)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("model.picker")
        .font(.system(size: 14))
        .background(modelSettingsBackground)
        .safeAreaInset(edge: .top, spacing: 0) {
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
        .tint(.primary)
        .toolbar(defaults ? .visible : .hidden, for: .navigationBar)
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
    private var optionControls: some View {
        let controls = defaults ? model.snapshot.defaultModelOptionControls(scope: scope ?? .global)
            : model.snapshot.modelOptionControls(key: model.coreDraftKey)
        let controlsProvider = defaults ? model.snapshot.defaultModel(scope: scope ?? .global)?.model.instanceId
            : selectedModel?.instanceId
        if controlsProvider == provider, !controls.isEmpty {
            Divider()
            ModelOptionControls(controls: controls, defaults: defaults, disabled: disabled) { id, value in
                if let scope {
                    model.perform(.selectDefaultModelOption(scope: scope, id: id, value: value))
                } else {
                    model.perform(.selectModelOption(threadId: model.coreDraftKey, id: id, value: value))
                }
            }
            .padding(.horizontal, 12).padding(.top, 4)
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
        .padding(.horizontal, 12).frame(minHeight: 44)
        .overlay(alignment: .bottom) { Divider() }
        .padding(.bottom, 8)
        let choices = provider.isEmpty ? [] : model.snapshot.modelsMatching(provider: provider, query: search)
        ScrollView {
            LazyVStack(spacing: 0) {
                if defaults {
                    Button { selectModel(nil) } label: {
                        ModelChoiceRow(label: "自動", selected: selectedModel == nil)
                    }
                    .accessibilityIdentifier("model.choice.automatic")
                    .accessibilityValue(selectedModel == nil ? "選択中" : "")
                    .disabled(disabled)
                }
                if choices.isEmpty {
                    Text(loadingModels ? "モデルを読み込み中…" : "利用可能なモデルがありません")
                        .foregroundStyle(.secondary).padding(.horizontal, 12)
                }
                ForEach(choices, id: \.model) { choice in
                    Button { selectModel(choice.model) } label: {
                        ModelChoiceRow(label: choice.displayName, selected: selectedModel == choice.model)
                    }
                    .accessibilityIdentifier("model.choice." + choice.id)
                    .accessibilityValue(selectedModel == choice.model ? "選択中" : "")
                    .disabled(disabled)
                }
            }
            .buttonStyle(.plain).foregroundStyle(.primary)
        }
        .frame(height: min(CGFloat(max(choices.count + (defaults ? 1 : 0), 1)) * 44, 264))
        .padding(.bottom, 8)
        ForEach(provider.isEmpty ? [] : model.snapshot.modelErrorMessages(provider: provider), id: \.self) { error in
            Text(accountErrorMessage(message: error)).font(.caption).foregroundStyle(.red)
                .accessibilityIdentifier("model.error")
        }
    }

    private func selectModel(_ choice: ModelRef?) {
        if let scope {
            model.perform(.selectDefaultModel(scope: scope, model: choice))
        } else if let choice {
            model.chooseModel(choice)
        }
    }

    private func selectScope(_ scope: ModelDefaultsScope) {
        self.scope = scope
        providerOverride = nil
        search = ""
    }

    private func selectProvider(_ provider: String) {
        providerOverride = provider
        search = ""
        if !defaults, let choice = model.snapshot.modelForInstance(threadId: model.coreDraftKey, provider: provider) {
            model.chooseModel(choice)
        }
    }
}

private struct ModelAgentRail: View {
    let instances: [ProviderInstance]
    let provider: String
    let disabled: Bool
    let select: (String) -> Void

    var body: some View {
        VStack(spacing: 8) {
            ForEach(instances, id: \.reference.instanceId) { instance in
                let value = instance.reference.instanceId
                Button { select(value) } label: {
                    ProviderIconView(driver: instance.reference.driver, size: 24)
                        .frame(width: 44, height: 44).contentShape(Rectangle())
                        .background(provider == value ? Color(white: 0.15) : .clear,
                                    in: RoundedRectangle(cornerRadius: 8))
                        .overlay(alignment: .leading) {
                            if provider == value {
                                Capsule().fill(Color.accentColor).frame(width: 2, height: 24).offset(x: -4)
                            }
                        }
                }
                .buttonStyle(.plain)
                .accessibilityLabel(instance.displayName)
                .accessibilityIdentifier("model.provider." + value)
                .accessibilityValue(provider == value ? "選択中" : "")
                .accessibilityAddTraits(provider == value ? .isSelected : [])
                .disabled(disabled)
            }
            Spacer(minLength: 0)
        }
        .frame(width: 44)
    }
}

private struct ModelChoiceRow: View {
    let label: String
    let selected: Bool

    var body: some View {
        HStack {
            Text(label).fontWeight(selected ? .medium : .regular)
            Spacer()
            if selected {
                Image(systemName: "checkmark").foregroundStyle(Color.accentColor)
            }
        }
        .padding(.horizontal, 12)
        .frame(minHeight: 44)
        .contentShape(Rectangle())
        .background(selected ? Color.accentColor.opacity(0.14) : .clear,
                    in: RoundedRectangle(cornerRadius: 10))
    }
}

private struct ModelOptionControls: View {
    let controls: [ModelOptionControl]
    let defaults: Bool
    let disabled: Bool
    let select: (String, ModelOptionValue?) -> Void

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            ForEach(Array(controls.enumerated()), id: \.offset) { _, control in
                VStack(alignment: .leading, spacing: 8) {
                    let automatic = defaults && !control.isExplicit
                    let selected = automatic ? nil : control.value
                    Menu {
                        if defaults {
                            Button { select(control.id, nil) } label: {
                                if automatic {
                                    Label("自動", systemImage: "checkmark")
                                } else {
                                    Text("自動")
                                }
                            }
                        }
                        ForEach(Array(control.choices.enumerated()), id: \.offset) { _, option in
                            choice(option.label, value: option.value, id: control.id, selected: selected)
                        }
                    } label: {
                        HStack(spacing: 6) {
                            Text(control.label)
                            Spacer(minLength: 8)
                            Text(automatic ? "自動" : control.valueLabel ?? "未設定").foregroundStyle(.secondary)
                            Image(systemName: "chevron.down").font(.caption).foregroundStyle(.secondary)
                        }
                        .frame(minHeight: 44).contentShape(Rectangle())
                    }
                    .disabled(disabled || control.disabledReason != nil)
                    .accessibilityLabel(control.label)
                    .accessibilityValue(automatic ? "自動" : control.valueLabel ?? "未設定")
                    .accessibilityIdentifier("model.option." + control.id)
                    if let description = control.disabledReason ?? control.description {
                        Text(description).font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
        }
        .buttonStyle(.plain)
    }

    private func choice(_ label: String, value: ModelOptionValue, id: String,
                        selected: ModelOptionValue?) -> some View {
        Button { select(id, value) } label: {
            if selected == value {
                Label(label, systemImage: "checkmark")
            } else {
                Text(label)
            }
        }
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
