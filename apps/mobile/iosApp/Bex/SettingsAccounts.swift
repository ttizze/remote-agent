import AgentCore
import Foundation
import SwiftUI

/// Browser profiles, provider accounts and environments.
struct BrowserProfilesView: View {
    @ObservedObject var model: BexAppViewModel
    let profiles: [BrowserProfile]
    let defaultProfileId: String
    let onDefault: (String) -> Void
    let onRemove: (String) -> Void
    @State private var editingId: String?
    @State private var editingName = ""
    @State private var newName = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(profiles, id: \.id) { profile in
                let builtIn = profile.id == "default" || profile.id == "incognito"
                VStack(alignment: .leading, spacing: 4) {
                    HStack {
                        Text(profile.name).foregroundStyle(AppTheme.text)
                        Spacer()
                        if profile.id != "incognito" {
                            Button(profile.id == defaultProfileId ? "Default" : "Use") {
                                onDefault(profile.id)
                            }
                        }
                        if !builtIn {
                            Button("Rename") {
                                editingId = profile.id
                                editingName = profile.name
                            }
                            .foregroundStyle(AppTheme.primary)
                            Button("Remove", role: .destructive) {
                                onRemove(profile.id)
                            }
                        }
                    }
                    if editingId == profile.id {
                        TextField("Profile name", text: $editingName)
                            .textFieldStyle(.roundedBorder)
                        HStack {
                            Button("Save") {
                                model.perform(.renameBrowserProfile(
                                    profileId: profile.id,
                                    name: editingName
                                ))
                                editingId = nil
                            }
                            Button("Cancel") { editingId = nil }
                        }
                        .font(AppTheme.font(13))
                    }
                }
            }
            HStack {
                TextField("New profile name", text: $newName)
                    .textFieldStyle(.roundedBorder)
                Button("New profile") {
                    model.perform(.createBrowserProfile(
                        profileId: UUID().uuidString,
                        requestedName: newName.isEmpty ? nil : newName
                    ))
                    newName = ""
                }
                .disabled(profiles.filter { $0.id != "default" && $0.id != "incognito" }.count >= 24)
            }
        }
    }
}

struct ProviderAccountsPage: View {
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
struct ConnectionsScreen: View {
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

    func statusColor(_ profile: HostProfile) -> Color {
        guard profile.id == model.selectedProfileId else { return AppTheme.muted }
        return model.isConnected ? AppTheme.emeraldIcon : model.isConnecting ? AppTheme.amber : AppTheme.rose
    }
}
