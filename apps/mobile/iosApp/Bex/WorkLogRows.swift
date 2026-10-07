import AgentCore
import SwiftUI

struct WorkLogRowView: View {
    let row: WorkLogRow
    let actions: FeedActions

    var body: some View {
        switch row {
        case let .activity(activity):
            ActivityRow(activity: activity, actions: actions)
        case let .providerFailure(failure):
            ProviderFailureView(failure: failure, retry: failure.retryPreparation.map { run in
                { actions.perform(.retryPreparation(runId: run)) }
            })
        }
    }
}

private struct ActivityRow: View {
    let activity: WorkActivityRow
    let actions: FeedActions
    @State private var copied = false

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 5.25) {
                WorkIconView(icon: activity.icon, toolIcon: activity.toolIcon, tone: activity.iconTone)
                Text(activity.label)
                    .font(AppTheme.font(14, weight: activity.labelTone == .default ? .regular : .medium))
                    .foregroundStyle(activity.labelTone.color)
                    .lineLimit(1).truncationMode(.tail)
                    .shimmering(activity.shimmer)
                    .frame(maxWidth: .infinity, alignment: .leading)
                if copied {
                    Text("Copied").font(AppTheme.font(11, weight: .medium)).foregroundStyle(AppTheme.emerald)
                }
                if activity.failureMark {
                    Image(systemName: "xmark").font(.system(size: 11))
                        .foregroundStyle(AppTheme.dangerForeground.opacity(0.4))
                }
                if activity.canExpand {
                    Image(systemName: "chevron.down").font(.system(size: 11))
                        .rotationEffect(.degrees(activity.expanded ? 180 : 0))
                        .foregroundStyle(AppTheme.muted)
                        .frame(width: 14)
                }
            }
            .frame(minHeight: 28)
            .padding(.horizontal, 1.75)
            .contentShape(Rectangle())
            .onTapGesture(perform: tap)
            .onLongPressGesture {
                UIPasteboard.general.string = activity.copyText
                copied = true
                Task {
                    try? await Task.sleep(for: .seconds(1.2))
                    copied = false
                }
            }
            if let preview = activity.answerPreview {
                Text(preview).font(AppTheme.font(14))
                    .foregroundStyle(activity.answerHighlighted ? AppTheme.text : AppTheme.muted)
                    .padding(.leading, 24.5)
            }
            if activity.expanded {
                ActivityDetailView(activity: activity).padding(.leading, 24.5)
                    .transition(.opacity)
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(activity.accessibilityLabel)
        .accessibilityHint(activity.accessibilityHint)
    }

    private func tap() {
        if let thread = activity.opensThread {
            actions.openThread(thread)
            return
        }
        guard activity.canExpand else { return }
        if !activity.expanded, activity.loadDetail {
            actions.perform(.loadItemDetail(itemId: activity.id))
        }
        withAnimation(.easeOut(duration: 0.14)) {
            actions.toggle(\.expandedEntries, activity.id)
        }
    }
}

private struct ActivityDetailView: View {
    let activity: WorkActivityRow

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 6) {
                if let detail = activity.detail {
                    if let reasoning = detail.reasoning {
                        ConversationMarkdown(source: reasoning)
                    }
                    if let call = detail.call?.command ?? detail.call?.argsText {
                        Text(call).font(AppTheme.mono(12)).foregroundStyle(AppTheme.text).textSelection(.enabled)
                    }
                    if let output = detail.output ?? detail.fullDetail {
                        Text(output).font(AppTheme.mono(12)).foregroundStyle(AppTheme.muted).textSelection(.enabled)
                    } else if activity.loadDetail {
                        Text("Loading output…").font(AppTheme.font(12)).foregroundStyle(AppTheme.muted)
                    } else if detail.reasoning == nil {
                        Text("No output.").font(AppTheme.font(12)).foregroundStyle(AppTheme.muted)
                    }
                    if let code = detail.failedExitCode {
                        Text("exit \(code)").font(AppTheme.mono(12)).foregroundStyle(AppTheme.dangerForeground)
                    }
                } else if activity.loadDetail {
                    Text("Loading output…").font(AppTheme.font(12)).foregroundStyle(AppTheme.muted)
                } else {
                    Text("Output is no longer available.").font(AppTheme.font(12)).foregroundStyle(AppTheme.muted)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .frame(maxHeight: 210)
        .fixedSize(horizontal: false, vertical: true)
    }
}

