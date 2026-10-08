import AgentCore
import Foundation
import SwiftUI

/// Settings controls and Host settings rows.
struct SettingsGroup<Content: View>: View {
    let title: String
    @ViewBuilder let content: () -> Content

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title).font(AppTheme.font(14, weight: .medium)).foregroundStyle(AppTheme.muted)
                .padding(.horizontal, 7)
            VStack(spacing: 0) { content() }
                .background(AppTheme.groupedCard, in: RoundedRectangle(cornerRadius: 24))
        }
    }
}

struct SettingsLink<Destination: View>: View {
    let symbol: String
    let label: String
    var value: String?
    @ViewBuilder let destination: () -> Destination

    var body: some View {
        NavigationLink(destination: destination) {
            HStack(spacing: 14) {
                Image(systemName: symbol).font(.system(size: 20)).frame(width: 24).foregroundStyle(AppTheme.text)
                Text(label).font(AppTheme.font(18)).foregroundStyle(AppTheme.text)
                Spacer()
                if let value {
                    Text(value).font(AppTheme.font(16)).foregroundStyle(AppTheme.muted).frame(maxWidth: 180)
                }
                Image(systemName: "chevron.right").font(.system(size: 16, weight: .semibold))
                    .foregroundStyle(AppTheme.muted)
            }
            .padding(16)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

/// What Return does in the composer on a hardware keyboard.
enum ComposerEnterBehavior: String, CaseIterable {
    /// Return sends; Shift-Return inserts a new line.
    case send
    /// Return inserts a new line; Command-Return sends.
    case newline

    static let storageKey = "composer.enterBehavior"

    var label: String {
        switch self {
        case .send: "Send message"
        case .newline: "Insert new line"
        }
    }

    var detail: String {
        switch self {
        case .send: "Return sends the message. Shift-Return inserts a new line."
        case .newline: "Return inserts a new line. Command-Return sends the message."
        }
    }
}

/// "Keyboard": the Return key's behavior with a hardware keyboard.
struct KeyboardSettingsPage: View {
    @AppStorage(ComposerEnterBehavior.storageKey) private var behavior = ComposerEnterBehavior.send

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                SettingsGroup(title: "Return key") {
                    ForEach(Array(ComposerEnterBehavior.allCases.enumerated()), id: \.element) { index, option in
                        Button { behavior = option } label: {
                            HStack(spacing: 16) {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(option.label).font(AppTheme.font(18)).foregroundStyle(AppTheme.text)
                                    Text(option.detail).font(AppTheme.font(14)).foregroundStyle(AppTheme.muted)
                                }
                                .frame(maxWidth: .infinity, alignment: .leading)
                                if behavior == option {
                                    Image(systemName: "checkmark").font(.system(size: 18, weight: .semibold))
                                        .foregroundStyle(AppTheme.text)
                                }
                            }
                            .padding(16)
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityAddTraits(behavior == option ? [.isSelected] : [])
                        .overlay(alignment: .top) {
                            if index > 0 {
                                Rectangle().fill(AppTheme.borderSubtle).frame(height: 1)
                            }
                        }
                    }
                }
                Text("Applies to the composer when a hardware keyboard is connected.")
                    .font(AppTheme.font(14)).foregroundStyle(AppTheme.muted).padding(.horizontal, 8)
            }
            .padding(.horizontal, 20).padding(.top, 16)
        }
        .background(AppTheme.sheet.ignoresSafeArea())
        .navigationTitle("Keyboard")
        .navigationBarTitleDisplayMode(.inline)
    }
}

/// Rows of the conversation settings sections core builds.
struct HostSettingsPage: View {
    @ObservedObject var model: BexAppViewModel
    let title: String
    let sections: [String]
    let projectId: String?

    init(model: BexAppViewModel, title: String, sections: [String], projectId: String? = nil) {
        self.model = model
        self.title = title
        self.sections = sections
        self.projectId = projectId
    }

    var body: some View {
        let scope: SettingsScope = projectId.map { .project(projectId: $0) } ?? .host
        let view = model.snapshot.settings(scope: scope)
        List {
            if !model.snapshot.hostSettingsLoaded() {
                ProgressView().frame(maxWidth: .infinity)
            }
            if let project = view.project {
                HStack(spacing: 12) {
                    ProjectGlyph(
                        name: project.label,
                        icon: ProjectIconImages.image(model.snapshot, project.projectId),
                        size: 32
                    )
                    VStack(alignment: .leading, spacing: 2) {
                        Text(project.label)
                            .font(AppTheme.font(17))
                            .fontWeight(.semibold)
                        Text("Project settings")
                            .font(AppTheme.font(12))
                            .foregroundStyle(AppTheme.muted)
                    }
                }
                .padding(.vertical, 4)
            }
            if let projectId, view.project?.hasOverrides == true {
                Button("Reset project overrides", role: .destructive) {
                    model.perform(.resetProjectSettings(projectId: projectId))
                }
            }
            ForEach(view.sections.filter { sections.contains($0.id) }, id: \.id) { section in
                Section {
                    ForEach(section.rows, id: \.id) { row in
                        SettingRowView(model: model, row: row, scope: scope)
                    }
                } header: {
                    Text(section.title)
                } footer: {
                    if let footer = section.footer {
                        Text(footer)
                    }
                }
            }
        }
        .scrollContentBackground(.hidden)
        .background(AppTheme.sheet)
        .navigationTitle(title)
        .navigationBarTitleDisplayMode(.inline)
    }
}

