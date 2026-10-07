import AgentCore
import SwiftUI

/// The floating capsule above the composer: what the thread is doing, its
/// agents and queue, and a button back to the end of the feed.
struct WorkingControl: View {
    let control: WorkingControlView
    let reconnect: () -> Void
    let showAgents: () -> Void
    let showQueue: () -> Void
    let scrollToEnd: () -> Void

    var body: some View {
        HStack(spacing: 14) {
            if control.hasCapsule {
                capsule.transition(.opacity)
            }
            if control.showScrollToEnd {
                Button {
                    Haptics.selection()
                    scrollToEnd()
                } label: {
                    Image(systemName: "chevron.down").font(.system(size: 14, weight: .semibold))
                        .frame(width: 38.5, height: 38.5)
                }
                .buttonStyle(.plain)
                .glassEffect(.regular, in: Circle())
                .accessibilityLabel("Scroll to end")
                .transition(.opacity.combined(with: .move(edge: .trailing)))
            }
        }
        .foregroundStyle(AppTheme.text)
        .animation(.easeOut(duration: 0.24), value: control.hasCapsule)
        .animation(.easeOut(duration: 0.24), value: control.showScrollToEnd)
    }

    private var capsule: some View {
        HStack(spacing: 0) {
            if let status = control.status {
                StatusSegment(status: status, label: control.statusLabel)
                    .padding(.horizontal, 14)
                    .contentShape(Rectangle())
                    .onTapGesture {
                        if control.statusInteractive {
                            reconnect()
                        }
                    }
                    .accessibilityLabel(control.statusAccessibilityLabel ?? control.statusLabel ?? "")
            }
            if let agents = control.agents {
                divider(after: control.status != nil)
                Button(action: showAgents) {
                    Label(agents.label, systemImage: "person.2").labelStyle(SegmentLabelStyle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(agents.accessibilityLabel)
            }
            if let queue = control.queue {
                divider(after: control.status != nil || control.agents != nil)
                Button(action: showQueue) {
                    Label(queue.label, systemImage: "list.number").labelStyle(SegmentLabelStyle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(queue.accessibilityLabel)
            }
        }
        .frame(height: 38.5)
        .glassEffect(.regular, in: Capsule())
    }

    @ViewBuilder
    private func divider(after previous: Bool) -> some View {
        if previous {
            Rectangle().fill(AppTheme.border).frame(width: 1, height: 14)
        }
    }
}

private struct SegmentLabelStyle: LabelStyle {
    func makeBody(configuration: Configuration) -> some View {
        HStack(spacing: 6) {
            configuration.icon.font(.system(size: 13))
            configuration.title.font(AppTheme.font(13, weight: .medium)).monospacedDigit().lineLimit(1)
        }
        .padding(.horizontal, 10.5)
        .frame(maxHeight: .infinity)
        .contentShape(Rectangle())
    }
}

private struct StatusSegment: View {
    let status: FloatingWorkingStatus
    let label: String?

    var body: some View {
        HStack(spacing: 6) {
            switch status {
            case let .working(startedAtMs):
                TimelineView(.periodic(from: .now, by: 1)) { clock in
                    let now = Int64(clock.date.timeIntervalSince1970 * 1000)
                    Text("Working \(workingDurationLabel(startedAtMs: startedAtMs, nowMs: now))")
                }
            case let .syncing(text):
                ProgressView().controlSize(.mini)
                Text(text)
            case .compacting:
                Image(systemName: "arrow.down.right.and.arrow.up.left").font(.system(size: 12))
                Text(label ?? "Compacting…")
            case let .background(text, _, waiting):
                Image(systemName: waiting ? "bolt" : "terminal").font(.system(size: 12))
                Text(text)
            case let .preparing(text):
                Image(systemName: "arrow.triangle.branch").font(.system(size: 12))
                Text(text).shimmering(true)
            case let .connection(tone, text):
                if tone == .reconnecting {
                    ProgressView().controlSize(.mini)
                } else {
                    Circle().fill(AppTheme.rose).frame(width: 7, height: 7)
                }
                Text(text).frame(maxWidth: 260)
            }
        }
        .font(AppTheme.font(13, weight: .medium))
        .monospacedDigit()
        .lineLimit(1)
    }
}
