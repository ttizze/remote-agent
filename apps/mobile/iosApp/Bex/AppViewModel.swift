import AgentCore
import Combine
import Foundation
import Network
import UIKit

@MainActor
final class BexAppViewModel: ObservableObject {
    @Published private(set) var snapshot = AgentCore.Snapshot.empty()
    @Published var screen: AppScreen = .profiles
    @Published var sideChatRequest: SideChatRequest?
    @Published var composerFocusRequest: UUID?
    @Published var isScanning = false
    @Published var transferError: String?
    @Published var transferring = false
    @Published var isConnecting = false
    @Published var pairingError: String?
    @Published private(set) var pairingInvitation: Invitation?
    @Published var notice: String?
    @Published var profiles: [HostProfile] = []
    @Published private(set) var selectedProfileId: String?
    @Published private(set) var conversation: ConversationPresentation?
    private(set) var list: ThreadList?
    private(set) var models: [Model] = []
    private var presentationTask: Task<Void, Never>?
    private var pendingPresentation: ConversationPresentationInput?

    private(set) var store: AgentStore?
    private var initialization: Task<Void, Never>?
    var observation: Task<Void, Never>?
    private var persistence: Task<Void, Never>?
    private var persistenceWrite: Task<Void, Never>?
    var connection: Task<Void, Never>?
    private let pathMonitor = NWPathMonitor()
    private var pending: [(Intent, (Result<Outcome, Error>) -> Void)] = []
    private var operations: [UUID: Task<Void, Never>] = [:]
    let liveActivities = TaskLiveActivities()

    init() {
        observeLiveActivityRequests()
        pathMonitor.pathUpdateHandler = { [weak self] path in
            let available = path.status == .satisfied
            Task { @MainActor [weak self] in
                guard let self, let owner = store else { return }
                await owner.networkChanged()
                if available, UIApplication.shared.applicationState == .active,
                   !isConnected, !isConnecting {
                    connect()
                }
            }
        }
        pathMonitor.start(queue: DispatchQueue(label: "bex.network-path"))
        do { profiles = try HostProfile.load() } catch { notice = error.localizedDescription }
        if let id = UserDefaults.standard.string(forKey: "bex.selected-host"),
           profiles.contains(where: { $0.id == id }) {
            selectProfile(id)
        }
    }

    deinit { pathMonitor.cancel() }

    func selectProfile(_ id: String) {
        guard profiles.contains(where: { $0.id == id }) else { return }
        screen = .threads
        if selectedProfileId == id, store != nil {
            connect(); return
        }
        let old = detachStore()
        selectedProfileId = id
        UserDefaults.standard.set(id, forKey: "bex.selected-host")
        publish(AgentCore.Snapshot.empty())
        let writing = persistenceWrite
        initialization = Task { [weak self] in
            await writing?.value
            await self?.initialize(id, previous: old)
        }
    }

    func removeProfile(_ id: String) {
        guard profiles.contains(where: { $0.id == id }) else { return }
        do {
            let remaining = profiles.filter { $0.id != id }
            try DeviceIdentity.remove(id)
            if selectedProfileId == id {
                let old = detachStore()
                selectedProfileId = nil
                UserDefaults.standard.removeObject(forKey: "bex.selected-host")
                notice = nil
                publish(AgentCore.Snapshot.empty())
                Task { [weak self] in
                    do { try await old?.shutdown() } catch { self?.notice = error.localizedDescription }
                }
            }
            profiles = remaining
            try HostProfile.save(profiles)
            screen = .profiles
        } catch { notice = error.localizedDescription }
    }

    /// Save and stop the current Host's work; the caller shuts down the returned store.
    private func detachStore() -> AgentStore? {
        persist()
        connection?.cancel()
        observation?.cancel()
        cancelInitialization()
        let old = store
        store = nil
        isConnecting = false
        return old
    }

    private func cancelInitialization() {
        initialization?.cancel()
        initialization = nil
        let cancelled = pending
        pending.removeAll()
        for (_, complete) in cancelled {
            complete(.failure(CancellationError()))
        }
    }