private struct ProviderFailureView: View {
    let failure: ProviderFailureRow
    /// Retries the failed workspace preparation of the run.
    let retry: (() -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 5.25) {
                Image(systemName: "exclamationmark.circle").font(.system(size: 14, weight: .medium))
                    .frame(width: 21, height: 21)
                Text(failure.summary).font(AppTheme.font(14, weight: .medium)).lineLimit(1)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Text(FeedTime.clock(failure.createdAt)).font(AppTheme.font(13)).foregroundStyle(AppTheme.tertiary)
            }
            .foregroundStyle(failure.warning ? AppTheme.warningForeground : AppTheme.rose)
            .frame(minHeight: 28)
            if !failure.message.isEmpty {
                Text(failure.message).font(AppTheme.font(13)).foregroundStyle(AppTheme.muted)
                    .textSelection(.enabled).padding(.leading, 24.5)
            }
            if let retry {
                Button(action: retry) {
                    HStack(spacing: 6) {
                        Image(systemName: "arrow.clockwise").font(.system(size: 13))
                            .foregroundStyle(AppTheme.color("mobileIcon"))
                        Text("Retry").font(AppTheme.font(14, weight: .medium)).foregroundStyle(AppTheme.text)
                    }
                    .padding(.horizontal, 16).frame(minHeight: 44)
                    .overlay(Capsule().stroke(AppTheme.border))
                    .contentShape(Capsule())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Retry workspace preparation")
                .padding(.leading, 28).padding(.top, 8)
            }
        }
        .onLongPressGesture { UIPasteboard.general.string = failure.copyText }
    }
}

extension WorkLabelTone {
    var color: Color {
        switch self {
        case .default: AppTheme.muted
        case .warning: AppTheme.warningForeground
        case .danger: AppTheme.rose
        }
    }
}

struct WorkIconView: View {
    let icon: WorkRowIcon
    let toolIcon: ToolIcon?
    var tone: WorkIconTone = .default

    var body: some View {
        Group {
            if case let .website(_, favicon, _) = toolIcon, let favicon, let url = URL(string: favicon) {
                AsyncImage(url: url) { image in
                    image.resizable().scaledToFit()
                } placeholder: {
                    Image(systemName: "globe")
                }
                .frame(width: 16, height: 16).clipShape(RoundedRectangle(cornerRadius: 3)).opacity(0.7)
            } else {
                Image(systemName: symbol).font(.system(size: 14, weight: .medium))
            }
        }
        .foregroundStyle(color)
        .frame(width: 21, height: 21)
    }

    private var color: Color {
        switch tone {
        case .default: AppTheme.muted
        case .warning: AppTheme.warningForeground
        case .destructive: AppTheme.rose
        case .failed: AppTheme.dangerForeground.opacity(0.4)
        }
    }

    private var symbol: String {
        switch icon {
        case .brain: "brain"
        case .logo: "app"
        case let .feed(feed): feed.symbol
        }
    }
}

extension WorkIcon {
    var symbol: String {
        switch self {
        case .agent: "sparkles"
        case .alert: "exclamationmark.circle"
        case .browser, .globe: "globe"
        case .computer: "desktopcomputer"
        case .check: "checkmark"
        case .command: "terminal"
        case .edit: "square.and.pencil"
        case .eye: "eye"
        case .search: "magnifyingglass"
        case .hammer: "hammer"
        case .lock: "lock"
        case .message: "bubble.left"
        case .warning: "exclamationmark.triangle"
        case .wrench: "wrench"
        case .zap: "bolt"
        }
    }
}

struct WorkToggleView: View {
    let toggle: WorkToggleRow
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 5.25) {
                WorkIconView(icon: toggle.icon, toolIcon: toggle.toolIcon)
                Text(toggle.summary).font(AppTheme.font(14)).foregroundStyle(AppTheme.muted).lineLimit(1)
                    .shimmering(toggle.shimmer)
                    .frame(maxWidth: .infinity, alignment: .leading)
                if toggle.hasFailure {
                    Image(systemName: "xmark").font(.system(size: 11))
                        .foregroundStyle(AppTheme.dangerForeground.opacity(0.4))
                }
                Image(systemName: "chevron.down").font(.system(size: 11))
                    .rotationEffect(.degrees(toggle.expanded ? 180 : 0))
                    .foregroundStyle(AppTheme.muted).frame(width: 14)
            }
            .frame(minHeight: 28)
            .padding(.horizontal, 1.75)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

