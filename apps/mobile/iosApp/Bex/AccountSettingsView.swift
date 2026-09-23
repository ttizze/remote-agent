import AgentCore
import SwiftUI

struct AccountSettingsView: View {
    @ObservedObject var model: BexAppViewModel
    var signInProvider: ProviderKind?
    @State private var appeared = false
    @Environment(\.scenePhase) private var scenePhase
    @State private var loginCode = ""
    @State private var changingAccount = false
    @State private var loadingAccounts = false
    @State private var loginError: String?
    @State private var startingLogin = false
    @State private var pollingLogin: Task<Void, Never>?
    private var login: AccountLogin? {
        model.snapshot.accountLogin()
    }

    var body: some View {
        List {
            Section {
                if let login {
                    if login.requiresCodeSubmission {
                        Text("ブラウザで Claude にログインし、表示された認証コードを貼り付けてください。")
                        SecureField("認証コード", text: $loginCode)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .accessibilityIdentifier("model.login.input")
                        Button("認証コードを送信") { submitLoginCode(login.loginId) }
                            .disabled(loginCode.trimmingCharacters(in: .whitespacesAndNewlines)
                                .isEmpty || startingLogin)
                            .accessibilityIdentifier("model.login.submit")
                    } else {
                        Text("ブラウザでログインし、次のコードを入力してください。")
                        Text(login.userCode).font(.title2.monospaced()).textSelection(.enabled)
                            .accessibilityIdentifier("model.login.code")
                    }
                    if let url = URL(string: login.verificationUrl),
                       url.scheme == "https" {
                        Link("ログインページを開く", destination: url)
                    }
                    Button("ログインをキャンセル") { cancelLogin() }
                } else {
                    Button { startLogin(.codex) } label: { Label("Codex にサインイン", systemImage: "person.badge.plus") }
                        .disabled(startingLogin || changingAccount || !model.isConnected)
                        .accessibilityIdentifier("model.account.add")
                    Button { startLogin(.claude) } label: { Label("Claude にサインイン", systemImage: "person.badge.plus") }
                        .disabled(startingLogin || changingAccount || !model.isConnected)
                        .accessibilityIdentifier("model.account.add.claude")
                }
                if startingLogin {
                    ProgressView("サインインを準備中…")
                }
                if let error = loginError ?? model.accountError ?? model.modelError {
                    Text(error).font(.caption).foregroundColor(.red)
                    Button("再読み込み") { refresh() }
                }
            }
            Section {
                ForEach(model.accounts, id: \.id) { account in
                    VStack(alignment: .leading, spacing: 12) {
                        Button { chooseAccount(account.id) } label: {
                            AccountIdentityRow(
                                account: account,
                                selected: model.snapshot.accountIsSelected(id: account.id)
                            )
                        }
                        .buttonStyle(.borderless)
                        .accessibilityIdentifier("model.account." + account.id)
                        .accessibilityValue(model.snapshot.accountIsSelected(id: account.id) ? "選択中" : "")
                        .disabled(changingAccount || login != nil || !model.isConnected)
                        AccountUsageView(usage: account.usage)
                        AccountSignOutButton(model: model, accountId: account.id, changingAccount: $changingAccount)
                    }.padding(.vertical, 4)
                }
                if loadingAccounts {
                    ProgressView("アカウントを読み込み中…")
                }
                if !loadingAccounts, model.accounts.isEmpty {
                    Text("サインインしてください。")
                }
            } footer: {
                Text("接続先に保存したアカウントを管理します。選択はサービスごとに接続先のHostで共有されます。")
            }
            .disabled(startingLogin)
        }
        .navigationTitle("アカウント")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button("再読み込み") { refresh() }
                    .disabled(startingLogin || changingAccount || loadingAccounts || !model.isConnected)
            }
        }
        .navigationBarBackButtonHidden(login != nil || startingLogin)
        .interactiveDismissDisabled(login != nil || startingLogin)
        .onAppear {
            if !appeared, let signInProvider, login == nil {
                startLogin(signInProvider)
            } else {
                refresh()
            }
            appeared = true
        }
        .onChange(of: scenePhase) { phase in
            if phase == .active, let id = login?.loginId {
                pollLogin(id)
            }
        }
        .onDisappear { pollingLogin?.cancel(); pollingLogin = nil }
    }

    private func startLogin(_ provider: ProviderKind) {
        loginCode = ""
        startingLogin = true
        loginError = nil
        model.perform(.startAccountLogin(StartAccountLogin(provider: provider))) { result in
            startingLogin = false
            if case let .failure(error) = result {
                loginError = error.localizedDescription; return
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
                loginError = error.localizedDescription
            } else {
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
            loadingAccounts = true
            model.perform(.listAccounts(ListAccounts())) { _ in loadingAccounts = false }
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
            refresh()
        }
    }
}

struct AppSettingsSheet: View {
    @ObservedObject var model: BexAppViewModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List {
                NavigationLink { AccountSettingsView(model: model) } label: {
                    Label("アカウント", systemImage: "person.crop.circle")
                }.accessibilityIdentifier("settings.accounts")
            }
            .navigationTitle("設定")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) {
                Button("完了") { dismiss() }.accessibilityIdentifier("settings.close")
            } }
        }
    }
}

struct AccountSignOutButton: View {
    @ObservedObject var model: BexAppViewModel
    let accountId: String
    @Binding var changingAccount: Bool

    var body: some View {
        Button("サインアウト", role: .destructive) {
            changingAccount = true
            model.perform(.logoutAccount(LogoutAccount(id: accountId))) { _ in changingAccount = false }
        }
        .buttonStyle(.borderless)
        .disabled(changingAccount || model.snapshot.accountLogin() != nil || !model.isConnected)
        .accessibilityIdentifier("account.logout." + accountId)
    }
}
