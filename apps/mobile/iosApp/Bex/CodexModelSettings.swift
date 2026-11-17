import AgentCore
import Foundation

extension ConversationModel {
    var accounts: [Account] {
        settings.accounts()?.accounts ?? []
    }

    var selectedModel: ModelRef? {
        settings.selectedModel()
    }

    func chooseModel(_ value: ModelRef) {
        perform(.selectModel(threadId: inputDraftKey, model: value))
    }

    func chooseEffort(_ value: String) {
        perform(.selectEffort(threadId: inputDraftKey, effort: value))
    }

    func chooseServiceTier(_ value: String) {
        perform(.selectServiceTier(threadId: inputDraftKey, serviceTier: value))
    }
}
