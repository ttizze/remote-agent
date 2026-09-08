import Combine
import Foundation
import RemoteAgentMobile

@MainActor
final class CodexModelSettings: ObservableObject {
    private let actions: IosConversationActions
    private let accountActions: IosAccountActions
    private var profileId: String?
    private var isConnected = false

    init(actions: IosConversationActions, accounts: IosAccountActions) {
        self.actions = actions
        accountActions = accounts
        accountSubscription = accountActions.observeAccounts { [weak self] state in
            guard let self else { return }
            let changedAccount = accountState?.selectedId != state.selectedId
            accountState = state
            if changedAccount {
                modelRequestId = UUID()
                models = []
                loadingModels = false
                loadModels()
            }
        }
    }

    func update(host: String?, connected: Bool) {
        let changedHost = profileId != host
        let becameConnected = !isConnected && connected
        profileId = host
        isConnected = connected
        if changedHost {
            modelRequestId = UUID()
            models = []
            loadingModels = false
            modelError = nil
        }
        if changedHost || becameConnected {
            loadModels()
        }
    }

    private var accountSubscription: HostEventSubscription?
    @Published private(set) var accountState: AccountSettingsState?
    var accounts: [HostAccount] {
        accountState?.accounts ?? []
    }

    var selectedAccountId: String? {
        accountState?.selectedId
    }

    var accountError: String? {
        accountState?.error
    }

    var changingAccount: Bool {
        accountState?.selecting ?? false
    }

    var login: HostAccountLogin? {
        accountState?.login
    }

    var startingLogin: Bool {
        accountState?.startingLogin ?? false
    }

    deinit { accountSubscription?.cancel() }

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
    }

    var turnOptions: CodexTurnOptions {
        CodexTurnOptions(model: selectedModel.isEmpty ? nil : selectedModel,
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

    func loadAccounts() {
        accountActions.refreshAccounts()
    }

    func chooseAccount(_ id: String) {
        accountActions.selectAccount(id: id)
    }

    func startLogin() {
        accountActions.startAccountLogin()
    }

    func cancelLogin() {
        accountActions.cancelAccountLogin()
    }

    func resumeLogin() {
        accountActions.resumeAccountLogin()
    }

    func pauseLogin() {
        accountActions.pauseAccountLogin()
    }
}
