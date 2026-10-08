import AgentCore
import Foundation

/// Intent dispatch, browser removal, push and environment settings.
@MainActor
extension BexAppViewModel {
    func perform(_ intent: Intent, completion: @escaping (Result<Outcome, Error>) -> Void = { _ in }) {
        let (routed, profile) = routeIntent(intent)
        if let profile, profile != selectedProfileId {
            selectProfile(profile)
            pending.append((routed, completion))
            return
        }
        performOnCurrent(routed, completion: completion)
    }

    /// Clears a browser profile in every connected Host before removing its
    /// device-owned row. Core validates the target set and folds receipts;
    /// this owner only maps each environment to its Store.
    func removeBrowserProfile(_ profileId: String) {
        browserProfileRemovalGeneration &+= 1
        let generation = browserProfileRemovalGeneration
        let request: (plan: AgentCore.BrowserProfileRemovalPlan, stores: [String: AgentStore])
        do {
            request = try browserProfileRemovalRequest(profileId: profileId, generation: generation)
        } catch {
            notice = error.localizedDescription
            return
        }
        Task { [weak self] in
            guard let self else { return }
            await finishBrowserProfileRemoval(
                plan: request.plan,
                stores: request.stores,
                generation: generation
            )
        }
    }

    private func finishBrowserProfileRemoval(
        plan: AgentCore.BrowserProfileRemovalPlan,
        stores: [String: AgentStore],
        generation: UInt64
    ) async {
        do {
            let result = try await clearBrowserProfileData(
                plan: plan,
                stores: stores,
                generation: generation
            )
            guard browserProfileRemovalGeneration == generation else { return }
            switch AgentCore.browserProfileRemovalDecision(
                plan: plan,
                callbackGeneration: generation,
                clearedEnvironmentIds: result.cleared,
                failed: result.failed
            ) {
            case .ready:
                await finishReadyBrowserProfileRemoval(
                    plan: plan,
                    stores: stores,
                    generation: generation
                )
            case .failed:
                notice = "Browser profile data could not be cleared on every connected Host; the profile was kept."
            case .pending, .stale:
                return
            }
        } catch is CancellationError {
            return
        } catch {
            notice = error.localizedDescription
        }
    }

    private func finishReadyBrowserProfileRemoval(
        plan: AgentCore.BrowserProfileRemovalPlan,
        stores: [String: AgentStore],
        generation: UInt64
    ) async {
        do {
            let result = try await removeBrowserProfileFromStores(
                plan: plan,
                stores: stores,
                generation: generation
            )
            guard browserProfileRemovalGeneration == generation else { return }
            if !result.failed, let canonical = result.canonical {
                synchronizeClientPreferences(
                    canonical,
                    includeSelected: true,
                    allowPendingSelected: true
                )
            }
            if result.failed {
                notice = "Browser profile could not be removed from every connected Host; try again."
            }
        } catch is CancellationError {
            return
        } catch {
            notice = error.localizedDescription
        }
    }

    private func browserProfileRemovalRequest(
        profileId: String,
        generation: UInt64
    ) throws -> (plan: AgentCore.BrowserProfileRemovalPlan, stores: [String: AgentStore]) {
        var snapshots = environmentSnapshots
        if let selectedProfileId {
            snapshots[selectedProfileId] = snapshot
        }
        let connected = snapshots.filter { $0.value.connected() }
        let environmentIds = Array(Set(connected.values.compactMap { $0.environmentId() })).sorted()
        let plan = try AgentCore.beginBrowserProfileRemoval(
            profiles: snapshot.browserDefaults().profiles,
            profileId: profileId,
            environmentIds: environmentIds,
            generation: generation
        )
        return (plan, browserProfileStores(for: connected))
    }

    private func browserProfileStores(
        for connected: [String: AgentCore.Snapshot]
    ) -> [String: AgentStore] {
        var stores: [String: AgentStore] = [:]
        for (profile, current) in connected {
            guard let environmentId = current.environmentId() else { continue }
            if profile == selectedProfileId, let store {
                stores[environmentId] = store
            } else if let owner = backgroundOwners[profile] {
                stores[environmentId] = owner
            }
        }
        return stores
    }

