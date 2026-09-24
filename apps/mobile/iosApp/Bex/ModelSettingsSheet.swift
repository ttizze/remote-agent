import AgentCore
import SwiftUI
import UIKit

struct ModelSettingsSheet: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @ObservedObject var model: BexAppViewModel
    @State private var providerOverride: ProviderKind?
    @State private var changingAccount = false
    @State private var loadingAccounts = false
    @State private var loadingModels = false
    @State private var loginCode = ""
    @State private var loginError: String?
    @State private var startingLogin = false
    @State private var pollingLogin: Task<Void, Never>?
    @State private var signOutId: String?

    private var provider: ProviderKind {
        providerOverride ?? model.accounts.first {
            model.snapshot.accountIsActiveForDraft(id: $0.id, threadId: model.coreDraftKey)
        }?.provider ?? model.accounts.first {
            model.snapshot.accountIsSelected(id: $0.id)
        }?.provider ?? .codex
    }

    private var providerName: String {
        provider == .codex ? "Codex" : "Claude"
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

    private var busy: Bool {
        changingAccount || startingLogin || login != nil || !model.isConnected
    }

    var body: some View {
        NavigationStack {
            List {
                Picker("サービス", selection: Binding(get: { provider }, set: selectProvider)) {
                    Text("Codex").tag(ProviderKind.codex)
                    Text("Claude").tag(ProviderKind.claude)
                }
                .pickerStyle(.segmented)
                .listRowInsets(EdgeInsets(top: 0, leading: 0, bottom: 0, trailing: 0))
                .listRowBackground(Color.clear)
                .accessibilityIdentifier("model.provider")
                .disabled(busy)

                if let login {
                    loginSection(login)
                } else {
                    modelSection
                    accountSection
                }
            }
            .contentMargins(.top, 12, for: .scrollContent)
            .navigationTitle("モデルとアカウント")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) {
                Button("完了") { dismiss() }
                    .disabled(startingLogin || login != nil)
                    .accessibilityIdentifier("model.close")
            } }
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
        .interactiveDismissDisabled(startingLogin || login != nil)
        .onAppear {
            if let id = login?.loginId {
                pollLogin(id)
            }
        }
        .onChange(of: scenePhase) { phase in
            if phase == .active, let id = login?.loginId {
                pollLogin(id)
            }
        }
        .onDisappear { pollingLogin?.cancel(); pollingLogin = nil }
    }

    private var accountSection: some View {
        Section {
            ForEach(accounts, id: \.id) { choice in
                let selected = choice.id == selectedAccount?.id
                VStack(alignment: .leading, spacing: 12) {
                    Button { chooseAccount(choice.id) } label: {
                        HStack(spacing: 12) {
                            Image(systemName: selected ? "checkmark.circle.fill" : "circle")
                                .foregroundStyle(Color.accentColor)
                            VStack(alignment: .leading, spacing: 4) {
                                Text(choice.email ?? choice.id)
                                    .font(.headline)
                                    .accessibilityIdentifier("account.identity." + choice.id)
                                if let plan = choice.planType, !plan.isEmpty {
                                    Text(plan.uppercased()).font(.caption).foregroundStyle(.secondary)
                                }
                            }
                            Spacer(minLength: 0)
                        }
                        .foregroundStyle(.primary)
                        .padding(.vertical, 4)
                    }
                    .accessibilityIdentifier("model.account." + choice.id)
                    .accessibilityValue(selected ? "選択中" : "")
                    .disabled(busy)
                    AccountUsageView(usage: choice.usage)
                    Button("サインアウト", role: .destructive) { signOutId = choice.id }
                        .frame(minHeight: 44)
                        .font(.subheadline)
                        .accessibilityIdentifier("account.logout." + choice.id)
                        .disabled(busy)
                }
            }
            if accounts.isEmpty, !loadingAccounts {
                Text("\(providerName) にサインインして利用を開始できます。")
                    .foregroundStyle(.secondary)
            }
            if loadingAccounts {
                ProgressView("アカウントを更新中…")
            }
            if startingLogin {
                ProgressView("サインインを準備中…")
            }
            if let error = loginError ?? model.accountError {
                Text(accountErrorMessage(message: error)).font(.caption).foregroundStyle(.red)
            }
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
        } header: {
            Text("アカウント")
        } footer: {
            Text("アカウントの切替は、同じ接続先を使う端末にも反映されます。")
        }
        .buttonStyle(.borderless)
    }

    private var modelSection: some View {
        Section {
            if let account = selectedAccount {
                AccountModelControls(
                    choices: model.snapshot.accountModels(id: account.id),
                    loading: loadingModels || changingAccount,
                    currentModel: model.snapshot.accountIsActiveForDraft(id: account.id, threadId: model.coreDraftKey)
                        ? model.currentModel : nil,
                    selectedModel: Binding(get: { model.selectedModel }, set: model.chooseModel),
                    selectedEffort: Binding(get: { model.selectedEffort }, set: model.chooseEffort),
                    selectedServiceTier: Binding(get: { model.selectedServiceTier }, set: model.chooseServiceTier)
                )
            } else {
                Text("サインインするとモデルを選べます。")
                    .foregroundStyle(.secondary)
            }
            ForEach(model.snapshot.modelErrorMessages(), id: \.self) { error in
                Text(accountErrorMessage(message: error)).font(.caption).foregroundStyle(.red)
            }
        } header: {
            Text("モデル設定")
        }
        .disabled(busy)
    }
}

