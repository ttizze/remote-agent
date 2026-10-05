import AgentCore
import Foundation
import SwiftUI

extension BexAppViewModel {
    var accounts: [Account] {
        snapshot.accounts()?.accounts ?? []
    }

    var accountError: String? {
        snapshot.accounts()?.error ?? snapshot.error()
    }

    var selectedModel: ModelRef? {
        snapshot.draft(key: coreDraftKey).model
    }

    var currentModel: Model? {
        models.first { $0.model == selectedModel }
    }

    func chooseModel(_ value: ModelRef) {
        perform(.selectModel(threadId: coreDraftKey, model: value))
    }
}

struct ModelComposerControls: View {
    let controls: ModelQuickControls
    let modelName: String
    let disabled: Bool
    let open: () -> Void
    let select: (String, ModelOptionValue) -> Void

    var body: some View {
        if let next = controls.toggleFastTo, let id = controls.fastOptionId {
            Button { select(id, next) } label: {
                Image(systemName: controls.fast ? "bolt.fill" : "bolt")
                    .foregroundStyle(controls.fast ? Color.accentColor : .secondary)
                    .frame(width: 44, height: 44)
            }
            .accessibilityLabel("Fast")
            .accessibilityValue(controls.fast ? "オン" : "オフ")
            .accessibilityIdentifier("model.fast")
            .disabled(disabled)
        }
        Button(action: open) {
            Text(modelName)
                .foregroundStyle(.primary)
                .font(.subheadline).lineLimit(1).truncationMode(.middle)
                .frame(minWidth: 44, minHeight: 44)
                .contentShape(Rectangle())
        }
        .layoutPriority(1)
        .accessibilityLabel("モデル設定")
        .accessibilityValue(modelName)
        .accessibilityIdentifier("model.settings")
        if let effort = controls.effort {
            Menu {
                ForEach(Array(effort.choices.enumerated()), id: \.offset) { _, choice in
                    if case let .string(value) = choice.value {
                        Button { select(effort.id, choice.value) } label: {
                            if choice.value == effort.value {
                                Label(choice.label, systemImage: "checkmark")
                            } else {
                                Text(choice.label)
                            }
                        }
                        .accessibilityIdentifier("model.effort." + value)
                    }
                }
            } label: {
                ReasoningStrengthIcon(level: controls.effortLevel, count: effort.choices.count)
                    .frame(width: 44, height: 44)
            }
            .accessibilityLabel(effort.label)
            .accessibilityValue(effort.valueLabel ?? "未設定")
            .accessibilityHint(effort.disabledReason ?? "")
            .accessibilityIdentifier("model.effort")
            .disabled(disabled || effort.disabledReason != nil)
        }
    }
}
