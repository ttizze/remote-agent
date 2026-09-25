import AgentCore
import SwiftUI

struct AccountUsageView: View {
    let usage: AccountUsage?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if let usage {
                if let error = usage.error {
                    Label(accountErrorMessage(message: error), systemImage: "exclamationmark.circle")
                        .font(.caption).foregroundStyle(.orange)
                }
                ForEach(Array(usage.windows.enumerated()), id: \.offset) { _, window in
                    UsageWindowView(window: window)
                }
                if usage.windows.contains(where: { $0.resetsAt != nil }) {
                    DisclosureGroup("リセット時刻") {
                        ForEach(Array(usage.windows.enumerated()), id: \.offset) { _, window in
                            if let reset = window.resetsAt {
                                let date = Date(timeIntervalSince1970: Double(reset))
                                LabeledContent(
                                    window.label,
                                    value: date.formatted(date: .abbreviated, time: .shortened)
                                )
                            }
                        }
                    }.font(.caption)
                }
                if usage.error == nil {
                    let date = Date(timeIntervalSince1970: Double(usage.fetchedAt))
                    Text("\(date.formatted(date: .omitted, time: .shortened)) 更新")
                        .font(.caption).foregroundStyle(.secondary)
                }
            } else {
                Text("使用量を取得中…").font(.caption).foregroundStyle(.secondary)
            }
        }
    }
}

struct WeeklyUsageView: View {
    let windows: [UsageWindow]

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if windows.isEmpty {
                Text("残量未取得").font(.caption).foregroundStyle(.secondary)
            }
            ForEach(Array(windows.enumerated()), id: \.offset) { _, window in
                HStack(spacing: 12) {
                    UsageBar(remainingPercent: window.remainingPercent)
                    Text("\(window.remainingPercent)%").font(.caption).monospacedDigit()
                        .foregroundStyle(.secondary)
                }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel("週間残量 \(window.remainingPercent)%")
            }
        }
    }
}

struct ReasoningStrengthIcon: View {
    let level: UInt32
    let count: Int

    var body: some View {
        HStack(alignment: .bottom, spacing: 2) {
            ForEach(0 ..< count, id: \.self) { index in
                RoundedRectangle(cornerRadius: 1)
                    .fill(index < Int(level) ? Color.accentColor : Color.secondary.opacity(0.35))
                    .frame(width: 3, height: 5 + CGFloat(index) * 2.5)
            }
        }
        .frame(width: 28, height: 22)
        .accessibilityHidden(true)
    }
}

private struct UsageWindowView: View {
    let window: UsageWindow

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack {
                Text(window.label)
                Spacer()
                Text("残り \(window.remainingPercent)%").monospacedDigit()
            }.font(.caption)
            UsageBar(remainingPercent: window.remainingPercent)
        }
    }
}

private struct UsageBar: View {
    let remainingPercent: UInt32

    var body: some View {
        ProgressView(value: Double(remainingPercent), total: 100)
            .tint(remainingPercent <= 20 ? .orange : .green)
    }
}
