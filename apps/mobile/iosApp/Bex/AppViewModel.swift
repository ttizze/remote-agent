import AgentCore
import Combine
import Foundation

@MainActor
final class BexAppViewModel: ObservableObject {
    @Published private(set) var snapshot = AgentCore.Snapshot.empty()
    @Published var screen: AppScreen = .profiles
    @Published var isScanning = false
    @Published var transferError: String?
    @Published var transferring = false
    @Published var sending = false
    @Published var transcribing = false
    @Published var isConnecting = false
    @Published var pairingError: String?
    @Published var connectionError: String?
    @Published var notice: String?
    @Published var loadingThreads = false
    @Published var loadingHistory = false
    @Published var interruptingTurnId: String?
    @Published var profiles: [HostProfile] = []
    @Published private(set) var selectedProfileId: String?
    @Published private(set) var conversation: ConversationPresentation?
    private(set) var list: ThreadList?
    private(set) var models: [Model] = []
    private let presentation = ConversationPresentationCache()
    private var presentationTask: Task<Void, Never>?
    private var pendingPresentation: PresentationInput?
    private struct PresentationInput {
        let source: AgentCore.Thread?
        let snapshot: AgentCore.Snapshot
        let host: String?
    }

    private var store: AgentStore?
    private var initialization: Task<Void, Never>?
    private var observation: Task<Void, Never>?
    private var persistence: Task<Void, Never>?
    private var persistenceWrite: Task<Void, Never>?
    private var connection: Task<Void, Never>?
    private var pending: [(Intent, (Result<Outcome, Error>) -> Void)] = []
    private var operations: [UUID: Task<Void, Never>] = [:]

    init() {
        if let data = UserDefaults.standard.data(forKey: "bex.hosts.iroh") {
            do {
                profiles = try JSONDecoder().decode([HostProfile].self, from: data)
            } catch { notice = error.localizedDescription }
        }
        if let id = UserDefaults.standard.string(forKey: "bex.selected-host"),
           profiles.contains(where: { $0.id == id }) {
            selectProfile(id)
        }
    }

    func selectProfile(_ id: String) {
        guard profiles.contains(where: { $0.id == id }) else { return }
        screen = .threads
        if selectedProfileId == id, store != nil {
            connect(); return
        }
        persist()
        connection?.cancel()
        observation?.cancel()
        initialization?.cancel()
        let old = store
        store = nil
        let cancelled = pending
        pending.removeAll()
        for (_, complete) in cancelled {
            complete(.failure(CancellationError()))
        }
        isConnecting = false
        selectedProfileId = id
        UserDefaults.standard.set(id, forKey: "bex.selected-host")
        publish(AgentCore.Snapshot.empty())
        let writing = persistenceWrite
        initialization = Task { [weak self] in
            await writing?.value
            await self?.initialize(id, previous: old)
        }
    }

    private func initialize(_ id: String, previous old: AgentStore?) async {
        if let old {
            try? await old.shutdown()
        }
        do {
            let bytes = try await SnapshotFiles.load(id)
            let owner = try await AgentStore.offline(persisted: bytes)
            guard !Task.isCancelled, selectedProfileId == id else { try? await owner.shutdown(); return }
            store = owner
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
            guard selectedProfileId == id else { return }
            connectionError = error.localizedDescription
            initialization = nil
            let queued = pending
            pending.removeAll()
            for (_, complete) in queued {
                complete(.failure(error))
            }
        }
    }

