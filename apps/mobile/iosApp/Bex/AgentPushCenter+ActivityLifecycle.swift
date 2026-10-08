import ActivityKit
import Foundation
import UIKit

extension AgentPushCenter {
    /// Starts one local ActivityKit card for a specific Host and listens for
    /// its push token. The Host can continue that card while the app is closed.
    @available(iOS 16.1, *)
    func startActivity(hostId: String, contentState: AgentActivityAttributes.ContentState) async throws -> String {
        let activity = try Activity<AgentActivityAttributes>.request(
            attributes: AgentActivityAttributes(),
            content: ActivityContent(
                state: contentState,
                staleDate: Date().addingTimeInterval(600)
            ),
            pushType: .token
        )
        activityIds[hostId] = activity.id
        activityHosts[activity.id] = hostId
        startingHosts.remove(hostId)
        observeActivityToken(activity, hostId: hostId)
        return activity.id
    }

    @available(iOS 16.1, *)
    private func updateActivity(hostId: String, contentState: AgentActivityAttributes.ContentState) async {
        guard let id = activityIds[hostId] else { return }
        guard let activity = Activity<AgentActivityAttributes>.activities.first(where: { $0.id == id }) else {
            removeActivity(hostId: hostId, id: id)
            return
        }
        await activity.update(ActivityContent(state: contentState, staleDate: Date().addingTimeInterval(600)))
    }

    @available(iOS 16.1, *)
    private func endActivity(hostId: String, contentState: AgentActivityAttributes.ContentState) async {
        guard let id = activityIds[hostId] else { return }
        guard let activity = Activity<AgentActivityAttributes>.activities.first(where: { $0.id == id }) else {
            removeActivity(hostId: hostId, id: id)
            return
        }
        await activity.end(
            ActivityContent(state: contentState, staleDate: nil),
            dismissalPolicy: .after(Date().addingTimeInterval(300))
        )
        removeActivity(hostId: hostId, id: id)
    }

    /// Reconciles every retained Host's foreground card. A completed Host
    /// ends only its own card; active Hosts continue to receive updates.
    @available(iOS 16.1, *)
    func reconcileActivities(states: [String: AgentActivityAttributes.ContentState]) async {
        pendingActivityStates = states
        guard !activityReconciliationRunning else { return }
        activityReconciliationRunning = true
        defer { activityReconciliationRunning = false }
        while let next = pendingActivityStates {
            pendingActivityStates = nil
            await reconcileActivitiesNow(states: next)
        }
    }

    @available(iOS 16.1, *)
    private func reconcileActivitiesNow(states: [String: AgentActivityAttributes.ContentState]) async {
        let known = Set(knownHostIds())
        let enabled = Set(known.filter { liveActivitiesAllowed(for: $0) })
        for (hostId, _) in activityIds where !enabled.contains(hostId) {
            await endActivity(
                hostId: hostId,
                contentState: states[hostId] ?? emptyContentState()
            )
        }
        for hostId in enabled {
            let state = states[hostId]
            if let state, state.activeCount > 0 {
                if activityIds[hostId] != nil {
                    await updateActivity(hostId: hostId, contentState: state)
                } else if UIApplication.shared.applicationState == .active,
                          !startingHosts.contains(hostId) {
                    startingHosts.insert(hostId)
                    do { _ = try await startActivity(hostId: hostId, contentState: state) } catch {
                        startingHosts.remove(hostId)
                    }
                }
            } else if let state {
                await endActivity(hostId: hostId, contentState: state)
            } else if activityIds[hostId] != nil {
                await endActivity(hostId: hostId, contentState: emptyContentState())
            }
        }
        submitRegistrations()
    }

    @available(iOS 16.1, *)
    func observeActivityTokens() {
        guard activityUpdatesTask == nil else { return }
        activityUpdatesTask = Task { @MainActor [weak self] in
            for await activity in Activity<AgentActivityAttributes>.activityUpdates {
                guard let self else { return }
                let hostId = hostId(for: activity)
                guard knownHostIds().contains(hostId) else {
                    Task { @MainActor in
                        await activity.end(
                            ActivityContent(state: activity.content.state, staleDate: nil),
                            dismissalPolicy: .immediate
                        )
                    }
                    continue
                }
                activityIds[hostId] = activity.id
                activityHosts[activity.id] = hostId
                observeActivityToken(activity, hostId: hostId)
            }
        }
        if #available(iOS 17.2, *), pushToStartUpdatesTask == nil {
            pushToStartUpdatesTask = Task { @MainActor [weak self] in
                for await token in Activity<AgentActivityAttributes>.pushToStartTokenUpdates {
                    guard let self else { return }
                    pushToStartToken = token.map { String(format: "%02x", $0) }.joined()
                    submitRegistrations()
                }
            }
        }
        for activity in Activity<AgentActivityAttributes>.activities {
            let hostId = hostId(for: activity)
            guard knownHostIds().contains(hostId) else {
                Task { @MainActor in
                    await activity.end(
                        ActivityContent(state: activity.content.state, staleDate: nil),
                        dismissalPolicy: .immediate
                    )
                }
                continue
            }
            activityIds[hostId] = activity.id
            activityHosts[activity.id] = hostId
            observeActivityToken(activity, hostId: hostId)
        }
    }

    @available(iOS 16.1, *)
    private func hostId(for activity: Activity<AgentActivityAttributes>) -> String {
        if let known = activityHosts[activity.id] {
            return known
        }
        return activity.content.state.activities.first?.environmentId ?? "unknown"
    }

    @available(iOS 16.1, *)
    private func observeActivityToken(_ activity: Activity<AgentActivityAttributes>, hostId: String) {
        activityTokenTasks[hostId]?.cancel()
        activityStateTasks[hostId]?.cancel()
        activityTokenTasks[hostId] = Task { @MainActor [weak self] in
            for await token in activity.pushTokenUpdates {
                guard let self else { return }
                guard activityIds[hostId] == activity.id else { return }
                activityTokens[hostId] = token.map { String(format: "%02x", $0) }.joined()
                submitRegistration(hostId: hostId, token: deviceToken ?? "")
            }
        }
        activityStateTasks[hostId] = Task { @MainActor [weak self] in
            for await state in activity.activityStateUpdates {
                guard let self else { return }
                guard state == .ended || state == .dismissed || state == .stale else { continue }
                removeActivity(hostId: hostId, id: activity.id)
                return
            }
        }
    }

    @available(iOS 16.1, *)
    private func removeActivity(hostId: String, id: String) {
        guard activityIds[hostId] == id else { return }
        activityIds.removeValue(forKey: hostId)
        activityHosts.removeValue(forKey: id)
        activityTokens.removeValue(forKey: hostId)
        activityTokenTasks.removeValue(forKey: hostId)?.cancel()
        activityStateTasks.removeValue(forKey: hostId)?.cancel()
        submitRegistrations()
    }

    @available(iOS 16.1, *)
    private func emptyContentState() -> AgentActivityAttributes.ContentState {
        AgentActivityAttributes.ContentState(
            title: "Agent activity",
            subtitle: "Agent work completed",
            activeCount: 0,
            updatedAt: ISO8601DateFormatter().string(from: Date()),
            activities: []
        )
    }
}
