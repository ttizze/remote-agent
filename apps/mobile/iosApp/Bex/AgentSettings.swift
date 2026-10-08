import AgentCore
import SwiftUI

struct AgentSettingsScreen: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @ObservedObject var model: BexAppViewModel
    @State var provider: ProviderKind?
    let close: () -> Void
    @State private var changingAccount = false
    @State private var loadingAccounts = false
    @State private var loginCode = ""
    @State private var loginError: String?
    @State private var loginRequestInFlight = false
    @State private var cancellingLogin = false
    @State private var pollingLogin: Task<Void, Never>?
    @State private var signOutId: String?
    private var providerName: String {
        guard let provider = login?.provider ?? provider else { return "エージェント" }
        return provider == .codex ? "Codex" : "Claude Code"
    }

    private var accounts: [Account] {
        model.accounts.filter { $0.provider == provider }
    }

    private var login: AccountLogin? {
        model.snapshot.accountLogin()
    }

    private var loginInProgress: Bool {
        loginRequestInFlight || cancellingLogin || login != nil
    }

    private var busy: Bool {
        changingAccount || loginInProgress || !model.isConnected
    }

    private var loginProgressMessage: String? {
        if cancellingLogin {
            return "サインインを中止中…"
        }
        guard loginRequestInFlight else { return nil }
        return login == nil ? "サインインを準備中…" : "認証を確認中…"
    }

    var body: some View {
        List {
            if let login {
                AccountLoginSection(
                    login: login, providerName: providerName, loginCode: $loginCode,
                    progressMessage: loginProgressMessage, loginError: loginError,
                    submit: { submitLoginCode(login.loginId, provider: login.provider) },
                    retry: { pollLogin(login.loginId, provider: login.provider) }
                )
            } else {
                Section {
                    Picker("エージェント", selection: $provider) {
                        Text("Codex").tag(Optional(ProviderKind.codex))
                        Text("Claude Code").tag(Optional(ProviderKind.claude))
                    }
                    .pickerStyle(.segmented)
                    .accessibilityIdentifier("account.provider")
                    .disabled(busy)
                    if let agent = model.snapshot.connectionSetup().agents.first(where: { $0.provider == provider }) {
                        Label(
                            agent.label,
                            systemImage: agent.availability == .ready ? "checkmark.circle.fill" : "circle"
                        )
                        .font(.caption).foregroundStyle(agent.availability == .ready ? .green : .secondary)
                    }
                }
                accountSection
            }
        }
        .safeAreaInset(edge: .top, spacing: 0) {
            SettingsScopeBar { Text("すべてのプロジェクト") } environment: {
                EnvironmentScopeMenu(model: model).disabled(changingAccount || loginInProgress)
            }
        }
        .navigationTitle("エージェント")
        .navigationBarTitleDisplayMode(.inline)
        .navigationBarBackButtonHidden(true)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button("戻る", systemImage: "chevron.left") {
                    if loginInProgress {
                        cancelLogin()
                    } else {
                        dismiss()
                    }
                }
                .disabled(cancellingLogin)
                .accessibilityIdentifier("model.back")
            }
            ToolbarItem(placement: .confirmationAction) {
                Button("完了", action: close).disabled(loginInProgress)
                    .accessibilityIdentifier("model.close")
            }
        }
        .alert("サインアウトしますか？", isPresented: Binding(get: { signOutId != nil }, set: {
            if !$0 {
                signOutId = nil
            }
        })) {
            if let id = signOutId {
                Button("サインアウト", role: .destructive) { signOut(id) }
                    .accessibilityIdentifier("account.logout.confirm")
            }
            Button("キャンセル", role: .cancel) { signOutId = nil }
                .accessibilityIdentifier("account.logout.cancel")
        } message: {
            Text("この環境に保存されたアカウントからサインアウトします。再び使うにはサインインが必要です。")
        }
        .interactiveDismissDisabled(loginInProgress)
        .onAppear {
            provider = provider ?? model.snapshot.modelProviderForDraft(threadId: model.coreDraftKey)
            refresh()
        }
        .onChange(of: model.snapshot.modelProviderForDraft(threadId: model.coreDraftKey)) { value in
            if provider == nil {
                provider = value
            }
        }
        .onChange(of: model.isConnected) { connected in
            if connected {
                refresh()
            }
        }
        .onChange(of: provider) { _ in loginError = nil }
        .onChange(of: model.selectedProfileId) { _ in
            loginError = nil
            loginCode = ""
            signOutId = nil
        }
        .onChange(of: scenePhase) { phase in
            if phase == .active, let login {
                pollLogin(login.loginId, provider: login.provider)
            }
        }
        .onDisappear { pollingLogin?.cancel(); pollingLogin = nil }
    }
}

