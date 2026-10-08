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
    /// Counts requests to focus the composer with the cursor at the end of the draft.
    @Published var composerFocusRequests = 0
    var draftEdits = DraftRevision()
    private var composerKey = ""
    @Published var timelineRows: [TimelineRow] = []
    @Published var threadView: ThreadView?
    /// Set by a WidgetKit URL and consumed by the native Usage navigation.
    @Published private(set) var usageDeepLinkRequests = 0
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
    private var pushRegistration: AgentPushRegistration?
    private var pendingPushActive: (deviceId: String, active: Bool)?
    private var pendingPushThread: (hostId: String, threadId: String)?
    private var activityUpdater: ((AgentActivityAttributes.ContentState?) -> Void)?

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
        connection?.cancel()
        isConnecting = false
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
                connection?.cancel()
                isConnecting = false
                let unregistration = unregisterPush()
                let old = detachStore(deactivatePush: false)
                selectedProfileId = nil
                UserDefaults.standard.removeObject(forKey: "bex.selected-host")
                notice = nil
                publish(AgentCore.Snapshot.empty())
                Task { [weak self] in
                    await unregistration?.value
                    do { try await old?.shutdown() } catch { self?.notice = error.localizedDescription }
                }
            }
            profiles = remaining
            try HostProfile.save(profiles)
            screen = .profiles
        } catch { notice = error.localizedDescription }
    }

    private func detachStore(deactivatePush: Bool = true) -> AgentStore? {
        persist()
        if deactivatePush {
            self.deactivatePush()
        }
        draftEdits.reset()
        presentation?.cancel()
        presentation = nil
        presentationTick?.cancel()
        threadView = nil
        timelineRows = []
        observation?.cancel()
        cancelInitialization()
        let old = store
        store = nil
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
            let owner = try await AgentStore.offline(
                stateFile: SnapshotFiles.stateFile(id),
                modelDefaults: SnapshotFiles.modelDefaults(),
                cacheDirectory: SnapshotFiles.cacheDirectory(id),
                diagnosticsDirectory: SnapshotFiles.diagnosticsDirectory(id)
            )
            guard !Task.isCancelled, selectedProfileId == id else { try? await owner.shutdown(); return }
            store = owner
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
            registerPushIfReady()
            openPendingPushThreadIfReady()
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
                    // The new store reads the state the current one keeps for this Host.
                    if self?.selectedProfileId == id {
                        try? await self?.store?.flush()
                    }
                    let owner = try await AgentStore.connect(connection: Connection(
                        ticket: invitation.endpoint,
                        identity: DeviceIdentity.loadOrGenerate(id),
                        invitation: invitation.invitation,
                        useRelays: true
                    ), stateFile: SnapshotFiles.stateFile(id), modelDefaults: SnapshotFiles.modelDefaults(),
                    cacheDirectory: SnapshotFiles.cacheDirectory(id),
                    diagnosticsDirectory: SnapshotFiles.diagnosticsDirectory(
                        id
                    ))
                    guard let self, !Task.isCancelled else { try? await owner.shutdown(); return }
                    let old = detachStore()
                    publish(AgentCore.Snapshot.empty())
                    profiles.removeAll { $0.id == id }
                    profiles.append(HostProfile(id: id, name: invitation.hostName, ticket: invitation.endpoint))
                    try HostProfile.save(profiles)
                    UserDefaults.standard.set(id, forKey: "bex.selected-host")
                    selectedProfileId = id
                    store = owner
                    publish(owner.snapshot())
                    registerPushIfReady()
                    screen = .threads
                    openPendingPushThreadIfReady()
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

    func registerPush(_ registration: AgentPushRegistration) {
        pushRegistration = registration
        registerPushIfReady()
    }

    func setActivityUpdater(_ updater: @escaping (AgentActivityAttributes.ContentState?) -> Void) {
        activityUpdater = updater
        updater(activityContentState())
    }

    /// Projects the core awareness snapshot into the shared ActivityKit
    /// record. Host state and phase decisions remain core-owned; this method
    /// only supplies Codable fields to the lifecycle owner.
    private func activityContentState() -> AgentActivityAttributes.ContentState? {
        let activities = snapshot.awarenessActivities()
        guard !activities.isEmpty else { return nil }
        let items = activities.map { activity in
            let phase = canonicalActivityPhase(activity.phase)
            return AgentActivityAttributes.Item(
                environmentId: activity.environmentId,
                threadId: activity.threadId,
                projectTitle: activity.projectTitle,
                threadTitle: activity.threadTitle,
                modelTitle: activity.modelTitle ?? "Model",
                phase: phase,
                status: activityStatus(phase),
                updatedAt: activityTimestamp(activity.updatedAtMs),
                deepLink: AgentPushCenter.threadDeepLink(
                    hostId: activity.environmentId,
                    threadId: activity.threadId
                )
            )
        }
        let activeCount = items.reduce(into: UInt32(0)) { result, item in
            if !["completed", "failed", "stale"].contains(item.phase) {
                result += 1
            }
        }
        let subtitle: String
        if activeCount == 1, let item = items.first, items.count == 1 {
            subtitle = item.status
        } else if activeCount > 0 {
            subtitle = "\(activeCount) active agent activities"
        } else {
            subtitle = items.first?.status ?? "Agent activity"
        }
        return AgentActivityAttributes.ContentState(
            title: items.first?.projectTitle ?? "Agent activity",
            subtitle: subtitle,
            activeCount: activeCount,
            updatedAt: items.map(\.updatedAt).max() ?? activityTimestamp(0),
            activities: items
        )
    }

    private func canonicalActivityPhase(_ value: String) -> String {
        switch value {
        case "waitingApproval", "waiting_for_approval": return "waiting_for_approval"
        case "waitingInput", "waiting_for_input": return "waiting_for_input"
        case "starting", "running", "completed", "failed", "stale": return value
        default: return "stale"
        }
    }

    private func activityStatus(_ phase: String) -> String {
        switch phase {
        case "starting": return "Connecting"
        case "running": return "Working"
        case "waiting_for_approval": return "Approval"
        case "waiting_for_input": return "Input"
        case "completed": return "Done"
        case "failed": return "Failed"
        default: return "Waiting"
        }
    }

    private func activityTimestamp(_ millis: Int64) -> String {
        ISO8601DateFormatter().string(from: Date(timeIntervalSince1970: TimeInterval(max(0, millis)) / 1_000))
    }

    /// Core owns the notification mode; the UIApplication delegate only adds
    /// the OS authorization state before registering the Host token.
    func pushPreferences() -> AgentPushPreferences {
        let mode = snapshot.preferences().notificationMode
        let notificationsEnabled = mode == .notifications || mode == .notificationsAndSound
        return AgentPushPreferences(
            notificationsEnabled: notificationsEnabled,
            notifyOnApproval: notificationsEnabled,
            notifyOnInput: notificationsEnabled,
            notifyOnCompletion: notificationsEnabled,
            notifyOnFailure: notificationsEnabled,
            liveActivitiesEnabled: true
        )
    }

    func setPushActive(deviceId: String, active: Bool) {
        pendingPushActive = (deviceId, active)
        guard store != nil else { return }
        pendingPushActive = nil
        perform(.setPushDeviceActive(deviceId: deviceId, active: active))
    }

    func visiblePushThreadDeepLink() -> String? {
        guard screen == .thread, let host = selectedProfileId, let thread = selectedThreadId else {
            return nil
        }
        return AgentPushCenter.threadDeepLink(hostId: host, threadId: thread)
    }

    func openPushThread(hostId: String, threadId: String) {
        guard profiles.contains(where: { $0.id == hostId }) else { return }
        guard selectedProfileId == hostId, store != nil else {
            pendingPushThread = (hostId, threadId)
            if selectedProfileId != hostId {
                selectProfile(hostId)
            }
            return
        }
        openThread(threadId)
    }

    func openUsageDeepLink() {
        usageDeepLinkRequests += 1
    }

    /// The Usage root calls this after presenting the requested limits tab.
    func consumeUsageDeepLinkRequest() {
        guard usageDeepLinkRequests > 0 else { return }
        usageDeepLinkRequests -= 1
    }

    private func registerPushIfReady() {
        guard store != nil else { return }
        guard let registration = pushRegistration else {
            applyPendingPushActiveIfReady()
            return
        }
        let preferences = pushPreferences()
        let native = PushDeviceRegistration(
            deviceId: registration.deviceId,
            platform: "ios",
            token: registration.token,
            liveActivityToken: registration.liveActivityToken,
            pushToStartToken: registration.pushToStartToken,
            bundleId: registration.bundleId,
            apnsEnvironment: registration.apnsEnvironment,
            notificationsEnabled: registration.notificationsEnabled && preferences.notificationsEnabled,
            notifyOnApproval: preferences.notifyOnApproval,
            notifyOnInput: preferences.notifyOnInput,
            notifyOnCompletion: preferences.notifyOnCompletion,
            notifyOnFailure: preferences.notifyOnFailure,
            liveActivitiesEnabled: registration.liveActivitiesEnabled && preferences.liveActivitiesEnabled
        )
        perform(.registerPushDevice(registration: native)) { [weak self] result in
            guard case .success = result else { return }
            self?.setPushActive(
                deviceId: registration.deviceId,
                active: native.notificationsEnabled || native.liveActivitiesEnabled
            )
        }
    }

    private func applyPendingPushActiveIfReady() {
        guard let pending = pendingPushActive, store != nil else { return }
        pendingPushActive = nil
        perform(.setPushDeviceActive(deviceId: pending.deviceId, active: pending.active))
    }

    private func openPendingPushThreadIfReady() {
        guard let pending = pendingPushThread,
              pending.hostId == selectedProfileId,
              store != nil else { return }
        pendingPushThread = nil
        openThread(pending.threadId)
    }

    private func deactivatePush() {
        guard let registration = pushRegistration, store != nil else { return }
        setPushActive(deviceId: registration.deviceId, active: false)
        pendingPushActive = nil
    }

    private func unregisterPush() -> Task<Void, Never>? {
        guard let registration = pushRegistration, let owner = store else { return nil }
        do {
            let receipt = try owner.dispatch(
                intent: .unregisterPushDevice(deviceId: registration.deviceId)
            )
            pendingPushActive = nil
            return Task { _ = try? await receipt.wait() }
        } catch {
            pendingPushActive = nil
            return nil
        }
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
        let notificationModeChanged = snapshot.preferences().notificationMode != next.preferences().notificationMode
        let threadChanged = snapshot.selectedThreadId() != next.selectedThreadId()
        snapshot = next
        activityUpdater?(activityContentState())
        if notificationModeChanged {
            registerPushIfReady()
        }
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

    /// Saves the model preferences every Host shares; the store writes its own state.
    func persist() {
        guard let owner = store else { return }
        let current = owner.snapshot()
        let previous = persistenceWrite
        persistenceWrite = Task { [weak self] in
            await previous?.value
            do {
                try await SnapshotFiles.saveModelPreferences(current)
            } catch { self?.notice = error.localizedDescription }
        }
    }

    func persistBeforeBackground() async {
        persist()
        await persistenceWrite?.value
        try? await store?.flush()
    }
}
