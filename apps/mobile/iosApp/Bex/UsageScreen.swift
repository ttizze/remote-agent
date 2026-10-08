import AgentCore
import Foundation
import SwiftUI

/// Host usage totals and provider limits. Core supplies the same projections
/// to desktop and mobile so clients do not interpret transcript data locally.
struct UsageScreen: View {
    @ObservedObject var model: BexAppViewModel
    @State private var tab: UsageTab

    init(model: BexAppViewModel, initialTab: UsageTab = .usage) {
        self.model = model
        _tab = State(initialValue: initialTab)
    }

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
                UsagePreferencesEditor(model: model) { load() }
            } else {
                Section("Limits") {
                    let pools = model.snapshot.usageLimitPools(
                        nowMs: Int64(Date().timeIntervalSince1970 * 1000)
                    )
                    let accounts = model.snapshot.usageLimits()
                    let pooledIds = Set(pools.flatMap { $0.accounts }.map(\.id))
                    ForEach(pools, id: \.provider) { pool in
                        UsageLimitPoolView(pool: pool) { account in
                            let sourceId = account.resetCreditAccountId ?? account.id
                            if let source = model.snapshot.accounts()?.accounts.first(where: { $0.id == sourceId }) {
                                model.perform(.consumeResetCredit(
                                    provider: source.provider,
                                    accountId: source.id,
                                    creditId: account.nextCreditId
                                ))
                                model.perform(.loadAccounts)
                            }
                        }
                    }
                    ForEach(accounts.filter { !pooledIds.contains($0.id) }, id: \.id) { account in
                        UsageLimitAccountView(account: account, showWindows: false) {
                            let sourceId = account.resetCreditAccountId ?? account.id
                            if let source = model.snapshot.accounts()?.accounts.first(where: { $0.id == sourceId }) {
                                model.perform(.consumeResetCredit(
                                    provider: source.provider,
                                    accountId: source.id,
                                    creditId: account.nextCreditId
                                ))
                                model.perform(.loadAccounts)
                            }
                        }
                    }
                    if pools.isEmpty && accounts.isEmpty {
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

private struct UsagePreferencesEditor: View {
    @ObservedObject var model: BexAppViewModel
    let onChanged: () -> Void
    @State private var aliasModel = ""
    @State private var aliasTarget = ""
    @State private var priceModel = ""
    @State private var inputPrice = ""
    @State private var outputPrice = ""
    @State private var cacheReadPrice = ""
    @State private var cacheWritePrice = ""
    @State private var error: String?

    var body: some View {
        let preferences = model.snapshot.usagePreferences()
        Section("Usage preferences") {
            Text("Model aliases")
                .font(.headline)
            ForEach(preferences.modelAliases.keys.sorted(), id: \.self) { modelName in
                HStack {
                    Text("\(modelName) → \(preferences.modelAliases[modelName] ?? "")")
                        .font(.caption)
                    Spacer()
                    Button("Remove") { update { $0.modelAliases.removeValue(forKey: modelName) } }
                        .font(.caption)
                }
            }
            TextField("Source model", text: $aliasModel)
            TextField("Bucket and pricing model", text: $aliasTarget)
            Button("Save alias") {
                let source = aliasModel.trimmingCharacters(in: .whitespacesAndNewlines)
                let target = aliasTarget.trimmingCharacters(in: .whitespacesAndNewlines)
                guard !source.isEmpty, !target.isEmpty else { return }
                update { $0.modelAliases[source] = target }
                aliasModel = ""
                aliasTarget = ""
            }

            Text("Price overrides (USD per million tokens)")
                .font(.headline)
            ForEach(preferences.priceOverrides.keys.sorted(), id: \.self) { modelName in
                let prices = preferences.priceOverrides[modelName]
                VStack(alignment: .leading, spacing: 3) {
                    HStack {
                        Text(modelName)
                        Spacer()
                        Button("Remove") { update { $0.priceOverrides.removeValue(forKey: modelName) } }
                            .font(.caption)
                    }
                    if let prices {
                        Text("input \(prices.inputCostPerMillionTokens) · output \(prices.outputCostPerMillionTokens)")
                            .font(.caption)
                            .foregroundStyle(AppTheme.muted)
                    }
                }
            }
            TextField("Model price key", text: $priceModel)
            TextField("Input price", text: $inputPrice)
                .keyboardType(.decimalPad)
            TextField("Output price", text: $outputPrice)
                .keyboardType(.decimalPad)
            TextField("Cache read price (optional)", text: $cacheReadPrice)
                .keyboardType(.decimalPad)
            TextField("Cache write price (optional)", text: $cacheWritePrice)
                .keyboardType(.decimalPad)
            Button("Save price override") {
                savePrice()
            }
            if let error {
                Text(error)
                    .font(.caption)
                    .foregroundStyle(AppTheme.warningForeground)
            }
        }
    }

    private func update(_ change: (inout UsagePreferences) -> Void) {
        var preferences = model.snapshot.usagePreferences()
        change(&preferences)
        model.perform(.setUsagePreferences(preferences: preferences))
        onChanged()
        error = nil
    }

    private func savePrice() {
        let modelName = priceModel.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !modelName.isEmpty,
              let input = Double(inputPrice),
              let output = Double(outputPrice)
        else {
            error = "Enter a model key and numeric input/output prices."
            return
        }
        let override = PriceOverride(
            inputCostPerMillionTokens: input,
            outputCostPerMillionTokens: output,
            cacheReadCostPerMillionTokens: Double(cacheReadPrice),
            cacheWriteCostPerMillionTokens: Double(cacheWritePrice)
        )
        update { $0.priceOverrides[modelName] = override }
        priceModel = ""
        inputPrice = ""
        outputPrice = ""
        cacheReadPrice = ""
        cacheWritePrice = ""
    }
}

enum UsageTab: Hashable {
    case usage
    case limits
}

private struct UsageLimitPoolView: View {
    let pool: UsageLimitPool
    let useReset: (UsageLimitAccount) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(pool.provider).font(.headline)
            ForEach(Array(pool.windows.enumerated()), id: \.offset) { _, window in
                VStack(alignment: .leading, spacing: 4) {
                    HStack {
                        Text(window.label)
                        Spacer()
                        Text("\(window.remainingPercent)% remaining").monospacedDigit()
                    }
                    ProgressView(value: Double(window.remainingPercent), total: 100)
                    if let pace = window.pace {
                        Text("Pace: \(pace)")
                            .font(.caption)
                            .foregroundStyle(AppTheme.muted)
                    }
                    ForEach(window.resets, id: \.accountId) { reset in
                        HStack {
                            Text("Reset \(reset.label)")
                                .font(.caption)
                            Spacer()
                            Text("+\(reset.restoresPercent)% at \(Date(timeIntervalSince1970: Double(reset.at) / 1000).formatted(date: .abbreviated, time: .shortened))")
                                .font(.caption)
                                .foregroundStyle(AppTheme.muted)
                        }
                    }
                    ForEach(window.columns, id: \.accountId) { column in
                        HStack {
                            Text(column.label)
                                .font(.caption)
                            Spacer()
                            Text(column.window.map { "\($0.remainingPercent)% left" } ?? "No limit reported")
                                .font(.caption)
                                .foregroundStyle(AppTheme.muted)
                        }
                    }
                }
            }
            ForEach(pool.accounts.filter {
                $0.resetCreditCount > 0 || $0.externalLabel != nil || $0.error != nil
            }, id: \.id) { account in
                UsageLimitAccountView(account: account, showWindows: false) {
                    useReset(account)
                }
            }
        }
    }
}

private struct UsageLimitAccountView: View {
    let account: UsageLimitAccount
    let showWindows: Bool
    let useReset: () -> Void
    @State private var confirmingReset = false

    init(
        account: UsageLimitAccount,
        showWindows: Bool = true,
        useReset: @escaping () -> Void
    ) {
        self.account = account
        self.showWindows = showWindows
        self.useReset = useReset
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(account.email ?? account.id).font(.headline)
            if showWindows {
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