struct ThinkingRow: View {
    var body: some View {
        HStack(spacing: 5.25) {
            Image(systemName: "brain").font(.system(size: 14, weight: .medium)).frame(width: 21, height: 21)
            Text("Thinking").font(AppTheme.font(14)).shimmering(true)
            Spacer()
        }
        .foregroundStyle(AppTheme.muted)
        .frame(minHeight: 28)
    }
}

struct SubagentGroupView: View {
    let card: SubagentGroupCard
    let toggle: () -> Void
    let open: (String) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if card.grouped {
                Button(action: toggle) {
                    HStack(spacing: 10.5) {
                        Image(systemName: "person.2").font(.system(size: 14)).frame(width: 24.5, height: 24.5)
                            .background(AppTheme.card, in: Circle())
                        VStack(alignment: .leading, spacing: 1) {
                            Text(card.label).font(AppTheme.font(14, weight: .medium)).foregroundStyle(AppTheme.text)
                            Text(card.summary).font(AppTheme.font(12)).foregroundStyle(card.tone.color)
                        }
                        Spacer()
                        if let elapsed = card.elapsed {
                            Text(elapsed).font(AppTheme.font(13)).monospacedDigit().foregroundStyle(AppTheme.muted)
                        }
                        Image(systemName: card.expanded ? "chevron.up" : "chevron.down").font(.system(size: 11))
                            .foregroundStyle(AppTheme.muted)
                    }
                    .frame(minHeight: 49)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(card.accessibilityLabel)
            }
            if card.showsMembers {
                VStack(spacing: 0) {
                    ForEach(Array(card.members.enumerated()), id: \.offset) { _, member in
                        SubagentLinkRow(link: member.link, elapsed: member.elapsed, open: open)
                            .padding(10.5)
                            .accessibilityHint(member.accessibilityHint)
                    }
                }
                .padding(3.5)
                .background(AppTheme.card.opacity(0.3), in: RoundedRectangle(cornerRadius: 10.5))
                .overlay(RoundedRectangle(cornerRadius: 10.5).stroke(AppTheme.border))
            }
        }
    }
}

extension SubagentGroupTone {
    var color: Color {
        switch self {
        case .default: AppTheme.muted
        case .active: AppTheme.sky
        case .failed: AppTheme.rose
        }
    }
}

struct SubagentLinkRow: View {
    let link: SubagentLink
    let elapsed: String?
    let open: (String) -> Void

    var body: some View {
        Button {
            if let thread = link.thread {
                open(thread)
            }
        } label: {
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 5) {
                    Circle().fill(link.liveStatus.tone.color).frame(width: 7, height: 7)
                    Text(link.title).font(AppTheme.font(14, weight: .medium)).foregroundStyle(AppTheme.text)
                        .lineLimit(1)
                    Text("·").foregroundStyle(AppTheme.muted)
                    Text(link.statusLabel).font(AppTheme.font(13, weight: .medium))
                        .foregroundStyle(link.liveStatus.tone.color)
                    if let elapsed {
                        Text(elapsed).font(AppTheme.font(13)).monospacedDigit().foregroundStyle(AppTheme.muted)
                    }
                    Spacer(minLength: 0)
                    if link.thread != nil {
                        Image(systemName: "chevron.right").font(.system(size: 12)).foregroundStyle(AppTheme.muted)
                    }
                }
                if let model = link.model {
                    HStack(spacing: 4) {
                        if let driver = link.driver {
                            Image(driver.iconName).resizable().scaledToFit().frame(width: 12, height: 12)
                        }
                        Text(model).font(AppTheme.font(13)).foregroundStyle(AppTheme.muted).lineLimit(1)
                    }
                }
                if let detail = link.detail {
                    Text(detail).font(link.detailIsPath ? AppTheme.mono(12) : AppTheme.font(13))
                        .foregroundStyle(link.failed ? AppTheme.rose : AppTheme.muted).lineLimit(3)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(link.thread == nil)
    }
}

extension ItemStatus {
    var tone: StatusTone {
        switch self {
        case .pending, .running, .waiting: .working
        case .completed: .completed
        case .failed: .failed
        case .interrupted, .cancelled: .inactive
        }
    }
}
