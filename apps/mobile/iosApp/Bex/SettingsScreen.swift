import AgentCore
import Foundation
import SwiftUI

/// Settings: connections, thread behavior, follow-ups, archive, provider
/// accounts and new-thread defaults.
struct SettingsScreen: View {
    @ObservedObject var model: BexAppViewModel
    var projectId: String?
    @Environment(\.dismiss) private var dismiss

    init(model: BexAppViewModel, projectId: String? = nil) {
        self.model = model
        self.projectId = projectId
    }

    var body: some View {
        NavigationStack {
            if let projectId {
                HostSettingsPage(
                    model: model,
                    title: "Project settings",
                    sections: ["behavior", "auto-settle", "new-threads", "source-control", "agent", "maintenance"],
                    projectId: projectId
                )
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 20) {
                        SettingsGroup(title: "Connections") {
                            SettingsLink(symbol: "point.3.connected.trianglepath.dotted", label: "Environments",
                                         value: "\(model.environmentSettings().count)") {
                                ConnectionsScreen(model: model, close: { dismiss() })
                            }
                        }
                        SettingsGroup(title: "Interface") {
                            SettingsLink(symbol: "paintbrush", label: "Appearance") { MobileAppearancePage() }
                            SettingsLink(symbol: "keyboard", label: "Keyboard") { KeyboardSettingsPage() }
                        }
                        SettingsGroup(title: "Projects & threads") {
                            SettingsLink(symbol: "text.bubble", label: "Thread behavior") {
                                HostSettingsPage(
                                    model: model,
                                    title: "Thread behavior",
                                    sections: ["usage-limits", "auto-settle", "behavior", "maintenance"]
                                )
                            }
                            SettingsLink(symbol: "bell", label: "Notifications") {
                                HostSettingsPage(model: model, title: "Notifications", sections: ["notifications"])
                            }
                            SettingsLink(symbol: "globe", label: "Browser") {
                                HostSettingsPage(model: model, title: "Browser", sections: ["browser"])
                            }
                            SettingsLink(symbol: "arrow.turn.left.up", label: "Follow-ups") {
                                HostSettingsPage(model: model, title: "Follow-ups", sections: ["follow-ups"])
                            }
                            SettingsLink(symbol: "archivebox", label: "Archived Threads") {
                                ArchivedScreen(model: model)
                            }
                        }
                        SettingsGroup(title: "Server settings") {
                            SettingsLink(symbol: "calendar.badge.clock", label: "Scheduled tasks") {
                                ScheduledTasksScreen(model: model)
                            }
                            SettingsLink(symbol: "chart.bar.xaxis", label: "Usage") {
                                UsageScreen(model: model)
                            }
                            SettingsLink(symbol: "person.crop.circle", label: "Provider accounts") {
                                ProviderAccountsPage(model: model)
                            }
                            SettingsLink(symbol: "arrow.triangle.2.circlepath", label: "Load balancing") {
                                LoadBalancingSettingsPage(model: model)
                            }
                            SettingsLink(symbol: "plus.bubble", label: "New threads") {
                                HostSettingsPage(model: model, title: "New threads", sections: ["new-threads"])
                            }
                            SettingsLink(symbol: "gearshape.2", label: "Agent") {
                                HostSettingsPage(model: model, title: "Agent", sections: ["agent"])
                            }
                            SettingsLink(symbol: "arrow.triangle.branch", label: "Source control") {
                                HostSettingsPage(model: model, title: "Source control", sections: ["source-control"])
                            }
                            SettingsLink(symbol: "arrow.triangle.branch", label: "Worktrees") {
                                WorktreeSettingsScreen(model: model).id(model.selectedProfileId)
                            }
                            SettingsLink(symbol: "internaldrive", label: "Storage") {
                                HostSettingsPage(model: model, title: "Storage", sections: ["storage"])
                            }
                            SettingsLink(symbol: "waveform.path.ecg", label: "Background activity") {
                                BackgroundDiagnosticsPage(model: model)
                            }
                        }
                        SettingsGroup(title: "App") {
                            SettingsLink(symbol: "arrow.down.circle", label: "App updates") {
                                NativeUpdatePage(model: model)
                            }
                            PrivacyPolicyButton().font(AppTheme.font(18)).padding(16)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        }
                    }
                    .padding(.horizontal, 16).padding(.vertical, 12)
                }
            }
            .background(AppTheme.sheet.ignoresSafeArea())
            .navigationTitle("Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
            .onAppear {
                model.perform(.loadSettings)
                model.perform(.loadWorktreeSettings)
                model.perform(.loadBackgroundPolicy)
            }
        }
        .tint(AppTheme.color("mobilePrimaryText"))
    }
}

