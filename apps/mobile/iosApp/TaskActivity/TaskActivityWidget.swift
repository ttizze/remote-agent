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
            let view = context.state.display.current
            VStack(alignment: .leading, spacing: 10) {
                Text("Bex · \(context.state.hostName)").font(.headline).lineLimit(1)
                TaskIconRow(icons: view.icons)
                    .frame(height: 22)
                Text(view.label).font(.subheadline)
            }
            .padding(16)
            .activityBackgroundTint(Color(.secondarySystemBackground))
            .activitySystemActionForegroundColor(.primary)
            .widgetURL(context.attributes.url)
        } dynamicIsland: { context in
            let view = context.state.display.current
            return DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    Text("Bex").font(.headline)
                }
                DynamicIslandExpandedRegion(.trailing) {
                    Text("\(view.total)件").font(.caption)
                }
                DynamicIslandExpandedRegion(.bottom) {
                    VStack(alignment: .leading, spacing: 8) {
                        TaskIconRow(icons: view.icons).frame(height: 22)
                        Text(view.label).font(.subheadline)
                        Text(context.state.hostName).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            } compactLeading: {
                compactIcons(view)
            } compactTrailing: {
                EmptyView()
            } minimal: {
                TaskIconRow(icons: Array(view.icons.prefix(1))).accessibilityLabel(view.label)
            }
            .widgetURL(context.attributes.url)
        }
    }

    private func compactIcons(_ view: TaskActivityAttributes.View) -> some View {
        let icons = view.icons
        return HStack(spacing: 2) {
            TaskIconRow(icons: icons)
                .frame(width: CGFloat(icons.count) * 10, height: 22, alignment: .leading)
            if view.overflow > 0 {
                Text("+\(view.overflow)").font(.caption2).monospacedDigit()
            }
        }
    }
}

private struct TaskIconRow: View {
    let icons: [TaskActivityAttributes.Icon]

    var body: some View {
        HStack(spacing: 1) {
            ForEach(icons.indices, id: \.self) { index in
                Image(systemName: icons[index].kind.symbol)
                    .resizable().scaledToFit()
                    .foregroundStyle(icons[index].kind == .waiting ? .orange : .primary)
                    .accessibilityLabel(icons[index].label)
                    .frame(maxWidth: 22)
            }
        }
    }
}

private extension TaskActivityAttributes.IconKind {
    var symbol: String {
        switch self {
        case .running: "circle.dotted"
        case .waiting: "person.crop.circle.badge.questionmark"
        case .unknown: "arrow.clockwise.circle"
        case .finished: "checkmark.circle.fill"
        }
    }
}
