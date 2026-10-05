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
                        .font(.caption).foregroundStyle(.orange)
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

struct WeeklyUsageView: View {
    let windows: [UsageWindow]
    var compact = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if windows.isEmpty {
                Text("残量未取得").font(.caption).foregroundStyle(.secondary)
            }
            ForEach(Array(windows.enumerated()), id: \.offset) { _, window in
                UsageWindowView(window: window, compact: compact)
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel("週間残量 \(window.remainingPercent)%")
            }
        }
    }
}

struct ReasoningStrengthIcon: View {
    let level: UInt32
    let count: Int

    var body: some View {
        HStack(alignment: .bottom, spacing: 2) {
            ForEach(0 ..< count, id: \.self) { index in
                RoundedRectangle(cornerRadius: 1)
                    .fill(index < Int(level) ? Color.accentColor : Color.secondary.opacity(0.35))
                    .frame(width: 3, height: 6 + 10 * CGFloat(index + 1) / CGFloat(count))
            }
        }
        .frame(width: 28, height: 22)
        .accessibilityHidden(true)
    }
}

private struct UsageWindowView: View {
    let window: UsageWindow
    var compact = false

    var body: some View {
        let tint: Color = window.remainingPercent <= 20 ? .orange : compact ? .accentColor : .green
        if compact {
            HStack(spacing: 8) {
                Text("週間残量")
                ProgressView(value: Double(window.remainingPercent), total: 100)
                    .tint(tint)
                Text("\(window.remainingPercent)%").monospacedDigit()
            }
            .font(.caption2).foregroundStyle(.secondary)
        } else {
            VStack(alignment: .leading, spacing: 5) {
                HStack {
                    Text(window.label)
                    Spacer()
                    Text("残り \(window.remainingPercent)%").monospacedDigit()
                }.font(.caption)
                ProgressView(value: Double(window.remainingPercent), total: 100)
                    .tint(tint)
                if let reset = window.resetsAt {
                    let date = Date(timeIntervalSince1970: Double(reset))
                    Text("リセット: \(date.formatted(date: .abbreviated, time: .shortened))")
                        .font(.caption2).foregroundStyle(.secondary)
                }
            }
        }
    }
}

struct AccountIdentityView: View {
    let account: Account?
    let driver: String?

    var body: some View {
        HStack(spacing: 4) {
            if let account {
                ProviderIconView(driver: driver, size: 12)
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
    let progressMessage: String?
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
                        .isEmpty || progressMessage != nil)
                    .accessibilityIdentifier("model.login.submit")
            } else {
                ProgressView("ブラウザでの認証を待っています…")
            }
            if let progressMessage {
                ProgressView(progressMessage)
            }
            if let error = loginError {
                Text(accountErrorMessage(message: error)).font(.caption).foregroundStyle(.red)
                Button("認証状態を再確認", action: retry)
            }
        }
    }
}

struct ProviderIconView: View {
    let driver: String?
    let size: CGFloat

    var body: some View {
        Group {
            switch driver {
            case "codex": Image("openai").resizable().accessibilityLabel("OpenAI")
            case "claudeAgent": Image("claude").resizable().accessibilityLabel("Anthropic")
            default: Image(systemName: "cpu").resizable().accessibilityLabel("エージェント")
            }
        }
        .scaledToFit().frame(width: size, height: size)
        .foregroundStyle(driver == "claudeAgent" ? Color(red: 0.85, green: 0.47, blue: 0.34) : .primary)
    }
}
