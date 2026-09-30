import AgentCore
import Foundation

extension BexAppViewModel {
    var accounts: [Account] {
        snapshot.accounts()?.accounts ?? []
    }

    var accountError: String? {
        snapshot.accounts()?.error ?? snapshot.error()
    }

    var selectedModel: ModelRef? {
        snapshot.draft(key: coreDraftKey).model
    }

    var currentModel: Model? {
        models.first { $0.model == selectedModel }
    }

    func chooseModel(_ value: ModelRef) {
        perform(.selectModel(threadId: coreDraftKey, model: value))
    }

    func chooseEffort(_ value: String) {
        perform(.selectEffort(threadId: coreDraftKey, effort: value))
    }

    func chooseServiceTier(_ value: String) {
        perform(.selectServiceTier(threadId: coreDraftKey, serviceTier: value))
    }
}
