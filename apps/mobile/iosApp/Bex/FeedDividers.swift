import AgentCore
import SwiftUI

struct ContextDivider: View {
    let label: String
    let icon: String
    let danger: Bool
    var shimmer = false

    var body: some View {
        HStack(spacing: 8) {
            Rectangle().fill(AppTheme.border).frame(height: 1)
            Image(systemName: icon).font(.system(size: 12))
            Text(label).font(AppTheme.font(13, weight: .medium)).shimmering(shimmer)
            Rectangle().fill(AppTheme.border).frame(height: 1)
        }
        .foregroundStyle(danger ? AppTheme.rose : AppTheme.muted)
    }
}

struct HandoffRow: View {
    let divider: HandoffDivider

    var body: some View {
        VStack(spacing: 4) {
            ContextDivider(label: divider.label, icon: divider.icon.symbol, danger: divider.tone == .danger)
            HStack(spacing: 4) {
                ForEach(Array(divider.endpoints.from.enumerated()), id: \.offset) { index, endpoint in
                    if index > 0 {
                        Text(",")
                    }
                    ProviderIcon(instance: endpoint.instance, size: 12)
                    Text(endpoint.model ?? endpoint.instance)
                }
                Text("→")
                ProviderIcon(instance: divider.endpoints.to.instance, size: 12)
                Text(divider.endpoints.to.model ?? divider.endpoints.to.instance)
            }
            .font(AppTheme.font(13)).foregroundStyle(AppTheme.muted).lineLimit(1)
        }
    }
}

extension DividerIcon {
    var symbol: String {
        switch self {
        case .stop: "stop.circle"
        case .compaction: "arrow.down.right.and.arrow.up.left"
        case .handoff: "arrow.left.arrow.right"
        case .fork: "arrow.triangle.branch"
        }
    }
}

struct LifecycleRowView: View {
    let row: LifecycleRow
    let actions: FeedActions

    var body: some View {
        switch row {
        case let .interruptRequest(request):
            ContextDivider(label: request.label, icon: "stop.circle", danger: false)
        case let .divider(divider):
            VStack(spacing: 4) {
                ContextDivider(label: divider.label, icon: divider.icon.symbol, danger: divider.tone == .danger)
                if let action = divider.action {
                    Button(action.label) { actions.openThread(action.thread) }
                        .font(AppTheme.font(13, weight: .medium))
                }
            }
        case let .createdThread(created):
            Button { actions.openThread(created.thread) } label: {
                HStack {
                    Image(systemName: "text.bubble").font(.system(size: 14))
                    Text(created.label).font(AppTheme.font(14)).lineLimit(1)
                    Spacer()
                    Text(created.actionLabel).font(AppTheme.font(13, weight: .medium))
                }
                .foregroundStyle(AppTheme.muted)
                .frame(minHeight: 28)
            }
            .buttonStyle(.plain)
            .accessibilityLabel(created.accessibilityLabel)
        case let .subagent(link):
            SubagentLinkRow(link: link, elapsed: nil, open: actions.openThread)
        }
    }
}

struct PlanCardView: View {
    let plan: PlanCard
    let useArtifactTemplate: (ArtifactTemplate) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(plan.title).font(AppTheme.font(14, weight: .bold))
            ConversationMarkdown(source: plan.displayedMarkdown, useArtifactTemplate: useArtifactTemplate)
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(AppTheme.groupedCard, in: RoundedRectangle(cornerRadius: 14))
    }
}

struct WorkingSinceRow: View {
    let createdAt: Int64?

    var body: some View {
        TimelineView(.periodic(from: .now, by: 1)) { clock in
            let now = Int64(clock.date.timeIntervalSince1970 * 1000)
            Text("Working for \(workingTimerLabel(startedAtMs: createdAt ?? now, nowMs: now))")
                .font(AppTheme.font(14)).foregroundStyle(AppTheme.muted).shimmering(true)
                .frame(minHeight: 28, alignment: .leading)
        }
    }
}

/// A highlight that sweeps across live labels.
struct Shimmer: ViewModifier {
    let active: Bool
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var phase: CGFloat = -1

    func body(content: Content) -> some View {
        if active, !reduceMotion {
            content
                .overlay {
                    GeometryReader { proxy in
                        LinearGradient(
                            colors: [.clear, AppTheme.screen.opacity(0.7), .clear],
                            startPoint: .leading, endPoint: .trailing
                        )
                        .frame(width: 72)
                        .offset(x: phase * (proxy.size.width + 72))
                    }
                    .mask(content)
                    .allowsHitTesting(false)
                }
                .onAppear {
                    withAnimation(.linear(duration: 1.35).delay(1.45).repeatForever(autoreverses: false)) {
                        phase = 1
                    }
                }
        } else {
            content
        }
    }
}

extension View {
    func shimmering(_ active: Bool) -> some View {
        modifier(Shimmer(active: active))
    }
}