/// Mobile color scheme, theme and independent text/code/terminal controls.
private struct MobileAppearancePage: View {
    @State private var appearance = MobileAppearanceState.load()

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                SettingsGroup(title: "Color scheme") {
                    ForEach(MobileAppearanceState.ColorScheme.allCases, id: \.self) { scheme in
                        Button { update { $0.colorScheme = scheme } } label: {
                            HStack {
                                Text(scheme.label).font(AppTheme.font(18)).foregroundStyle(AppTheme.text)
                                Spacer()
                                if appearance.colorScheme == scheme {
                                    Image(systemName: "checkmark").foregroundStyle(AppTheme.primary)
                                }
                            }
                            .padding(16)
                        }
                        .buttonStyle(.plain)
                    }
                }
                SettingsGroup(title: "Themes") {
                    sharedThemePicker
                    Divider().overlay(AppTheme.borderSubtle)
                    themePicker("Light theme", dark: false)
                    Divider().overlay(AppTheme.borderSubtle)
                    themePicker("Dark theme", dark: true)
                }
                SettingsGroup(title: "Text") {
                    StepperRow(label: "Base size", value: baseBinding, range: 11 ... 22)
                }
                SettingsGroup(title: "Code") {
                    ToggleRow("Custom size", isOn: codeCustomBinding)
                    if appearance.codeFontSize != nil {
                        StepperRow(label: "Code size", value: codeBinding, range: 8 ... 18)
                    }
                    ToggleRow("Wrap long lines", isOn: wrapBinding)
                }
                SettingsGroup(title: "Terminal") {
                    ToggleRow("Custom size", isOn: terminalCustomBinding)
                    if appearance.terminalFontSize != nil {
                        Stepper(value: terminalBinding, in: 6 ... 14, step: 0.5) {
                            HStack {
                                Text("Terminal size").font(AppTheme.font(18)).foregroundStyle(AppTheme.text)
                                Spacer()
                                Text(String(format: "%.1f", appearance.terminalFontSize ?? 10.5))
                                    .font(AppTheme.mono(14)).foregroundStyle(AppTheme.muted)
                            }
                        }
                        .padding(16)
                    }
                }
            }
            .padding(.horizontal, 20).padding(.vertical, 16)
        }
        .background(AppTheme.sheet.ignoresSafeArea())
        .navigationTitle("Appearance")
        .navigationBarTitleDisplayMode(.inline)
    }

    private func themePicker(_ title: String, dark: Bool) -> some View {
        Picker(title, selection: Binding(
            get: { (dark ? appearance.darkTheme : appearance.lightTheme) ?? "" },
            set: { value in
                appearance = appearance.assigningTheme(value.isEmpty ? nil : value, dark: dark)
                AppTheme.update(appearance)
            }
        )) {
            ForEach(MobileAppearanceState.themes, id: \.label) { theme in
                Text(theme.label).tag(theme.id ?? "")
            }
        }
        .font(AppTheme.font(18))
        .padding(16)
    }

    private var sharedThemePicker: some View {
        Picker("Both appearances", selection: Binding(
            get: { appearance.theme ?? "" },
            set: { value in
                update {
                    $0.theme = value.isEmpty ? nil : value
                    $0.lightTheme = nil
                    $0.darkTheme = nil
                }
            }
        )) {
            ForEach(MobileAppearanceState.themes, id: \.label) { theme in
                Text(theme.label).tag(theme.id ?? "")
            }
        }
        .font(AppTheme.font(18))
        .padding(16)
    }

    private var baseBinding: Binding<Int> {
        Binding(get: { appearance.baseFontSize }, set: { value in update { $0.baseFontSize = value } })
    }

    private var codeBinding: Binding<Int> {
        Binding(get: { appearance.codeFontSize ?? 12 }, set: { value in update { $0.codeFontSize = value } })
    }

    private var terminalBinding: Binding<Double> {
        Binding(get: { appearance.terminalFontSize ?? 10.5 }, set: { value in update { $0.terminalFontSize = value } })
    }

    private var codeCustomBinding: Binding<Bool> {
        Binding(get: { appearance.codeFontSize != nil }, set: { enabled in
            update { $0.codeFontSize = enabled ? ($0.codeFontSize ?? 12) : nil }
        })
    }

    private var terminalCustomBinding: Binding<Bool> {
        Binding(get: { appearance.terminalFontSize != nil }, set: { enabled in
            update { $0.terminalFontSize = enabled ? ($0.terminalFontSize ?? 10.5) : nil }
        })
    }

    private var wrapBinding: Binding<Bool> {
        Binding(get: { appearance.codeWordWrap }, set: { value in update { $0.codeWordWrap = value } })
    }

    private func update(_ edit: (inout MobileAppearanceState) -> Void) {
        var next = appearance
        edit(&next)
        appearance = next.normalized()
        AppTheme.update(appearance)
    }
}

private struct StepperRow: View {
    let label: String
    @Binding var value: Int
    let range: ClosedRange<Int>

    var body: some View {
        Stepper(value: $value, in: range) {
            HStack {
                Text(label).font(AppTheme.font(18)).foregroundStyle(AppTheme.text)
                Spacer()
                Text("\(value)").font(AppTheme.mono(14)).foregroundStyle(AppTheme.muted)
            }
        }
        .padding(16)
    }
}

private struct ToggleRow: View {
    let title: String
    @Binding var isOn: Bool

    init(_ title: String, isOn: Binding<Bool>) {
        self.title = title
        _isOn = isOn
    }

    var body: some View {
        Toggle(title, isOn: $isOn).font(AppTheme.font(18)).padding(16)
    }
}
