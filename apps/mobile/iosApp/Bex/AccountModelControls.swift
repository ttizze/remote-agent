import AgentCore
import SwiftUI

struct AccountModelControls: View {
    let choices: [Model]
    let currentModel: Model?
    @Binding var selectedModel: String
    @Binding var selectedEffort: String
    @Binding var selectedServiceTier: String

    var body: some View {
        HStack {
            Text("モデル")
            Spacer()
            Menu {
                ForEach(choices, id: \.id) { choice in
                    Button { selectedModel = choice.model } label: {
                        if selectedModel == choice.model {
                            Label(choice.displayName, systemImage: "checkmark")
                        } else {
                            Text(choice.displayName)
                        }
                    }
                    .accessibilityIdentifier("model.choice." + choice.id)
                }
            } label: {
                HStack {
                    Text(currentModel?.displayName ?? "モデルを選択")
                    Image(systemName: "chevron.down").font(.caption)
                }
            }
            .buttonStyle(.borderless)
            .accessibilityIdentifier("model.choice.menu")
            .accessibilityValue(currentModel?.displayName ?? "モデルを選択")
        }
        if let current = currentModel, !current.supportedReasoningEfforts.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                Text("推論の強度").font(.caption).foregroundColor(.secondary)
                Picker("推論の強度", selection: Binding(
                    get: { selectedEffort.isEmpty ? current.defaultReasoningEffort : selectedEffort
                    },
                    set: { selectedEffort = $0 }
                )) {
                    ForEach(current.supportedReasoningEfforts, id: \.reasoningEffort) {
                        Text($0.reasoningEffort).tag($0.reasoningEffort)
                    }
                }
                .pickerStyle(.segmented)
                .accessibilityIdentifier("model.quick.effort")
            }
        }
        if let tiers = currentModel?.serviceTiers, !tiers.isEmpty {
            Picker(
                "速度",
                selection: $selectedServiceTier
            ) {
                Text("標準").tag("default")
                ForEach(tiers.filter { $0.id != "default" }, id: \.id) {
                    Text($0.name ?? $0.id).tag($0.id)
                }
            }
            .pickerStyle(.menu)
            .accessibilityIdentifier("model.service-tier")
        }
    }
}

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
                    VStack(alignment: .leading, spacing: 5) {
                        HStack {
                            Text(window.label)
                            Spacer()
                            Text("残り \(window.remainingPercent)%").monospacedDigit()
                        }.font(.caption)
                        ProgressView(value: Double(window.remainingPercent), total: 100)
                            .tint(window.remainingPercent <= 20 ? .orange : .green)
                    }
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
