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
        HStack(alignment: .top, spacing: 16) {
            ModelAgentRail(provider: provider, disabled: disabled || (!defaults && !model.isNewThread),
                           select: selectProvider)
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
                            provider: account.provider, id: account.id
                        ), compact: true)
                            .accessibilityIdentifier("model.account.usage").padding(.horizontal, 12)
                            .padding(.bottom, 12)
                    }
                    Divider()
                    modelSection
                    quickControls
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
    private var quickControls: some View {
        let controls = defaults ? model.snapshot.defaultModelControls(scope: scope ?? .global)
            : model.snapshot.modelQuickControls(threadId: model.coreDraftKey)
        let controlsProvider = defaults ? model.snapshot.defaultModel(scope: scope ?? .global)?.model.provider
            : selectedModel?.provider
        if controlsProvider == provider, !controls.efforts.isEmpty || controls.toggleFastTo != nil {
            Divider()
            HStack(spacing: 16) {
                if !controls.efforts.isEmpty {
                    let effort = defaults ? preferences.effort ?? "自動" : controls.effort
                    Menu {
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
                        .pickerStyle(.inline).labelsHidden()
                    } label: {
                        ModelControlLabel(value: effort, icon: ReasoningStrengthIcon(level: controls.effortLevel,
                                                                                     count: controls.efforts.count))
                    }
                    .accessibilityLabel("思考の深さ")
                    .accessibilityIdentifier("model.sheet.effort")
                    .accessibilityValue(effort)
                    .disabled(disabled)
                }
                if let tier = controls.fastServiceTier {
                    let value = defaults && preferences.serviceTier == nil ? "自動" : controls.fast ? "高速" : "通常"
                    Menu {
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
                        .pickerStyle(.inline).labelsHidden()
                    } label: {
                        ModelControlLabel(value: value, icon: Image(systemName: controls.fast ? "bolt.fill" : "bolt"))
                    }
                    .accessibilityLabel("速度")
                    .accessibilityIdentifier(defaults ? "model.defaults.speed" : "model.sheet.fast")
                    .accessibilityValue(value).disabled(disabled)
                }
            }
            .buttonStyle(.plain)
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
                        .foregroundStyle(.secondary).padding(.horizontal, 12)
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
        .frame(height: min(CGFloat(max(choices.count + (defaults ? 1 : 0), 1)) * 44, 264))
        .padding(.bottom, 8)
        ForEach(model.snapshot.modelErrorMessages(provider: provider), id: \.self) { error in
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

private struct ModelAgentRail: View {
    let provider: ProviderKind
    let disabled: Bool
    let select: (ProviderKind) -> Void

    var body: some View {
        VStack(spacing: 8) {
            ForEach([ProviderKind.codex, .claude], id: \.self) { value in
                Button { select(value) } label: {
                    Image(value == .codex ? "openai" : "claude")
                        .resizable().scaledToFit().frame(width: 24, height: 24)
                        .foregroundStyle(value == .claude ? Color(red: 0.85, green: 0.47, blue: 0.34) : .primary)
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
                .accessibilityLabel(value == .codex ? "Codex" : "Claude Code")
                .accessibilityIdentifier("model.provider." + (value == .codex ? "codex" : "claude"))
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

private struct ModelControlLabel<Icon: View>: View {
    let value: String
    let icon: Icon

    var body: some View {
        HStack(spacing: 6) {
            icon
            Text(value)
            Image(systemName: "chevron.down").font(.caption).foregroundStyle(.secondary)
        }
        .frame(minHeight: 44).contentShape(Rectangle())
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
