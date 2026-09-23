import AgentCore
import Foundation

extension BexAppViewModel {
    var accounts: [Account] {
        snapshot.accounts()?.accounts ?? []
    }

    var accountError: String? {
        snapshot.accounts()?.error ?? snapshot.error()
    }

    var selectedModel: String {
        snapshot.draft(key: coreDraftKey).model ?? ""
    }

    var selectedEffort: String {
        snapshot.draft(key: coreDraftKey).effort ?? ""
    }

    var selectedServiceTier: String {
        snapshot.draft(key: coreDraftKey).serviceTier ?? ""
    }

    var currentModel: Model? {
        models.first { $0.model == selectedModel }
    }

    func chooseModel(_ value: String) {
        perform(.selectModel(threadId: coreDraftKey, model: value))
    }

    func chooseEffort(_ value: String) {
        perform(.selectEffort(threadId: coreDraftKey, effort: value))
    }

    func chooseServiceTier(_ value: String) {
        perform(.selectServiceTier(threadId: coreDraftKey, serviceTier: value))
    }
}
