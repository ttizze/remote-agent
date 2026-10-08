import AgentCore
import Foundation
import UIKit

/// Snapshot observation, notifications and persistence.
@MainActor
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
                try await applyClientPreferences(to: owner)
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

    func observe(_ owner: AgentStore, host: String) {
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

    func publish(
        _ next: AgentCore.Snapshot,
        syncClientPreferences: Bool = true
    ) {
        guard next.supersedes(previous: snapshot), next !== snapshot else { return }
        updateSelectedHostName(from: next)
        if snapshot.error() != next.error() {
            notice = next.error()
        }
        let threadChanged = snapshot.selectedThreadId() != next.selectedThreadId()
        snapshot = next
        publishEnvironmentIfNeeded(next, syncClientPreferences: syncClientPreferences)
        updatePublishedDraft(from: next, threadChanged: threadChanged)
        schedulePresentation()
        schedulePersistence()
    }

    private func updateSelectedHostName(from next: AgentCore.Snapshot) {
        guard let name = next.hostName(),
              let index = profiles.firstIndex(where: { $0.id == selectedProfileId }),
              profiles[index].name != name else { return }
        profiles[index].name = name
        do { try HostProfile.save(profiles) } catch { notice = error.localizedDescription }
    }

    private func publishEnvironmentIfNeeded(
        _ next: AgentCore.Snapshot,
        syncClientPreferences: Bool
    ) {
        guard syncClientPreferences,
              let id = selectedProfileId,
              let profile = profiles.first(where: { $0.id == id }) else { return }
        publishEnvironment(profile, next)
    }

    private func updatePublishedDraft(from next: AgentCore.Snapshot, threadChanged: Bool) {
        if threadChanged {
            threadView = nil
            showScrollToEnd = false
            disclosure = .empty
        }
        let key = next.currentDraftKey()
        if key != composerKey {
            draftEdits.reset(); composerKey = key
        }
        guard draftEdits.pending == nil else { return }
        composerText = next.draft().text
        draftEdits.base = composerText
    }

    private func schedulePersistence() {
        persistence?.cancel()
        persistence = Task { [weak self] in
            do { try await Task.sleep(nanoseconds: 250_000_000) } catch { return }
            self?.persist()
        }
    }

    func deliverAttentionEvents(
        previous: AgentCore.Snapshot,
        current: AgentCore.Snapshot
    ) {
        let appActive = UIApplication.shared.applicationState == .active
        let modeChanged = previous.preferences().notificationMode
            != current.preferences().notificationMode
        if appActive || modeChanged {
            LocalNotifications.clearDelivered()
        }
        let attentionEvents = AgentCore.notificationEvents(
            previous: previous,
            current: current,
            appVisible: appActive,
            appFocused: appActive
        )
        for event in attentionEvents {
            if event.inApp {
                notice = "\(event.kind): \(event.body)"
                notificationThreadRoute = event.deepLink
            }
            if event.operatingSystem {
                LocalNotifications.deliver(
                    title: event.title,
                    body: event.body,
                    sound: event.sound,
                    threadId: event.threadId,
                    deepLink: event.deepLink,
                    kind: String(describing: event.kind),
                    soundKind: String(describing: event.soundKind)
                )
            } else if event.sound {
                LocalNotifications.playSound(soundKind: String(describing: event.soundKind))
            }
        }
        if appActive || !nativeNotificationsEnabled(current) {
            LocalNotifications.clearDelivered()
        } else {
            LocalNotifications.updateBadge()
        }
    }

    func nativeNotificationsEnabled(_ snapshot: AgentCore.Snapshot) -> Bool {
        // Keep the decision in core; this string projection only avoids a
        // second native enum declaration while consuming the generated value.
        String(describing: snapshot.preferences().notificationMode)
            .lowercased()
            .contains("notifications")
    }

    func openNotificationThread() {
        guard let route = notificationThreadRoute else { return }
        LocalNotifications.acknowledge(route)
        if AgentPushCenter.isActivityOverviewDeepLink(route) {
            notificationThreadRoute = nil
            notice = nil
            openActivityOverviewDeepLink()
            return
        }
        guard let target = AgentPushCenter.threadTarget(from: route) else { return }
        notificationThreadRoute = nil
        notice = nil
        openPushThread(hostId: target.hostId, threadId: target.threadId)
    }

    /// Saves the model preferences every Host shares; the store writes its own state.
    func persist() {
        guard let owner = store else { return }
        synchronizeClientPreferences(owner.snapshot())
    }

    func clearSelectedClientPreferencesHandoff() {
        selectedClientPreferencesRetry?.cancel()
        selectedClientPreferencesRetry = nil
        pendingSelectedClientPreferences = nil
    }

    private func beginSelectedClientPreferencesHandoff(
        owner: AgentStore,
        data: Data
    ) -> UInt64 {
        selectedClientPreferencesRetry?.cancel()
        selectedClientPreferencesRetry = nil
        nextClientPreferencesHandoffToken &+= 1
        let token = nextClientPreferencesHandoffToken
        pendingSelectedClientPreferences = .init(
            owner: owner,
            data: data,
            token: token,
            minimumRevision: owner.snapshot().revision() &+ 1
        )
        return token
    }

    private func retrySelectedClientPreferences(
        owner: AgentStore,
        data: Data,
        token: UInt64
    ) {
        selectedClientPreferencesRetry?.cancel()
        selectedClientPreferencesRetry = Task { [weak self, owner] in
            var delayNanoseconds: UInt64 = 250_000_000
            while !Task.isCancelled {
                do {
                    try await Task.sleep(nanoseconds: delayNanoseconds)
                } catch {
                    return
                }
                guard let self,
                      let pending = pendingSelectedClientPreferences,
                      pending.owner === owner,
                      pending.token == token,
                      pending.data == data,
                      store === owner,
                      clientPreferencesData == data else {
                    return
                }
                do {
                    let receipt = try owner.applyClientPreferences(preferences: data)
                    _ = try await receipt.wait()
                    return
                } catch {
                    delayNanoseconds = min(delayNanoseconds * 2, 300_000_000_000)
                }
            }
        }
    }

    func synchronizeClientPreferences(
        _ source: AgentCore.Snapshot,
        includeSelected: Bool = false,
        allowPendingSelected: Bool = false
    ) {
        guard let data = try? source.serializeModelPreferences() else {
            return
        }
        guard admitsClientPreferences(
            source: source,
            data: data,
            includeSelected: includeSelected,
            allowPendingSelected: allowPendingSelected
        ) else { return }
        let selectedNeedsSync = includeSelected && selectedClientPreferencesNeedSync(data)
        let changed = data != clientPreferencesData
        guard changed || selectedNeedsSync else { return }
        if changed {
            persistClientPreferences(data)
        }
        if includeSelected {
            applyClientPreferencesToSelected(data)
        }
        if changed {
            applyClientPreferencesToBackground(data)
        }
    }

    private func admitsClientPreferences(
        source: AgentCore.Snapshot,
        data: Data,
        includeSelected: Bool,
        allowPendingSelected: Bool
    ) -> Bool {
        if allowPendingSelected {
            clearSelectedClientPreferencesHandoff()
            return true
        }
        guard !includeSelected,
              let pending = pendingSelectedClientPreferences,
              let selected = store,
              selected === pending.owner else { return true }
        if data == pending.data {
            guard source.revision() >= pending.minimumRevision else { return false }
            clearSelectedClientPreferencesHandoff()
            return true
        }
        return false
    }

    private func selectedClientPreferencesNeedSync(_ data: Data) -> Bool {
        guard let selected = store else { return false }
        guard let pending = pendingSelectedClientPreferences else { return true }
        return pending.owner !== selected || pending.data != data
    }

    private func persistClientPreferences(_ data: Data) {
        clientPreferencesData = data
        clientPreferencesGeneration &+= 1
        let previous = persistenceWrite
        persistenceWrite = Task { [weak self] in
            await previous?.value
            do {
                try await SnapshotFiles.saveModelPreferences(data)
            } catch { self?.notice = error.localizedDescription }
        }
    }

    private func applyClientPreferencesToSelected(_ data: Data) {
        guard let selected = store else { return }
        let token = beginSelectedClientPreferencesHandoff(owner: selected, data: data)
        guard let receipt = try? selected.applyClientPreferences(preferences: data) else {
            retrySelectedClientPreferences(owner: selected, data: data, token: token)
            return
        }
        Task { [weak self, selected] in
            do {
                _ = try await receipt.wait()
            } catch {
                self?.retrySelectedClientPreferences(owner: selected, data: data, token: token)
            }
        }
    }

    private func applyClientPreferencesToBackground(_ data: Data) {
        for owner in backgroundOwners.values {
            guard let receipt = try? owner.applyClientPreferences(preferences: data) else { continue }
            Task { _ = try? await receipt.wait() }
        }
    }

    func persistBeforeBackground() async {
        persist()
        await persistenceWrite?.value
        try? await store?.flush()
    }
}
