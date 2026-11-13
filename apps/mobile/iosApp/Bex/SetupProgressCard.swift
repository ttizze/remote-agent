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
            ConversationPreparation(showSteps: preparation.showSteps, error: preparation.error) {
                HStack {
                    ConversationActivityHeader(summary: preparation.text, canCollapse: false,
                                               isInProgress: preparation.shimmer, expanded: false)
                    Text(verbatim: elapsed(preparation.startedAtMs, preparation.endedAtMs, context.date))
                        .font(.caption)
                }
            } steps: {
                ForEach(preparation.steps, id: \.id) { step in
                    ConversationPreparationStep(label: step.label, detail: step.detail,
                                                tail: step.tail.joined(separator: "\n"), symbol: symbol(step.state),
                                                pending: step.state == "pending", running: step.state == "running",
                                                failed: step.state == "failed",
                                                elapsed: step.state == "pending" || step.state == "skipped" ? nil :
                                                    elapsed(step.startedAtMs, step.endedAtMs, context.date))
                }
            } actions: {
                actions
            } details: {
                if details {
                    ForEach(preparation.details, id: \.label) { detail in
                        LabeledContent(detail.label, value: detail.value)
                            .textSelection(.enabled).font(.caption.monospaced())
                    }.font(.caption)
                }
            }
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
