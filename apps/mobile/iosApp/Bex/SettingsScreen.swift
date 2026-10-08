import AgentCore
import Foundation
import SwiftUI

/// Settings: connections, thread behavior, follow-ups, archive, provider
/// accounts and new-thread defaults.
struct SettingsScreen: View {
    @ObservedObject var model: BexAppViewModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
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
                            HostSettingsPage(model: model, title: "Thread behavior",
                                                     sections: ["usage-limits", "auto-settle", "behavior", "maintenance"])
                        }
                        SettingsLink(symbol: "bell", label: "Notifications") {
                            HostSettingsPage(model: model, title: "Notifications", sections: ["notifications"])
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
                    StepperRow(label: "Base size", value: baseBinding, range: 11...22)
                }
                SettingsGroup(title: "Code") {
                    ToggleRow("Custom size", isOn: codeCustomBinding)
                    if appearance.codeFontSize != nil {
                        StepperRow(label: "Code size", value: codeBinding, range: 8...18)
                    }
                    ToggleRow("Wrap long lines", isOn: wrapBinding)
                }
                SettingsGroup(title: "Terminal") {
                    ToggleRow("Custom size", isOn: terminalCustomBinding)
                    if appearance.terminalFontSize != nil {
                        Stepper(value: terminalBinding, in: 6...14, step: 0.5) {
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

private struct NativeUpdatePage: View {
    @ObservedObject var model: BexAppViewModel

    private func buildSetting(_ key: String) -> String? {
        guard let value = Bundle.main.object(forInfoDictionaryKey: key) as? String,
              !value.isEmpty,
              !value.contains("$(") else {
            return nil
        }
        return value
    }

    private var currentVersion: String {
        buildSetting("APP_UPDATE_VERSION")
            ?? buildSetting("CFBundleShortVersionString")
            ?? "0.0.0"
    }

    private var updateChannel: UpdateChannel {
        switch buildSetting("APP_RELEASE_CHANNEL") {
        case "nightly": .nightly
        case "preview": .preview
        default: .stable
        }
    }

    var body: some View {
        List {
            Section("App updates") {
                if let update = model.snapshot.nativeUpdate() {
                    Text(
                        update.updateAvailable
                            ? "Version \(update.latestVersion ?? "new") is available"
                            : update.message ?? "Up to date"
                    )
                    if update.updateAvailable,
                       let storeURL = update.storeUrl,
                       let url = URL(string: storeURL),
                       url.scheme == "https" {
                        Link("Open TestFlight", destination: url)
                    }
                } else {
                    ProgressView("Checking for updates…")
                }
            }
        }
        .scrollContentBackground(.hidden)
        .background(AppTheme.sheet)
        .navigationTitle("App updates")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear {
            model.perform(.loadNativeUpdate(request: NativeUpdateRequest(
                platform: .ios,
                currentVersion: currentVersion,
                channel: updateChannel
            )))
        }
    }
}

private struct BackgroundDiagnosticsPage: View {
    @ObservedObject var model: BexAppViewModel

    private let profiles = ["balanced", "performance", "battery-saver"]

    var body: some View {
        let rows = model.snapshot.backgroundRows()
        let selected = rows.first(where: { $0.key == "profile" })?.value.lowercased()
        let gitFetchSeconds = backgroundIntervalSeconds(rows, key: "automaticGitFetchIntervalMs")
        let providerHealthSeconds = backgroundIntervalSeconds(rows, key: "providerHealthRefreshIntervalMs")
        List {
            Section {
                Button("Refresh diagnostics") {
                    model.perform(.loadDiagnostics(traceFilePath: ""))
                }
                ForEach(rows, id: \.key) { row in
                    if row.key != "profile" &&
                        row.key != "automaticGitFetchIntervalMs" &&
                        row.key != "providerHealthRefreshIntervalMs" {
                        HStack {
                            Text(row.key)
                            Spacer()
                            Text(row.value).foregroundStyle(AppTheme.muted)
                        }
                    }
                }
            } header: {
                Text("Host policy")
            }
            Section("Profile") {
                ForEach(profiles, id: \.self) { profile in
                    Button {
                        model.perform(.setBackgroundProfile(profile: profile))
                    } label: {
                        HStack {
                            Text(profile.replacingOccurrences(of: "-", with: " ").capitalized)
                                .foregroundStyle(AppTheme.text)
                            Spacer()
                            if selected == profile { Image(systemName: "checkmark") }
                        }
                    }
                }
            }
            Section("Intervals") {
                Picker(
                    "Git fetch interval",
                    selection: Binding(get: { gitFetchSeconds }, set: { value in
                        model.perform(.setAutomaticGitFetchInterval(seconds: UInt32(value)))
                    }),
                ) {
                    ForEach([0, 15, 30, 60, 300, 900], id: \.self) { value in
                        Text(value == 0 ? "Disabled" : "\(value) seconds").tag(value)
                    }
                }
                Picker(
                    "Provider health interval",
                    selection: Binding(get: { providerHealthSeconds }, set: { value in
                        model.perform(.setProviderHealthRefreshInterval(seconds: UInt32(value)))
                    }),
                ) {
                    ForEach([0, 60, 300, 900, 1800], id: \.self) { value in
                        Text(value == 0 ? "Disabled" : "\(value) seconds").tag(value)
                    }
                }
            }
            DiagnosticRows(title: "Host resources", rows: model.snapshot.hostResourceRows())
            DiagnosticRows(title: "Processes", rows: model.snapshot.processRows())
            DiagnosticRows(title: "Process history", rows: model.snapshot.processHistoryRows())
            DiagnosticRows(title: "Traces", rows: model.snapshot.traceRows())
        }
        .scrollContentBackground(.hidden)
        .background(AppTheme.sheet)
        .navigationTitle("Background activity")
        .navigationBarTitleDisplayMode(.inline)
    }
}

private func backgroundIntervalSeconds(_ rows: [DiagnosticRow], key: String) -> Int {
    guard let value = rows.first(where: { $0.key == key })?.value,
          let milliseconds = Int(value) else {
        return 0
    }
    return max(0, milliseconds / 1_000)
}

private struct DiagnosticRows: View {
    let title: String
    let rows: [DiagnosticRow]

    var body: some View {
        Section(title) {
            ForEach(rows, id: \.key) { row in
                HStack {
                    Text(row.key)
                    Spacer()
                    Text(row.value).foregroundStyle(AppTheme.muted)
                }
            }
        }
    }
}

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
private struct KeyboardSettingsPage: View {
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
private struct HostSettingsPage: View {
    @ObservedObject var model: BexAppViewModel
    let title: String
    let sections: [String]

    var body: some View {
        let view = model.snapshot.settings(scope: .host)
        List {
            if !model.snapshot.hostSettingsLoaded() {
                ProgressView().frame(maxWidth: .infinity)
            }
            ForEach(view.sections.filter { sections.contains($0.id) }, id: \.id) { section in
                Section {
                    ForEach(section.rows, id: \.id) { row in
                        SettingRowView(model: model, row: row)
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

private struct LoadBalancingSettingsPage: View {
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

private struct SettingRowView: View {
    @ObservedObject var model: BexAppViewModel
    let row: SettingsRow

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
            }
            if let description = row.description {
                Text(description).font(AppTheme.font(13)).foregroundStyle(AppTheme.muted)
            }
        }
        .font(AppTheme.font(16))
    }

    private func apply(_ value: SettingValue) {
        if let intent = model.snapshot.settingIntent(scope: .host, id: row.id, value: value) {
            model.perform(intent)
        }
    }
}

private struct ProviderAccountsPage: View {
    @ObservedObject var model: BexAppViewModel
    @State private var code = ""

    var body: some View {
        let usageLimits = model.snapshot.usageLimits()
        List {
            Section("Provider accounts") {
                ForEach(model.snapshot.accounts()?.accounts ?? [], id: \.id) { account in
                    let limits = usageLimits.first {
                        $0.sourceAccountIds.contains(account.id)
                    }
                    VStack(alignment: .leading, spacing: 8) {
                        AccountIdentityView(account: account)
                        AccountUsageView(limits: limits) {
                            let sourceId = limits?.resetCreditAccountId ?? account.id
                            let source = model.snapshot.accounts()?.accounts.first {
                                $0.id == sourceId
                            }
                            model.perform(.consumeResetCredit(
                                provider: source?.provider ?? account.provider,
                                accountId: sourceId,
                                creditId: limits?.nextCreditId
                            ))
                            model.perform(.loadAccounts)
                        }
                        HStack {
                            Button("Select") {
                                model.perform(.selectAccount(provider: account.provider, id: account.id))
                            }
                            Button("Remove", role: .destructive) {
                                model.perform(.deleteAccount(provider: account.provider, id: account.id))
                            }
                        }
                        .buttonStyle(.borderless)
                    }
                }
                Button("Sign in to Codex") { model.perform(.startLogin(provider: .codex)) }
                Button("Sign in to Claude") { model.perform(.startLogin(provider: .claude)) }
            }
            if !model.snapshot.providerAdvisories().isEmpty {
                Section("Provider installations") {
                    ForEach(model.snapshot.providerAdvisories(), id: \.instanceId) { advisory in
                        VStack(alignment: .leading, spacing: 6) {
                            HStack {
                                Text(advisory.displayName).font(AppTheme.font(16, weight: .medium))
                                Spacer()
                                Text(advisory.status).font(.caption).foregroundStyle(.secondary)
                            }
                            if let current = advisory.currentVersion,
                               let latest = advisory.latestVersion {
                                Text("\(current) → \(latest)")
                                    .font(.caption.monospaced())
                                    .foregroundStyle(.secondary)
                            }
                            if let message = advisory.message {
                                Text(message).font(.caption).foregroundStyle(.secondary)
                            }
                            if advisory.canUpdate {
                                Button("Update provider") {
                                    model.perform(
                                        .updateProvider(instance: advisory.instanceId, targetVersion: nil)
                                    ) { result in
                                        if case .success = result {
                                            model.perform(.loadProviders)
                                        }
                                    }
                                }
                                .buttonStyle(.borderless)
                            }
                        }
                    }
                }
            }
            if let login = model.snapshot.accountLogin() {
                AccountLoginSection(
                    login: login, providerName: login.provider == .codex ? "Codex" : "Claude", loginCode: $code,
                    loginError: model.snapshot.accounts()?.error,
                    submit: {
                        model.perform(.completeLogin(provider: login.provider, id: login.loginId, code: code))
                        code = ""
                    },
                    retry: { model.perform(.loadAccounts) }
                )
                Button("Cancel sign in") {
                    model.perform(.cancelLogin(provider: login.provider, id: login.loginId))
                    code = ""
                }
            }
        }
        .scrollContentBackground(.hidden)
        .background(AppTheme.sheet)
        .navigationTitle("Provider accounts")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear {
            model.perform(.loadAccounts)
            model.perform(.loadProviders)
        }
    }
}

/// "Environments": the paired Hosts.
private struct ConnectionsScreen: View {
    @ObservedObject var model: BexAppViewModel
    let close: () -> Void
    @State private var removing: HostProfile?

    var body: some View {
        List {
            if model.profiles.isEmpty {
                Text("No environments connected yet. Tap + to add one.").foregroundStyle(AppTheme.muted)
            }
            ForEach(model.profiles) { profile in
                Button { model.selectProfile(profile.id) } label: {
                    HStack(spacing: 8) {
                        Circle().fill(statusColor(profile)).frame(width: 8, height: 8)
                        Image(systemName: "laptopcomputer").font(.system(size: 14))
                        Text(profile.name).font(AppTheme.font(16, weight: .bold))
                        Spacer()
                        if profile.id == model.selectedProfileId {
                            Image(systemName: "checkmark").foregroundStyle(AppTheme.primary)
                        }
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .swipeActions {
                    Button("Remove", role: .destructive) { removing = profile }
                }
            }
        }
        .scrollContentBackground(.hidden)
        .background(AppTheme.sheet)
        .navigationTitle("Environments")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    close()
                    model.openPairing()
                } label: { Image(systemName: "plus") }
                    .accessibilityLabel("Add environment")
            }
        }
        .alert("Remove this environment?", isPresented: Binding(
            get: { removing != nil }, set: {
                if !$0 {
                    removing = nil
                }
            }
        )) {
            if let removing {
                Button("Remove", role: .destructive) { model.removeProfile(removing.id) }
            }
            Button("Cancel", role: .cancel) { removing = nil }
        } message: {
            Text("This removes the pairing and its key from this device. Pair again to reconnect.")
        }
    }

    private func statusColor(_ profile: HostProfile) -> Color {
        guard profile.id == model.selectedProfileId else { return AppTheme.muted }
        return model.isConnected ? AppTheme.emeraldIcon : model.isConnecting ? AppTheme.amber : AppTheme.rose
    }
}
