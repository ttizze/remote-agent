import AgentCore
import SwiftUI

struct ModelSettingsSheet: View {
    @Environment(\.dismiss) private var dismiss
    @ObservedObject var model: BexAppViewModel
    @State private var search = ""
    @State private var providerOverride: ProviderKind?
    @State private var loadingModels = false
    private var provider: ProviderKind {
        if model.isNewThread, let providerOverride {
            return providerOverride
        }
        return model.snapshot.modelProviderForDraft(threadId: model.coreDraftKey)
    }

    private var selectedAccount: Account? {
        model.accounts.first { $0.provider == provider && model.snapshot.accountIsSelected(
            provider: $0.provider,
            id: $0.id
        ) }
    }

    var body: some View {
        NavigationStack {
            List {
                if model.isNewThread {
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
                modelSection
                let controls = model.snapshot.modelQuickControls(threadId: model.coreDraftKey)
                if !controls.efforts.isEmpty || controls.toggleFastTo != nil {
                    Section {
                        if !controls.efforts.isEmpty {
                            Picker("思考の深さ", selection: Binding(get: { controls.effort }, set: model.chooseEffort)) {
                                ForEach(controls.efforts, id: \.self) { Text($0).tag($0) }
                            }
                            .accessibilityIdentifier("model.sheet.effort")
                        }
                        if let next = controls.toggleFastTo {
                            Toggle(
                                "Fast",
                                isOn: Binding(get: { controls.fast }, set: { _ in model.chooseServiceTier(next) })
                            )
                            .accessibilityIdentifier("model.sheet.fast")
                        }
                    }.disabled(!model.isConnected || model.sending)
                }
                Section {
                    HStack {
                        VStack(alignment: .leading, spacing: 6) {
                            Text("使用中のアカウント").font(.caption).foregroundStyle(.secondary)
                            AccountIdentityView(account: selectedAccount)
                        }
                        Spacer(minLength: 8)
                        NavigationLink {
                            AgentSettingsScreen(model: model, provider: provider, close: { dismiss() })
                        } label: { Text("管理") }
                            .fixedSize()
                            .accessibilityIdentifier("model.accounts.manage")
                    }
                }
            }
            .contentMargins(.top, 12, for: .scrollContent)
            .safeAreaInset(edge: .top, spacing: 0) {
                SettingsScopeBar(projects: "この会話", environment: model.selectedProfileName ?? "未選択")
            }
            .navigationTitle(model.isNewThread ? "モデル" : provider == .codex ? "Codex" : "Claude Code")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("完了") { dismiss() }.accessibilityIdentifier("model.close")
                }
            }
        }
        .onAppear {
            model.perform(.listAccounts(ListAccounts()))
            loadingModels = true
            model.perform(.loadModels(LoadModels())) { _ in loadingModels = false }
        }
    }

    private var modelSection: some View {
        Section {
            TextField("モデルを検索", text: $search)
                .textInputAutocapitalization(.never).autocorrectionDisabled()
                .accessibilityIdentifier("model.search")
            let choices = model.snapshot.providerModelsMatching(provider: provider, query: search)
            if choices.isEmpty {
                Text(loadingModels ? "モデルを読み込み中…" : "利用可能なモデルがありません")
                    .foregroundStyle(.secondary)
            }
            ForEach(choices, id: \.id) { choice in
                Button { model.chooseModel(choice.model) } label: {
                    HStack {
                        Text(choice.displayName).foregroundStyle(.primary)
                        Spacer()
                        if model.selectedModel == choice.model {
                            Image(systemName: "checkmark")
                        }
                    }
                }
                .accessibilityIdentifier("model.choice." + choice.id)
                .accessibilityValue(model.selectedModel == choice.model ? "選択中" : "")
                .disabled(!model.isConnected || model.sending)
            }
            ForEach(model.snapshot.modelErrorMessages(), id: \.self) { error in
                Text(accountErrorMessage(message: error)).font(.caption).foregroundStyle(.red)
            }
        }
    }

    private func selectProvider(_ provider: ProviderKind) {
        providerOverride = provider
        search = ""
        if let choice = model.snapshot.modelForProvider(threadId: model.coreDraftKey, provider: provider) {
            model.chooseModel(choice)
        }
    }
}
