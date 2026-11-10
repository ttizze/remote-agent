import AgentCore
import SwiftUI

struct BrowserCredentialsSheet: View {
    @Environment(\.locale) private var locale
    let credentials: BrowserCredentials
    let send: (BrowserAction) -> Void
    let close: () -> Void

    var body: some View {
        BrowserCredentialsView(credentials: NativeBrowserCredentials(
            profiles: credentials.profiles.map { .init(id: $0.id, label: $0.label) },
            importErrors: credentials.importErrors.map { .init(
                browser: $0.browser,
                message: localizedUiText(language: locale.appLanguage, source: $0.message)
            ) },
            origin: credentials.origin, accounts: credentials.accounts,
            passwordError: localizedUiText(
                language: locale.appLanguage,
                source: credentials.passwordError ?? "このサイトの保存パスワードはありません。"
            )
        ), send: { action in
            switch action {
            case let .importCookies(profile): send(.importCookies(profile: profile))
            case let .importPasswords(profile): send(.importPasswords(profile: profile))
            case let .fillPassword(origin, username): send(.fillPassword(origin: origin, username: username))
            case let .deletePasswords(origin): send(.deletePasswords(origin: origin))
            }
        }, close: close)
    }
}
