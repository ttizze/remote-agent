import AgentCore
import SwiftUI

struct AgentSettingsScreen: View {
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.locale) private var locale
    @ObservedObject var model: AppViewModel
    @State var provider: ProviderKind?
    let close: () -> Void
    @State private var changingAccount = false
    @State private var loadingAccounts = false
    @State private var credentialProvider = KeyProvider.deepSeek
    @State private var loginCode = ""
    @State private var loginError: String?
    @State private var loginRequestInFlight = false
    @State private var cancellingLogin = false
    @State private var pollingLogin: Task<Void, Never>?
    @State private var signOutId: String?
    private var providerName: String {
        guard let provider = login?.provider ?? provider else {
            return localizedUiText(language: locale.appLanguage, source: "エージェント")
        }
        return provider.bexName
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
            Section("適用範囲") {
                SettingsScopeFields(profiles: model.profiles, selectedProfileId: model.selectedProfileId,
                                    disabled: changingAccount || loginInProgress, select: model.selectProfile)
            }
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
                        Text("DeepSeek Harness").tag(Optional(ProviderKind.deepSeek))
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
                if provider == .deepSeek {
                    credentialSection
                } else {
                    accountSection
                }
            }
        }
        .navigationTitle("エージェント")
        .navigationBarTitleDisplayMode(.inline)
        .navigationBarBackButtonHidden(loginInProgress)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                if loginInProgress {
                    Button("キャンセル", action: cancelLogin)
                        .disabled(cancellingLogin)
                        .accessibilityIdentifier("model.back")
                }
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
        .onChange(of: provider) { _ in loginError = nil; loginCode = ""; refresh() }
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
    private var credentialSection: some View {
        let controls = model.snapshot.credentialControls()
        let selected = controls.first { $0.provider == credentialProvider }
        let updating = model.snapshot.operationRunning(key: .dshCredentials)
        return Section("接続先のAPIキー") {
            Picker("接続先", selection: $credentialProvider) {
                ForEach(controls, id: \.provider) { route in Text(route.label).tag(route.provider) }
            }.onChange(of: credentialProvider) { _ in loginCode = "" }
            if let selected {
                AppText(selected.status).font(.caption).foregroundStyle(.secondary)
                if let url = URL(string: selected.keyUrl) {
                    Link("APIキーを取得", destination: url)
                }
                SecureField("APIキー", text: $loginCode).textInputAutocapitalization(.never).autocorrectionDisabled()
                Button("保存") {
                    let key = loginCode; loginCode = ""
                    model.perform(.updateDshCredential(KeyUpdate(provider: credentialProvider, key: key)))
                }
                .disabled(updating || loginCode.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !model
                    .isConnected)
                if selected.canRemove {
                    Button("この接続先のキーを削除", role: .destructive) {
                        model.perform(.updateDshCredential(KeyUpdate(provider: credentialProvider, key: nil)))
                    }.disabled(updating || !model.isConnected)
                }
            }
            Button("設定状態を取得", action: refresh).disabled(updating || !model.isConnected)
            if let error = model.snapshot
                .operationError(key: .dshCredentials) {
                AppText(error).font(.caption).foregroundStyle(.red)
            }
        }
    }

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
                AppText(accountErrorMessage(message: error)).font(.caption).foregroundStyle(.red)
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
        if provider == .deepSeek {
            model.perform(.readDshCredentials(ReadCredentials()))
        }
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
            loginError = accountErrorMessage(message: error.localizedDescription)
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
                    loginError = accountErrorMessage(message: error.localizedDescription); return
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
                loginError = accountErrorMessage(message: error.localizedDescription)
            }
        }
    }
}

extension ProviderKind {
    var bexName: String {
        switch self {
        case .codex: "Codex"
        case .claude: "Claude Code"
        case .deepSeek: "DeepSeek Harness"
        }
    }

    var bexKey: String {
        switch self {
        case .codex: "codex"
        case .claude: "claude"
        case .deepSeek: "deepseek"
        }
    }

    var bexIcon: String {
        switch self {
        case .codex: "openai"
        case .claude: "claude"
        case .deepSeek: "deepseek"
        }
    }
}
