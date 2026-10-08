import AgentCore
import SwiftUI

/// Host usage totals and provider limits. Core supplies the same projections
/// to desktop and mobile so clients do not interpret transcript data locally.
struct UsageScreen: View {
    @ObservedObject var model: BexAppViewModel
    @State private var tab = UsageTab.usage

    var body: some View {
        let view = model.snapshot.usagePage()
        List {
            Section {
                Picker("View", selection: $tab) {
                    Text("Usage").tag(UsageTab.usage)
                    Text("Limits").tag(UsageTab.limits)
                }
                .pickerStyle(.segmented)
            }
            if tab == .usage {
                Section("Summary") {
                    LabeledContent("Tokens", value: "\(view.totalTokensLabel) · \(view.sessions) sessions")
                    LabeledContent("Estimated cost", value: "\(view.costLabel) · \(view.pricingStatus)")
                    Button("Refresh rates") { refreshRates() }
                    if view.state == "loading" {
                        ProgressView()
                    }
                    if let error = view.error {
                        Text(error).font(.caption).foregroundStyle(AppTheme.dangerForeground)
                    }
                }
                Section("Usage by model") {
                    ForEach(Array(view.rows.enumerated()), id: \.offset) { _, row in
                        VStack(alignment: .leading, spacing: 4) {
                            Text("\(row.provider) · \(row.model)")
                            Text("\(row.day) · \(row.tokens) tokens · \(row.costLabel)")
                                .font(.caption).foregroundStyle(AppTheme.muted)
                        }
                    }
                    if view.rows.isEmpty, view.state == "ready" {
                        Text("No usage in this period.").foregroundStyle(AppTheme.muted)
                    }
                }
                Section("Daily trend") {
                    ForEach(Array(view.chart.enumerated()), id: \.offset) { _, point in
                        VStack(alignment: .leading, spacing: 4) {
                            HStack {
                                Text(point.day)
                                Spacer()
                                Text("\(point.tokens) tokens")
                                    .font(.caption).monospacedDigit()
                            }
                            ProgressView(value: Double(point.tokens), total: Double(maxTokens))
                            Text(point.costUsd.formatted(.currency(code: "USD")))
                                .font(.caption).foregroundStyle(AppTheme.muted)
                        }
                    }
                }
            } else {
                Section("Limits") {
                    ForEach(model.snapshot.usageLimits(), id: \.id) { account in
                        UsageLimitAccountView(account: account) {
                            if let source = model.snapshot.accounts()?.accounts.first(where: { $0.id == account.id }) {
                                model.perform(.consumeResetCredit(
                                    provider: source.provider,
                                    accountId: source.id,
                                    creditId: account.nextCreditId
                                ))
                                model.perform(.loadAccounts)
                            }
                        }
                    }
                    if model.snapshot.usageLimits().isEmpty {
                        Text("Provider limits are unavailable until accounts are loaded.")
                            .foregroundStyle(AppTheme.muted)
                    }
                }
            }
        }
        .scrollContentBackground(.hidden)
        .background(AppTheme.sheet)
        .navigationTitle("Usage")
        .navigationBarTitleDisplayMode(.inline)
        .refreshable { load() }
        .onAppear { load() }
    }

    private var maxTokens: UInt64 {
        max(viewChartTokens.max() ?? 1, 1)
    }

    private var viewChartTokens: [UInt64] {
        model.snapshot.usagePage().chart.map(\.tokens)
    }

    private func load() {
        model.perform(.loadUsageSummary(input: summaryInput()))
        model.perform(.loadAccounts)
    }

    private func refreshRates() {
        let input = summaryInput()
        model.perform(.refreshUsageRates) { result in
            if case .success = result {
                model.perform(.loadUsageSummary(input: input))
            }
        }
    }

    private func summaryInput() -> UsageSummaryInput {
        let now = Date()
        let start = Calendar.current.date(byAdding: .day, value: -30, to: now) ?? now
        let formatter = DateFormatter()
        formatter.calendar = Calendar.current
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyy-MM-dd"
        let preferences = model.snapshot.usagePreferences()
        let input = UsageSummaryInput(
            sinceDay: formatter.string(from: start),
            untilDay: formatter.string(from: now),
            timeZone: TimeZone.current.identifier,
            resolution: .day,
            sinceTime: nil,
            untilTime: nil,
            modelAliases: preferences.modelAliases,
            priceOverrides: preferences.priceOverrides
        )
        return input
    }
}

private enum UsageTab: Hashable {
    case usage
    case limits
}

private struct UsageLimitAccountView: View {
    let account: UsageLimitAccount
    let useReset: () -> Void
    @State private var confirmingReset = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(account.email ?? account.id).font(.headline)
            ForEach(account.windows, id: \.id) { window in
                VStack(alignment: .leading, spacing: 4) {
                    HStack {
                        Text(window.label)
                        Spacer()
                        Text("\(window.remainingPercent)% remaining").monospacedDigit()
                    }
                    ProgressView(value: Double(window.remainingPercent), total: 100)
                    if let reset = window.resetsAt {
                        Text("Resets \(Date(timeIntervalSince1970: Double(reset)).formatted(date: .abbreviated, time: .shortened))")
                            .font(.caption).foregroundStyle(AppTheme.muted)
                    }
                }
            }
            if account.resetCreditCount > 0 {
                HStack {
                    Text("Reset credits: \(account.resetCreditCount)")
                        .font(.caption).foregroundStyle(AppTheme.muted)
                    Spacer()
                    Button("Use reset") { confirmingReset = true }
                        .font(.caption)
                }
            }
            if let externalLabel = account.externalLabel, let externalURL = URL(string: account.externalUrl ?? "") {
                Link(externalLabel, destination: externalURL).font(.caption)
            }
            if let error = account.error {
                Text(error).font(.caption).foregroundStyle(AppTheme.warningForeground)
            }
        }
        .alert("Use a reset credit?", isPresented: $confirmingReset) {
            Button("Cancel", role: .cancel) {}
            Button("Use credit") { useReset() }
        } message: {
            Text("This redeems one credit and clears the current rate-limit windows.")
        }
    }
}
