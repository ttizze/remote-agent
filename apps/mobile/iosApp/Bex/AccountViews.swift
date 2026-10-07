import AgentCore
import SwiftUI
import UIKit

struct AccountUsageView: View {
    let usage: AccountUsage?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if let usage {
                if let error = usage.error {
                    Label(accountErrorMessage(message: error), systemImage: "exclamationmark.circle")
                        .font(.caption).foregroundStyle(AppTheme.warningForeground)
                }
                ForEach(Array(usage.windows.enumerated()), id: \.offset) { _, window in
                    UsageWindowView(window: window)
                }
                if usage.error == nil {
                    let date = Date(timeIntervalSince1970: Double(usage.fetchedAt))
                    Text("\(date.formatted(date: .omitted, time: .shortened)) 更新")
                        .font(.caption).foregroundStyle(.secondary)
                }
            } else {
                Text("使用量を取得中…").font(.caption).foregroundStyle(.secondary)
            }
        }
    }
}

private struct UsageWindowView: View {
    let window: UsageWindow
    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack { Text(window.label); Spacer(); Text("残り \(window.remainingPercent)%").monospacedDigit() }
                .font(AppTheme.font(12))
            ProgressView(value: Double(window.remainingPercent), total: 100)
                .tint(AppTheme.color(window.remainingPercent <= 20 ? "warningForeground" : "successForeground"))
            if let reset = window.resetsAt {
                let date = Date(timeIntervalSince1970: Double(reset))
                Text("リセット: \(date.formatted(date: .abbreviated, time: .shortened))")
                    .font(AppTheme.font(11)).foregroundStyle(AppTheme.tertiary)
            }
        }
    }
}

struct AccountIdentityView: View {
    let account: Account?

    var body: some View {
        HStack(spacing: 4) {
            if let account {
                Image(account.provider == .codex ? "openai" : "anthropic")
                    .resizable()
                    .scaledToFit()
                    .frame(width: 12, height: 12)
                    .accessibilityLabel(account.provider == .codex ? "OpenAI" : "Anthropic")
                Text(account.email ?? account.id)
                    .lineLimit(1)
                    .accessibilityIdentifier("account.identity." + account.id)
            } else {
                Text("未選択")
            }
        }
    }
}

struct AccountLoginSection: View {
    let login: AccountLogin
    let providerName: String
    @Binding var loginCode: String
    let loginError: String?
    let submit: () -> Void
    let retry: () -> Void

    var body: some View {
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
                Button("サインインを完了", action: submit)
                    .disabled(loginCode.trimmingCharacters(in: .whitespacesAndNewlines)
                        .isEmpty)
                    .accessibilityIdentifier("model.login.submit")
            } else {
                ProgressView("ブラウザでの認証を待っています…")
            }
            if let error = loginError {
                Text(accountErrorMessage(message: error)).font(.caption)
                    .foregroundStyle(AppTheme.dangerForeground)
                Button("認証状態を再確認", action: retry)
            }
        }
    }
}
