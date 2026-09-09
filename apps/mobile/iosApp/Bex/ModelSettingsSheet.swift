import SwiftUI

struct ModelSettingsSheet: View {
    @ObservedObject var model: BexAppViewModel
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    var body: some View {
        NavigationView {
            List {
                ForEach(model.settings.accounts, id: \.id) { account in
                    Section {
                        Button { model.controller.settings.selectAccount(id: account.id) } label: {
                            HStack(spacing: 12) {
                                Image(systemName: "person.crop.circle").font(.title2)
                                VStack(alignment: .leading, spacing: 3) {
                                    Text(account.email).foregroundColor(.primary)
                                    Text(account.planType.uppercased()).font(.caption).foregroundColor(.secondary)
                                }
                                Spacer()
                                if account.id == model.settings.selectedId {
                                    Image(systemName: "checkmark.circle.fill")
                                }
                            }.padding(.vertical, 4)
                        }
                        .accessibilityIdentifier("model.account." + account.id)
                        .accessibilityValue(account.id == model.settings.selectedId ? "選択中" : "")
                        .disabled(model.settings.selecting)
                        if account.id == model.settings.selectedId {
                            Button { model.controller.settings.chooseModel(value: "") } label: {
                                HStack {
                                    Text("Codex の既定モデル").foregroundColor(.primary)
                                    Spacer()
                                    if model.settings.selectedModel.isEmpty {
                                        Image(systemName: "checkmark")
                                    }
                                }.padding(.leading, 36)
                            }
                            .accessibilityIdentifier("model.choice.default")
                            ForEach(model.settings.models, id: \.id) { choice in
                                Button { model.controller.settings.chooseModel(value: choice.model) } label: {
                                    HStack {
                                        Text(choice.displayName).foregroundColor(.primary)
                                        Spacer()
                                        if model.settings.selectedModel == choice.model {
                                            Image(systemName: "checkmark")
                                        }
                                    }.padding(.leading, 36)
                                }
                                .accessibilityIdentifier("model.choice." + choice.id)
                                .accessibilityValue(model.settings.selectedModel == choice.model ? "選択中" : "")
                            }
                        }
                    }
                }
                if model.settings.loadingModels || model.settings.selecting {
                    ProgressView()
                }
                if let current = model.settings.currentModel, !current.reasoningEfforts.isEmpty {
                    Section("推論の強度") {
                        Picker("推論の強度", selection: Binding(
                            get: { model.settings.selectedEffort
                            },
                            set: model.controller.settings.chooseEffort
                        )) {
                            ForEach(current.reasoningEfforts, id: \.self) { Text($0).tag($0) }
                        }
                        .pickerStyle(.segmented)
                        .accessibilityIdentifier("model.quick.effort")
                    }
                }
                Section {
                    if let login = model.settings.login {
                        Text("ブラウザでログインし、次のコードを入力してください。")
                        Text(login.userCode).font(.title2.monospaced()).textSelection(.enabled)
                            .accessibilityIdentifier("model.login.code")
                        if let url = URL(string: login.verificationUrl),
                           url.scheme == "https" {
                            Link("ログインページを開く", destination: url)
                        }
                        Button("ログインをキャンセル") { model.controller.settings.cancelAccountLogin() }
                    } else {
                        Button { model.controller.settings.startAccountLogin() } label: { Label(
                            "Codex アカウントを追加",
                            systemImage: "plus"
                        ) }
                        .disabled(model.settings.startingLogin)
                        .accessibilityIdentifier("model.account.add")
                    }
                    if let error = model.settings.error ?? model.settings.modelError {
                        Text(error).font(.caption).foregroundColor(.red)
                        Button("再読み込み") { refresh() }
                    }
                }
            }
            .navigationTitle("アカウントとモデル")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) {
                Button("完了") { dismiss() }.disabled(model.settings.login != nil || model.settings.startingLogin)
                    .accessibilityIdentifier("model.close")
            } }
        }
        .navigationViewStyle(StackNavigationViewStyle())
        .interactiveDismissDisabled(model.settings.login != nil || model.settings.startingLogin)
        .onAppear { refresh() }
        .onChange(of: scenePhase) { phase in
            if phase == .active {
                model.controller.settings.setAccountLoginPolling(enabled: true)
            }
        }
        .onDisappear { model.controller.settings.setAccountLoginPolling(enabled: false) }
    }

    private func refresh() {
        model.controller.settings.refreshAccounts()
        model.controller.settings.loadModels()
    }
}
