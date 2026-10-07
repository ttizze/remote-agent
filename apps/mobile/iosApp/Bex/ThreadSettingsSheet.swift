import AgentCore
import SwiftUI

/// "Thread settings": the model catalog, its options and the runtime mode of
/// the draft the composer edits.
struct ThreadSettingsSheet: View {
    @ObservedObject var model: BexAppViewModel
    let controls: ComposerControls
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""
    @State private var rail: PickerRail?
    @State private var staged: ModelPickerRow?

    var body: some View {
        let picker = model.snapshot.modelPicker(query: query, rail: rail)
        NavigationStack {
            List {
                ForEach(groups(picker.rows), id: \.0) { provider, rows in
                    Section {
                        ForEach(rows, id: \.key) { row in
                            ModelRowView(row: row, selected: (staged?.key ?? selectedKey(picker)) == row.key) {
                                staged = row
                            } star: {
                                model.perform(.toggleFavoriteModel(instanceId: row.instanceId, model: row.slug))
                            }
                        }
                    } header: {
                        HStack(spacing: 6) {
                            if let driver = rows.first?.driver {
                                Image(driver.iconName).resizable().scaledToFit().frame(width: 15, height: 15)
                            }
                            Text(provider).font(AppTheme.font(14, weight: .medium)).foregroundStyle(AppTheme.muted)
                        }
                    }
                }
                if picker.rows.isEmpty {
                    Text(picker.emptyLabel ?? (rail == .favorites ? "No favorite models" : "No available models"))
                        .font(AppTheme.font(14)).foregroundStyle(AppTheme.muted)
                }
                OptionsSection(model: model, controls: controls)
            }
            .listStyle(.insetGrouped)
            .scrollContentBackground(.hidden)
            .background(AppTheme.color("surfaceOverlay"))
            .searchable(text: $query, prompt: "Find a model")
            .navigationTitle("Thread settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .primaryAction) { filterMenu(picker) }
                ToolbarItem(placement: .confirmationAction) {
                    Button(staged == nil ? "Done" : "Save", action: save)
                }
            }
        }
        .tint(AppTheme.color("mobilePrimaryText"))
    }

    private func selectedKey(_ picker: ModelPickerView) -> String? {
        picker.rows.first(where: \.selected)?.key
    }

    private func groups(_ rows: [ModelPickerRow]) -> [(String, [ModelPickerRow])] {
        var order: [String] = []
        var grouped: [String: [ModelPickerRow]] = [:]
        for row in rows {
            if grouped[row.providerName] == nil {
                order.append(row.providerName)
            }
            grouped[row.providerName, default: []].append(row)
        }
        return order.map { ($0, grouped[$0] ?? []) }
    }

    private func filterMenu(_ picker: ModelPickerView) -> some View {
        Menu {
            Menu("Provider") {
                Button { rail = nil } label: {
                    Label("All providers", systemImage: rail == nil ? "checkmark" : "")
                }
                ForEach(Array(picker.rail.enumerated()), id: \.offset) { _, item in
                    Button { rail = item.rail } label: {
                        Label(item.label, systemImage: rail == item.rail ? "checkmark" : "")
                    }
                    .disabled(item.disabled)
                }
            }
        } label: {
            Image(systemName: rail == nil ? "line.3.horizontal.decrease" : "line.3.horizontal.decrease.circle.fill")
        }
        .accessibilityLabel("Model filters")
    }

    private func save() {
        if let staged {
            guard staged.disabledReason == nil else { return }
            model.perform(.setModel(instanceId: staged.instanceId, driver: staged.driver, model: staged.slug,
                                    options: []))
        }
        dismiss()
    }
}

private struct ModelRowView: View {
    let row: ModelPickerRow
    let selected: Bool
    let select: () -> Void
    let star: () -> Void

