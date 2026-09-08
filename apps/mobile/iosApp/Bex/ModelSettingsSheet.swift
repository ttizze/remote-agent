import SwiftUI

struct ModelSettingsSheet: View {
    @ObservedObject var model: CodexModelSettings
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @State private var login: [String: Any]?
    @State private var loginError: String?
    @State private var startingLogin = false
    @State private var pollingLogin: Task<Void, Never>?

    var body: some View {
        NavigationView {
            List {
                ForEach(model.accounts) { account in
                    Section {
                        Button { model.chooseAccount(account.id) } label: {
                            HStack(spacing: 12) {
                                Image(systemName: "person.crop.circle").font(.title2)
                                VStack(alignment: .leading, spacing: 3) {
                                    Text(account.email).foregroundColor(.primary)
                                    Text(account.plan.uppercased()).font(.caption).foregroundColor(.secondary)
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
                    if let login {
                        Text("ブラウザでログインし、次のコードを入力してください。")
                        Text(login["userCode"] as? String ?? "").font(.title2.monospaced()).textSelection(.enabled)
                            .accessibilityIdentifier("model.login.code")
                        if let value = login["verificationUrl"] as? String, let url = URL(string: value),
                           url.scheme == "https" {
                            Link("ログインページを開く", destination: url)
                        }
                        Button("ログインをキャンセル") { cancelLogin() }
                    } else {
                        Button { startLogin() } label: { Label("Codex アカウントを追加", systemImage: "plus") }
                            .disabled(startingLogin)
                            .accessibilityIdentifier("model.account.add")
                    }
                    if let error = loginError ?? model.accountError ?? model.modelError {
                        Text(error).font(.caption).foregroundColor(.red)
                        Button("再読み込み") { refresh() }
                    }
                }
            }
            .navigationTitle("アカウントとモデル")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) {
                Button("完了") { dismiss() }.disabled(login != nil || startingLogin)
                    .accessibilityIdentifier("model.close")
            } }
        }
        .navigationViewStyle(StackNavigationViewStyle())
        .interactiveDismissDisabled(login != nil || startingLogin)
        .onAppear { refresh() }
        .onChange(of: scenePhase) { phase in
            if phase == .active, let id = login?["loginId"] as? String {
                pollLogin(id)
            }
        }
        .onDisappear { pollingLogin?.cancel(); pollingLogin = nil }
    }

    private func startLogin() {
        startingLogin = true
        loginError = nil
        model.account("host/account/login/start", [:]) { result, error in
            startingLogin = false
            loginError = error
            guard let result, let id = result["loginId"] as? String else { return }
            login = result
            pollLogin(id)
        }
    }

    private func refresh() {
        if let id = login?["loginId"] as? String {
            pollLogin(id)
        } else {
            model.loadAccounts(); model.loadModels()
        }
    }

    private func pollLogin(_ id: String) {
        guard pollingLogin == nil else { return }
        loginError = nil
        pollingLogin = Task { @MainActor in
            while !Task.isCancelled {
                do { try await Task.sleep(nanoseconds: 2_000_000_000) } catch { return }
                let (status, error): ([String: Any]?, String?) = await withCheckedContinuation { continuation in
                    model.account("host/account/login/status", ["loginId": id]) { continuation.resume(returning: (
                        $0,
                        $1
                    )) }
                }
                guard !Task.isCancelled else { return }
                if let error {
                    loginError = error
                    pollingLogin = nil
                    return
                }
                if status?["completed"] as? Bool == true {
                    login = nil
                    pollingLogin = nil
                    model.loadAccounts(selecting: status?["accountId"] as? String)
                    return
                }
            }
        }
    }

    private func cancelLogin() {
        guard let id = login?["loginId"] as? String else { return }
        pollingLogin?.cancel()
        pollingLogin = nil
        login = nil
        model.account("host/account/login/cancel", ["loginId": id]) { _, error in
            loginError = error
            model.loadAccounts()
        }
    }
}