    func pair(_ contents: String) {
        do {
            let invitation = try parseInvitation(contents: contents, now: UInt64(Date().timeIntervalSince1970))
            let id = try ticketIdentity(ticket: invitation.endpoint)
            pairingError = nil
            isConnecting = true
            connection?.cancel()
            connection = Task { [weak self] in
                do {
                    let identity = try DeviceIdentity.loadOrGenerate(id)
                    let owner = try await AgentStore.connect(connection: Connection(
                        ticket: invitation.endpoint,
                        identity: identity,
                        invitation: invitation.invitation,
                        useRelays: true
                    ), persisted: Data())
                    guard let self, !Task.isCancelled else { try? await owner.shutdown(); return }
                    persist()
                    observation?.cancel()
                    initialization?.cancel()
                    if let old = store {
                        try? await old.shutdown()
                    }
                    profiles.removeAll { $0.id == id }
                    profiles.append(HostProfile(id: id, name: "PC Host", ticket: invitation.endpoint))
                    try UserDefaults.standard.set(JSONEncoder().encode(profiles), forKey: "bex.hosts.iroh")
                    UserDefaults.standard.set(id, forKey: "bex.selected-host")
                    selectedProfileId = id
                    store = owner
                    publish(owner.snapshot())
                    screen = .threads
                    isConnecting = false
                    observe(owner, host: id)
                } catch { self?.isConnecting = false; self?.pairingError = error.localizedDescription }
            }
        } catch { pairingError = error.localizedDescription }
    }

    func connect(force: Bool = false) {
        guard let owner = store, let profile = profiles.first(where: { $0.id == selectedProfileId }),
              !isConnecting else { return }
        if !force, snapshot.connected() {
            refreshTaskList()
            if let id = snapshot.navigation().threadId {
                perform(.readThread(ReadThread(threadId: id)))
            }
            return
        }
        isConnecting = true
        connectionError = nil
        connection = Task { [weak self] in
            do {
                try await owner.reconnect(connection: Connection(ticket: profile.ticket,
                                                                 identity: DeviceIdentity.loadOrGenerate(profile.id),
                                                                 invitation: nil, useRelays: true))
                guard let self, selectedProfileId == profile.id, !Task.isCancelled else { return }
                publish(owner.snapshot())
                isConnecting = false
            } catch {
                guard self?.selectedProfileId == profile.id else { return }
                self?.isConnecting = false
                self?.connectionError = error.localizedDescription
            }
        }
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
                        notice = error.localizedDescription
                    }
                }
                completion(result)
            }
        } catch { completion(.failure(error)) }
    }
}

/// Snapshot observation, persistence and foreground recovery.
extension BexAppViewModel {
    private func observe(_ owner: AgentStore, host: String) {
        let initial = snapshot
        observation = Task { [weak self] in
            var previous = initial
            while !Task.isCancelled {
                do {
                    _ = try await owner.nextSnapshot(previous: previous)
                    guard let self, selectedProfileId == host, !Task.isCancelled else { return }
                    let latest = owner.snapshot()
                    publish(latest)
                    previous = latest
                } catch { return }
            }
        }
    }

    private func publish(_ next: AgentCore.Snapshot) {
        let listChanged = !next.listUnchanged(other: snapshot)
        if listChanged {
            list = next.threadList()
        }
        if !next.modelsUnchanged(other: snapshot) {
            models = next.models()
        }
        let changed = !next.conversationUnchanged(other: snapshot)
        let source = next.navigation().threadId.flatMap { next.conversation(id: $0) }
        snapshot = next
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
        pendingPresentation = PresentationInput(source: source, snapshot: snapshot, host: selectedProfileId)
        guard presentationTask == nil else { return }
        presentationTask = Task { [weak self] in
            while let self, let input = pendingPresentation {
                pendingPresentation = nil
                let rendered = await presentation.project(input.source, snapshot: input.snapshot)
                if selectedProfileId == input.host, snapshot.requestsUnchanged(other: input.snapshot),
                   snapshot.navigation().threadId == input.source?.id() {
                    conversation = rendered
                }
                // Keep one background projection in flight and coalesce stream deltas.
                // Rows enter the lazy stack with parsed Markdown and a stable initial height.
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
        let active = Array(operations.values)
        for operation in active {
            await operation.value
        }
        persist()
        await persistenceWrite?.value
    }

    func restoreAfterForeground() {
        connect(force: true)
    }
}