    var body: some View {
        HStack(spacing: 8) {
            Button(action: select) {
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(row.name).font(AppTheme.font(16, weight: .medium)).foregroundStyle(AppTheme.text)
                        if let reason = row.disabledReason {
                            Text(reason).font(AppTheme.font(13)).foregroundStyle(AppTheme.muted)
                        }
                    }
                    Spacer()
                    if selected {
                        Image(systemName: "checkmark").font(.system(size: 16, weight: .semibold))
                            .foregroundStyle(AppTheme.primary)
                    }
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            Button(action: star) {
                Image(systemName: row.favorite ? "star.fill" : "star").font(.system(size: 18))
                    .frame(width: 44, height: 44)
            }
            .buttonStyle(.plain)
            .foregroundStyle(row.favorite ? AppTheme.amber : AppTheme.muted)
            .accessibilityLabel(row.favorite ? "Remove from favorites" : "Add to favorites")
        }
        .frame(minHeight: 38.5)
    }
}

private struct OptionsSection: View {
    @ObservedObject var model: BexAppViewModel
    let controls: ComposerControls

    var body: some View {
        let traits = model.snapshot.traits()
        Section {
            ForEach(Array(traits.controls.enumerated()), id: \.offset) { _, control in
                switch control {
                case let .select(id, label, choices, selected, note, disabled):
                    NavigationLink {
                        ChoicePage(title: label, choices: choices.map {
                            Choice(id: $0.id, label: $0.label, description: $0.description)
                        }, selected: selected) { model.perform(.selectTrait(descriptorId: id, choice: $0)) }
                    } label: {
                        OptionRow(label: label, value: choices.first { $0.id == selected }?.label ?? "")
                    }
                    .disabled(disabled)
                    .accessibilityHint(note ?? "")
                case let .toggle(id, label, isOn):
                    Toggle(label, isOn: Binding(get: { isOn }, set: {
                        model.perform(.toggleTrait(descriptorId: id, on: $0))
                    }))
                    .font(AppTheme.font(14, weight: .medium))
                }
            }
            NavigationLink {
                ChoicePage(title: "Runtime", choices: controls.runtimeModeChoices.map {
                    Choice(id: $0.mode.identifier, label: $0.label, description: $0.description)
                }, selected: controls.runtimeMode.mode.identifier) { id in
                    if let choice = controls.runtimeModeChoices.first(where: { $0.mode.identifier == id }) {
                        model.perform(.setRuntimeMode(mode: choice.mode))
                    }
                }
            } label: {
                OptionRow(label: "Runtime", value: controls.runtimeMode.label)
            }
        } header: {
            Text("Options").font(AppTheme.font(14, weight: .medium)).foregroundStyle(AppTheme.muted)
        }
    }
}

extension RuntimeMode {
    var identifier: String {
        switch self {
        case .approvalRequired: "approval-required"
        case .autoAcceptEdits: "auto-accept-edits"
        case .auto: "auto"
        case .fullAccess: "full-access"
        }
    }
}

private struct OptionRow: View {
    let label: String
    let value: String

    var body: some View {
        HStack {
            Text(label).font(AppTheme.font(14, weight: .medium))
            Spacer()
            Text(value).font(AppTheme.font(14)).foregroundStyle(AppTheme.muted)
        }
        .frame(minHeight: 38.5)
    }
}

struct Choice {
    let id: String
    let label: String
    let description: String?
}

struct ChoicePage: View {
    let title: String
    let choices: [Choice]
    let selected: String?
    let pick: (String) -> Void
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        List(choices, id: \.id) { choice in
            Button {
                pick(choice.id)
                dismiss()
            } label: {
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(choice.label).font(AppTheme.font(16, weight: .medium)).foregroundStyle(AppTheme.text)
                        if let description = choice.description {
                            Text(description).font(AppTheme.font(14)).foregroundStyle(AppTheme.muted)
                        }
                    }
                    Spacer()
                    if choice.id == selected {
                        Image(systemName: "checkmark").foregroundStyle(AppTheme.primary)
                    }
                }
                .frame(minHeight: 49)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .sensoryFeedback(.selection, trigger: selected)
        }
        .navigationTitle(title)
        .navigationBarTitleDisplayMode(.inline)
    }
}