    private func initialize(_ id: String, previous old: AgentStore?) async {
        let preparationStarted = ProcessInfo.processInfo.systemUptime
        // Draining the previous Host's transport must not delay opening this Host.
        async let previousClosed: Void? = try? old?.shutdown()
        do {
            let bytes = try await SnapshotFiles.load(id)
            let snapshotRead = ProcessInfo.processInfo.systemUptime - preparationStarted
            let owner = try await AgentStore.offline(
                persisted: bytes,
                diagnosticsDirectory: SnapshotFiles.diagnosticsDirectory(id)
            )
            guard !Task.isCancelled, selectedProfileId == id else { try? await owner.shutdown(); return }
            store = owner
            owner.recordConnectionEvent(phase: .snapshotRead, value: UInt64(snapshotRead * 1_000_000))
            owner.recordConnectionEvent(
                phase: .appPreparation,
                value: UInt64((ProcessInfo.processInfo.systemUptime - preparationStarted) * 1_000_000)
            )
            owner.recordConnectionEvent(
                phase: .clientBuild,
                value: UInt64(Bundle.main.infoDictionary?["CFBundleVersion"] as? String ?? "0") ?? 0
            )
            initialization = nil
            publish(owner.snapshot())
            perform(.showThreadList)
            let queued = pending
            pending.removeAll()
            for (intent, complete) in queued {
                perform(intent, completion: complete)
            }
            observe(owner, host: id)
            connect()
        } catch {
            guard !Task.isCancelled, selectedProfileId == id else { return }
            initialization = nil
            let queued = pending
            pending.removeAll()
            for (_, complete) in queued {
                complete(.failure(error))
            }
        }
        _ = await previousClosed
    }

    func openPairing() {
        persist()
        connection?.cancel()
        isConnecting = false
        pairingError = nil
        pairingInvitation = nil
        screen = .pairing
    }

    func dismissPairing() {
        connection?.cancel()
        isConnecting = false
        pairingInvitation = nil
        screen = .profiles
    }

    func preparePairing(_ contents: String) {
        guard !isConnecting else { return }
        pairingInvitation = nil
        do {
            pairingInvitation = try parseInvitation(contents: contents, now: UInt64(Date().timeIntervalSince1970))
            pairingError = nil
        } catch { pairingError = error.localizedDescription }
    }

    func confirmPairing() {
        guard !isConnecting, let invitation = pairingInvitation else { return }
        do {
            let id = try validateInvitation(invitation: invitation, now: UInt64(Date().timeIntervalSince1970))
            pairingError = nil
            screen = .pairing
            isConnecting = true
            connection?.cancel()
            connection = Task { [weak self] in
                do {
                    await self?.persistenceWrite?.value
                    let persisted = try SnapshotFiles.withModelPreferences(Data())
                    let owner = try await AgentStore.connect(connection: Connection(
                        ticket: invitation.endpoint,
                        identity: DeviceIdentity.loadOrGenerate(id),
                        invitation: invitation.invitation,
                        useRelays: true
                    ), persisted: persisted, diagnosticsDirectory: SnapshotFiles.diagnosticsDirectory(id))
                    guard let self, !Task.isCancelled else { try? await owner.shutdown(); return }
                    persist()
                    observation?.cancel()
                    cancelInitialization()
                    let old = store
                    profiles.removeAll { $0.id == id }
                    profiles.append(HostProfile(id: id, name: invitation.hostName, ticket: invitation.endpoint))
                    try HostProfile.save(profiles)
                    UserDefaults.standard.set(id, forKey: "bex.selected-host")
                    selectedProfileId = id
                    store = owner
                    publish(owner.snapshot())
                    screen = .threads
                    pairingInvitation = nil
                    isConnecting = false
                    observe(owner, host: id)
                    perform(.loadHostName(LoadHostName()))
                    try? await old?.shutdown()
                } catch {
                    guard !Task.isCancelled else { return }
                    self?.isConnecting = false; self?.pairingError = error.localizedDescription
                }
            }
        } catch { pairingError = error.localizedDescription }
    }

