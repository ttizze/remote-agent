import ActivityKit
import AgentCore
import SwiftUI
import WidgetKit

private struct ActivityDisplayRow: Decodable {
    let project: String
    let title: String
    let status: String
    let glyph: String
    let color: String
}

private struct ActivityDisplay: Decodable {
    let headline: String
    let attention: String
    let compactLabel: String
    let summary: String
    let expandedLabel: String
    let minimalGlyph: Bool
    let glyph: String
    let color: String
    let deepLink: String?
    let rows: [ActivityDisplayRow]

    static func project(_ content: AgentActivityAttributes.ContentState, stale: Bool,
                        light: Bool, monochrome: Bool, reduced: Bool) -> ActivityDisplay? {
        guard let encoded = try? JSONEncoder().encode(content),
              let json = String(data: encoded, encoding: .utf8) else { return nil }
        let projected = agentActivityWidgetJson(json: json, stale: stale, light: light,
                                                monochrome: monochrome, reduced: reduced)
        return projected.data(using: .utf8).flatMap { try? JSONDecoder().decode(ActivityDisplay.self, from: $0) }
    }
}

private enum ActivityLayout { case banner, compactLeading, compactTrailing, minimal, expandedLeading, expandedBottom }

private struct ActivityPresentation: View {
    let content: AgentActivityAttributes.ContentState
    let stale: Bool
    let layout: ActivityLayout
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.widgetRenderingMode) private var renderingMode
    @Environment(\.isLuminanceReduced) private var reduced
    @Environment(\.activityFamily) private var activityFamily

    private var display: ActivityDisplay? {
        ActivityDisplay.project(content, stale: stale, light: colorScheme == .light,
                                monochrome: renderingMode != .fullColor, reduced: reduced)
    }

    var body: some View {
        if let view = display {
            Group {
                switch layout {
                case .banner:
                    if activityFamily == .small {
                        VStack(alignment: .leading, spacing: 5) {
                            HStack(spacing: 7) {
                                mark(size: 14)
                                Text(view.summary).font(.system(size: 13, weight: .bold))
                                    .foregroundStyle(tint(view.color)).lineLimit(1)
                                Spacer(minLength: 6)
                            }
                            rows(Array(view.rows.prefix(1)), showProject: false)
                        }.padding(10)
                    } else {
                        VStack(alignment: .leading, spacing: 6) {
                            ZStack(alignment: .leading) {
                                mark(size: 13)
                                HStack(spacing: 6) {
                                    Spacer(minLength: 0)
                                    Text(view.headline)
                                        .foregroundStyle(content.activeCount == 0 ? tint(view.color) : Color.primary)
                                    if !view.attention.isEmpty {
                                        Text("·").foregroundStyle(.secondary)
                                        Text(view.attention).foregroundStyle(tint(view.color))
                                    }
                                    Spacer(minLength: 0)
                                }.font(.system(size: 13, weight: .semibold)).lineLimit(1)
                            }
                            rows(view.rows)
                        }.padding(14)
                    }
                case .compactLeading:
                    mark(size: 14)
                case .compactTrailing:
                    Text(view.compactLabel).font(.system(size: 11, weight: .semibold)).foregroundStyle(tint(view.color))
                case .minimal:
                    if view.minimalGlyph {
                        Image(systemName: view.glyph).font(.system(size: 13)).foregroundStyle(tint(view.color))
                    } else {
                        mark(size: 11)
                    }
                case .expandedLeading:
                    HStack(spacing: 5) {
                        mark(size: 15)
                        Text(view.expandedLabel).font(.system(size: 13, weight: .bold))
                            .foregroundStyle(tint(view.color))
                    }.padding(.leading, 4).padding(.vertical, 4)
                case .expandedBottom:
                    rows(Array(view.rows.prefix(3))).padding(.vertical, 2).padding(.horizontal, 8)
                }
            }.widgetURL(view.deepLink.flatMap(URL.init(string:)))
        }
    }

    private func rows(_ items: [ActivityDisplayRow], showProject: Bool = true) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            ForEach(Array(items.enumerated()), id: \.offset) { _, row in
                HStack(spacing: 7) {
                    Text(row.title).font(.system(size: 13, weight: .semibold)).lineLimit(1)
                    if showProject {
                        Text(row.project).font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1)
                    }
                    Spacer(minLength: 8)
                    Text(row.status).font(.system(size: 11, weight: .semibold)).foregroundStyle(tint(row.color))
                        .layoutPriority(1)
                }
            }
        }
    }

    private func mark(size: CGFloat) -> some View {
        Image("AgentMark").resizable().scaledToFit().frame(width: size, height: size)
    }

    private func tint(_ value: String) -> Color {
        if value == "primary" {
            return .primary
        }
        if value == "secondary" {
            return .secondary
        }
        guard let hex = UInt32(value.dropFirst(), radix: 16) else { return .primary }
        return Color(red: Double((hex >> 16) & 255) / 255,
                     green: Double((hex >> 8) & 255) / 255,
                     blue: Double(hex & 255) / 255)
    }
}

struct AgentActivityWidget: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: AgentActivityAttributes.self) { context in
            ActivityPresentation(content: context.state, stale: context.isStale, layout: .banner)
                .activityBackgroundTint(.clear)
        } dynamicIsland: { context in
            DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    ActivityPresentation(content: context.state, stale: context.isStale, layout: .expandedLeading)
                }
                DynamicIslandExpandedRegion(.bottom) {
                    ActivityPresentation(content: context.state, stale: context.isStale, layout: .expandedBottom)
                }
            } compactLeading: {
                ActivityPresentation(content: context.state, stale: context.isStale, layout: .compactLeading)
            } compactTrailing: {
                ActivityPresentation(content: context.state, stale: context.isStale, layout: .compactTrailing)
            } minimal: {
                ActivityPresentation(content: context.state, stale: context.isStale, layout: .minimal)
            }
            .widgetURL(ActivityDisplay.project(context.state, stale: context.isStale,
                                               light: false, monochrome: false, reduced: false)?.deepLink
                    .flatMap(URL.init(string:)))
        }
        .supplementalActivityFamilies([.small, .medium])
    }
}

@main
struct AgentWidgets: WidgetBundle {
    var body: some Widget {
        SubscriptionUsageWidget()
        AgentActivityWidget()
    }
}
