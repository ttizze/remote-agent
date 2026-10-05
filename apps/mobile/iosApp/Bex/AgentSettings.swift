import AgentCore
import SwiftUI

struct AgentSettingsScreen: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @ObservedObject var model: BexAppViewModel
    @State var provider = ""
    let close: () -> Void
    @State private var changingAccount = false
    @State private var loadingAccounts = false
    @State private var importingHistory = false
    @State private var loginCode = ""
    @State private var loginError: String?
    @State private var loginRequestInFlight = false
    @State private var cancellingLogin = false
    @State private var pollingLogin: Task<Void, Never>?
    @State private var signOutId: String?
    private var providerName: String {
        let id = login?.instanceId ?? provider
        return id.isEmpty ? "エージェント" : model.snapshot.instanceName(instanceId: id)
    }

    private var accounts: [Account] {
        model.accounts.filter { $0.instanceId == provider }
    }

    private var login: AccountLogin? {
        model.snapshot.accountLogin()
    }

    private var loginInProgress: Bool {
        loginRequestInFlight || cancellingLogin || login != nil
    }

    private var busy: Bool {
        changingAccount || loginInProgress || !model.isConnected || provider.isEmpty
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
                    submit: { submitLoginCode(login.loginId, provider: login.instanceId) },
                    retry: { pollLogin(login.loginId, provider: login.instanceId) }
                )
            } else {
                Section {
                    Picker("エージェント", selection: $provider) {
                        ForEach(model.snapshot.providerInstances(), id: \.reference.instanceId) { instance in
                            Text(instance.displayName).tag(instance.reference.instanceId)
                        }
                    }
                    .pickerStyle(.segmented)
                    .accessibilityIdentifier("account.provider")
                    .disabled(busy)
                    if let agent = model.snapshot.connectionSetup().agents.first(where: { $0.instanceId == provider }) {
                        Label(
                            agent.label,
                            systemImage: agent.availability == .ready ? "checkmark.circle.fill" : "circle"
                        )
                        .font(.caption).foregroundStyle(agent.availability == .ready ? .green : .secondary)
                    }
                }
                accountSection
                Section("会話履歴") {
                    Button("既存の会話を取り込み・再試行") {
                        importingHistory = true
                        model.perform(.importHistory(ImportHistory())) { _ in importingHistory = false }
                    }
                    .disabled(busy || importingHistory)
                    .accessibilityIdentifier("history.import")
                    if importingHistory {
                        ProgressView("会話を検索中…")
                    }
                }
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
        .onAppear(perform: refresh)
        .onChange(of: model.isConnected) { connected in
            if connected {
                refresh()
            }
        }
        .onChange(of: model.snapshot.providerInstances().map(\.reference.instanceId)) { _ in
            provider = model.snapshot.providerSelection(
                key: model.coreDraftKey,
                preferred: provider.isEmpty ? nil : provider
            ) ?? ""
        }
        .onChange(of: provider) { _ in loginError = nil }
        .onChange(of: model.selectedProfileId) { _ in
            loginError = nil
            loginCode = ""
            signOutId = nil
        }
        .onChange(of: scenePhase) { phase in
            if phase == .active, let login {
                pollLogin(login.loginId, provider: login.instanceId)
            }
        }
        .onDisappear { pollingLogin?.cancel(); pollingLogin = nil }
    }
}

extension AgentSettingsScreen {
    private var accountSection: some View {
        Section {
            ForEach(accounts, id: \.id) { account in
                let selected = model.snapshot.accountIsSelected(provider: account.instanceId, id: account.id)
                VStack(alignment: .leading, spacing: 12) {
                    HStack(spacing: 12) {
                        Button { chooseAccount(account.id) } label: {
                            HStack(spacing: 12) {
                                AccountIdentityView(
                                    account: account,
                                    driver: model.snapshot.instanceDriver(instanceId: account.instanceId)
                                )
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
                        if let error = account.usage?.error {
                            Label(accountErrorMessage(message: error), systemImage: "exclamationmark.circle")
                                .font(.caption).foregroundStyle(.orange)
                        } else {
                            WeeklyUsageView(windows: model.snapshot.accountWeeklyUsage(
                                provider: account.instanceId,
                                id: account.id
                            ))
                        }
                        DisclosureGroup("使用量の詳細") { AccountUsageView(usage: account.usage) }
                            .font(.caption)
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
                    .accessibilityIdentifier("model.account.add").disabled(busy)
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
        changingAccount = true
        model.perform(.selectAccount(SelectAccount(instanceId: provider, id: id))) { _ in changingAccount = false }
    }

    private func refresh() {
        provider = model.snapshot.providerSelection(
            key: model.coreDraftKey,
            preferred: provider.isEmpty ? nil : provider
        ) ?? ""
        if let login {
            pollLogin(login.loginId, provider: login.instanceId); return
        }
        loadingAccounts = true
        model.perform(.listAccounts(ListAccounts())) { _ in loadingAccounts = false }
        model.perform(.loadModels(LoadModels()))
    }

    private func signOut(_ id: String) {
        signOutId = nil
        changingAccount = true
        model.perform(.logoutAccount(LogoutAccount(instanceId: provider, id: id))) { _ in changingAccount = false }
    }

    private func startLogin() {
        loginCode = ""
        loginRequestInFlight = true
        loginError = nil
        model.perform(.startAccountLogin(StartAccountLogin(instanceId: provider)), completion: finishLoginRequest)
    }

    private func submitLoginCode(_ id: String, provider: String) {
        loginRequestInFlight = true
        loginError = nil
        let code = loginCode
        loginCode = ""
        model.perform(
            .submitAccountLogin(SubmitAccountLogin(instanceId: provider, id: id, code: code)),
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
            pollLogin(login.loginId, provider: login.instanceId)
        }
    }

    private func pollLogin(_ id: String, provider: String) {
        guard pollingLogin == nil, !cancellingLogin else { return }
        loginError = nil
        pollingLogin = Task { @MainActor in
            defer { pollingLogin = nil }
            while !Task.isCancelled {
                do { try await Task.sleep(nanoseconds: 2_000_000_000) } catch { return }
                guard login?.loginId == id, login?.instanceId == provider else { return }
                let result: Result<Outcome, Error> = await withCheckedContinuation { continuation in
                    model.perform(.readAccountLogin(ReadAccountLogin(
                        instanceId: provider,
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
            instanceId: login.instanceId,
            id: login.loginId
        ))) { result in
            cancellingLogin = false
            if case let .failure(error) = result {
                loginError = model.snapshot.error() ?? error.localizedDescription
            }
        }
    }
}