struct LoadBalancingSettingsPage: View {
    @ObservedObject var model: BexAppViewModel

    var body: some View {
        let rows = model.loadBalancingPreferences()
        List {
            Section {
                Toggle("Automatic routing", isOn: Binding(
                    get: { model.snapshot.preferences().loadBalancingEnabled },
                    set: { model.setLoadBalancingEnabled($0) }
                ))
                Text("Choose a connected environment for matching new threads.")
                    .font(AppTheme.font(13)).foregroundStyle(AppTheme.muted)
            }
            if rows.count < 2 {
                Section {
                    Text("Connect at least two environments to choose weights.")
                        .foregroundStyle(AppTheme.muted)
                }
            } else {
                Section("Environment weights") {
                    ForEach(rows, id: \.environmentId) { row in
                        VStack(alignment: .leading, spacing: 6) {
                            HStack {
                                Text(row.environmentLabel)
                                Spacer()
                                Text(row.connectionState).foregroundStyle(AppTheme.muted)
                            }
                            Picker("Weight", selection: Binding(
                                get: { row.weight },
                                set: { model.setLoadBalancingWeight(environmentId: row.environmentId, weight: $0) }
                            )) {
                                Text("Prefer").tag(UInt8(100))
                                Text("Normal").tag(UInt8(50))
                                Text("Less often").tag(UInt8(25))
                                Text("Manual only").tag(UInt8(0))
                            }
                            .pickerStyle(.menu)
                            .disabled(!model.snapshot.preferences().loadBalancingEnabled)
                        }
                    }
                }
            }
        }
        .scrollContentBackground(.hidden)
        .background(AppTheme.sheet)
        .navigationTitle("Load balancing")
        .navigationBarTitleDisplayMode(.inline)
    }
}

struct SettingRowView: View {
    @ObservedObject var model: BexAppViewModel
    let row: SettingsRow
    let scope: SettingsScope
    @State private var folderPickerPresented = false

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            switch row.control {
            case let .switch(isOn):
                Toggle(row.title, isOn: Binding(get: { isOn }, set: { apply(.switch(on: $0)) }))
            case let .choice(choices, selected):
                Text(row.title).font(AppTheme.font(16, weight: .medium))
                ForEach(choices, id: \.id) { choice in
                    Button { apply(.choice(id: choice.id)) } label: {
                        HStack {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(choice.label).foregroundStyle(AppTheme.text)
                                if let description = choice.description {
                                    Text(description).font(AppTheme.font(13)).foregroundStyle(AppTheme.muted)
                                }
                            }
                            Spacer()
                            if choice.id == selected {
                                Image(systemName: "checkmark").foregroundStyle(AppTheme.primary)
                            }
                        }
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                }
            case let .number(value, min, max):
                Stepper("\(row.title): \(value)", value: Binding(get: { Int(value) }, set: {
                    apply(.number(value: UInt32($0)))
                }), in: Int(min) ... Int(max))
            case let .model(modelLabel, traitsLabel):
                HStack {
                    Text(row.title)
                    Spacer()
                    Text([modelLabel, traitsLabel].compactMap(\.self).joined(separator: " · "))
                        .foregroundStyle(AppTheme.muted)
                }
            case let .text(value, placeholder):
                VStack(alignment: .leading, spacing: 8) {
                    TextField(placeholder ?? row.title, text: Binding(
                        get: { value },
                        set: { apply(.text(value: $0)) }
                    ))
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    if row.id == .addProjectBaseDirectory {
                        Button("Choose folder") { folderPickerPresented = true }
                            .buttonStyle(.borderless)
                    }
                }
            case let .browserProfiles(profiles, defaultProfileId):
                BrowserProfilesView(
                    model: model,
                    profiles: profiles,
                    defaultProfileId: defaultProfileId,
                    onDefault: { apply(.choice(id: $0)) },
                    onRemove: model.removeBrowserProfile
                )
            }
            if row.resettable, let intent = model.snapshot.settingReset(scope: scope, row: row) {
                Button("Reset") {
                    model.perform(intent)
                }
                .font(AppTheme.font(13))
                .foregroundStyle(AppTheme.primary)
            }
            if let description = row.description {
                Text(description).font(AppTheme.font(13)).foregroundStyle(AppTheme.muted)
            }
            if let source = row.source {
                Text(source == .project ? "Project override" : "Inherited from Host")
                    .font(AppTheme.font(12)).foregroundStyle(AppTheme.primary)
            }
        }
        .font(AppTheme.font(16))
        .sheet(isPresented: $folderPickerPresented) {
            RemoteFolderPicker(model: model, initialPath: valueForFolderPicker) { path in
                apply(.text(value: path))
                folderPickerPresented = false
            }
        }
    }

    private var valueForFolderPicker: String? {
        guard row.id == .addProjectBaseDirectory,
              case let .text(value, _) = row.control else { return nil }
        return value
    }

    func apply(_ value: SettingValue) {
        if let intent = model.snapshot.settingIntent(scope: scope, id: row.id, value: value) {
            model.perform(intent)
        }
    }
}
