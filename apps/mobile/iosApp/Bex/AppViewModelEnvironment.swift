import AgentCore
import Foundation

/// Environment-scoped routing and aggregate settings projections.
@MainActor
extension BexAppViewModel {
    func scopedValue(_ value: String?) -> (String?, String?) {
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

    func routeIntent(_ intent: Intent) -> (Intent, String?) {
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
            let generation = backgroundTaskGenerations[profile, default: 0]
            let preferencesGeneration = clientPreferencesGeneration
            let beforePreferences = clientPreferencesData
            let beforeStorePreferences = try? owner.snapshot().serializeModelPreferences()
            guard let receipt = try? owner.dispatch(intent: .setLoadBalancingEnabled(enabled: enabled)) else {
                continue
            }
            Task { [weak self, owner] in
                let result = try? await receipt.wait()
                guard result != nil else { return }
                guard let self,
                      backgroundTaskGenerations[profile] == generation,
                      backgroundOwners[profile] === owner,
                      selectedProfileId != profile,
                      clientPreferencesGeneration == preferencesGeneration,
                      let host = profiles.first(where: { $0.id == profile }) else {
                    return
                }
                let updated = owner.snapshot()
                guard beforeStorePreferences == beforePreferences,
                      let afterStorePreferences = try? updated.serializeModelPreferences(),
                      afterStorePreferences != beforePreferences else { return }
                synchronizeClientPreferences(
                    updated,
                    includeSelected: true,
                    allowPendingSelected: true
                )
                publishEnvironment(host, updated)
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
            let generation = backgroundTaskGenerations[profile, default: 0]
            let preferencesGeneration = clientPreferencesGeneration
            let beforePreferences = clientPreferencesData
            let beforeStorePreferences = try? owner.snapshot().serializeModelPreferences()
            guard let receipt = try? owner.dispatch(intent: intent) else { return }
            Task { [weak self, owner] in
                let result = try? await receipt.wait()
                guard result != nil else { return }
                guard let self,
                      backgroundTaskGenerations[profile] == generation,
                      backgroundOwners[profile] === owner,
                      selectedProfileId != profile,
                      clientPreferencesGeneration == preferencesGeneration,
                      let host = profiles.first(where: { $0.id == profile }) else {
                    return
                }
                let updated = owner.snapshot()
                guard beforeStorePreferences == beforePreferences,
                      let afterStorePreferences = try? updated.serializeModelPreferences(),
                      afterStorePreferences != beforePreferences else { return }
                synchronizeClientPreferences(
                    updated,
                    includeSelected: true,
                    allowPendingSelected: true
                )
                publishEnvironment(host, updated)
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
