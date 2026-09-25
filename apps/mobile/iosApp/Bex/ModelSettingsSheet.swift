import AgentCore
import SwiftUI

struct ModelSettingsSheet: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @ObservedObject var model: BexAppViewModel
    private enum Page { case models, accounts, manage }
    var managementOnly = false
    @State private var page: Page = .models
    @State private var search = ""
    @State private var providerOverride: ProviderKind?
    @State private var changingAccount = false
    @State private var loadingAccounts = false
    @State private var loadingModels = false
    @State private var loginCode = ""
    @State private var loginError: String?
    @State private var loginRequestInFlight = false
    @State private var cancellingLogin = false
    @State private var pollingLogin: Task<Void, Never>?
    @State private var signOutId: String?
    private var provider: ProviderKind {
        providerOverride ?? model.snapshot.modelProviderForDraft(threadId: model.coreDraftKey)
    }

    private var providerName: String {
        provider == .codex ? "Codex" : "Claude Code"
    }

    private var accounts: [Account] {
        model.accounts.filter { $0.provider == provider }
    }

    private var selectedAccount: Account? {
        accounts.first { model.snapshot.accountIsSelected(id: $0.id) }
    }

    private var login: AccountLogin? {
        model.snapshot.accountLogin()
    }

    private var loginInProgress: Bool {
        loginRequestInFlight || cancellingLogin || login != nil
    }

    private var loginProgressMessage: String? {
        if cancellingLogin {
            return "サインインを中止中…"
        }
        guard loginRequestInFlight else { return nil }
        return login == nil ? "サインインを準備中…" : "認証を確認中…"
    }

    private var busy: Bool {
        changingAccount || loginInProgress || !model.isConnected
    }

    var body: some View {
        NavigationStack {
            List {
                if let login {
                    AccountLoginSection(
                        login: login, providerName: providerName, loginCode: $loginCode,
                        progressMessage: loginProgressMessage, loginError: loginError,
                        submit: { submitLoginCode(login.loginId) }, retry: { pollLogin(login.loginId) }
                    )
                } else {
                    switch page {
                    case .models:
                        Section {
                            Menu {
                                Button("Codex") { selectProvider(.codex) }
                                    .accessibilityIdentifier("model.provider.codex")
                                Button("Claude Code") { selectProvider(.claude) }
                                    .accessibilityIdentifier("model.provider.claude")
                            } label: {
                                LabeledContent("エージェント") {
                                    HStack {
                                        Text(providerName)
                                        Image(systemName: "chevron.right").font(.caption)
                                    }
                                }.foregroundStyle(.primary)
                            }
                            .accessibilityIdentifier("model.provider")
                            .disabled(busy)
                            Button { page = .accounts } label: {
                                VStack(alignment: .leading, spacing: 10) {
                                    HStack {
                                        Text("アカウント")
                                        Spacer()
                                        AccountIdentityView(account: selectedAccount).foregroundStyle(.secondary)
                                        Image(systemName: "chevron.right").font(.caption)
                                    }
                                    WeeklyUsageView(windows: selectedAccount.map {
                                        model.snapshot.accountWeeklyUsage(id: $0.id)
                                    } ?? [])
                                }
                                .foregroundStyle(.primary)
                            }
                            .accessibilityIdentifier("model.accounts")
                        }
                        modelSection
                    case .accounts:
                        accountSection
                        Section {
                            Button("アカウントを管理", systemImage: "gearshape") { page = .manage }
                                .accessibilityIdentifier("model.accounts.manage")
                        }
                    case .manage:
                        Picker("接続先", selection: Binding(get: { provider }, set: { providerOverride = $0 })) {
                            Text("OpenAI").tag(ProviderKind.codex)
                            Text("Anthropic").tag(ProviderKind.claude)
                        }
                        .accessibilityIdentifier("account.provider")
                        .disabled(busy)
                        accountSection
                    }
                }
            }
            .contentMargins(.top, 12, for: .scrollContent)
            .navigationTitle(page == .models ? "" : page == .accounts ? "アカウント" : "アカウントを管理")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                if loginInProgress || (!managementOnly && page != .models) {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("戻る", systemImage: "chevron.left") {
                            if loginInProgress {
                                cancelLogin()
                            } else {
                                if page == .manage {
                                    providerOverride = nil
                                }
                                page = page == .manage ? .accounts : .models
                            }
                        }
                        .disabled(cancellingLogin)
                        .accessibilityIdentifier("model.back")
                    }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("完了") { dismiss() }
                        .disabled(loginInProgress)
                        .accessibilityIdentifier("model.close")
                }
            }
            .alert("サインアウトしますか？", isPresented: Binding(
                get: { signOutId != nil }, set: {
                    if !$0 {
                        signOutId = nil
                    }
                }
            )) {
                if let id = signOutId {
                    Button("サインアウト", role: .destructive) { signOut(id) }
                        .accessibilityIdentifier("account.logout.confirm")
                }
                Button("キャンセル", role: .cancel) { signOutId = nil }
                    .accessibilityIdentifier("account.logout.cancel")
            } message: {
                Text("この接続先に保存されたアカウントからサインアウトします。再び使うにはサインインが必要です。")
            }
        }
        .interactiveDismissDisabled(loginInProgress)
        .onAppear {
            if managementOnly {
                page = .manage
            }
            refresh()
        }
        .onChange(of: scenePhase) { phase in
            if phase == .active, let id = login?.loginId {
                pollLogin(id)
            }
        }
        .onDisappear { pollingLogin?.cancel(); pollingLogin = nil }
    }
}