extension ModelSettingsSheet {
    private func loginSection(_ login: AccountLogin) -> some View {
        Section("\(providerName) にサインイン") {
            Text("1. ブラウザでサインイン")
                .font(.headline)
            if !login.requiresCodeSubmission {
                Text("ログインページで次のコードを入力してください。")
                HStack {
                    Text(login.userCode).font(.title2.monospaced()).textSelection(.enabled)
                        .accessibilityIdentifier("model.login.code")
                    Spacer()
                    Button("コピー") { UIPasteboard.general.string = login.userCode }
                }
            }
            if let url = URL(string: login.verificationUrl), url.scheme == "https" {
                Link("ログインページを開く", destination: url)
            }
            if login.requiresCodeSubmission {
                Text("2. 認証コードを貼り付け")
                    .font(.headline)
                Text("ブラウザに表示されたコードを入力してください。")
                    .font(.subheadline).foregroundStyle(.secondary)
                SecureField("認証コード", text: $loginCode)
                    .textInputAutocapitalization(.never).autocorrectionDisabled()
                    .accessibilityIdentifier("model.login.input")
                Button("サインインを完了") { submitLoginCode(login.loginId) }
                    .disabled(loginCode.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || startingLogin)
                    .accessibilityIdentifier("model.login.submit")
            } else {
                ProgressView("ブラウザでの認証を待っています…")
            }
            if startingLogin {
                ProgressView("認証を確認中…")
            }
            if let error = loginError {
                Text(accountErrorMessage(message: error)).font(.caption).foregroundStyle(.red)
                Button("認証状態を再確認") { pollLogin(login.loginId) }
            }
            Button("サインインを中止", role: .cancel) { cancelLogin() }
                .disabled(startingLogin)
                .accessibilityIdentifier("model.login.cancel")
        }
    }

    private func selectProvider(_ provider: ProviderKind) {
        providerOverride = provider
        loginError = nil
        if let account = selectedAccount {
            chooseAccount(account.id)
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
        startingLogin = true
        loginError = nil
        model.perform(.startAccountLogin(StartAccountLogin(provider: provider))) { result in
            startingLogin = false
            if case let .failure(error) = result {
                loginError = model.snapshot.error() ?? error.localizedDescription; return
            }
            if let id = login?.loginId {
                pollLogin(id)
            }
        }
    }

    private func submitLoginCode(_ id: String) {
        startingLogin = true
        loginError = nil
        let code = loginCode
        loginCode = ""
        model.perform(.submitAccountLogin(SubmitAccountLogin(id: id, code: code))) { result in
            startingLogin = false
            if case let .failure(error) = result {
                loginError = model.snapshot.error() ?? error.localizedDescription
            } else {
                pollLogin(id)
            }
        }
    }

    private func pollLogin(_ id: String) {
        guard pollingLogin == nil else { return }
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
        guard let id = login?.loginId else { return }
        pollingLogin?.cancel()
        startingLogin = true
        model.perform(.cancelAccountLogin(CancelAccountLogin(id: id))) { result in
            startingLogin = false
            if case let .failure(error) = result {
                loginError = model.snapshot.error() ?? error.localizedDescription
            }
        }
    }
}
