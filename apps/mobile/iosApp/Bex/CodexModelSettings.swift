import Combine
import Foundation
import RemoteAgentMobile

struct CodexAccountChoice: Identifiable {
    let id: String
    let email: String
    let plan: String
}

@MainActor
final class CodexModelSettings: ObservableObject {
    private let actions: IosConversationActions
    private var profileId: String?
    private var isConnected = false

    init(actions: IosConversationActions) {
        self.actions = actions
    }

    func update(host: String?, connected: Bool) {
        let changedHost = profileId != host
        let becameConnected = !isConnected && connected
        profileId = host
        isConnected = connected
        if changedHost {
            accounts = []
            selectedAccountId = nil
            accountError = nil
            changingAccount = false
            modelRequestId = UUID()
            models = []
            loadingModels = false
            modelError = nil
            applyTurnOptions()
        }
        if changedHost || becameConnected {
            loadModels()
        }
    }

    @Published private(set) var accounts: [CodexAccountChoice] = []
    @Published private(set) var selectedAccountId: String?
    @Published private(set) var accountError: String?
    @Published private(set) var changingAccount = false
    private var modelRequestId = UUID()
    @Published private(set) var models: [CodexModel] = []
    @Published private(set) var modelError: String?
    @Published private(set) var loadingModels = false
    @Published private var modelChoices = UserDefaults.standard
        .dictionary(forKey: "bex.models.v1") as? [String: String] ?? [:]
    @Published private var effortChoices = UserDefaults.standard
        .dictionary(forKey: "bex.efforts.v1") as? [String: String] ?? [:]
    var selectedModel: String {
        modelChoices[profileId ?? ""] ?? ""
    }

    var selectedEffort: String {
        effortChoices[profileId ?? ""] ?? ""
    }

    var currentModel: CodexModel? {
        models.first { $0.model == selectedModel }
    }

    func chooseModel(_ value: String) {
        guard let host = profileId else { return }
        modelChoices[host] = value
        effortChoices[host] = models.first { $0.model == value }?.defaultReasoningEffort ?? ""
        persistTurnOptions()
    }

    func chooseEffort(_ value: String) {
        guard let host = profileId else { return }
        effortChoices[host] = value.isEmpty ? (currentModel?.defaultReasoningEffort ?? "") : value
        persistTurnOptions()
    }

    private func persistTurnOptions() {
        UserDefaults.standard.set(modelChoices, forKey: "bex.models.v1")
        UserDefaults.standard.set(effortChoices, forKey: "bex.efforts.v1")
        applyTurnOptions()
    }

    func applyTurnOptions() {
        guard let host = profileId else { return }
        actions.setTurnOptions(hostIdentity: host, model: selectedModel.isEmpty ? nil : selectedModel,
                               effort: selectedEffort.isEmpty ? currentModel?.defaultReasoningEffort : selectedEffort)
    }

    func loadModels() {
        guard isConnected, let host = profileId, !loadingModels else { return }
        loadingModels = true
        let requestId = UUID()
        modelRequestId = requestId
        modelError = nil
        actions.listModels { [weak self] models, error in
            guard let self, profileId == host, modelRequestId == requestId else { return }
            loadingModels = false
            self.models = models ?? []
            modelError = error
        }
    }

    func loadAccounts(selecting accountId: String? = nil) {
        guard isConnected, let host = profileId else { return }
        accountError = nil
        account("host/account/list", [:]) { [weak self] result, error in
            guard let self, profileId == host else { return }
            accountError = error ?? result?["error"] as? String
            guard let result else { return }
            accounts = (result["accounts"] as? [[String: Any]] ?? []).compactMap { value in
                guard let id = value["id"] as? String, let email = value["email"] as? String else { return nil }
                return CodexAccountChoice(id: id, email: email, plan: value["planType"] as? String ?? "")
            }
            selectedAccountId = result["selectedId"] as? String
            if let accountId {
                chooseAccount(accountId)
            }
        }
    }

    func chooseAccount(_ id: String) {
        guard !changingAccount, let host = profileId, id != selectedAccountId else { return }
        changingAccount = true
        accountError = nil
        account("host/account/select", ["accountId": id]) { [weak self] result, error in
            guard let self, profileId == host else { return }
            changingAccount = false
            accountError = error ?? result?["persistenceError"] as? String
            guard result != nil else { return }
            selectedAccountId = id
            models = []
            loadingModels = false
            loadModels()
        }
    }

    func account(_ method: String, _ params: [String: Any], completion: @escaping ([String: Any]?, String?) -> Void) {
        do {
            try actions.accountRequest(method: method, paramsJson: jsonString(params)) { result, error in
                completion(result.map(jsonObject), error)
            }
        } catch { completion(nil, error.localizedDescription) }
    }
}
