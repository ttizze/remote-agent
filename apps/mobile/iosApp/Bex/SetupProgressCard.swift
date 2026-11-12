#if !BEX_NSH_CONVERSATION
    import AgentCore
    import SwiftUI

    struct SetupProgressCard: View {
        let preparation: PreparationPresentation
        let openTerminal: (SetupTerminal) -> Void
        let perform: (Intent) -> Void
        @Environment(\.locale) private var locale
        @State private var details = false

        var body: some View {
            TimelineView(.periodic(from: .now, by: 1)) { context in
                VStack(alignment: .leading, spacing: 6) {
                    HStack {
                        ThreadActivityHeader(summary: preparation.text, canCollapse: false,
                                             isInProgress: preparation.shimmer, expanded: false)
                        Text(verbatim: elapsed(preparation.startedAtMs, preparation.endedAtMs, context.date))
                            .font(.caption)
                    }
                    if preparation.showSteps {
                        Divider()
                        ForEach(preparation.steps, id: \.id) { step in
                            HStack(spacing: 8) {
                                if step.state == "running" {
                                    ProgressView().controlSize(.mini).frame(width: 20)
                                } else {
                                    Image(systemName: symbol(step.state)).frame(width: 20)
                                }
                                Text(verbatim: step.label).frame(maxWidth: .infinity, alignment: .leading)
                                if let detail = step.detail {
                                    Text(verbatim: detail).font(.caption)
                                }
                                if step.state != "pending", step.state != "skipped" {
                                    Text(verbatim: elapsed(step.startedAtMs, step.endedAtMs, context.date))
                                        .font(.caption)
                                }
                            }
                            .opacity(step.state == "pending" ? 0.4 : 1)
                            .foregroundStyle(step.state == "failed" ? Color.red : Color.secondary)
                            if !step.tail.isEmpty {
                                Text(verbatim: step.tail.joined(separator: "\n")).font(.caption.monospaced())
                                    .textSelection(.enabled).padding(8).frame(maxWidth: .infinity, alignment: .leading)
                                    .background(.quaternary, in: RoundedRectangle(cornerRadius: 6)).padding(
                                        .leading,
                                        28
                                    )
                            }
                        }
                    }
                    if let error = preparation.error {
                        Text(verbatim: error).font(.caption).foregroundStyle(.red)
                    }
                    ViewThatFits(in: .horizontal) {
                        HStack { actions }
                        VStack(alignment: .leading) { actions }
                    }
                    if details {
                        ForEach(preparation.details, id: \.label) { detail in
                            LabeledContent(detail.label, value: detail.value)
                                .textSelection(.enabled).font(.caption.monospaced())
                        }.font(.caption)
                    }
                }.font(.subheadline).foregroundStyle(.secondary).monospacedDigit()
                    .accessibilityIdentifier("task.preparation")
            }
        }

        @ViewBuilder private var actions: some View {
            if preparation.details.isEmpty == false {
                Button("詳細", systemImage: details ? "chevron.down" : "chevron.right") { details.toggle() }
            }
            if let terminal = preparation.terminal {
                Button("ターミナルを開く") { openTerminal(terminal) }
            }
            if let work = preparation.workLocally {
                Button("ローカルで作業") { perform(.retryPreparation(work)) }
            }
            if let cancel = preparation.cancel {
                Button("キャンセル") { perform(.interrupt(cancel)) }
            }
            if let retry = preparation.retry {
                Button("再試行") { perform(.retryPreparation(retry)) }
            }
        }

        private func elapsed(_ start: UInt64?, _ end: UInt64?, _ date: Date) -> String {
            guard let start else { return "" }
            return preparationElapsed(
                language: locale.appLanguage,
                start: start,
                end: end,
                now: UInt64(max(0, date.timeIntervalSince1970 * 1000))
            )
        }

        private func symbol(_ state: String) -> String {
            switch state {
            case "done": "checkmark"
            case "skipped": "minus"
            case "failed": "xmark"
            case "warning": "exclamationmark.triangle"
            default: "circle"
            }
        }
    }
#endif
