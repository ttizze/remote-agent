import AgentCore
import SwiftUI

/// "Thread settings": the model catalog, its options and the runtime mode of
/// the draft the composer edits.
struct ThreadSettingsSheet: View {
    @ObservedObject var model: BexAppViewModel
    let controls: ComposerControls
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""
    @State private var filter = CatalogFilter.all
    @State private var showLegacy = false
    /// Providers whose section was opened or closed since the sheet opened.
    @State private var expansionOverrides: [String] = []
    /// The model Save applies.
    @State private var staged: StagedModel?
    @State private var unavailable = false

    var body: some View {
        let catalog = model.snapshot.catalogSheet(options: CatalogSheetOptions(
            filter: filter, showLegacy: showLegacy, query: query,
            expansionOverrides: expansionOverrides, stagedKey: staged.map { stagedModelKey(staged: $0) }
        ))
        NavigationStack {
            List {
                ForEach(sections(catalog.items), id: \.key) { section in
                    Section {
                        ForEach(section.models, id: \.key) { row in
                            CatalogModelRow(row: row) { press(row) } star: {
                                Haptics.selection()
                                model.perform(.toggleFavoriteModel(instanceId: row.instanceId, model: row.slug))
                            }
                        }
                    } header: {
                        if let header = section.header {
                            ProviderSectionHeader(header: header) { toggle(header.key) }
                        }
                    }
                }
                if let emptyLabel = catalog.emptyLabel {
                    Text(emptyLabel).font(AppTheme.font(14)).foregroundStyle(AppTheme.muted)
                        .frame(maxWidth: .infinity).padding(.vertical, 56)
                        .listRowBackground(Color.clear)
                }
                OptionsSection(model: model, controls: controls, staged: $staged)
            }
            .listStyle(.insetGrouped)
            .scrollContentBackground(.hidden)
            .background(AppTheme.sheet)
            .searchable(text: $query, prompt: "Find a model")
            .navigationTitle("Thread settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .primaryAction) { filterMenu(catalog) }
                ToolbarItem(placement: .confirmationAction) {
                    Button(staged == nil ? "Done" : "Save", action: save)
                }
            }
        }
        .tint(AppTheme.color("mobilePrimaryText"))
        .alert("Model unavailable", isPresented: $unavailable) {
            Button("OK", role: .cancel) {}
        } message: {
            Text("Set up this provider on web or desktop, or select another model.")
        }
    }

    /// Each provider header with the model rows under it.
    private func sections(_ items: [CatalogSheetItem]) -> [CatalogSection] {
        var sections: [CatalogSection] = []
        for item in items {
            switch item {
            case let .provider(key, instance, collapsible, collapsed, modelCount):
                sections.append(CatalogSection(key: key, header: ProviderHeaderItem(
                    key: key, instance: instance, collapsible: collapsible, collapsed: collapsed, modelCount: modelCount
                ), models: []))
            case let .model(key, instanceId, driver, slug, label, favorite, applied, displayed, isLegacy,
                            isDefault, unavailable, _, _):
                let row = CatalogModelItem(key: key, instanceId: instanceId, driver: driver, slug: slug, label: label,
                                           favorite: favorite, applied: applied, displayed: displayed,
                                           isLegacy: isLegacy, isDefault: isDefault, unavailable: unavailable)
                if sections.isEmpty {
                    sections.append(CatalogSection(key: "models", header: nil, models: []))
                }
                sections[sections.count - 1].models.append(row)
            }
        }
        return sections
    }

    private func toggle(_ provider: String) {
        if let index = expansionOverrides.firstIndex(of: provider) {
            expansionOverrides.remove(at: index)
        } else {
            expansionOverrides.append(provider)
        }
    }

    /// Pressing the applied model drops the staged one; another model is staged.
    private func press(_ row: CatalogModelItem) {
        Haptics.selection()
        staged = model.snapshot.stageModel(current: staged,
                                           pressed: StagedModel(instanceId: row.instanceId, driver: row.driver,
                                                                model: row.slug, options: []),
                                           pressedIsApplied: row.applied)
    }

    private func filterMenu(_ catalog: CatalogSheetView) -> some View {
        Menu {
            Menu("Provider") {
                filterButton("All providers", .all)
                filterButton("Favorites", .favorites)
                ForEach(catalog.providers, id: \.instanceId) { provider in
                    filterButton(provider.displayName, .instance(instanceId: provider.instanceId))
                }
            }
            if catalog.hasLegacyModels {
                Toggle("Show legacy models", isOn: $showLegacy)
            }
        } label: {
            Image(systemName: filter == .all && !showLegacy
                ? "line.3.horizontal.decrease" : "line.3.horizontal.decrease.circle.fill")
        }
        .accessibilityLabel("Model filters")
    }

    private func filterButton(_ title: String, _ value: CatalogFilter) -> some View {
        Button { filter = value } label: {
            if filter == value {
                Label(title, systemImage: "checkmark")
            } else {
                Text(title)
            }
        }
    }

    private func save() {
        if let staged {
            Haptics.selection()
            guard model.snapshot.canSaveStagedModel(staged: staged) else {
                unavailable = true
                return
            }
            model.perform(.saveStagedModel(staged: staged))
        }
        dismiss()
    }
}

private struct ProviderHeaderItem {
    let key: String
    let instance: ProviderInstance
    let collapsible: Bool
    let collapsed: Bool
    let modelCount: UInt32
}

