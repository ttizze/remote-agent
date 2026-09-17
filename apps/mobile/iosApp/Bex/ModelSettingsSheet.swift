import AgentCore
import SwiftUI

struct ModelSettingsSheet: View {
    @ObservedObject var model: BexAppViewModel
    @Environment(\.dismiss) private var dismiss
    @State private var changingAccount = false
    @State private var loadingModels = false
    @State private var loadingAccounts = false

    var body: some View {
        NavigationStack {
            List {
                Section("アカウント") {
                    ForEach(model.accounts, id: \.id) { account in
                        VStack(alignment: .leading, spacing: 10) {
                            Button {
                                changingAccount = true
                                model
                                    .perform(.selectAccountForDraft(SelectAccountForDraft(
                                        id: account.id,
                                        threadId: model.coreDraftKey
                                    ))) { _ in
                                        changingAccount = false
                                    }
                            } label: {
                                AccountIdentityRow(
                                    account: account,
                                    selected: model.snapshot.accountIsActiveForDraft(
                                        id: account.id,
                                        threadId: model.coreDraftKey
                                    )
                                )
                            }
                            .buttonStyle(.borderless)
                            .accessibilityIdentifier("model.account." + account.id)
                            .accessibilityValue(model.snapshot.accountIsActiveForDraft(
                                id: account.id,
                                threadId: model.coreDraftKey
                            ) ? "選択中" : "")
                            .disabled(changingAccount || !model.isConnected || model.snapshot.accountLogin() != nil)
                            AccountUsageView(usage: account.usage, showsDetails: false)
                            if model.snapshot.accountIsActiveForDraft(id: account.id, threadId: model.coreDraftKey) {
                                accountModelControls(account.id)
                                    .disabled(changingAccount || loadingModels || !model.isConnected)
                            }
                        }.padding(.vertical, 4)
                    }
                    if loadingAccounts {
                        ProgressView("アカウントを読み込み中…")
                    }
                    if !loadingAccounts, model.accounts.isEmpty {
                        Text("アカウントを管理から追加できます。")
                    }
                }

                Section {
                    NavigationLink { AccountSettingsView(model: model) } label: {
                        Label("アカウントを管理", systemImage: "person.crop.circle")
                    }.accessibilityIdentifier("model.accounts.manage")
                    ForEach(model.snapshot.modelErrorMessages(), id: \.self) { error in
                        Text(error).font(.caption).foregroundColor(.red)
                    }
                    if loadingModels || changingAccount {
                        ProgressView()
                    }
                    if let error = model.accountError {
                        Text(error).font(.caption).foregroundColor(.red)
                    }
                } footer: {
                    Text("アカウント選択はサービスごとに接続先のHostで共有されます。")
                }
            }
            .navigationTitle("モデルとアカウント")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) {
                Button("完了") { dismiss() }
                    .accessibilityIdentifier("model.close")
            } }
        }
        .onAppear {
            loadingAccounts = true
            model.perform(.listAccounts(ListAccounts())) { _ in loadingAccounts = false }
            loadingModels = true
            model.perform(.loadModels(LoadModels())) { _ in loadingModels = false }
        }
    }

    @ViewBuilder
    private func accountModelControls(_ accountID: String) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("モデル").font(.caption).foregroundColor(.secondary)
            Menu {
                ForEach(model.snapshot.accountModels(id: accountID), id: \.id) { choice in
                    Button { model.chooseModel(choice.model) } label: {
                        if model.selectedModel == choice.model {
                            Label(choice.displayName, systemImage: "checkmark")
                        } else {
                            Text(choice.displayName)
                        }
                    }
                    .accessibilityIdentifier("model.choice." + choice.id)
                }
            } label: {
                HStack {
                    Text(model.currentModel?.displayName ?? "モデルを選択")
                    Spacer()
                    Image(systemName: "chevron.up.chevron.down")
                }
            }
            .buttonStyle(.borderless)
            .accessibilityIdentifier("model.choice.menu")
            .accessibilityValue(model.currentModel?.displayName ?? "モデルを選択")
        }
        modelTuning
    }

    @ViewBuilder
    private var modelTuning: some View {
        if let current = model.currentModel, !current.supportedReasoningEfforts.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                Text("推論の強度").font(.caption).foregroundColor(.secondary)
                Picker("推論の強度", selection: Binding(
                    get: { model.selectedEffort.isEmpty ? current.defaultReasoningEffort : model.selectedEffort
                    },
                    set: model.chooseEffort
                )) {
                    ForEach(current.supportedReasoningEfforts, id: \.reasoningEffort) {
                        Text($0.reasoningEffort).tag($0.reasoningEffort)
                    }
                }
                .pickerStyle(.segmented)
                .accessibilityIdentifier("model.quick.effort")
            }
        }
        if let tiers = model.currentModel?.serviceTiers, !tiers.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                Text("速度").font(.caption).foregroundColor(.secondary)
                Picker(
                    "サービス階層",
                    selection: Binding(get: { model.selectedServiceTier }, set: model.chooseServiceTier)
                ) {
                    Text("既定").tag("")
                    ForEach(tiers, id: \.id) { Text($0.id).tag($0.id) }
                }.accessibilityIdentifier("model.service-tier")
            }
        }
    }
}

struct AccountIdentityRow: View {
    let account: Account
    let selected: Bool

    var body: some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                Text(account.provider == .claude ? "Claude" : "Codex").font(.headline)
                Text(account.email ?? account.id).font(.subheadline).foregroundColor(.secondary)
                    .lineLimit(2)
                if let plan = account.planType, !plan.isEmpty {
                    Text(plan.uppercased()).font(.caption).foregroundColor(.secondary)
                }
            }.foregroundColor(.primary)
            Spacer(minLength: 8)
            if selected {
                Image(systemName: "checkmark.circle.fill")
            }
        }
    }
}

struct AccountUsageView: View {
    let usage: AccountUsage?
    var showsDetails = true

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if let usage {
                if let error = usage.error {
                    Text(error).font(.caption).foregroundColor(.secondary)
                }
                ForEach(Array(usage.windows.enumerated()), id: \.offset) { _, window in
                    VStack(alignment: .leading, spacing: 5) {
                        HStack {
                            Text(window.label)
                            Spacer()
                            Text("残り \(window.remainingPercent)%").monospacedDigit()
                        }.font(.caption)
                        ProgressView(value: Double(window.remainingPercent), total: 100)
                            .tint(window.remainingPercent <= 20 ? .orange : .green)
                        if showsDetails, let reset = window.resetsAt {
                            let date = Date(timeIntervalSince1970: Double(reset))
                            Text("\(date.formatted(date: .abbreviated, time: .shortened)) にリセット")
                                .font(.caption).foregroundColor(.secondary)
                        }
                    }
                }
                if showsDetails, usage.error == nil {
                    let date = Date(timeIntervalSince1970: Double(usage.fetchedAt))
                    Text("\(date.formatted(date: .omitted, time: .shortened)) 時点")
                        .font(.caption).foregroundColor(.secondary)
                }
            } else {
                Text("使用量は未取得です").font(.caption).foregroundColor(.secondary)
            }
        }
    }
}
