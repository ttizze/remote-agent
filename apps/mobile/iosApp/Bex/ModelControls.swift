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
        if !controls.efforts.isEmpty, let id = controls.effortOptionId {
            Menu {
                ForEach(controls.efforts, id: \.self) { effort in
                    Button { select(id, .string(effort)) } label: {
                        if effort == controls.effort {
                            Label(effort, systemImage: "checkmark")
                        } else {
                            Text(effort)
                        }
                    }
                    .accessibilityIdentifier("model.effort." + effort)
                }
            } label: {
                ReasoningStrengthIcon(level: controls.effortLevel, count: controls.efforts.count)
                    .frame(width: 44, height: 44)
            }
            .accessibilityLabel("推論の強度")
            .accessibilityValue(controls.effort)
            .accessibilityIdentifier("model.effort")
            .disabled(disabled)
        }
    }
}