    private func clearBrowserProfileData(
        plan: AgentCore.BrowserProfileRemovalPlan,
        stores: [String: AgentStore],
        generation: UInt64
    ) async throws -> (cleared: [String], failed: Bool) {
        var cleared: [String] = []
        var failed = false
        for environmentId in plan.environmentIds {
            guard let owner = stores[environmentId] else {
                failed = true
                continue
            }
            guard browserProfileStoresAreCurrent(stores) else { throw CancellationError() }
            do {
                let receipt = try owner.dispatch(intent: .previewClearProfileData(profileId: plan.profileId))
                _ = try await receipt.wait()
                cleared.append(environmentId)
            } catch is CancellationError {
                throw CancellationError()
            } catch {
                failed = true
            }
            guard browserProfileRemovalGeneration == generation else { throw CancellationError() }
            guard browserProfileStoresAreCurrent(stores) else { throw CancellationError() }
        }
        return (cleared, failed)
    }

    private func removeBrowserProfileFromStores(
        plan: AgentCore.BrowserProfileRemovalPlan,
        stores: [String: AgentStore],
        generation: UInt64
    ) async throws -> (failed: Bool, canonical: AgentCore.Snapshot?) {
        var failed = false
        for environmentId in plan.environmentIds {
            guard browserProfileRemovalGeneration == generation else { throw CancellationError() }
            guard let owner = stores[environmentId] else {
                failed = true
                continue
            }
            do {
                let receipt = try owner.dispatch(intent: .removeBrowserProfile(profileId: plan.profileId))
                _ = try await receipt.wait()
            } catch is CancellationError {
                throw CancellationError()
            } catch {
                failed = true
            }
            guard browserProfileStoresAreCurrent(stores) else { throw CancellationError() }
        }
        guard browserProfileRemovalGeneration == generation else { throw CancellationError() }
        guard !failed else { return (true, nil) }
        guard browserProfileStoresAreCurrent(stores) else { throw CancellationError() }
        let canonicalOwner: AgentStore? = if selectedProfileId != nil,
                                             let selectedOwner = store,
                                             let environmentId = snapshot.environmentId(),
                                             stores[environmentId] === selectedOwner {
            selectedOwner
        } else {
            stores.keys.sorted().compactMap { stores[$0] }.first
        }
        return (false, canonicalOwner?.snapshot())
    }

    private func browserProfileStoresAreCurrent(_ expected: [String: AgentStore]) -> Bool {
        var snapshots = environmentSnapshots
        if let selectedProfileId {
            snapshots[selectedProfileId] = snapshot
        }
        let current = browserProfileStores(for: snapshots.filter { $0.value.connected() })
        return current.count == expected.count && expected.allSatisfy { environmentId, owner in
            current[environmentId] === owner
        }
    }

    func performOnCurrent(
        _ intent: Intent,
        completion: @escaping (Result<Outcome, Error>) -> Void = { _ in }
    ) {
        guard let owner = store else {
            if initialization != nil {
                pending.append((intent, completion))
            } else {
                completion(.failure(CocoaError(.fileReadUnknown)))
            }
            return
        }
        do {
            let beforePreferences = try? owner.snapshot().serializeModelPreferences()
            let receipt = try owner.dispatch(intent: intent)
            let id = UUID()
            let host = selectedProfileId
            operations[id] = Task { [weak self] in
                let result: Result<Outcome, Error>
                do { result = try await .success(receipt.wait()) } catch { result = .failure(error) }
                guard let self else { return }
                operations[id] = nil
                if selectedProfileId == host, store === owner {
                    let afterPreferences = try? owner.snapshot().serializeModelPreferences()
                    if case .success = result, beforePreferences != afterPreferences {
                        synchronizeClientPreferences(owner.snapshot(), allowPendingSelected: true)
                    }
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