private struct CatalogModelItem {
    let key: String
    let instanceId: String
    let driver: Driver
    let slug: String
    let label: String
    let favorite: Bool
    let applied: Bool
    let displayed: Bool
    let isLegacy: Bool
    let isDefault: Bool
    let unavailable: Bool
}

private struct CatalogSection {
    let key: String
    let header: ProviderHeaderItem?
    var models: [CatalogModelItem]
}

/// A provider's icon and name; a collapsible one shows its count while closed.
private struct ProviderSectionHeader: View {
    let header: ProviderHeaderItem
    let toggle: () -> Void

    var body: some View {
        if header.collapsible {
            Button(action: toggle) { content.frame(minHeight: 44) }
                .buttonStyle(.plain)
                .accessibilityLabel("\(header.instance.displayName), \(header.modelCount) models")
                .accessibilityValue(header.collapsed ? "Collapsed" : "Expanded")
        } else {
            content.frame(minHeight: 36).accessibilityAddTraits(.isHeader)
        }
    }

    private var content: some View {
        HStack(spacing: 8) {
            Image(header.instance.driver.iconName).resizable().scaledToFit().frame(width: 15, height: 15)
            Text(header.instance.displayName).font(AppTheme.font(14, weight: .medium))
                .foregroundStyle(AppTheme.muted)
            if header.collapsible {
                Spacer()
                if header.collapsed {
                    Text("\(header.modelCount)").font(AppTheme.font(12, weight: .medium))
                        .foregroundStyle(AppTheme.muted)
                }
                Image(systemName: header.collapsed ? "chevron.down" : "chevron.up").font(.system(size: 12))
                    .foregroundStyle(AppTheme.color("mobileIconMuted"))
            }
        }
        .textCase(nil)
        .contentShape(Rectangle())
    }
}

private struct CatalogModelRow: View {
    let row: CatalogModelItem
    let select: () -> Void
    let star: () -> Void

    var body: some View {
        HStack(spacing: 8) {
            Button(action: select) {
                HStack(spacing: 8) {
                    Text(row.label).font(AppTheme.font(16, weight: .medium)).foregroundStyle(AppTheme.text)
                        .lineLimit(1)
                    if row.isDefault {
                        Text("Default").font(AppTheme.font(10, weight: .bold)).foregroundStyle(AppTheme.muted)
                            .padding(.horizontal, 6).padding(.vertical, 2)
                            .background(AppTheme.subtleStrong, in: RoundedRectangle(cornerRadius: 6))
                    }
                    if row.isLegacy {
                        Text("Legacy").font(AppTheme.font(10, weight: .bold)).foregroundStyle(AppTheme.muted)
                            .padding(.horizontal, 6).padding(.vertical, 2)
                            .background(AppTheme.subtle, in: RoundedRectangle(cornerRadius: 6))
                    }
                    if row.unavailable {
                        Text("Unavailable").font(AppTheme.font(12)).foregroundStyle(AppTheme.text)
                    }
                    Spacer(minLength: 0)
                    if row.displayed {
                        Image(systemName: "checkmark").font(.system(size: 16, weight: .semibold))
                            .foregroundStyle(AppTheme.color("mobileIcon"))
                    }
                }
                .frame(minHeight: 44)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(row.unavailable)
            .accessibilityAddTraits(row.displayed ? .isSelected : [])
            Button(action: star) {
                Image(systemName: row.favorite ? "star.fill" : "star").font(.system(size: 18))
                    .foregroundStyle(AppTheme.color(row.favorite ? "mobileIcon" : "mobileIconMuted"))
                    .frame(width: 44, height: 44)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("\(row.favorite ? "Remove from" : "Add to") favorites: \(row.label)")
        }
    }
}

private struct OptionsSection: View {
    @ObservedObject var model: BexAppViewModel
    let controls: ComposerControls
    @Binding var staged: StagedModel?

    private func edit(_ next: StagedModel) {
        staged = next
        model.perform(.rememberModelOptions(instanceId: next.instanceId, model: next.model, options: next.options))
    }

    var body: some View {
        let traits = staged.map { model.snapshot.stagedModelTraits(staged: $0) } ?? model.snapshot.traits()
        Section {
            ForEach(Array(traits.controls.enumerated()), id: \.offset) { _, control in
                switch control {
                case let .select(id, label, choices, selected, note, disabled):
                    NavigationLink {
                        ChoicePage(title: label, choices: choices.map {
                            Choice(id: $0.id, label: $0.label, description: $0.description)
                        }, selected: selected) { choice in
                            if let staged {
                                edit(model.snapshot.selectStagedTrait(staged: staged, descriptorId: id, choice: choice))
                            } else {
                                model.perform(.selectTrait(descriptorId: id, choice: choice))
                            }
                        }
                    } label: {
                        OptionRow(label: label, value: choices.first { $0.id == selected }?.label ?? "")
                    }
                    .disabled(disabled)
                    .accessibilityHint(note ?? "")
                case let .toggle(id, label, isOn):
                    Toggle(label, isOn: Binding(get: { isOn }, set: {
                        if let staged {
                            edit(model.snapshot.toggleStagedTrait(staged: staged, descriptorId: id, on: $0))
                        } else {
                            model.perform(.toggleTrait(descriptorId: id, on: $0))
                        }
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
                Haptics.selection()
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
        }
        .navigationTitle(title)
        .navigationBarTitleDisplayMode(.inline)
    }
}
