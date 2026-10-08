import AgentCore
import SwiftUI

struct NativeUpdatePage: View {
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

struct BackgroundDiagnosticsPage: View {
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
                    if row.key != "profile",
                       row.key != "automaticGitFetchIntervalMs",
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
                            if selected == profile {
                                Image(systemName: "checkmark")
                            }
                        }
                    }
                }
            }
            Section("Intervals") {
                Picker(
                    "Git fetch interval",
                    selection: Binding(get: { gitFetchSeconds }, set: { value in
                        model.perform(.setAutomaticGitFetchInterval(seconds: UInt32(value)))
                    })
                ) {
                    ForEach([0, 15, 30, 60, 300, 900], id: \.self) { value in
                        Text(value == 0 ? "Disabled" : "\(value) seconds").tag(value)
                    }
                }
                Picker(
                    "Provider health interval",
                    selection: Binding(get: { providerHealthSeconds }, set: { value in
                        model.perform(.setProviderHealthRefreshInterval(seconds: UInt32(value)))
                    })
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
    return max(0, milliseconds / 1000)
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
