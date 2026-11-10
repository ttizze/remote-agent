#if os(iOS)
    import SwiftUI
#endif

public struct NativeBrowserCredentials: Decodable {
    public struct Profile: Decodable {
        public let id: String
        public let label: String
        public init(id: String, label: String) {
            self.id = id; self.label = label
        }
    }

    public struct ImportError: Decodable {
        public let browser: String
        public let message: String
        public init(browser: String, message: String) {
            self.browser = browser; self.message = message
        }
    }

    public let profiles: [Profile]
    public let importErrors: [ImportError]
    public let origin: String?
    public let accounts: [String]
    public let passwordError: String?
    public init(
        profiles: [Profile],
        importErrors: [ImportError],
        origin: String?,
        accounts: [String],
        passwordError: String?
    ) {
        self.profiles = profiles; self.importErrors = importErrors; self.origin = origin
        self.accounts = accounts; self.passwordError = passwordError
    }
}

public enum BrowserCredentialsAction {
    case importCookies(profile: String)
    case importPasswords(profile: String)
    case fillPassword(origin: String, username: String)
    case deletePasswords(origin: String)
}

#if os(iOS)
    public struct BrowserCredentialsView: View {
        let credentials: NativeBrowserCredentials
        let send: (BrowserCredentialsAction) -> Void
        let close: () -> Void

        public init(
            credentials: NativeBrowserCredentials,
            send: @escaping (BrowserCredentialsAction) -> Void,
            close: @escaping () -> Void
        ) {
            self.credentials = credentials; self.send = send; self.close = close
        }

        public var body: some View {
            NavigationStack {
                List {
                    Section("接続先のブラウザから取り込む") {
                        if credentials.profiles.isEmpty {
                            Text("取り込めるブラウザプロファイルがありません。")
                        }
                        ForEach(credentials.profiles, id: \.label) { profile in
                            VStack(alignment: .leading, spacing: 12) {
                                Text(profile.label)
                                Button("ログイン状態（Cookie）") { send(.importCookies(profile: profile.id)) }
                                Button("保存パスワード → Keychain") { send(.importPasswords(profile: profile.id)) }
                            }.padding(.vertical, 4)
                        }
                    }
                    if !credentials.importErrors.isEmpty {
                        Section("読み取れなかったブラウザ") {
                            ForEach(credentials.importErrors, id: \.browser) { error in
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(error.browser)
                                    Text(verbatim: error.message).font(.caption).foregroundStyle(.secondary)
                                }
                            }
                        }
                    }
                    if let origin = credentials.origin {
                        Section("保存パスワード · \(origin)") {
                            if credentials.accounts.isEmpty {
                                Text(verbatim: credentials.passwordError ?? "このサイトの保存パスワードはありません。")
                            }
                            ForEach(credentials.accounts, id: \.self) { username in
                                Button(username.isEmpty ? "ユーザー名なし" : username) {
                                    send(.fillPassword(origin: origin, username: username))
                                }
                            }
                            Button("このサイトの保存パスワードを削除", role: .destructive) {
                                send(.deletePasswords(origin: origin))
                            }.disabled(credentials.accounts.isEmpty)
                        }
                    }
                }
                .navigationTitle("ブラウザデータ")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar { ToolbarItem(placement: .confirmationAction) { Button("閉じる", action: close) } }
            }
        }
    }
#endif
