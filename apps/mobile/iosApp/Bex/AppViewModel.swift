import AgentCore
import Combine
import Foundation
import UIKit

@MainActor
final class BexAppViewModel: ObservableObject {
    @Published private(set) var snapshot = AgentCore.Snapshot.empty()
    @Published var screen: AppScreen = .profiles
    @Published var isScanning = false
    @Published var isConnecting = false
    @Published var pairingError: String?
    @Published var pairingInvitation: Invitation?
    @Published var notice: String?
    @Published var profiles: [HostProfile] = []
    @Published private(set) var selectedProfileId: String?
    @Published var composerText = ""
    var draftEdits = DraftRevision()
    private var composerKey = ""
    @Published var threadView: ThreadView?
    @Published var disclosure = TimelineDisclosure.empty {
        didSet {
            if disclosure != oldValue {
                schedulePresentation()
            }
        }
    }

    @Published var showScrollToEnd = false {
        didSet {
            if showScrollToEnd != oldValue {
                schedulePresentation()
            }
        }
    }

    var presentation: Task<Void, Never>?
    var presentationTick: Task<Void, Never>?

    private(set) var store: AgentStore?
    private var initialization: Task<Void, Never>?
    private var observation: Task<Void, Never>?
    private var persistence: Task<Void, Never>?
    private var persistenceWrite: Task<Void, Never>?
    var connection: Task<Void, Never>?
    private var pending: [(Intent, (Result<Outcome, Error>) -> Void)] = []
    private var operations: [UUID: Task<Void, Never>] = [:]

    init() {
        do { profiles = try HostProfile.load() } catch { notice = error.localizedDescription }
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

    private func detachStore() -> AgentStore? {
        persist()
        draftEdits.reset()
        presentation?.cancel()
        presentation = nil
        presentationTick?.cancel()
        threadView = nil
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
        async let previousClosed: Void? = try? old?.shutdown()
        do {
            let bytes = try await SnapshotFiles.load(id)
            let snapshotRead = ProcessInfo.processInfo.systemUptime - preparationStarted
            let owner = try await AgentStore.offline(
                persisted: bytes,
                cacheDirectory: SnapshotFiles.cacheDirectory(id),
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
            notice = error.localizedDescription
            let queued = pending
            pending.removeAll()
            for (_, complete) in queued {
                complete(.failure(error))
            }
        }
        _ = await previousClosed
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
                    self?.persist()
                    await self?.persistenceWrite?.value
                    let persisted = try await SnapshotFiles.load(id)
                    let owner = try await AgentStore.connect(connection: Connection(
                        ticket: invitation.endpoint,
                        identity: DeviceIdentity.loadOrGenerate(id),
                        invitation: invitation.invitation,
                        useRelays: true
                    ), persisted: persisted, cacheDirectory: SnapshotFiles.cacheDirectory(id),
                    diagnosticsDirectory: SnapshotFiles.diagnosticsDirectory(
                        id
                    ))
                    guard let self, !Task.isCancelled else { try? await owner.shutdown(); return }
                    persist()
                    observation?.cancel()
                    cancelInitialization()
                    let old = store
                    presentation?.cancel()
                    presentation = nil
                    store = nil
                    publish(AgentCore.Snapshot.empty())
                    draftEdits.reset()
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
                if selectedProfileId == host, store === owner {
                    publish(owner.snapshot())
                    if case let .failure(error) = result {
                        notice = snapshot.error() ?? error.localizedDescription
                    }
                    completion(result)
                } else {
                    completion(.failure(CancellationError()))
                }
            }
        } catch { completion(.failure(error)) }
    }
}

extension BexAppViewModel {
    func connect(afterForeground: Bool = false) {
        guard screen != .pairing, let owner = store,
              let profile = profiles.first(where: { $0.id == selectedProfileId }),
              !isConnecting || afterForeground else { return }
        if afterForeground {
            owner.appBecameActive()
        }
        connection?.cancel()
        isConnecting = true
        notice = nil
        connection = Task { [weak self] in
            let started = ProcessInfo.processInfo.systemUptime
            self?.recordScene(UIApplication.shared.applicationState == .active ? 1 : 2)
            owner.recordConnectionEvent(phase: .uiConnectStart, value: afterForeground ? 1 : 0)
            do {
                let identityStarted = ProcessInfo.processInfo.systemUptime
                let identity = try DeviceIdentity.loadOrGenerate(profile.id)
                owner.recordConnectionEvent(
                    phase: .identityRead,
                    value: UInt64((ProcessInfo.processInfo.systemUptime - identityStarted) * 1_000_000)
                )
                try await owner.resume(connection: Connection(ticket: profile.ticket,
                                                              identity: identity,
                                                              invitation: nil, useRelays: true))
                guard let self, selectedProfileId == profile.id, store === owner, !Task.isCancelled else { return }
                let elapsed = (ProcessInfo.processInfo.systemUptime - started) * 1_000_000
                owner.recordConnectionEvent(phase: .uiConnectReady, value: UInt64(elapsed))
                publish(owner.snapshot())
                notice = snapshot.error()
                isConnecting = false
            } catch {
                owner.recordConnectionEvent(
                    phase: Task.isCancelled ? .uiConnectCancelled : .uiConnectFailed,
                    value: UInt64((ProcessInfo.processInfo.systemUptime - started) * 1_000_000)
                )
                guard self?.selectedProfileId == profile.id, self?.store === owner, !Task.isCancelled else { return }
                self?.isConnecting = false
                self?.notice = error.localizedDescription
            }
        }
    }

    private func observe(_ owner: AgentStore, host: String) {
        let initial = snapshot
        observation = Task { [weak self] in
            var previous = initial
            while !Task.isCancelled {
                do {
                    _ = try await owner.nextSnapshot(previous: previous)
                    guard let self, selectedProfileId == host, store === owner, !Task.isCancelled else { return }
                    let latest = owner.snapshot()
                    publish(latest)
                    if previous.connected(), !latest.connected(), !isConnecting {
                        connect()
                    }
                    previous = latest
                } catch { return }
            }
        }
    }

    private func publish(_ next: AgentCore.Snapshot) {
        if !next.supersedes(previous: snapshot) {
            return
        }
        if next === snapshot {
            return
        }
        if let name = next.hostName(),
           let index = profiles.firstIndex(where: { $0.id == selectedProfileId }),
           profiles[index].name != name {
            profiles[index].name = name
            do { try HostProfile.save(profiles) } catch { notice = error.localizedDescription }
        }
        if snapshot.error() != next.error() {
            notice = next.error()
        }
        let threadChanged = snapshot.selectedThreadId() != next.selectedThreadId()
        snapshot = next
        if threadChanged {
            threadView = nil
            showScrollToEnd = false
            disclosure = .empty
        }
        let key = next.currentDraftKey()
        if key != composerKey {
            draftEdits.reset(); composerKey = key
        }
        if draftEdits.pending == nil {
            composerText = next.draft().text
            draftEdits.base = composerText
        }
        schedulePresentation()
        persistence?.cancel()
        persistence = Task { [weak self] in
            do { try await Task.sleep(nanoseconds: 250_000_000) } catch { return }
            self?.persist()
        }
    }

    func persist() {
        guard let id = selectedProfileId, let owner = store else { return }
        let current = owner.snapshot()
        let previous = persistenceWrite
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
    }
}
