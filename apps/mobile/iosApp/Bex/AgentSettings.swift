import AgentCore
import SwiftUI
import UIKit

enum T3 {
    static let palette = theme(dark: true)
    static func color(_ role: String) -> Color {
        Color(uiColor: uiColor(role))
    }

    static func uiColor(_ role: String) -> UIColor {
        let value = UInt32(palette.colors[role, default: "#f5f5f5"].dropFirst(), radix: 16) ?? 0
        return UIColor(red: CGFloat((value >> 16) & 255) / 255,
                       green: CGFloat((value >> 8) & 255) / 255, blue: CGFloat(value & 255) / 255, alpha: 1)
    }

    static func font(_ size: CGFloat = 16, weight: Font.Weight = .regular) -> Font {
        .custom(
            weight == .bold ? "DMSans-Bold" : weight == .medium || weight == .semibold ? "DMSans-Medium" :
                "DMSans-Regular",
            size: size
        )
    }

    static func status(_ tone: StatusTone) -> Color {
        switch tone {
        case .muted: color("textMuted")
        case .info: color("updateForeground")
        case .warning: color("warningForeground")
        case .input: color("inputForeground")
        case .error: color("errorForeground")
        case .success: color("successForeground")
        }
    }
}

struct SettingsSheet: View {
    @ObservedObject var model: BexAppViewModel
    @Environment(\.dismiss) private var dismiss
    @State private var code = ""
    var body: some View {
        NavigationStack {
            List {
                Section("Connection") {
                    Button(model.selectedProfileName ?? "Hosts") { dismiss(); model.showProfiles() }
                    Text(model.isConnected ? "Connected" : "Offline").foregroundStyle(T3.color("textMuted"))
                    Button("Reconnect") { model.connect(afterForeground: true) }
                }
                Section("Provider accounts") {
                    ForEach(model.snapshot.accounts()?.accounts ?? [], id: \.id) { account in
                        VStack(alignment: .leading, spacing: 8) {
                            AccountIdentityView(account: account)
                            AccountUsageView(usage: account.usage)
                            HStack {
                                Button("Select") { model.perform(.selectAccount(
                                    provider: account.provider,
                                    id: account.id
                                )) }
                                Button("Remove", role: .destructive) { model.perform(.deleteAccount(
                                    provider: account.provider,
                                    id: account.id
                                )) }
                            }
                        }
                    }
                    Button("Sign in to Codex") { model.perform(.startLogin(provider: .codex)) }
                    Button("Sign in to Claude") { model.perform(.startLogin(provider: .claude)) }
                }
                if let login = model.snapshot.accountLogin() {
                    AccountLoginSection(login: login, providerName: login.provider == .codex ? "Codex" : "Claude",
                                        loginCode: $code,
                                        loginError: model.snapshot.accounts()?.error,
                                        submit: { model.perform(.completeLogin(
                                            provider: login.provider,
                                            id: login.loginId,
                                            code: code
                                        )); code = "" },
                                        retry: { model.perform(.loadAccounts) })
                    Button("Cancel sign in") { model.perform(.cancelLogin(
                        provider: login.provider,
                        id: login.loginId
                    )); code = "" }
                }
                Section("Workspace") {
                    NavigationLink("Worktree settings") {
                        WorktreeSettingsScreen(model: model).id(model.selectedProfileId)
                    }
                }
                if let error = model.notice {
                    Section { BexNotice(text: error) }
                }
                Section { PrivacyPolicyButton() }
            }
            .scrollContentBackground(.hidden).background(T3.color("canvas"))
            .navigationTitle("Settings").navigationBarTitleDisplayMode(.inline)
            .toolbar { Button("Done") { dismiss() } }
            .onAppear { model.perform(.loadAccounts) }
        }
        .tint(T3.color("mobilePrimaryText")).font(T3.font(14))
    }
}

struct ThreadActions: View {
    @ObservedObject var model: BexAppViewModel
    let id: String
    let pinned: Bool
    let archived: Bool
    let settled: Bool
    var body: some View {
        Button(pinned ? "Unpin" : "Pin") { action(pinned ? .unpin : .pin) }
        if pinned {
            Button("Move up") { model.perform(.movePinned(threadId: id, up: true)) }
            Button("Move down") { model.perform(.movePinned(threadId: id, up: false)) }
        }
        Button(settled ? "Un-settle" : "Settle") { action(settled ? .unsettle : .settle) }
        Button("Snooze for 1 hour") {
            action(.snooze(until: ISO8601DateFormatter().string(from: Date().addingTimeInterval(3600))))
        }
        Button("Mark unread") { action(.markUnread) }
        Button(archived ? "Unarchive" : "Archive") { action(archived ? .unarchive : .archive) }
        Button("Delete", role: .destructive) {
            action(.delete); if model.selectedThreadId == id {
                model.showThreadList()
            }
        }
    }

    private func action(_ value: ThreadAction) {
        model.perform(.thread(threadId: id, action: value))
    }
}
