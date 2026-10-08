import AgentCore
import Combine
import Foundation
import UIKit

@MainActor
final class BexAppViewModel: ObservableObject {
    private struct PushActivitySource: Encodable {
        let environmentId: String
        let threadId: String
        let projectTitle: String
        let threadTitle: String
        let modelTitle: String
        let phase: String
        let headline: String
        let updatedAtMs: Int64
        let deepLink: String
    }

    private struct PendingLoadBalancedNewThread {
        let projectId: String
        let sourceEnvironmentId: String
        let startedAt: Date
    }

    @Published private(set) var snapshot = AgentCore.Snapshot.empty()
    @Published var screen: AppScreen = .profiles
    @Published var isScanning = false
    @Published var isConnecting = false
    @Published var pairingError: String?
    @Published var pairingInvitation: Invitation?
    @Published var notice: String?
    @Published var profiles: [HostProfile] = []
    @Published private(set) var environments: [EnvironmentRow] = []
    /// Latest immutable core snapshot for each saved environment.
    @Published private(set) var environmentSnapshots: [String: AgentCore.Snapshot] = [:]
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
    private var backgroundOwners: [String: AgentStore] = [:]
    private var backgroundTasks: [String: Task<Void, Never>] = [:]
    private var initialization: Task<Void, Never>?
    private var observation: Task<Void, Never>?
    private var persistence: Task<Void, Never>?
    private var persistenceWrite: Task<Void, Never>?
    var connection: Task<Void, Never>?
    private var pending: [(Intent, (Result<Outcome, Error>) -> Void)] = []
    private var operations: [UUID: Task<Void, Never>] = [:]
    private var pushRegistrations: [String: AgentPushRegistration] = [:]
    private var pushDeviceIdProvider: ((String) -> String?)?
    private var pendingPushActive: [String: (deviceId: String, active: Bool)] = [:]
    private var pushGenerations: [String: UInt64] = [:]
    /// The Host store that has accepted the current registration. A retained
    /// registration must be replayed when a profile switches stores, even if
    /// its APNs token did not change.
    private var registeredPushOwners: [String: AgentStore] = [:]
    private var registeredPushConfigurations: [String: PushDeviceRegistration] = [:]
    private var pendingPushThread: (hostId: String, threadId: String)?
    private var activityUpdater: (([String: AgentActivityAttributes.ContentState]) -> Void)?
    private var incomingShareHandoffsInFlight: Set<URL> = []
    private var pendingLoadBalancedNewThread: PendingLoadBalancedNewThread?
    private let usageWidget = UsageWidgetPublisher()

    init() {
        do { profiles = try HostProfile.load() } catch { notice = error.localizedDescription }
        if let id = UserDefaults.standard.string(forKey: "bex.selected-host"),
           profiles.contains(where: { $0.id == id }) {
            selectProfile(id)
        } else {
            startBackgroundProfiles(nil)
        }
    }