    func perform(_ intent: Intent, completion: @escaping (Result<Outcome, Error>) -> Void = { _ in }) {
        guard let owner = store else {
            if initialization != nil {
                pending.append((intent, completion))
            } else {
                completion(.failure(CocoaError(.fileReadUnknown)))
            }
            return
        }
        do {
            let receipt = try owner.dispatch(intent: intent)
            publish(owner.snapshot())
            let id = UUID()
            let host = selectedProfileId
            operations[id] = Task { [weak self] in
                let result: Result<Outcome, Error>
                do { result = try await .success(receipt.wait()) } catch { result = .failure(error) }
                guard let self else { return }
                operations[id] = nil
                if selectedProfileId == host {
                    publish(owner.snapshot())
                    if case let .failure(error) = result {
                        notice = snapshot.error() ?? error.localizedDescription
                    }
                }
                completion(result)
            }
        } catch { completion(.failure(error)) }
    }
}

extension BexAppViewModel {
    func publish(_ next: AgentCore.Snapshot) {
        if let name = next.hostName(),
           let index = profiles.firstIndex(where: { $0.id == selectedProfileId }),
           profiles[index].name != name {
            profiles[index].name = name
            do { try HostProfile.save(profiles) } catch { notice = error.localizedDescription }
        }
        if snapshot.error() != next.error() {
            notice = next.error()
        }
        let listChanged = !next.listUnchanged(other: snapshot)
        if listChanged {
            list = next.threadList()
            if list != nil {
                store?.recordConnectionEvent(phase: .listPublished, value: next.connected() ? 1 : 0)
            }
        }
        if !next.modelsUnchanged(other: snapshot) {
            models = next.models()
        }
        let changed = !next.conversationUnchanged(other: snapshot)
        let source = next.conversationSource()
        snapshot = next
        synchronizeLiveActivities(foreground: UIApplication.shared.applicationState == .active)
        if changed {
            projectConversation(source)
        }
        persistence?.cancel()
        if listChanged {
            // Completion badges can outlive the process; do not debounce their write.
            persist()
        } else {
            persistence = Task { [weak self] in
                do { try await Task.sleep(nanoseconds: 250_000_000) } catch { return }
                self?.persist()
            }
        }
    }

    private func projectConversation(_ source: AgentCore.Thread?) {
        if conversation?.id != source?.id() {
            conversation = nil
        }
        pendingPresentation = ConversationPresentationInput(source: source, snapshot: snapshot, host: selectedProfileId)
        guard presentationTask == nil else { return }
        presentationTask = Task { [weak self] in
            while let self, let input = pendingPresentation {
                pendingPresentation = nil
                let previous = conversation
                let rendered = await Task.detached(priority: .userInitiated) {
                    ConversationPresentation.project(input.source, snapshot: input.snapshot, previous: previous)
                }.value
                if selectedProfileId == input.host, snapshot.conversationUnchanged(other: input.snapshot) {
                    conversation = rendered
                }
                // Coalesce updates off MainActor before publishing parsed, stably sized rows.
                try? await Task.sleep(nanoseconds: 100_000_000)
            }
            self?.presentationTask = nil
        }
    }

    func persist() {
        guard let id = selectedProfileId, let owner = store else { return }
        let current = owner.snapshot()
        let previous = persistenceWrite
        // Serialize immutable snapshots off MainActor and commit writes in order.
        persistenceWrite = Task { [weak self] in
            await previous?.value
            do {
                try await SnapshotFiles.save(id, snapshot: current)
            } catch { self?.notice = error.localizedDescription }
        }
    }

    func persistBeforeBackground() async {
        for operation in Array(operations.values) {
            await operation.value
        }
        persist()
        await persistenceWrite?.value
        await liveActivities.flush()
    }
}