extension AgentSettingsScreen {
    private var accountSection: some View {
        Section {
            ForEach(accounts, id: \.id) { account in
                let selected = model.snapshot.accountIsSelected(provider: account.provider, id: account.id)
                VStack(alignment: .leading, spacing: 12) {
                    HStack(spacing: 12) {
                        Button { chooseAccount(account.id) } label: {
                            HStack(spacing: 12) {
                                AccountIdentityView(account: account)
                                if let plan = account.planType, !plan.isEmpty {
                                    Text(plan.uppercased()).font(.caption2).foregroundStyle(.secondary)
                                }
                                Spacer(minLength: 0)
                                Image(systemName: selected
                                    ? "checkmark.circle.fill" : "circle").foregroundStyle(Color.accentColor)
                            }.foregroundStyle(.primary)
                        }
                        .accessibilityIdentifier("model.account." + account.id)
                        .accessibilityValue(selected ? "選択中" : "")
                        .disabled(busy)
                        Menu {
                            Button("サインアウト", role: .destructive) { signOutId = account.id }
                                .accessibilityIdentifier("account.logout." + account.id)
                        } label: { Image(systemName: "ellipsis").frame(width: 32, height: 44) }
                            .accessibilityLabel("アカウントの操作")
                            .accessibilityIdentifier("account.actions." + account.id)
                            .disabled(busy)
                    }
                    if selected {
                        AccountUsageView(usage: account.usage)
                            .accessibilityIdentifier("account.usage." + account.id)
                    }
                }
            }
            if accounts.isEmpty, !loadingAccounts {
                Text("\(providerName) にサインインして利用を開始できます。")
                    .foregroundStyle(.secondary)
            }
            if loadingAccounts {
                ProgressView("アカウントを更新中…")
            }
            if let loginProgressMessage {
                ProgressView(loginProgressMessage)
            }
            if let error = loginError ?? model.accountError {
                Text(accountErrorMessage(message: error)).font(.caption).foregroundStyle(.red)
            }
        } header: {
            HStack {
                Text("アカウント")
                Spacer()
                Button("追加", systemImage: "plus", action: startLogin)
                    .accessibilityIdentifier("model.account.add").disabled(busy || provider == nil)
                Button(action: refresh) { Image(systemName: "arrow.clockwise") }
                    .accessibilityLabel("モデルと使用量を更新")
                    .accessibilityIdentifier("account.refresh")
                    .disabled(loadingAccounts || busy)
            }
        } footer: {
            Text("この環境の\(providerName)で使用します。同じ環境を使う端末にも反映されます。")
        }
        .buttonStyle(.borderless)
    }

    private func chooseAccount(_ id: String) {
        guard let provider else { return }
        changingAccount = true
        model.perform(.selectAccount(SelectAccount(provider: provider, id: id))) { _ in changingAccount = false }
    }

    private func refresh() {
        if let login {
            pollLogin(login.loginId, provider: login.provider); return
        }
        loadingAccounts = true
        model.perform(.listAccounts(ListAccounts())) { _ in loadingAccounts = false }
        model.perform(.loadModels(LoadModels()))
    }

    private func signOut(_ id: String) {
        guard let provider else { return }
        signOutId = nil
        changingAccount = true
        model.perform(.logoutAccount(LogoutAccount(provider: provider, id: id))) { _ in changingAccount = false }
    }

    private func startLogin() {
        guard let provider else { return }
        loginCode = ""
        loginRequestInFlight = true
        loginError = nil
        model.perform(.startAccountLogin(StartAccountLogin(provider: provider)), completion: finishLoginRequest)
    }

    private func submitLoginCode(_ id: String, provider: ProviderKind) {
        loginRequestInFlight = true
        loginError = nil
        let code = loginCode
        loginCode = ""
        model.perform(
            .submitAccountLogin(SubmitAccountLogin(provider: provider, id: id, code: code)),
            completion: finishLoginRequest
        )
    }

    private func finishLoginRequest(_ result: Result<Outcome, Error>) {
        loginRequestInFlight = false
        if cancellingLogin {
            cancelLogin(); return
        }
        if case let .failure(error) = result {
            loginError = model.snapshot.error() ?? error.localizedDescription
        } else if let login {
            pollLogin(login.loginId, provider: login.provider)
        }
    }

    private func pollLogin(_ id: String, provider: ProviderKind) {
        guard pollingLogin == nil, !cancellingLogin else { return }
        loginError = nil
        pollingLogin = Task { @MainActor in
            defer { pollingLogin = nil }
            while !Task.isCancelled {
                do { try await Task.sleep(nanoseconds: 2_000_000_000) } catch { return }
                guard login?.loginId == id, login?.provider == provider else { return }
                let result: Result<Outcome, Error> = await withCheckedContinuation { continuation in
                    model.perform(.readAccountLogin(ReadAccountLogin(
                        provider: provider,
                        id: id,
                        threadId: nil
                    ))) {
                        continuation.resume(returning: $0)
                    }
                }
                guard !Task.isCancelled else { return }
                if case let .failure(error) = result {
                    loginError = model.snapshot.error() ?? error.localizedDescription; return
                }
            }
        }
    }

    private func cancelLogin() {
        cancellingLogin = true
        loginCode = ""
        loginError = nil
        pollingLogin?.cancel()
        // Finish the in-flight start or code submission before cancelling on the Host.
        guard !loginRequestInFlight else { return }
        guard let login else {
            cancellingLogin = false; return
        }
        model.perform(.cancelAccountLogin(CancelAccountLogin(
            provider: login.provider,
            id: login.loginId
        ))) { result in
            cancellingLogin = false
            if case let .failure(error) = result {
                loginError = model.snapshot.error() ?? error.localizedDescription
            }
        }
    }
}
