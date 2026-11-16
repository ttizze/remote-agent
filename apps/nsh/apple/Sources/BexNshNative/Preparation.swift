import NativeUI
import SwiftUI

struct NativePreparation: Decodable, Equatable {
    struct Step: Decodable, Equatable {
        let id: String
        let label: String
        let detail: String?
        let tail: String
        let icon: String
        let dim: Bool
        let running: Bool
        let time: String?
    }

    struct Detail: Decodable, Equatable { let label: String; let value: String }
    let steps: [Step]
    let details: [Detail]
    let error: String?
}

struct MacPreparation: View {
    let value: NativePreparation
    let summary: [TranscriptPart]
    let document: MessageDocument
    let elapsed: String?
    let actions: [ConversationButton]
    let send: ([String: Any]) -> Void
    var body: some View {
        ConversationPreparation(showSteps: !value.steps.isEmpty, error: value.error) {
            HStack {
                TranscriptContent(parts: summary, document: document, media: [:], send: send)
                if let elapsed {
                    Text(verbatim: elapsed).font(.caption)
                }
            }
        } steps: {
            ForEach(value.steps, id: \.id) { step in
                ConversationPreparationStep(label: step.label, detail: step.detail, tail: step.tail,
                                            symbol: MessageAttributes.symbol(step.icon) ?? "circle", pending: step.dim,
                                            running: step.running, failed: step.icon == "close", elapsed: step.time)
            }
        } actions: {
            ForEach(actions, id: \.action) { action in
                Button(action.title) { send(["action": action.action]) }.disabled(action.enabled == false)
            }
        } details: {
            ForEach(value.details, id: \.label) { detail in
                LabeledContent(detail.label, value: detail.value).textSelection(.enabled).font(.caption.monospaced())
            }.font(.caption)
        }
    }
}
