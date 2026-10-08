import AgentCore
import Foundation
import SwiftUI

struct TerminalMenu: View {
    @ObservedObject var model: BexAppViewModel
    let open: (String?) -> Void

    var body: some View {
        let view = model.threadView
        Menu {
            let scripts = view?.scripts?.rows ?? []
            if scripts.isEmpty {
                Button {} label: {
                    Label("No project scripts", systemImage: "play")
                    Text("This project has no saved scripts yet")
                }
                .disabled(true)
            }
            ForEach(scripts, id: \.script.id) { row in
                Button {
                    if let thread = view?.threadId {
                        model.perform(.runProjectScript(threadId: thread, scriptId: row.script.id, cols: 80,
                                                        rows: 24)) { result in
                            if case let .success(.terminalOpened(terminalId)) = result {
                                open(terminalId)
                            }
                        }
                    }
                } label: {
                    Label(row.label, systemImage: row.script.icon.symbol)
                    Text(row.script.command)
                }
            }
            ForEach((view?.terminals ?? []).filter(\.running), id: \.terminalId) { tab in
                Button { open(tab.terminalId) } label: {
                    Label(tab.label, systemImage: "terminal")
                    Text(tab.menuSubtitle)
                }
            }
            Button { open(nil) } label: {
                Label("Open new terminal", systemImage: "plus")
                Text("Start another shell for this thread")
            }
        } label: {
            Image(systemName: "terminal")
        }
        .accessibilityLabel("Terminal")
        .disabled(!model.snapshot.canOpenTerminal())
    }
}

extension TerminalTab {
    /// "Ready · app": the status, then the shell's folder.
    var menuSubtitle: String {
        let folder = URL(fileURLWithPath: cwd).lastPathComponent
        return cwd.isEmpty || folder.isEmpty ? menuStatus : "\(menuStatus) · \(folder)"
    }
}

extension ProjectScriptIcon {
    var symbol: String {
        switch self {
        case .play: "play"
        case .test: "flask"
        case .lint: "checklist"
        case .configure: "wrench.and.screwdriver"
        case .build: "hammer"
        case .debug: "ladybug"
        }
    }
}

struct LoadEarlierButton: View {
    let history: ThreadHistoryView
    let load: () -> Void

    var body: some View {
        VStack(spacing: 6) {
            Button(action: load) {
                HStack(spacing: 6) {
                    if history.loading {
                        ProgressView().controlSize(.mini)
                    } else {
                        Image(systemName: "chevron.up").font(.system(size: 12)).foregroundStyle(AppTheme.primary)
                    }
                    Text(history.loading ? "Loading earlier activity…" : "Load earlier activity")
                        .font(AppTheme.font(14, weight: .medium)).foregroundStyle(AppTheme.text)
                }
                .padding(.horizontal, 14).padding(.vertical, 7)
                .frame(minHeight: 31.5)
                .background(AppTheme.card.opacity(0.8), in: Capsule())
                .overlay(Capsule().stroke(AppTheme.border.opacity(0.6)))
            }
            .buttonStyle(.plain)
            .disabled(history.loading)
            if let error = history.error {
                Text(error).font(AppTheme.font(13)).foregroundStyle(AppTheme.muted)
            }
        }
        .frame(maxWidth: .infinity)
        .padding(.bottom, 12)
    }
}

struct ErrorBanner: View {
    let banner: ThreadErrorBanner
    let dismiss: () -> Void

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: "exclamationmark.circle").font(.system(size: 14))
            Text(banner.text).font(AppTheme.font(13)).frame(maxWidth: .infinity, alignment: .leading)
            Button(action: dismiss) { Image(systemName: "xmark").font(.system(size: 12)) }
                .buttonStyle(.plain)
                .accessibilityLabel(banner.dismissLabel)
        }
        .foregroundStyle(AppTheme.dangerForeground)
        .padding(10.5)
        .background(AppTheme.danger, in: RoundedRectangle(cornerRadius: 10.5))
        .padding(.horizontal, 10.5)
    }
}

struct LimitRecoveryCard: View {
    let recovery: UsageLimitRecovery
    let act: (RecoveryAction) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(recovery.title).font(AppTheme.font(13)).foregroundStyle(AppTheme.text)
            HStack(spacing: 8) {
                if recovery.canSchedule {
                    Button(recovery.resumeLabel) { act(.resume) }
                }
                if recovery.snoozeEnabled {
                    Button(recovery.snoozeLabel) { act(.snooze) }
                }
            }
            .buttonStyle(.plain)
            .font(AppTheme.font(13, weight: .medium))
            .padding(.horizontal, 10).padding(.vertical, 6)
            .background(AppTheme.subtle, in: RoundedRectangle(cornerRadius: 7))
        }
        .padding(10.5)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(AppTheme.screen, in: RoundedRectangle(cornerRadius: 10.5))
        .overlay(RoundedRectangle(cornerRadius: 10.5).stroke(AppTheme.warningForeground.opacity(0.25)))
        .padding(.horizontal, 10.5)
    }
}
