import AgentCore
import Foundation

/// Native push registration and activity routing.
@MainActor
extension BexAppViewModel {
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

    func activityContentState(_ source: AgentCore.Snapshot) -> AgentActivityAttributes.ContentState? {
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
                deepLink: AgentCore.agentActivityThreadDeepLink(
                    environmentId: environment,
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

    func pushHostIds() -> [String] {
        profiles.map(\.id).sorted()
    }

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
        return AgentCore.agentActivityThreadDeepLink(environmentId: host, threadId: thread)
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

    func openActivityOverviewDeepLink() {
        pendingPushThread = nil
        if !profiles.isEmpty {
            screen = .profiles
        }
    }

    /// The Usage root calls this after presenting the requested limits tab.
    func consumeUsageDeepLinkRequest() {
        guard usageDeepLinkRequests > 0 else { return }
        usageDeepLinkRequests -= 1
    }

    func pushOwner(_ hostId: String) -> AgentStore? {
        if hostId == selectedProfileId {
            return store
        }
        return backgroundOwners[hostId]
    }

    func registerPushIfReady(_ hostId: String, force: Bool = false) {
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
        dispatchPush(
            hostId: hostId,
            owner: owner,
            intent: .registerPushDevice(registration: native)
        ) { [weak self] result in
            guard let self,
                  pushGenerations[hostId] == generation,
                  pushRegistrations[hostId] == registration,
                  pushOwner(hostId) === owner,
                  case .success = result else { return }
            registeredPushOwners[hostId] = owner
            registeredPushConfigurations[hostId] = native
        }
    }

    func dispatchPush(
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
                      pushOwner(hostId) === owner,
                      !Task.isCancelled else { return }
                completion(result)
            }
        } catch { completion(.failure(error)) }
    }

    func applyPendingPushActiveIfReady(_ hostId: String) {
        guard let pending = pendingPushActive[hostId] else { return }
        setPushActive(hostId: hostId, deviceId: pending.deviceId, active: pending.active)
    }

    func openPendingPushThreadIfReady() {
        guard let pending = pendingPushThread,
              pending.hostId == selectedProfileId,
              store != nil else { return }
        pendingPushThread = nil
        openThread(pending.threadId)
    }

    func unregisterPush(_ hostId: String) -> Task<Void, Never>? {
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
}
