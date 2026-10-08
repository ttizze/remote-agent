import ActivityKit
import SwiftUI
import WidgetKit

@main
struct BexTaskWidgets: WidgetBundle {
    var body: some Widget { BexTaskActivity() }
}

private struct BexTaskActivity: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: TaskActivityAttributes.self) { context in
            VStack(alignment: .leading, spacing: 10) {
                Text("Bex · \(context.state.hostName)").font(.headline).lineLimit(1)
                TaskIconRow(icons: context.state.summary.icons, stale: context.isStale || !context.state.connected)
                    .frame(height: 22)
                Text(statusLabel(context)).font(.subheadline)
            }
            .padding(16)
            .activityBackgroundTint(Color(.secondarySystemBackground))
            .activitySystemActionForegroundColor(.primary)
            .widgetURL(context.attributes.url)
        } dynamicIsland: { context in
            DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    Text("Bex").font(.headline)
                }
                DynamicIslandExpandedRegion(.trailing) {
                    Text("\(context.state.summary.total)件").font(.caption)
                }
                DynamicIslandExpandedRegion(.bottom) {
                    VStack(alignment: .leading, spacing: 8) {
                        TaskIconRow(icons: context.state.summary.icons,
                                    stale: context.isStale || !context.state.connected).frame(height: 22)
                        Text(statusLabel(context)).font(.subheadline)
                        Text(context.state.hostName).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            } compactLeading: {
                compactIcons(context, leading: true)
            } compactTrailing: {
                compactIcons(context, leading: false)
            } minimal: {
                TaskIconRow(icons: Array(context.state.summary.icons.prefix(1)),
                            stale: context.isStale || !context.state.connected)
                    .accessibilityLabel(statusLabel(context))
            }
            .widgetURL(context.attributes.url)
        }
    }

    private func compactIcons(_ context: ActivityViewContext<TaskActivityAttributes>, leading: Bool) -> some View {
        let icons = context.state.summary.icons
        let middle = (icons.count + 1) / 2
        let half = leading ? Array(icons.prefix(middle)) : Array(icons.dropFirst(middle))
        return HStack(spacing: 2) {
            TaskIconRow(icons: half, stale: context.isStale || !context.state.connected)
                .frame(width: CGFloat(half.count) * 10, height: 22)
            if !leading && context.state.summary.total > icons.count {
                Text("+\(context.state.summary.total - icons.count)").font(.caption2).monospacedDigit()
            }
        }
    }

    private func statusLabel(_ context: ActivityViewContext<TaskActivityAttributes>) -> String {
        context.isStale || !context.state.connected ? "更新待ち" : context.state.summary.statusLabel
    }
}

private struct TaskIconRow: View {
    let icons: [String]
    let stale: Bool

    var body: some View {
        HStack(spacing: 1) {
            ForEach(icons.indices, id: \.self) { index in
                Image(systemName: stale ? "arrow.clockwise.circle" : icons[index])
                    .resizable().scaledToFit()
                    .foregroundStyle(icons[index] == "person.crop.circle.badge.questionmark" ? .orange : .primary)
                    .accessibilityLabel(stale ? "更新待ち" : icons[index] == "person.crop.circle.badge.questionmark" ? "確認待ち" : "実行中")
                    .frame(maxWidth: 22)
            }
        }
    }
}
