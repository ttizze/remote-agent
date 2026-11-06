import AgentCore
import Foundation
import Network
import Observation
import UIKit

@MainActor
@Observable
final class AppViewModel {
    @ObservationIgnored private(set) var publication = Presentation.empty()
    @ObservationIgnored private(set) var snapshot = AgentCore.Snapshot.empty()
    private(set) var settings = Presentation.empty().settings()
    private(set) var connectionStates: [String: HostConnectionState] = [:]
    private(set) var isConnected = false
    private(set) var loadingThreads = false
    private(set) var notices: [ErrorScope: [ErrorNotice]] = [:]
    /// NavigationStack writes its own path during interactive transitions. Keep that path as the
    /// native source of truth rather than reconstructing it and discarding nonempty writes.
    var navigationPath: [AppScreen] = []

    var connections: [String: Task<Void, Never>] = [:]
    var pairingConnection: Task<Void, Never>?
    var isConnecting: Bool {
        selectedProfileId.flatMap { connections[$0] } != nil
    }

    var pairingError: String?
    private(set) var pairingInvitation: Invitation?
    var notice: String?
    var profiles: [HostProfile] = []
    private(set) var selectedProfileId: String?
    private(set) var allProfiles = false
    private(set) var list: ThreadList?

    @ObservationIgnored lazy var sessions = ClientSessions(recentChats: false, clientBuild: Self.clientBuild,
                                                           deviceName: UIDevice.current.name)
    private(set) var chat: ConversationModel?
    private var conversations: [String: ConversationModel] = [:]
    private(set) var session: ClientSession?

    var store: AgentStore? {
        session?.store()
    }

    var observations: [String: Task<Void, Never>] = [:]
    private let pathMonitor = NWPathMonitor()
    let liveActivities = TaskLiveActivities()

    init() {
        observeLiveActivityRequests()
        pathMonitor.pathUpdateHandler = { [weak self] path in
            let available = path.status == .satisfied
            Task { @MainActor [weak self] in
                guard let self else { return }
                await sessions.networkChanged()
                if available, UIApplication.shared.applicationState == .active {
                    connectProfiles()
                }
            }
        }
        pathMonitor.start(queue: DispatchQueue(label: "agent.network-path"))
        do { profiles = try HostProfile.load() } catch { notice = error.localizedDescription }
        let selection = resolveConnectionSelection(hosts: profiles.map(\.id),
                                                   lastHost: UserDefaults.standard.string(forKey: "bex.selected-host"),
                                                   all: UserDefaults.standard.bool(forKey: "bex.all-hosts"))
        allProfiles = selection.all
        if let id = selection.hostId {
            activateProfile(id)
        }
        connectProfiles()
    }

    deinit { pathMonitor.cancel() }

    func selectProfile(_ id: String) {
        guard profiles.contains(where: { $0.id == id }) else { return }
        allProfiles = false
        UserDefaults.standard.set(false, forKey: "bex.all-hosts")
        activateProfile(id)
        showThreadList()
        updateTaskList()
    }

    func selectAllProfiles() {
        allProfiles = true
        UserDefaults.standard.set(true, forKey: "bex.all-hosts")
        showThreadList()
        updateTaskList()
    }

    func activateProfile(_ id: String) {
        guard profiles.contains(where: { $0.id == id }) else { return }
        if selectedProfileId == id, session != nil {
            return
        }
        resetPresentation()
        selectedProfileId = id
        UserDefaults.standard.set(id, forKey: "bex.selected-host")
        let storage = MobileSessionStorage(host: id)
        let owner = sessions.open(host: id, storage: storage, platform: storage)
        session = owner
        chat = conversations[id] ?? ConversationModel(host: id, session: owner)
        conversations[id] = chat
        _ = try? owner.dispatch(intent: .showThreadList)
        publish(owner.presentation())
        observe(owner, host: id)
    }

    func updateTaskList() {
        list = sessions.threadList(host: allProfiles ? nil : selectedProfileId)
    }

    private static var clientBuild: ClientBuild {
        ClientBuild(
            number: UInt64(Bundle.main.infoDictionary?["CFBundleVersion"] as? String ?? "0") ?? 0,
            revision: Bundle.main.infoDictionary?["BEXBuildRevision"] as? String ?? "development"
        )
    }

    func removeProfile(_ id: String) {
        guard profiles.contains(where: { $0.id == id }) else { return }
        do { try HostProfile.save(profiles.filter { $0.id != id }) } catch {
            notice = error.localizedDescription
            return
        }
        connections.removeValue(forKey: id)?.cancel()
        observations.removeValue(forKey: id)?.cancel()
        conversations.removeValue(forKey: id)
        connectionStates.removeValue(forKey: id)
        let old = sessions.remove(host: id)
        if selectedProfileId == id {
            resetPresentation()
            selectedProfileId = nil
            session = nil
            chat = nil
            UserDefaults.standard.removeObject(forKey: "bex.selected-host")
        }
        profiles.removeAll { $0.id == id }
        let selection = resolveConnectionSelection(
            hosts: profiles.map(\.id),
            lastHost: selectedProfileId,
            all: allProfiles
        )
        allProfiles = selection.all
        UserDefaults.standard.set(allProfiles, forKey: "bex.all-hosts")
        if let host = selection.hostId {
            activateProfile(host)
        }
        updateTaskList()
        Task { [weak self] in
            do {
                do { try await old?.shutdown() } catch { self?.notice = error.localizedDescription }
                try DeviceIdentity.remove(id)
                try? FileManager.default.removeItem(at: MobileSessionStorage(host: id).conversations())
            } catch { self?.notice = error.localizedDescription }
        }
    }