extension ModelSettingsSheet {
    private var accountSection: some View {
        Section {
            ForEach(accounts, id: \.id) { choice in
                let selected = choice.id == selectedAccount?.id
                VStack(alignment: .leading, spacing: 12) {
                    Button { chooseAccount(choice.id) } label: {
                        HStack(spacing: 12) {
                            VStack(alignment: .leading, spacing: 4) {
                                AccountIdentityView(account: choice)
                                if let plan = choice.planType, !plan.isEmpty {
                                    Text(plan.uppercased()).font(.caption).foregroundStyle(.secondary)
                                }
                            }
                            Spacer(minLength: 0)
                            Image(systemName: selected ? "checkmark.circle.fill" : "circle")
                                .foregroundStyle(Color.accentColor)
                        }
                        .foregroundStyle(.primary)
                    }
                    .accessibilityIdentifier("model.account." + choice.id)
                    .accessibilityValue(selected ? "選択中" : "")
                    .disabled(busy)
                    if page == .manage {
                        AccountUsageView(usage: choice.usage)
                        Button("サインアウト", role: .destructive) { signOutId = choice.id }
                            .frame(minHeight: 44)
                            .font(.subheadline)
                            .accessibilityIdentifier("account.logout." + choice.id)
                            .disabled(busy)
                    } else {
                        WeeklyUsageView(windows: model.snapshot.accountWeeklyUsage(id: choice.id))
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
            if page == .manage {
                HStack {
                    Button { startLogin() } label: {
                        Label(accounts.isEmpty ? "サインイン" : "アカウントを追加", systemImage: "plus.circle.fill")
                            .frame(minHeight: 44)
                    }
                    .accessibilityIdentifier("model.account.add")
                    Spacer(minLength: 8)
                    Button(action: refresh) {
                        Image(systemName: "arrow.clockwise").frame(minWidth: 44, minHeight: 44)
                    }
                    .accessibilityLabel("モデルと使用量を更新")
                    .accessibilityIdentifier("account.refresh")
                    .disabled(loadingAccounts || loadingModels)
                }
                .font(.subheadline)
                .disabled(busy)
            }
        } footer: {
            Text("アカウントの切替は、同じ接続先を使う端末にも反映されます。")
        }
        .buttonStyle(.borderless)
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
                .disabled(busy)
            }
            ForEach(model.snapshot.modelErrorMessages(), id: \.self) { error in
                Text(accountErrorMessage(message: error)).font(.caption).foregroundStyle(.red)
            }
        }
    }
}

extension ModelSettingsSheet {
    private func selectProvider(_ provider: ProviderKind) {
        providerOverride = provider
        loginError = nil
        if let choice = model.snapshot.modelForProvider(threadId: model.coreDraftKey, provider: provider) {
            model.chooseModel(choice)
        }
    }

    private func chooseAccount(_ id: String) {
        changingAccount = true
        model.perform(.selectAccountForDraft(SelectAccountForDraft(id: id, threadId: model.coreDraftKey))) { _ in
            changingAccount = false
        }
    }

    private func refresh() {
        if let id = login?.loginId {
            pollLogin(id); return
        }
        loadingAccounts = true
        model.perform(.listAccounts(ListAccounts())) { _ in loadingAccounts = false }
        loadingModels = true
        model.perform(.loadModels(LoadModels())) { _ in loadingModels = false }
    }

    private func signOut(_ id: String) {
        signOutId = nil
        changingAccount = true
        model.perform(.logoutAccount(LogoutAccount(id: id))) { _ in changingAccount = false }
    }

    private func startLogin() {
        loginCode = ""
        loginRequestInFlight = true
        loginError = nil
        model.perform(.startAccountLogin(StartAccountLogin(provider: provider)), completion: finishLoginRequest)
    }

    private func submitLoginCode(_ id: String) {
        loginRequestInFlight = true
        loginError = nil
        let code = loginCode
        loginCode = ""
        model.perform(.submitAccountLogin(SubmitAccountLogin(id: id, code: code)), completion: finishLoginRequest)
    }

    private func finishLoginRequest(_ result: Result<Outcome, Error>) {
        loginRequestInFlight = false
        if cancellingLogin {
            cancelLogin(); return
        }
        if case let .failure(error) = result {
            loginError = model.snapshot.error() ?? error.localizedDescription
        } else if let id = login?.loginId {
            pollLogin(id)
        }
    }

    private func pollLogin(_ id: String) {
        guard pollingLogin == nil, !cancellingLogin else { return }
        loginError = nil
        let draftKey = model.coreDraftKey
        pollingLogin = Task { @MainActor in
            defer { pollingLogin = nil }
            while !Task.isCancelled, login?.loginId == id {
                do { try await Task.sleep(nanoseconds: 2_000_000_000) } catch { return }
                let result: Result<Outcome, Error> = await withCheckedContinuation { continuation in
                    model.perform(.readAccountLogin(ReadAccountLogin(id: id, threadId: draftKey))) {
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
        guard let id = login?.loginId else {
            cancellingLogin = false; return
        }
        model.perform(.cancelAccountLogin(CancelAccountLogin(id: id))) { result in
            cancellingLogin = false
            if case let .failure(error) = result {
                loginError = model.snapshot.error() ?? error.localizedDescription
            }
        }
    }
}
