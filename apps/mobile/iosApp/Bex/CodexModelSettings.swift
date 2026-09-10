import AgentCore
import Foundation

extension BexAppViewModel {
    var accounts: [Account] {
        snapshot.accounts()?.accounts ?? []
    }

    var selectedAccountId: String? {
        snapshot.accounts()?.selectedId
    }

    var accountError: String? {
        snapshot.accounts()?.error ?? notice
    }

    var modelError: String? {
        notice ?? snapshot.error()
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

    func loadAccounts(selecting id: String? = nil) {
        perform(.listAccounts(ListAccounts())) { [weak self] result in
            if case .success = result, let id {
                self?.chooseAccount(id)
            }
        }
    }

    func chooseAccount(_ id: String) {
        perform(.selectAccount(SelectAccount(id: id)))
    }
}
