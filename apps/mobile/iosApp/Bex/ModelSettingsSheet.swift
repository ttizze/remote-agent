import SwiftUI

struct ModelSettingsSheet: View {
    @ObservedObject var model: CodexModelSettings
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    var body: some View {
        NavigationView {
            List {
                ForEach(model.accounts, id: \.id) { account in
                    Section {
                        Button { model.chooseAccount(account.id) } label: {
                            HStack(spacing: 12) {
                                Image(systemName: "person.crop.circle").font(.title2)
                                VStack(alignment: .leading, spacing: 3) {
                                    Text(account.email).foregroundColor(.primary)
                                    Text(account.planType.uppercased()).font(.caption).foregroundColor(.secondary)
                                }
                                Spacer()
                                if account.id == model.selectedAccountId {
                                    Image(systemName: "checkmark.circle.fill")
                                }
                            }.padding(.vertical, 4)
                        }
                        .accessibilityIdentifier("model.account." + account.id)
                        .accessibilityValue(account.id == model.selectedAccountId ? "選択中" : "")
                        .disabled(model.changingAccount)
                        if account.id == model.selectedAccountId {
                            Button { model.chooseModel("") } label: {
                                HStack {
                                    Text("Codex の既定モデル").foregroundColor(.primary)
                                    Spacer()
                                    if model.selectedModel.isEmpty {
                                        Image(systemName: "checkmark")
                                    }
                                }.padding(.leading, 36)
                            }
                            .accessibilityIdentifier("model.choice.default")
                            ForEach(model.models, id: \.id) { choice in
                                Button { model.chooseModel(choice.model) } label: {
                                    HStack {
                                        Text(choice.displayName).foregroundColor(.primary)
                                        Spacer()
                                        if model.selectedModel == choice.model {
                                            Image(systemName: "checkmark")
                                        }
                                    }.padding(.leading, 36)
                                }
                                .accessibilityIdentifier("model.choice." + choice.id)
                                .accessibilityValue(model.selectedModel == choice.model ? "選択中" : "")
                            }
                        }
                    }
                }
                if model.loadingModels || model.changingAccount {
                    ProgressView()
                }
                if let current = model.currentModel, !current.reasoningEfforts.isEmpty {
                    Section("推論の強度") {
                        Picker("推論の強度", selection: Binding(
                            get: { model.selectedEffort.isEmpty ? current.defaultReasoningEffort : model.selectedEffort
                            },
                            set: model.chooseEffort
                        )) {
                            ForEach(current.reasoningEfforts, id: \.self) { Text($0).tag($0) }
                        }
                        .pickerStyle(.segmented)
                        .accessibilityIdentifier("model.quick.effort")
                    }
                }
                Section {
                    if let login = model.login {
                        Text("ブラウザでログインし、次のコードを入力してください。")
                        Text(login.userCode).font(.title2.monospaced()).textSelection(.enabled)
                            .accessibilityIdentifier("model.login.code")
                        if let url = URL(string: login.verificationUrl),
                           url.scheme == "https" {
                            Link("ログインページを開く", destination: url)
                        }
                        Button("ログインをキャンセル") { model.cancelLogin() }
                    } else {
                        Button { model.startLogin() } label: { Label("Codex アカウントを追加", systemImage: "plus") }
                            .disabled(model.startingLogin)
                            .accessibilityIdentifier("model.account.add")
                    }
                    if let error = model.accountError ?? model.modelError {
                        Text(error).font(.caption).foregroundColor(.red)
                        Button("再読み込み") { refresh() }
                    }
                }
            }
            .navigationTitle("アカウントとモデル")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) {
                Button("完了") { dismiss() }.disabled(model.login != nil || model.startingLogin)
                    .accessibilityIdentifier("model.close")
            } }
        }
        .navigationViewStyle(StackNavigationViewStyle())
        .interactiveDismissDisabled(model.login != nil || model.startingLogin)
        .onAppear { refresh() }
        .onChange(of: scenePhase) { phase in
            if phase == .active {
                model.resumeLogin()
            }
        }
        .onDisappear { model.pauseLogin() }
    }

    private func refresh() {
        model.loadAccounts()
        model.loadModels()
    }
}