    private func resetPresentation() {
        notice = nil
        publication = Presentation.empty()
        settings = publication.settings()
        snapshot = AgentCore.Snapshot.empty()
        isConnected = false
        loadingThreads = false
    }

    func openPairing() {
        persist()
        cancelPairing()
        navigationPath = [.profiles]
    }

    func cancelPairing() {
        pairingConnection?.cancel()
        pairingConnection = nil
        pairingError = nil
        pairingInvitation = nil
    }

    func preparePairing(_ contents: String) {
        guard pairingConnection == nil else { return }
        pairingInvitation = nil
        do {
            pairingInvitation = try parseInvitation(contents: contents, now: UInt64(Date().timeIntervalSince1970))
            pairingError = nil
        } catch { pairingError = error.localizedDescription }
    }

    func confirmPairing() {
        guard pairingConnection == nil, let invitation = pairingInvitation else { return }
        do {
            let id = try validateInvitation(invitation: invitation, now: UInt64(Date().timeIntervalSince1970))
            pairingError = nil
            pairingConnection = Task { [weak self] in
                guard let self else { return }
                do {
                    let storage = MobileSessionStorage(host: id)
                    let owner = try await sessions.pair(storage: storage, platform: storage, invitation: invitation)
                    guard !Task.isCancelled else { try? await owner.shutdown(); return }
                    let paired = profiles.filter { $0.id != id } + [
                        HostProfile(id: id, name: invitation.hostName, ticket: invitation.endpoint)
                    ]
                    try HostProfile.save(paired)
                    connections.removeValue(forKey: id)?.cancel()
                    observations.removeValue(forKey: id)?.cancel()
                    sessions.adopt(host: id, session: owner)
                    if selectedProfileId == id {
                        session = nil
                    }
                    guard !Task.isCancelled else { return }
                    profiles = paired
                    pairingInvitation = nil
                    pairingConnection = nil
                    selectProfile(id)
                } catch {
                    guard !Task.isCancelled else { return }
                    pairingConnection = nil
                    pairingError = error.localizedDescription
                }
            }
        } catch { pairingError = error.localizedDescription }
    }

    func perform(_ intent: Intent, completion: @escaping (Result<Outcome, Error>) -> Void = { _ in }) {
        guard let owner = session else {
            completion(.failure(CocoaError(.fileReadUnknown)))
            return
        }
        do {
            let receipt = try owner.dispatch(intent: intent)
            publish(owner.presentation())
            let host = selectedProfileId
            Task { [weak self] in
                let result: Result<Outcome, Error>
                do { result = try await .success(receipt.wait()) } catch { result = .failure(error) }
                guard let self else { return }
                if selectedProfileId == host, session === owner {
                    publish(owner.presentation())
                }
                completion(result)
            }
        } catch { completion(.failure(error)) }
    }
}

extension AppViewModel {
    func publish(_ value: Presentation) {
        let changes = value.changes(previous: publication)
        let next = value.snapshot()
        guard changes.changed else { return }
        updateProfile(next, host: selectedProfileId)
        if changes.list {
            updateTaskList()
            if list != nil {
                store?.recordConnectionEvent(phase: .listPublished, value: next.connected() ? 1 : 0)
            }
        }
        chat?.publish(value)
        publication = value
        if changes.settings {
            settings = value.settings()
        }
        updateConnection(connected: next.connected(), loading: next.operationRunning(key: .sessionList))
        let scopes: [ErrorScope] = [.application, .projects, .settings, .workspace]
        let notices = Dictionary(uniqueKeysWithValues: scopes.map { ($0, next.errorNotices(scope: $0)) })
        if self.notices != notices {
            self.notices = notices
        }
        snapshot = next
        synchronizeLiveActivities(foreground: UIApplication.shared.applicationState == .active)
        if let error = session?.persistenceError() {
            notice = error
        }
    }

    private func updateConnection(connected: Bool, loading: Bool) {
        if isConnected != connected {
            isConnected = connected
        }
        if loadingThreads != loading {
            loadingThreads = loading
        }
    }

    func updateConnectionState(_ snapshot: AgentCore.Snapshot, host: String) {
        let next = HostConnectionState(snapshot)
        if connectionStates[host] != next {
            connectionStates[host] = next
        }
    }

    func updateProfile(_ next: AgentCore.Snapshot, host: String?) {
        // The Host's latest addresses let the next cold connection dial it directly.
        if let index = profiles.firstIndex(where: { $0.id == host }) {
            var profile = profiles[index]
            profile.name = next.hostName() ?? profile.name
            profile.ticket = next.hostTicket() ?? profile.ticket
            if profile != profiles[index] {
                profiles[index] = profile
                do { try HostProfile.save(profiles) } catch { notice = error.localizedDescription }
            }
        }
    }

    func persist() {
        Task { [weak self] in
            guard let self else { return }
            do { try await sessions.flush() } catch { notice = error.localizedDescription }
        }
    }

    func persistBeforeBackground() async {
        do { try await sessions.flush() } catch { notice = error.localizedDescription }
        await liveActivities.flush()
    }
}
