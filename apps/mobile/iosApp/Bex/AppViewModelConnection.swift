import AgentCore
import Foundation
import UIKit

/// Host stores, background supervision and pairing.
@MainActor
extension BexAppViewModel {
    func detachStore() -> AgentStore? {
        if automaticRouteProfileId == nil {
            invalidatePendingLoadBalancedNewThread()
        }
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
        pendingSelectedClientPreferences = nil
        return old
    }

    func cancelInitialization() {
        initialization?.cancel()
        initialization = nil
        let cancelled = pending
        pending.removeAll()
        for (_, complete) in cancelled {
            complete(.failure(CancellationError()))
        }
    }

    func initialize(_ id: String, previous old: AgentStore?) async {
        let preparationStarted = ProcessInfo.processInfo.systemUptime
        async let previousClosed: Void? = try? old?.shutdown()
        do {
            let owner = try await AgentStore.offline(
                stateFile: SnapshotFiles.stateFile(id),
                modelDefaults: clientPreferencesData,
                cacheDirectory: SnapshotFiles.cacheDirectory(id),
                diagnosticsDirectory: SnapshotFiles.diagnosticsDirectory(id)
            )
            do {
                try await applyClientPreferences(to: owner)
            } catch {
                try? await owner.shutdown()
                throw error
            }
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
            if let id = selectedProfileId {
                registerPushIfReady(id)
            }
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
    func startBackgroundProfiles(_ selected: String?) {
        for profile in profiles where profile.id != selected && backgroundTasks[profile.id] == nil {
            superviseBackground(profile)
        }
    }

    func superviseBackground(_ profile: HostProfile) {
        let generation = backgroundTaskGenerations[profile.id, default: 0] &+ 1
        backgroundTaskGenerations[profile.id] = generation
        let task = Task { [weak self] in
            var delayNanoseconds: UInt64 = 250_000_000
            defer {
                if let self, self.backgroundTaskGenerations[profile.id] == generation {
                    self.backgroundTasks[profile.id] = nil
                    self.backgroundTaskGenerations[profile.id] = nil
                }
            }
            while !Task.isCancelled {
                guard let self,
                      profiles.contains(where: { $0.id == profile.id }),
                      selectedProfileId != profile.id
                else { return }
                do {
                    try await runBackgroundProfile(profile, generation: generation)
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

    private func runBackgroundProfile(_ profile: HostProfile, generation: UInt64) async throws {
        let owner: AgentStore
        var createdOwner: AgentStore?
        if let existing = backgroundOwners[profile.id] {
            owner = existing
        } else {
            let created = try await AgentStore.offline(
                stateFile: SnapshotFiles.stateFile(profile.id),
                modelDefaults: clientPreferencesData,
                cacheDirectory: SnapshotFiles.cacheDirectory(profile.id),
                diagnosticsDirectory: SnapshotFiles.diagnosticsDirectory(profile.id)
            )
            guard !Task.isCancelled else {
                try? await created.shutdown()
                throw CancellationError()
            }
            backgroundOwners[profile.id] = created
            owner = created
            createdOwner = created
        }
        do {
            try await applyClientPreferences(to: owner)
            guard ownsBackground(profile, owner: owner, generation: generation) else {
                throw CancellationError()
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
            guard ownsBackground(profile, owner: owner, generation: generation) else {
                throw CancellationError()
            }
            publishEnvironment(profile, owner.snapshot())
            var previous = owner.snapshot()
            while ownsBackground(profile, owner: owner, generation: generation) {
                _ = try await owner.nextSnapshot(previous: previous)
                guard ownsBackground(profile, owner: owner, generation: generation) else {
                    throw CancellationError()
                }
                let latest = owner.snapshot()
                publishEnvironment(profile, latest)
                if !latest.connected() {
                    break
                }
                previous = latest
            }
        } catch {
            if let createdOwner, backgroundOwners[profile.id] === createdOwner {
                backgroundOwners.removeValue(forKey: profile.id)
                try? await createdOwner.shutdown()
            }
            throw error
        }
    }

    func applyClientPreferences(to owner: AgentStore) async throws {
        while true {
            try Task.checkCancellation()
            let data = clientPreferencesData
            let generation = clientPreferencesGeneration
            if data.isEmpty {
                return
            }
            let receipt = try owner.applyClientPreferences(preferences: data)
            _ = try await receipt.wait()
            try Task.checkCancellation()
            if generation == clientPreferencesGeneration {
                return
            }
        }
    }

    private func ownsBackground(
        _ profile: HostProfile,
        owner: AgentStore,
        generation: UInt64
    ) -> Bool {
        !Task.isCancelled
            && selectedProfileId != profile.id
            && profiles.contains(where: { $0.id == profile.id })
            && backgroundTaskGenerations[profile.id] == generation
            && backgroundOwners[profile.id] === owner
    }

    func publishEnvironment(_ profile: HostProfile, _ next: AgentCore.Snapshot) {
        let previous = environmentSnapshots[profile.id]
        let pushPreferencesChanged = previous?.preferences().liveActivitiesEnabled
            != next.preferences().liveActivitiesEnabled
        environmentSnapshots[profile.id] = next
        if let previous {
            if previous.connected(), !next.connected(), let environmentId = previous.environmentId() {
                LocalNotifications.removeEnvironment(environmentId)
            }
            deliverAttentionEvents(previous: previous, current: next)
        }
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

    func publishUsageWidget() {
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

    private func finishPairing(id: String, invitation: Invitation) async {
        let expectedStore = store
        let expectedProfile = selectedProfileId
        do {
            persist()
            await persistenceWrite?.value
            // The new store reads the state the current one keeps for this Host.
            if selectedProfileId == id {
                try? await store?.flush()
            }
            let owner = try await AgentStore.connect(connection: Connection(
                ticket: invitation.endpoint,
                identity: DeviceIdentity.loadOrGenerate(id),
                invitation: invitation.invitation,
                useRelays: true
            ), stateFile: SnapshotFiles.stateFile(id), modelDefaults: clientPreferencesData,
            cacheDirectory: SnapshotFiles.cacheDirectory(id),
            diagnosticsDirectory: SnapshotFiles.diagnosticsDirectory(id))
            guard !Task.isCancelled else { try? await owner.shutdown(); return }
            do {
                try await applyClientPreferences(to: owner)
            } catch {
                try? await owner.shutdown()
                throw error
            }
            guard !Task.isCancelled, selectedProfileId == expectedProfile, store === expectedStore else {
                try? await owner.shutdown()
                return
            }
            let old = detachStore()
            publish(AgentCore.Snapshot.empty())
            profiles.removeAll { $0.id == id }
            profiles.append(HostProfile(id: id, name: invitation.hostName, ticket: invitation.endpoint))
            try HostProfile.save(profiles)
            UserDefaults.standard.set(id, forKey: "bex.selected-host")
            selectedProfileId = id
            store = owner
            publish(owner.snapshot())
            if let id = selectedProfileId {
                registerPushIfReady(id)
            }
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
            isConnecting = false
            pairingError = error.localizedDescription
        }
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
                await self?.finishPairing(id: id, invitation: invitation)
            }
        } catch { pairingError = error.localizedDescription }
    }
}
