import AgentCore
import AppIntents
import SwiftUI
import WidgetKit

enum UsagePeriod: String, AppEnum {
    case auto, session, weekly
    static var typeDisplayRepresentation = TypeDisplayRepresentation(name: "Quota period")
    static var caseDisplayRepresentations: [UsagePeriod: DisplayRepresentation] = [
        .auto: "Automatic", .session: "Session", .weekly: "Weekly"
    ]
}

struct UsageWidgetConfiguration: WidgetConfigurationIntent {
    static var title: LocalizedStringResource = "Subscription usage"
    static var description = IntentDescription("Show the subscription allowance remaining.")
    @Parameter(title: "Codex", default: .auto) var codexPeriod: UsagePeriod
    @Parameter(title: "Claude", default: .auto) var claudePeriod: UsagePeriod
}

struct SubscriptionUsageEntry: TimelineEntry {
    let date: Date
    let json: String
    let configuration: UsageWidgetConfiguration
}

struct SubscriptionUsageTimeline: AppIntentTimelineProvider {
    func placeholder(in _: Context) -> SubscriptionUsageEntry {
        SubscriptionUsageEntry(date: Date(), json: "{\"entries\":[]}", configuration: UsageWidgetConfiguration())
    }

    func snapshot(for configuration: UsageWidgetConfiguration, in _: Context) async -> SubscriptionUsageEntry {
        SubscriptionUsageEntry(date: Date(), json: read(), configuration: configuration)
    }

    func timeline(for configuration: UsageWidgetConfiguration,
                  in _: Context) async -> Timeline<SubscriptionUsageEntry> {
        let now = Date()
        let json = read()
        let stored = json.data(using: .utf8).flatMap { try? JSONDecoder().decode(UsageWidgetData.self, from: $0) }
        var dates = [now]
        dates
            .append(contentsOf: (stored?.entries ?? []).map { Date(timeIntervalSince1970: Double($0.date) / 1000) }
                .filter { $0 > now })
        return Timeline(entries: dates.map {
            SubscriptionUsageEntry(date: $0, json: json, configuration: configuration)
        }, policy: .never)
    }

    private func read() -> String {
        guard let path = UsageWidgetStorage.file,
              let data = try? Data(contentsOf: path), data.count <= 256 * 1024,
              let json = String(data: data, encoding: .utf8) else { return "{\"entries\":[]}" }
        return json
    }
}

struct SubscriptionUsageView: View {
    let entry: SubscriptionUsageEntry
    @Environment(\.widgetFamily) private var family
    @Environment(\.widgetRenderingMode) private var renderingMode
    @Environment(\.isLuminanceReduced) private var reduced

    private var data: UsageWidgetEntryData? {
        let json = subscriptionWidgetEntryJson(
            json: entry.json,
            nowMs: Int64(max(Date().timeIntervalSince1970, entry.date.timeIntervalSince1970) * 1000),
            layout: String(describing: family),
            codexPeriod: entry.configuration.codexPeriod.rawValue,
            claudePeriod: entry.configuration.claudePeriod.rawValue
        )
        return json.data(using: .utf8).flatMap { try? JSONDecoder().decode(UsageWidgetEntryData.self, from: $0) }
    }

    var body: some View {
        let current = data
        VStack(alignment: .leading, spacing: family == .accessoryRectangular ? 2 : 10) {
            if current?.providers.isEmpty != false {
                Text("Tap to connect").font(.caption).foregroundStyle(.secondary)
            }
            ForEach(current?.providers ?? [], id: \.name) { provider in
                VStack(alignment: .leading, spacing: 3) {
                    Text(provider.name).font(.caption.weight(.semibold)).lineLimit(1)
                    if provider.windows.isEmpty {
                        Text(provider.detail).font(.caption2).foregroundStyle(.secondary).lineLimit(1)
                    }
                    ForEach(provider.windows, id: \.self) { window in
                        HStack {
                            Text(window.label).lineLimit(1)
                            Spacer(minLength: 4)
                            Text("\(window.remaining)% left").layoutPriority(1)
                        }.font(.caption2)
                        ProgressView(value: Double(window.remaining), total: 100)
                            .tint(renderingMode != .fullColor || reduced ? .primary : provider.name == "Claude" ? Color(
                                red: 0.85,
                                green: 0.47,
                                blue: 0.34
                            ) : .gray)
                        if family != .accessoryRectangular {
                            if let reset = window.resetAt {
                                Text(
                                    "Next reset \(Date(timeIntervalSince1970: Double(reset) / 1000).formatted(date: .abbreviated, time: .shortened))"
                                )
                                .font(.caption2).foregroundStyle(.secondary).lineLimit(1)
                            } else {
                                Text("Reset time unavailable").font(.caption2).foregroundStyle(.secondary)
                            }
                        }
                    }
                }
            }
            if family != .accessoryRectangular, let checked = current?.checkedAt, checked > 0 {
                Text(
                    "As of \(Date(timeIntervalSince1970: Double(checked) / 1000).formatted(date: .abbreviated, time: .shortened))"
                )
                .font(.caption2).foregroundStyle(.secondary).lineLimit(1)
            }
        }
        .containerBackground(.background, for: .widget)
        .widgetURL(URL(string: "remoteagent://settings/usage?tab=limits"))
    }
}

struct SubscriptionUsageWidget: Widget {
    var body: some WidgetConfiguration {
        AppIntentConfiguration(
            kind: UsageWidgetStorage.kind,
            intent: UsageWidgetConfiguration.self,
            provider: SubscriptionUsageTimeline()
        ) { entry in
            SubscriptionUsageView(entry: entry)
        }
        .configurationDisplayName("Subscription usage")
        .description("Codex and Claude subscription allowance remaining.")
        .supportedFamilies([.systemSmall, .systemMedium, .systemLarge, .systemExtraLarge, .accessoryRectangular])
    }
}
