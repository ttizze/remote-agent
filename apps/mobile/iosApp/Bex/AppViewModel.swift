import AgentCore
import Combine
import Foundation
import OSLog

private let connectionPerformance = Logger(subsystem: "app.bex.BEX", category: "connection-performance")

@MainActor
final class BexAppViewModel: ObservableObject {
    @Published private(set) var snapshot = AgentCore.Snapshot.empty()
    @Published var screen: AppScreen = .profiles
    @Published var sideChatRequest: SideChatRequest?
    @Published var composerFocusRequest: UUID?
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
        connectionPerformance.info("app_model_started")
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

    func removeProfile(_ id: String) {
        guard profiles.contains(where: { $0.id == id }) else { return }
        do {
            let remaining = profiles.filter { $0.id != id }
            let encoded = try JSONEncoder().encode(remaining)
            try DeviceIdentity.remove(id)
            if selectedProfileId == id {
                persist()
                connection?.cancel()
                observation?.cancel()
                initialization?.cancel()
                initialization = nil
                let old = store
                store = nil
                selectedProfileId = nil
                UserDefaults.standard.removeObject(forKey: "bex.selected-host")
                let cancelled = pending
                pending.removeAll()
                for (_, complete) in cancelled {
                    complete(.failure(CancellationError()))
                }
                isConnecting = false
                notice = nil
                connectionError = nil
                publish(AgentCore.Snapshot.empty())
                Task { [weak self] in
                    do { try await old?.shutdown() } catch { self?.notice = error.localizedDescription }
                }
            }
            profiles = remaining
            UserDefaults.standard.set(encoded, forKey: "bex.hosts.iroh")
            screen = .profiles
        } catch { notice = error.localizedDescription }
    }

    private func initialize(_ id: String, previous old: AgentStore?) async {
        connectionPerformance.info("store_initialization_started")
        if let old {
            try? await old.shutdown()
        }
        do {
            let bytes = try await SnapshotFiles.load(id)
            connectionPerformance.info("persisted_snapshot_loaded")
            let owner = try await AgentStore.offline(persisted: bytes)
            guard !Task.isCancelled, selectedProfileId == id else { try? await owner.shutdown(); return }
            store = owner
            connectionPerformance.info("store_ready")
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
                    guard !Task.isCancelled else { try? await owner.shutdown(); return }
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
                } catch {
                    guard !Task.isCancelled else { return }
                    self?.isConnecting = false; self?.pairingError = error.localizedDescription
                }
            }
        } catch { pairingError = error.localizedDescription }
    }

    func connect(afterForeground: Bool = false) {
        guard let owner = store, let profile = profiles.first(where: { $0.id == selectedProfileId }),
              !isConnecting || afterForeground else { return }
        connection?.cancel()
        isConnecting = true
        connectionError = nil
        notice = nil
        connection = Task { [weak self] in
            let started = ProcessInfo.processInfo.systemUptime
            connectionPerformance.info("connection_started foreground=\(afterForeground)")
            do {
                try await owner.resume(connection: Connection(ticket: profile.ticket,
                                                              identity: DeviceIdentity.loadOrGenerate(profile.id),
                                                              invitation: nil, useRelays: true))
                guard let self, selectedProfileId == profile.id, !Task.isCancelled else { return }
                let elapsed = (ProcessInfo.processInfo.systemUptime - started) * 1000
                connectionPerformance.info("connection_ready elapsed_ms=\(elapsed)")
                publish(owner.snapshot())
                notice = snapshot.error()
                isConnecting = false
            } catch {
                let elapsed = (ProcessInfo.processInfo.systemUptime - started) * 1000
                connectionPerformance.info("connection_ended elapsed_ms=\(elapsed) cancelled=\(Task.isCancelled)")
                guard self?.selectedProfileId == profile.id, !Task.isCancelled else { return }
                self?.isConnecting = false
                self?.connectionError = error.localizedDescription
                self?.notice = error.localizedDescription
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
                        notice = snapshot.error() ?? error.localizedDescription
                    }
                }
                completion(result)
            }
        } catch { completion(.failure(error)) }
    }
}

/// Snapshot observation, persistence and foreground recovery.
extension BexAppViewModel {
    func browser(_ request: BrowserRequest) async throws -> BrowserFrame {
        guard let owner = store else { throw URLError(.notConnectedToInternet) }
        let host = selectedProfileId
        let frame = try await owner.browser(request: request)
        guard host == selectedProfileId else { throw CancellationError() }
        return frame
    }

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
                    if previous.connected(), !latest.connected(), !isConnecting {
                        // Recover a lost established connection once. If it
                        // fails, the next foreground activation retries it.
                        connect()
                    }
                    previous = latest
                } catch { return }
            }
        }
    }

    private func publish(_ next: AgentCore.Snapshot) {
        if snapshot.error() != next.error() {
            notice = next.error()
        }
        let listChanged = !next.listUnchanged(other: snapshot)
        if listChanged {
            list = next.threadList()
            let hasList = list != nil
            connectionPerformance.info("list_published connected=\(next.connected()) has_list=\(hasList)")
        }
        if !next.modelsUnchanged(other: snapshot) {
            models = next.models()
        }
        let changed = !next.conversationUnchanged(other: snapshot)
        let source = next.conversationSource()
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
                let previous = conversation
                let rendered = await Task.detached(priority: .userInitiated) {
                    ConversationPresentation.project(input.source, snapshot: input.snapshot, previous: previous)
                }.value
                if selectedProfileId == input.host, snapshot.requestsUnchanged(other: input.snapshot),
                   snapshot.navigation().draftKey == input.snapshot.navigation().draftKey {
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
}
