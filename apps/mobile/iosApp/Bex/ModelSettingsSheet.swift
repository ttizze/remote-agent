import AgentCore
import SwiftUI

struct ModelSettingsSheet: View {
    @ObservedObject var model: BexAppViewModel
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    private var login: AccountLogin? {
        model.snapshot.accountLogin()
    }

    @State private var changingAccount = false
    @State private var loadingModels = false
    @State private var loginError: String?
    @State private var startingLogin = false
    @State private var pollingLogin: Task<Void, Never>?

    var body: some View {
        NavigationStack {
            List {
                ForEach(model.accounts, id: \.id) { account in
                    Section {
                        Button { chooseAccount(account.id) } label: {
                            HStack(spacing: 12) {
                                Image(systemName: "person.crop.circle").font(.title2)
                                VStack(alignment: .leading, spacing: 3) {
                                    Text(account.email ?? account.id).foregroundColor(.primary)
                                    Text((account.planType ?? "").uppercased()).font(.caption)
                                        .foregroundColor(.secondary)
                                }
                                Spacer()
                                if account.id == model.selectedAccountId {
                                    Image(systemName: "checkmark.circle.fill")
                                }
                            }.padding(.vertical, 4)
                        }
                        .accessibilityIdentifier("model.account." + account.id)
                        .accessibilityValue(account.id == model.selectedAccountId ? "選択中" : "")
                        .disabled(changingAccount)
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
                if loadingModels || changingAccount {
                    ProgressView()
                }
                if let current = model.currentModel, !current.supportedReasoningEfforts.isEmpty {
                    Section("推論の強度") {
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
                    Section("サービス階層") {
                        Picker(
                            "サービス階層",
                            selection: Binding(get: { model.selectedServiceTier }, set: model.chooseServiceTier)
                        ) {
                            Text("既定").tag("")
                            ForEach(tiers, id: \.id) { Text($0.id).tag($0.id) }
                        }.accessibilityIdentifier("model.service-tier")
                    }
                }
                Section {
                    if let login {
                        Text("ブラウザでログインし、次のコードを入力してください。")
                        Text(login.userCode).font(.title2.monospaced()).textSelection(.enabled)
                            .accessibilityIdentifier("model.login.code")
                        if let url = URL(string: login.verificationUrl),
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
        .interactiveDismissDisabled(login != nil || startingLogin)
        .onAppear { refresh() }
        .onChange(of: scenePhase) { phase in
            if phase == .active, let id = login?.loginId {
                pollLogin(id)
            }
        }
        .onDisappear { pollingLogin?.cancel(); pollingLogin = nil }
    }

    private func startLogin() {
        startingLogin = true
        loginError = nil
        model.perform(.startAccountLogin(StartAccountLogin())) { result in
            startingLogin = false
            if case let .failure(error) = result {
                loginError = error.localizedDescription; return
            }
            if let id = login?.loginId {
                pollLogin(id)
            }
        }
    }

    private func chooseAccount(_ id: String) {
        changingAccount = true
        model.perform(.selectAccount(SelectAccount(id: id))) { _ in changingAccount = false }
    }

    private func refresh() {
        if let id = login?.loginId {
            pollLogin(id)
        } else {
            model.loadAccounts()
            loadingModels = true
            model.perform(.loadModels(LoadModels())) { _ in loadingModels = false }
        }
    }

    private func pollLogin(_ id: String) {
        guard pollingLogin == nil else { return }
        loginError = nil
        pollingLogin = Task { @MainActor in
            defer { pollingLogin = nil }
            while !Task.isCancelled, login?.loginId == id {
                do { try await Task.sleep(nanoseconds: 2_000_000_000) } catch { return }
                let result: Result<Outcome, Error> = await withCheckedContinuation { continuation in
                    model.perform(.readAccountLogin(ReadAccountLogin(id: id))) { continuation.resume(returning: $0) }
                }
                guard !Task.isCancelled else { return }
                if case let .failure(error) = result {
                    loginError = error.localizedDescription; return
                }
                if let status = model.snapshot.accountLoginStatus(), status.completed {
                    if let id = status.accountId {
                        model.chooseAccount(id)
                    }
                    return
                }
            }
        }
    }

    private func cancelLogin() {
        guard let id = login?.loginId else { return }
        pollingLogin?.cancel()
        model.perform(.cancelAccountLogin(CancelAccountLogin(id: id))) { result in
            if case let .failure(error) = result {
                loginError = error.localizedDescription
            }
            model.loadAccounts()
        }
    }
}
