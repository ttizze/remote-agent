import AgentCore
import SwiftUI

struct NewChatDefaultModelMenu: View {
    let selected: ModelRef?
    let choices: [AgentCore.Model]
    let disabled: Bool
    let select: (ModelRef?) -> Void

    var body: some View {
        let label = choices.first { $0.model == selected }?.displayName ?? selected?.id ?? "自動"
        Menu {
            Button("自動（Codexのデフォルト）") { select(nil) }
                .accessibilityIdentifier("model.defaults.new-chat.automatic")
            ForEach(choices, id: \.model) { choice in
                Button { select(choice.model) } label: {
                    let name = (choice.model.provider == .codex ? "Codex" : "Claude") + " · " + choice.displayName
                    if selected == choice.model {
                        Label(name, systemImage: "checkmark")
                    } else {
                        Text(name)
                    }
                }
                .accessibilityIdentifier("model.defaults.new-chat." + choice.id)
            }
        } label: {
            HStack {
                Text("新規チャットのモデル")
                Spacer()
                Text(label).lineLimit(1).foregroundStyle(.secondary)
                Image(systemName: "chevron.down").font(.caption)
            }
            .frame(minHeight: 44).padding(.horizontal, 32)
        }
        .accessibilityIdentifier("model.defaults.new-chat")
        .accessibilityValue(label)
        .disabled(disabled)
    }
}

struct ModelDefaultsScopeBar: View {
    let projects: [ModelScopeChoice]
    let environments: [ModelScopeChoice]
    let selected: ModelDefaultsScope
    let disabled: Bool
    let select: (ModelDefaultsScope) -> Void

    var body: some View {
        SettingsScopeBar {
            menu(projects, fallback: "すべてのプロジェクト", id: "settings.scope.projects")
        } environment: {
            menu(environments, fallback: "この環境", id: "settings.scope.environment")
        }
        .disabled(disabled)
    }

    private func menu(_ choices: [ModelScopeChoice], fallback: String, id: String) -> some View {
        Menu {
            ForEach(choices, id: \.id) { choice in
                Button { select(choice.scope) } label: {
                    if choice.scope == selected {
                        Label(choice.label, systemImage: "checkmark")
                    } else {
                        Text(choice.label)
                    }
                }
                .accessibilityIdentifier(id + "." + choice.id)
            }
        } label: {
            HStack(spacing: 4) {
                Text(choices.first { $0.scope == selected }?.label ?? fallback).lineLimit(1)
                Image(systemName: "chevron.down")
            }
        }
        .accessibilityIdentifier(id)
    }
}
