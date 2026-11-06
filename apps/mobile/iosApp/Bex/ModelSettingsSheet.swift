import AgentCore
import SwiftUI

struct ModelSettingsSheet: View {
    @Environment(\.dismiss) private var dismiss
    let model: ConversationModel

    var body: some View {
        NavigationStack {
            ModelSettingsScreen(model: model, close: { dismiss() })
        }
        .presentationDetents([.large])
        .presentationDragIndicator(.visible)
    }
}

struct ModelSettingsScreen: View {
    let model: ConversationModel
    let close: () -> Void
    @State private var search = ""
    @State private var providerOverride: ProviderKind?

    private var provider: ProviderKind? {
        providerOverride ?? model.settings.provider()
    }

    private var disabled: Bool {
        !model.isConnected || model.sending
    }

    private var selectedAccount: Account? {
        model.accounts.first {
            $0.provider == provider && model.settings.accountIsSelected(provider: $0.provider, id: $0.id)
        }
    }

    var body: some View {
        Form {
            ErrorNotices(notices: model.settings.notices(), perform: { model.perform($0) })
            Section("エージェント") {
                Picker("エージェント", selection: Binding<ProviderKind?>(
                    get: { provider }, set: {
                        if let value = $0 {
                            selectProvider(value)
                        }
                    }
                )) {
                    ForEach([ProviderKind.codex, .claude, .deepSeek], id: \.self) { value in
                        Text(value == .deepSeek ? "DeepSeek" : value.bexName).tag(Optional(value))
                    }
                }
                .pickerStyle(.segmented)
                .accessibilityIdentifier("model.provider")
                .disabled(disabled)
            }
            Section("モデル") { modelSection }
            quickControls
            Section {
                NavigationLink {
                    if let provider {
                        ProviderAccountScreen(model: model, provider: provider, close: close)
                    }
                } label: {
                    if provider == .deepSeek {
                        Label("APIキー", systemImage: "key")
                    } else {
                        AccountIdentityView(account: selectedAccount)
                    }
                }
                .accessibilityIdentifier("model.accounts.manage")
                .disabled(provider == nil)
                if let account = selectedAccount {
                    WeeklyUsageView(windows: model.settings.accountWeeklyUsage(
                        provider: account.provider, id: account.id
                    ))
                    .accessibilityIdentifier("model.account.usage")
                }
            } header: {
                Text(LocalizedStringKey(provider == .deepSeek ? "接続" : "アカウント"))
            } footer: {
                Text("選んだモデルと応答設定は、次の新規チャットにも引き継ぎます。")
            }
        }
        .accessibilityIdentifier("model.picker")
        .searchable(text: $search, placement: .navigationBarDrawer(displayMode: .always), prompt: "モデルを検索")
        .navigationTitle("モデル設定")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                Button("完了", action: close).accessibilityIdentifier("model.close")
            }
        }
        .onAppear {
            model.perform(.readProviderSettings(ReadProviderSettings()))
        }
        .onChange(of: model.host) { _ in
            providerOverride = nil
            search = ""
        }
    }

    @ViewBuilder
    private var quickControls: some View {
        let controls = model.settings.quickControls()
        if model.selectedModel?.provider == provider,
           !controls.efforts.isEmpty || controls.speedChoices.count > 1 {
            Section("応答") {
                if !controls.efforts.isEmpty {
                    Picker("思考の深さ", selection: Binding(get: { controls.effort }, set: model.chooseEffort)) {
                        ForEach(controls.efforts, id: \.self) { Text($0).tag($0) }
                    }
                    .pickerStyle(.menu)
                    .accessibilityIdentifier("model.sheet.effort")
                    .accessibilityValue(controls.effort)
                    .disabled(disabled)
                }
                if controls.speedChoices.count > 1 {
                    Picker("速度", selection: Binding(get: {
                        controls.speedChoices.first { $0.selected }?.id ?? "default"
                    }, set: model.chooseServiceTier)) {
                        ForEach(controls.speedChoices, id: \.id) { choice in
                            AppText(choice.label).tag(choice.id)
                        }
                    }
                    .pickerStyle(.menu)
                    .accessibilityIdentifier("model.sheet.fast")
                    .accessibilityValue(controls.fast ? "高速" : "通常")
                    .disabled(disabled)
                }
            }
        }
    }

    @ViewBuilder
    private var modelSection: some View {
        let choices = model.settings.modelsMatching(provider: provider, query: search)
        if choices.isEmpty {
            AppText(model.settings.modelsLoading(provider: provider) ? "モデルを読み込み中…" : "利用可能なモデルがありません")
                .foregroundStyle(.secondary)
        }
        Picker("モデル", selection: Binding(get: { model.selectedModel }, set: {
            if let choice = $0 {
                model.chooseModel(choice)
            }
        })) {
            ForEach(choices, id: \.model) { choice in
                Text(choice.displayName).tag(Optional(choice.model))
                    .accessibilityIdentifier("model.choice." + choice.id)
            }
        }
        .pickerStyle(.inline)
        .labelsHidden()
        .disabled(disabled)
        ForEach(model.settings.modelErrorMessages(provider: provider), id: \.self) { error in
            ErrorTextView(text: errorText(error: error)).font(.caption).foregroundStyle(.red)
                .accessibilityIdentifier("model.error")
        }
    }

    private func selectProvider(_ provider: ProviderKind) {
        providerOverride = provider
        search = ""
        if let choice = model.settings.modelForProvider(provider: provider) {
            model.chooseModel(choice)
        }
    }
}
