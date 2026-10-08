import ActivityKit
import SwiftUI
import WidgetKit

@main
struct BexTaskWidgets: WidgetBundle {
    var body: some Widget {
        BexTaskActivity()
    }
}

private struct BexTaskActivity: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: TaskActivityAttributes.self) { context in
            HStack(spacing: 14) {
                TaskStatusIcon(state: context.state, stale: context.isStale)
                    .font(.title)
                VStack(alignment: .leading, spacing: 5) {
                    Text(context.state.title).font(.headline).lineLimit(2).privacySensitive()
                    Text(statusLabel(context)).font(.subheadline)
                    Text("Bex · \(context.state.hostName)")
                        .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
                Spacer(minLength: 0)
            }
            .padding(16)
            .activityBackgroundTint(Color(.secondarySystemBackground))
            .activitySystemActionForegroundColor(.primary)
            .widgetURL(context.attributes.url)
        } dynamicIsland: { context in
            DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    TaskStatusIcon(state: context.state, stale: context.isStale)
                }
                DynamicIslandExpandedRegion(.trailing) {
                    Text(statusLabel(context)).font(.caption)
                }
                DynamicIslandExpandedRegion(.bottom) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(context.state.title).font(.headline).lineLimit(2).privacySensitive()
                        Text("Bex · \(context.state.hostName)")
                            .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            } compactLeading: {
                TaskStatusIcon(state: context.state, stale: context.isStale)
            } compactTrailing: {
                Text(statusLabel(context)).font(.caption2).lineLimit(1)
            } minimal: {
                TaskStatusIcon(state: context.state, stale: context.isStale)
            }
            .widgetURL(context.attributes.url)
        }
    }

    private func statusLabel(_ context: ActivityViewContext<TaskActivityAttributes>) -> String {
        context.isStale || !context.state.connected ? "更新待ち" : context.state.statusLabel
    }
}

private struct TaskStatusIcon: View {
    let state: TaskActivityAttributes.ContentState
    let stale: Bool

    var body: some View {
        Image(systemName: stale || !state.connected ? "arrow.clockwise.circle" : state.symbol)
            .foregroundStyle(state.status == "waiting" || state.status == "failed" ? .orange : .primary)
            .accessibilityLabel(stale || !state.connected ? "更新待ち" : state.statusLabel)
    }
}
