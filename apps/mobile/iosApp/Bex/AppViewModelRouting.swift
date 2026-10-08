import AgentCore
import Foundation
import UIKit

/// Routing, sharing, profile selection and profile removal.
@MainActor
extension BexAppViewModel {
    func selectProfile(_ id: String) {
        guard profiles.contains(where: { $0.id == id }) else { return }
        if automaticRouteProfileId != nil, automaticRouteProfileId != id {
            invalidatePendingLoadBalancedNewThread()
        }
        screen = .threads
        if selectedProfileId == id, store != nil {
            if automaticRouteProfileId == nil {
                invalidatePendingLoadBalancedNewThread()
            }
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
            invalidatePendingLoadBalancedNewThread()
            perform(.newThread(projectId: projectId))
            return
        }
        // A project chosen from the aggregate picker is already an explicit
        // environment route. Automatic balancing only applies to the source
        // Host's local project selection.
        if scopedValue(projectId).1 != nil {
            invalidatePendingLoadBalancedNewThread()
            perform(.newThread(projectId: projectId))
            return
        }
        let evaluation = AgentCore.environmentLoadBalancingRoute(
            snapshots: environmentSnapshotsForCore(),
            sourceEnvironmentId: sourceEnvironmentId,
            projectId: projectId,
            nowMs: Int64(Date().timeIntervalSince1970 * 1000)
        )
        if evaluation.pendingResources {
            loadBalancingAttemptGeneration &+= 1
            let generation = loadBalancingAttemptGeneration
            pendingLoadBalancedNewThread = PendingLoadBalancedNewThread(
                projectId: projectId,
                sourceEnvironmentId: sourceEnvironmentId,
                startedAt: Date(),
                generation: generation
            )
            requestLoadBalancingResources()
            Task { [weak self] in
                try? await Task.sleep(for: .seconds(3))
                self?.retryPendingLoadBalancedNewThread(generation: generation)
            }
            return
        }
        invalidatePendingLoadBalancedNewThread()
        guard let route = evaluation.route else {
            perform(.newThread(projectId: projectId))
            return
        }
        startRoutedNewThread(
            route,
            fallbackProjectId: projectId,
            sourceEnvironmentId: sourceEnvironmentId,
            generation: loadBalancingAttemptGeneration
        )
    }

    func startRoutedNewThread(
        _ route: AgentCore.EnvironmentLoadBalancedRouteView,
        fallbackProjectId: String,
        sourceEnvironmentId: String,
        generation: UInt64
    ) {
        automaticRouteProfileId = environments
            .first(where: { $0.environmentId == route.environmentId })?
            .profileId
        perform(.newThread(projectId: "\(route.environmentId):\(route.projectId)")) { [weak self] result in
            guard let self, loadBalancingAttemptGeneration == generation else { return }
            guard case .success = result else {
                guard snapshot.environmentId() == route.environmentId else { return }
                let fallback = "\(sourceEnvironmentId):\(fallbackProjectId)"
                guard scopedValue(fallback).1 != nil else { return }
                perform(.newThread(projectId: fallback))
                return
            }
            perform(.setModel(
                instanceId: route.providerInstance,
                driver: route.driver,
                model: route.model,
                options: route.options
            ))
            perform(.setRuntimeMode(mode: route.runtimeMode))
            perform(.setInteractionMode(mode: route.interactionMode))
        }
        automaticRouteProfileId = nil
    }

    private func continuePendingLoadBalancedNewThread(
        _ pending: PendingLoadBalancedNewThread,
        nowMs: Int64
    ) -> Bool {
        switch AgentCore.environmentLoadBalancingPendingAction(
            attemptGeneration: pending.generation,
            currentGeneration: loadBalancingAttemptGeneration,
            sourceEnvironmentId: pending.sourceEnvironmentId,
            selectedEnvironmentId: snapshot.environmentId(),
            startedAtMs: Int64(pending.startedAt.timeIntervalSince1970 * 1000),
            nowMs: nowMs,
            timeoutMs: 3000
        ) {
        case .cancel:
            invalidatePendingLoadBalancedNewThread()
            return false
        case .fallback:
            invalidatePendingLoadBalancedNewThread()
            perform(.newThread(
                projectId: "\(pending.sourceEnvironmentId):\(pending.projectId)"
            ))
            return false
        case .retry:
            return true
        }
    }

    func retryPendingLoadBalancedNewThread(generation: UInt64? = nil) {
        guard let pending = pendingLoadBalancedNewThread else { return }
        guard generation == nil || generation == pending.generation else { return }
        let nowMs = Int64(Date().timeIntervalSince1970 * 1000)
        guard continuePendingLoadBalancedNewThread(pending, nowMs: nowMs) else { return }
        let evaluation = AgentCore.environmentLoadBalancingRoute(
            snapshots: environmentSnapshotsForCore(),
            sourceEnvironmentId: pending.sourceEnvironmentId,
            projectId: pending.projectId,
            nowMs: nowMs
        )
        if evaluation.pendingResources {
            if let generation {
                Task { [weak self] in
                    try? await Task.sleep(for: .milliseconds(50))
                    self?.retryPendingLoadBalancedNewThread(generation: generation)
                }
            }
            return
        }
        invalidatePendingLoadBalancedNewThread()
        guard let route = evaluation.route else {
            perform(.newThread(
                projectId: "\(pending.sourceEnvironmentId):\(pending.projectId)"
            ))
            return
        }
        startRoutedNewThread(
            route,
            fallbackProjectId: pending.projectId,
            sourceEnvironmentId: pending.sourceEnvironmentId,
            generation: loadBalancingAttemptGeneration
        )
    }

    func invalidatePendingLoadBalancedNewThread() {
        loadBalancingAttemptGeneration &+= 1
        pendingLoadBalancedNewThread = nil
    }

    func requestLoadBalancingResources() {
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
            let environmentId = environmentSnapshots[id]?.environmentId() ?? id
            LocalNotifications.removeEnvironment(environmentId)
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
}
