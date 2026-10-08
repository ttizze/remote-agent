import AgentCore
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
                                     value: "\(model.profiles.count)") {
                            ConnectionsScreen(model: model, close: { dismiss() })
                        }
                    }
                    SettingsGroup(title: "Interface") {
                        SettingsLink(symbol: "keyboard", label: "Keyboard") { KeyboardSettingsPage() }
                    }
                    SettingsGroup(title: "Projects & threads") {
                        SettingsLink(symbol: "text.bubble", label: "Thread behavior") {
                            ConversationSettingsPage(model: model, title: "Thread behavior",
                                                     sections: ["usage-limits", "auto-settle", "behavior"])
                        }
                        SettingsLink(symbol: "arrow.turn.left.up", label: "Follow-ups") {
                            ConversationSettingsPage(model: model, title: "Follow-ups", sections: ["follow-ups"])
                        }
                        SettingsLink(symbol: "archivebox", label: "Archived Threads") {
                            ArchivedScreen(model: model)
                        }
                    }
                    SettingsGroup(title: "Server settings") {
                        SettingsLink(symbol: "person.crop.circle", label: "Provider accounts") {
                            ProviderAccountsPage(model: model)
                        }
                        SettingsLink(symbol: "plus.bubble", label: "New threads") {
                            ConversationSettingsPage(model: model, title: "New threads", sections: ["new-threads"])
                        }
                        SettingsLink(symbol: "arrow.triangle.branch", label: "Worktrees") {
                            WorktreeSettingsScreen(model: model).id(model.selectedProfileId)
                        }
                        SettingsLink(symbol: "waveform.path.ecg", label: "Background activity") {
                            BackgroundDiagnosticsPage(model: model)
                        }
                    }
                    SettingsGroup(title: "App") {
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
                model.perform(.loadConversationSettings)
                model.perform(.loadWorktreeSettings)
                model.perform(.loadBackgroundPolicy)
            }
        }
        .tint(AppTheme.color("mobilePrimaryText"))
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
private struct ConversationSettingsPage: View {
    @ObservedObject var model: BexAppViewModel
    let title: String
    let sections: [String]

    var body: some View {
        let view = model.snapshot.settings(scope: .host)
        List {
            if !model.snapshot.conversationSettingsLoaded() {
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
        List {
            Section("Provider accounts") {
                ForEach(model.snapshot.accounts()?.accounts ?? [], id: \.id) { account in
                    VStack(alignment: .leading, spacing: 8) {
                        AccountIdentityView(account: account)
                        AccountUsageView(usage: account.usage)
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
        .onAppear { model.perform(.loadAccounts) }
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