    func selectProfile(_ id: String) {
        guard profiles.contains(where: { $0.id == id }) else { return }
        pendingLoadBalancedNewThread = nil
        screen = .threads
        if selectedProfileId == id, store != nil {
            connect(); return
        }
        connection?.cancel()
        isConnecting = false
        let background = backgroundOwners.removeValue(forKey: id)
        backgroundTasks.removeValue(forKey: id)?.cancel()
        Task { try? await background?.shutdown() }
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

    func handleShortcut() {
        screen = .threads
        openNewThread(projectId: snapshot.selectedProjectId())
    }

    /// Opens a new draft on the selected repository's least-loaded connected
    /// environment. Core owns candidate matching and capacity scoring; this
    /// owner only promotes the Store and reapplies the source draft's
    /// user-selected model options and modes.
    func openNewThread(projectId: String?) {
        guard let projectId,
              let sourceEnvironmentId = snapshot.environmentId()
        else {
            pendingLoadBalancedNewThread = nil
            perform(.newThread(projectId: projectId))
            return
        }
        // A project chosen from the aggregate picker is already an explicit
        // environment route. Automatic balancing only applies to the source
        // Host's local project selection.
        if scopedValue(projectId).1 != nil {
            pendingLoadBalancedNewThread = nil
            perform(.newThread(projectId: projectId))
            return
        }
        let evaluation = AgentCore.environmentLoadBalancingRoute(
            snapshots: environmentSnapshotsForCore(),
            sourceEnvironmentId: sourceEnvironmentId,
            projectId: projectId,
            nowMs: Int64(Date().timeIntervalSince1970 * 1_000)
        )
        if evaluation.pendingResources {
            pendingLoadBalancedNewThread = PendingLoadBalancedNewThread(
                projectId: projectId,
                sourceEnvironmentId: sourceEnvironmentId,
                startedAt: Date()
            )
            requestLoadBalancingResources()
            Task { [weak self] in
                try? await Task.sleep(for: .seconds(3))
                self?.retryPendingLoadBalancedNewThread()
            }
            return
        }
        pendingLoadBalancedNewThread = nil
        guard let route = evaluation.route else {
            perform(.newThread(projectId: projectId))
            return
        }
        let sourceDraft = snapshot.newThreadDefaultsForProject(projectId: projectId)
        startRoutedNewThread(route, sourceDraft: sourceDraft, fallbackProjectId: projectId)
    }

    private func startRoutedNewThread(
        _ route: AgentCore.EnvironmentLoadBalancedRouteView,
        sourceDraft: AgentCore.Draft,
        fallbackProjectId: String
    ) {
        perform(.newThread(projectId: "\(route.environmentId):\(route.projectId)")) { [weak self] result in
            guard case .success = result else {
                self?.perform(.newThread(projectId: fallbackProjectId))
                return
            }
            self?.perform(.setModel(
                instanceId: route.providerInstance,
                driver: route.driver,
                model: route.model,
                options: sourceDraft.options
            ))
            self?.perform(.setRuntimeMode(mode: sourceDraft.runtimeMode))
            self?.perform(.setInteractionMode(mode: sourceDraft.interactionMode))
        }
    }

    private func retryPendingLoadBalancedNewThread() {
        guard let pending = pendingLoadBalancedNewThread else { return }
        guard snapshot.environmentId() == pending.sourceEnvironmentId else {
            pendingLoadBalancedNewThread = nil
            perform(.newThread(projectId: pending.projectId))
            return
        }
        guard Date().timeIntervalSince(pending.startedAt) <= 3 else {
            pendingLoadBalancedNewThread = nil
            perform(.newThread(projectId: pending.projectId))
            return
        }
        let evaluation = AgentCore.environmentLoadBalancingRoute(
            snapshots: environmentSnapshotsForCore(),
            sourceEnvironmentId: pending.sourceEnvironmentId,
            projectId: pending.projectId,
            nowMs: Int64(Date().timeIntervalSince1970 * 1_000)
        )
        guard !evaluation.pendingResources else { return }
        pendingLoadBalancedNewThread = nil
        guard let route = evaluation.route else {
            perform(.newThread(projectId: pending.projectId))
            return
        }
        startRoutedNewThread(
            route,
            sourceDraft: snapshot.newThreadDefaultsForProject(projectId: pending.projectId),
            fallbackProjectId: pending.projectId
        )
    }

    private func requestLoadBalancingResources() {
        perform(.refreshLoadBalancingResources)
        for owner in backgroundOwners.values {
            try? owner.dispatch(intent: .refreshLoadBalancingResources)
        }
    }

    func handleSurfaceURL(_ url: URL) {
        guard url.scheme == "remote-agent" else { return }
        if url.host == "new" {
            handleShortcut()
            return
        }
        guard url.host == "share" else { return }
        let query = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems ?? []
        let text = query.filter { $0.name == "text" }.compactMap(\.value).joined(separator: "\n")
        let urls = query.filter { $0.name == "url" }.compactMap(\.value)
        perform(.importShare(content: ShareContent(text: text, urls: urls)))
        screen = .threads
    }

    func ingestIncomingShareHandoffs() {
        let pending = RemoteAgentShareInbox.pending()
        guard !pending.isEmpty else { return }
        screen = .threads
        for handoff in pending {
            guard incomingShareHandoffsInFlight.insert(handoff.file).inserted else { continue }
            perform(.importShare(content: handoff.content)) { [weak self] result in
                self?.incomingShareHandoffsInFlight.remove(handoff.file)
                if case .success = result {
                    RemoteAgentShareInbox.remove(handoff.file)
                }
            }
        }
    }

    func removeProfile(_ id: String) {
        guard profiles.contains(where: { $0.id == id }) else { return }
        do {
            let remaining = profiles.filter { $0.id != id }
            let unregistration = unregisterPush(id)
            let background = backgroundOwners.removeValue(forKey: id)
            backgroundTasks.removeValue(forKey: id)?.cancel()
            environments.removeAll { $0.profileId == id }
            environmentSnapshots.removeValue(forKey: id)
            try DeviceIdentity.remove(id)
            if selectedProfileId == id {
                connection?.cancel()
                isConnecting = false
                let old = detachStore()
                selectedProfileId = nil
                UserDefaults.standard.removeObject(forKey: "bex.selected-host")
                notice = nil
                publish(AgentCore.Snapshot.empty())
                Task { [weak self] in
                    await unregistration?.value
                    do { try await old?.shutdown() } catch { self?.notice = error.localizedDescription }
                }
            }
            Task { [weak self] in
                do {
                    await unregistration?.value
                    try await background?.shutdown()
                } catch { self?.notice = error.localizedDescription }
            }
            profiles = remaining
            // Removing a Host also removes its ActivityKit card and token
            // association; the controller ends it before the Host store is
            // allowed to shut down.
            activityUpdater?(activityContentStatesForPush())
            publishUsageWidget()
            try HostProfile.save(profiles)
            startBackgroundProfiles(selectedProfileId)
            screen = .profiles
        } catch { notice = error.localizedDescription }
    }

    private func detachStore() -> AgentStore? {
        pendingLoadBalancedNewThread = nil
        persist()
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
            if let id = selectedProfileId { registerPushIfReady(id) }
            openPendingPushThreadIfReady()
            let queued = pending
            pending.removeAll()
            for (intent, complete) in queued {
                perform(intent, completion: complete)
            }
            observe(owner, host: id)
            connect()
            startBackgroundProfiles(id)
            ingestIncomingShareHandoffs()
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

    /// Keeps each saved Host's cached snapshot and transport supervised while
    /// another environment is selected in the foreground.
    private func startBackgroundProfiles(_ selected: String?) {
        for profile in profiles where profile.id != selected && backgroundTasks[profile.id] == nil {
            superviseBackground(profile)
        }
    }

    private func superviseBackground(_ profile: HostProfile) {
        let task = Task { [weak self] in
            var delayNanoseconds: UInt64 = 250_000_000
            defer { self?.backgroundTasks[profile.id] = nil }
            while !Task.isCancelled {
                guard let self,
                      profiles.contains(where: { $0.id == profile.id }),
                      selectedProfileId != profile.id
                else { return }
                do {
                    let owner: AgentStore
                    if let existing = backgroundOwners[profile.id] {
                        owner = existing
                    } else {
                        let created = try await AgentStore.offline(
                            stateFile: SnapshotFiles.stateFile(profile.id),
                            modelDefaults: SnapshotFiles.modelDefaults(),
                            cacheDirectory: SnapshotFiles.cacheDirectory(profile.id),
                            diagnosticsDirectory: SnapshotFiles.diagnosticsDirectory(profile.id)
                        )
                        guard !Task.isCancelled else {
                            try? await created.shutdown()
                            return
                        }
                        backgroundOwners[profile.id] = created
                        owner = created
                    }
                    registerPushIfReady(profile.id)
                    publishEnvironment(profile, owner.snapshot())
                    let identity = try DeviceIdentity.loadOrGenerate(profile.id)
                    try await owner.resume(connection: Connection(
                        ticket: profile.ticket,
                        identity: identity,
                        invitation: nil,
                        useRelays: true
                    ))
                    publishEnvironment(profile, owner.snapshot())
                    var previous = owner.snapshot()
                    while !Task.isCancelled, selectedProfileId != profile.id {
                        _ = try await owner.nextSnapshot(previous: previous)
                        let latest = owner.snapshot()
                        publishEnvironment(profile, latest)
                        if !latest.connected() {
                            break
                        }
                        previous = latest
                    }
                    delayNanoseconds = 250_000_000
                } catch is CancellationError {
                    return
                } catch {
                    if notice == nil {
                        notice = "\(profile.name): \(error.localizedDescription)"
                    }
                }
                guard !Task.isCancelled, selectedProfileId != profile.id else { return }
                try? await Task.sleep(nanoseconds: delayNanoseconds)
                delayNanoseconds = delayNanoseconds >= 150_000_000_000
                    ? 300_000_000_000
                    : delayNanoseconds * 2
            }
        }
        backgroundTasks[profile.id] = task
    }

    private func publishEnvironment(_ profile: HostProfile, _ next: AgentCore.Snapshot) {
        let previous = environmentSnapshots[profile.id]
        let pushPreferencesChanged = previous?.preferences().liveActivitiesEnabled
            != next.preferences().liveActivitiesEnabled
        environmentSnapshots[profile.id] = next
        let row = EnvironmentRow(
            profileId: profile.id,
            environmentId: next.environmentId() ?? profile.id,
            label: next.environmentLabel() ?? profile.name,
            state: next.environmentConnectionState() ?? "connecting",
            platform: next.environmentPlatform(),
            machine: next.environmentMachine(),
            capabilities: next.environmentCapabilities(),
            reconnectReason: next.environmentReconnectReason(),
            activities: next.awarenessActivities().map { activity in
                EnvironmentActivityRow(
                    environmentId: activity.environmentId,
                    threadId: next.scopedThreadId(threadId: activity.threadId) ?? activity.threadId,
                    title: activity.threadTitle,
                    headline: activity.headline,
                    detail: activity.detail,
                    phase: activity.phase,
                    updatedAtMs: activity.updatedAtMs
                )
            }
        )
        environments = (environments.filter { $0.profileId != profile.id } + [row])
            .sorted { $0.label.localizedCaseInsensitiveCompare($1.label) == .orderedAscending }
        activityUpdater?(activityContentStatesForPush())
        if pushRegistrations[profile.id] != nil {
            registerPushIfReady(profile.id, force: pushPreferencesChanged)
        }
        publishUsageWidget()
        retryPendingLoadBalancedNewThread()
    }

    private func publishUsageWidget() {
        do { try usageWidget.publish(subscriptionUsageWidgetsJson(
            snapshots: environmentSnapshotsForCore(),
            maxWindows: 6
        )) } catch { notice = error.localizedDescription }
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
                    if let id = selectedProfileId { registerPushIfReady(id) }
                    screen = .threads
                    openPendingPushThreadIfReady()
                    pairingInvitation = nil
                    isConnecting = false
                    observe(owner, host: id)
                    startBackgroundProfiles(id)
                    ingestIncomingShareHandoffs()
                    try? await old?.shutdown()
                } catch {
                    guard !Task.isCancelled else { return }
                    self?.isConnecting = false; self?.pairingError = error.localizedDescription
                }
            }
        } catch { pairingError = error.localizedDescription }
    }

    func perform(_ intent: Intent, completion: @escaping (Result<Outcome, Error>) -> Void = { _ in }) {
        let (routed, profile) = routeIntent(intent)
        if let profile, profile != selectedProfileId {
            selectProfile(profile)
            pending.append((routed, completion))
            return
        }
        performOnCurrent(routed, completion: completion)
    }

    private func performOnCurrent(_ intent: Intent,
                                  completion: @escaping (Result<Outcome, Error>) -> Void = { _ in }) {
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

    func setActivityUpdater(_ updater: @escaping ([String: AgentActivityAttributes.ContentState]) -> Void) {
        activityUpdater = updater
        updater(activityContentStatesForPush())
    }

    /// Projects immutable core awareness snapshots into one ActivityKit record
    /// per retained Host. Each record carries its environment id, so a Host
    /// receives only its own activity token and cannot overwrite another card.
    func activityContentStatesForPush() -> [String: AgentActivityAttributes.ContentState] {
        var snapshots = environmentSnapshots
        if let selectedProfileId {
            snapshots[selectedProfileId] = snapshot
        }
        return snapshots.compactMapValues { activityContentState($0) }
    }

    private func activityContentState(_ source: AgentCore.Snapshot) -> AgentActivityAttributes.ContentState? {
        let environmentId = source.environmentId() ?? ""
        let records = source.awarenessActivities().compactMap { activity -> PushActivitySource? in
            let environment = activity.environmentId.isEmpty ? environmentId : activity.environmentId
            guard !environment.isEmpty, !activity.threadId.isEmpty else { return nil }
            return PushActivitySource(
                environmentId: environment,
                threadId: activity.threadId,
                projectTitle: activity.projectTitle,
                threadTitle: activity.threadTitle,
                modelTitle: activity.modelTitle ?? "Model",
                phase: activity.phase,
                headline: activity.headline,
                updatedAtMs: activity.updatedAtMs,
                deepLink: AgentPushCenter.threadDeepLink(
                    hostId: environment,
                    threadId: activity.threadId
                )
            )
        }
        guard !records.isEmpty,
              let data = try? JSONEncoder().encode(records),
              let json = String(data: data, encoding: .utf8),
              let stateData = AgentCore.agentActivityContentStateJson(json: json).data(using: .utf8)
        else { return nil }
        return try? JSONDecoder().decode(AgentActivityAttributes.ContentState.self, from: stateData)
    }

    /// Core owns the Live Activity preference for each retained Host; APNs
    /// authorization and the device token remain native facts.
    func liveActivitiesEnabled(hostId: String) -> Bool {
        let source = hostId == selectedProfileId ? snapshot : environmentSnapshots[hostId]
        return source?.preferences().liveActivitiesEnabled ?? true
    }

    func pushHostIds() -> [String] { profiles.map(\.id).sorted() }

    func registerPush(hostId: String, registration: AgentPushRegistration) {
        guard profiles.contains(where: { $0.id == hostId }) else { return }
        if pushRegistrations[hostId] != registration {
            pushRegistrations[hostId] = registration
            pushGenerations[hostId, default: 0] &+= 1
            registeredPushOwners.removeValue(forKey: hostId)
            registeredPushConfigurations.removeValue(forKey: hostId)
        }
        registerPushIfReady(hostId)
    }

    func setPushActive(hostId: String, deviceId: String, active: Bool) {
        pendingPushActive[hostId] = (deviceId, active)
        guard let current = pushRegistrations[hostId], current.deviceId == deviceId,
              let owner = pushOwner(hostId) else { return }
        pendingPushActive[hostId] = nil
        dispatchPush(hostId: hostId, owner: owner, intent: .setPushDeviceActive(deviceId: deviceId, active: active))
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

    private func pushOwner(_ hostId: String) -> AgentStore? {
        if hostId == selectedProfileId { return store }
        return backgroundOwners[hostId]
    }

    private func registerPushIfReady(_ hostId: String, force: Bool = false) {
        guard let registration = pushRegistrations[hostId], let owner = pushOwner(hostId) else {
            registeredPushOwners.removeValue(forKey: hostId)
            applyPendingPushActiveIfReady(hostId)
            return
        }
        let native = PushDeviceRegistration(
            deviceId: registration.deviceId,
            platform: "ios",
            token: registration.token,
            liveActivityToken: registration.liveActivityToken,
            pushToStartToken: registration.pushToStartToken,
            bundleId: registration.bundleId,
            apnsEnvironment: registration.apnsEnvironment,
            pushAvailable: registration.pushAvailable,
            notificationsAuthorized: registration.notificationsAuthorized,
            liveActivitiesAvailable: registration.liveActivitiesAvailable
        )
        if force {
            registeredPushOwners.removeValue(forKey: hostId)
            registeredPushConfigurations.removeValue(forKey: hostId)
        }
        if !force, registeredPushOwners[hostId] === owner, registeredPushConfigurations[hostId] == native {
            applyPendingPushActiveIfReady(hostId)
            return
        }
        pushGenerations[hostId, default: 0] &+= 1
        let generation = pushGenerations[hostId, default: 0]
        dispatchPush(hostId: hostId, owner: owner, intent: .registerPushDevice(registration: native)) { [weak self] result in
            guard let self,
                  self.pushGenerations[hostId] == generation,
                  self.pushRegistrations[hostId] == registration,
                  self.pushOwner(hostId) === owner,
                  case .success = result else { return }
            self.registeredPushOwners[hostId] = owner
            self.registeredPushConfigurations[hostId] = native
            self.setPushActive(
                hostId: hostId,
                deviceId: registration.deviceId,
                active: native.pushAvailable &&
                    (native.notificationsAuthorized ||
                        (native.liveActivitiesAvailable && liveActivitiesEnabled(hostId: hostId)))
            )
        }
    }

    private func dispatchPush(
        hostId: String,
        owner: AgentStore,
        intent: Intent,
        completion: @escaping (Result<Outcome, Error>) -> Void = { _ in }
    ) {
        do {
            let receipt = try owner.dispatch(intent: intent)
            Task { [weak self] in
                let result: Result<Outcome, Error>
                do { result = try await .success(receipt.wait()) } catch { result = .failure(error) }
                guard let self,
                      self.pushOwner(hostId) === owner,
                      !Task.isCancelled else { return }
                completion(result)
            }
        } catch { completion(.failure(error)) }
    }

    private func applyPendingPushActiveIfReady(_ hostId: String) {
        guard let pending = pendingPushActive[hostId] else { return }
        setPushActive(hostId: hostId, deviceId: pending.deviceId, active: pending.active)
    }

    private func openPendingPushThreadIfReady() {
        guard let pending = pendingPushThread,
              pending.hostId == selectedProfileId,
              store != nil else { return }
        pendingPushThread = nil
        openThread(pending.threadId)
    }

    private func unregisterPush(_ hostId: String) -> Task<Void, Never>? {
        let deviceId = pushRegistrations[hostId]?.deviceId ?? pushDeviceIdProvider?(hostId)
        pushGenerations[hostId, default: 0] &+= 1
        pendingPushActive[hostId] = nil
        pushRegistrations.removeValue(forKey: hostId)
        registeredPushOwners.removeValue(forKey: hostId)
        registeredPushConfigurations.removeValue(forKey: hostId)
        guard let owner = pushOwner(hostId), let deviceId else { return nil }
        do {
            let receipt = try owner.dispatch(intent: .unregisterPushDevice(deviceId: deviceId))
            return Task { _ = try? await receipt.wait() }
        } catch { return nil }
    }

    func setPushDeviceIdProvider(_ provider: @escaping (String) -> String?) {
        pushDeviceIdProvider = provider
    }

    private func scopedValue(_ value: String?) -> (String?, String?) {
        guard let value, let separator = value.firstIndex(of: ":"), separator != value.startIndex else {
            return (value, nil)
        }
        let environmentId = String(value[..<separator])
        guard let profile = environments.first(where: { $0.environmentId == environmentId })?.profileId else {
            return (value, nil)
        }
        let local = String(value[value.index(after: separator)...])
        guard !local.isEmpty else { return (value, nil) }
        return (local, profile)
    }

    private func routeIntent(_ intent: Intent) -> (Intent, String?) {
        switch intent {
        case let .openThread(threadId):
            let (local, profile) = scopedValue(threadId)
            return (.openThread(threadId: local ?? threadId), profile)
        case let .newThread(projectId):
            let (local, profile) = scopedValue(projectId)
            return (.newThread(projectId: local), profile)
        case let .thread(threadId, action):
            let (local, profile) = scopedValue(threadId)
            return (.thread(threadId: local ?? threadId, action: action), profile)
        case let .moveThread(threadId, section, destination):
            let (local, profile) = scopedValue(threadId)
            return (.moveThread(threadId: local ?? threadId, section: section, destination: destination), profile)
        case let .filterProject(projectId):
            let (local, profile) = scopedValue(projectId)
            return (.filterProject(projectId: local), profile)
        case let .newThreadOnBranch(projectId, branch, worktreePath):
            let (local, profile) = scopedValue(projectId)
            return (.newThreadOnBranch(projectId: local ?? projectId, branch: branch,
                                       worktreePath: worktreePath), profile)
        case let .resetProjectSettings(projectId):
            let (local, profile) = scopedValue(projectId)
            return (.resetProjectSettings(projectId: local ?? projectId), profile)
        default:
            return (intent, nil)
        }
    }

    /// Selects the profile that owns a scoped project before opening settings.
    @discardableResult
    func selectScopedValue(_ value: String?) -> String? {
        let (local, profile) = scopedValue(value)
        if let profile, profile != selectedProfileId {
            selectProfile(profile)
        }
        return local
    }

    func environmentSnapshotsForCore() -> [AgentCore.Snapshot] {
        Array(environmentSnapshots.values)
    }

    func environmentProjects(_ query: String) -> [EnvironmentProjectRow] {
        AgentCore.environmentProjectRows(snapshots: environmentSnapshotsForCore(), query: query)
    }

    func environmentSettings() -> [EnvironmentSettingsEntryView] {
        AgentCore.environmentSettings(snapshots: environmentSnapshotsForCore())
    }

    func loadBalancingPreferences() -> [EnvironmentLoadBalancingPreferenceView] {
        AgentCore.environmentLoadBalancingPreferences(snapshots: environmentSnapshotsForCore())
    }

    func setLoadBalancingEnabled(_ enabled: Bool) {
        perform(.setLoadBalancingEnabled(enabled: enabled))
        for (profile, owner) in backgroundOwners {
            guard let receipt = try? owner.dispatch(intent: .setLoadBalancingEnabled(enabled: enabled)) else {
                continue
            }
            Task { [weak self, owner] in
                _ = try? await receipt.wait()
                guard let self, let host = self.profiles.first(where: { $0.id == profile }) else {
                    return
                }
                self.publishEnvironment(host, owner.snapshot())
            }
        }
    }

    func setLoadBalancingWeight(environmentId: String, weight: UInt8) {
        let intent = Intent.setLoadBalancingWeight(environmentId: environmentId, weight: weight)
        guard let profile = environments.first(where: { $0.environmentId == environmentId })?.profileId else {
            return
        }
        if profile == selectedProfileId {
            perform(intent)
        } else if let owner = backgroundOwners[profile] {
            guard let receipt = try? owner.dispatch(intent: intent) else { return }
            Task { [weak self, owner] in
                _ = try? await receipt.wait()
                guard let self, let host = self.profiles.first(where: { $0.id == profile }) else {
                    return
                }
                self.publishEnvironment(host, owner.snapshot())
            }
        } else {
            perform(intent)
        }
    }

    func environmentThreadList(nowMs: Int64, options: ThreadListOptions, query: String,
                               selectedProject: String?, selectedThread: String?) -> EnvironmentThreadListView {
        AgentCore.environmentThreadList(snapshots: environmentSnapshotsForCore(), nowMs: nowMs, options: options,
                                        query: query, selectedProject: selectedProject,
                                        selectedThread: selectedThread)
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
        startBackgroundProfiles(selectedProfileId)
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
        let becameUnavailable = snapshot.error() == nil && next.error() != nil
        if snapshot.error() != next.error() {
            notice = next.error()
        }
        if becameUnavailable && UIApplication.shared.applicationState != .active {
            let mode = next.preferences().notificationMode
            let notificationsEnabled = mode == .notifications || mode == .notificationsAndSound
            let soundEnabled = mode == .sound || mode == .notificationsAndSound
            if notificationsEnabled || soundEnabled {
                LocalNotifications.deliver(
                    title: "Bex needs your attention",
                    body: next.error() ?? "The Host reported an error.",
                    sound: soundEnabled
                )
            }
        }
        let threadChanged = snapshot.selectedThreadId() != next.selectedThreadId()
        snapshot = next
        if let id = selectedProfileId, let profile = profiles.first(where: { $0.id == id }) {
            publishEnvironment(profile, next)
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
