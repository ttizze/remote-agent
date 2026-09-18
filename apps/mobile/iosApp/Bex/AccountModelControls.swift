import AgentCore
import SwiftUI

struct AccountModelControls: View {
    let choices: [Model]
    let currentModel: Model?
    @Binding var selectedModel: String
    @Binding var selectedEffort: String
    @Binding var selectedServiceTier: String

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("モデル").font(.caption).foregroundColor(.secondary)
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
                    Spacer()
                    Image(systemName: "chevron.up.chevron.down")
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
            VStack(alignment: .leading, spacing: 8) {
                Text("速度").font(.caption).foregroundColor(.secondary)
                Picker(
                    "サービス階層",
                    selection: $selectedServiceTier
                ) {
                    Text("既定").tag("")
                    ForEach(tiers, id: \.id) { Text($0.id).tag($0.id) }
                }.accessibilityIdentifier("model.service-tier")
            }
        }
    }
}
