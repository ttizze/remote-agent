import AgentCore
import Foundation
import UIKit

extension BexAppViewModel {
    func connect(afterForeground: Bool = false) {
        guard screen != .pairing, let owner = store,
              let profile = profiles.first(where: { $0.id == selectedProfileId }),
              !isConnecting || afterForeground else { return }
        connection?.cancel()
        isConnecting = true
        notice = nil
        connection = Task { [weak self] in
            let started = ProcessInfo.processInfo.systemUptime
            recordScene(UIApplication.shared.applicationState == .active ? 1 : 2)
            owner.recordConnectionEvent(phase: .uiConnectStart, value: afterForeground ? 1 : 0)
            do {
                let identityStarted = ProcessInfo.processInfo.systemUptime
                let identity = try DeviceIdentity.loadOrGenerate(profile.id)
                owner.recordConnectionEvent(
                    phase: .identityRead,
                    value: UInt64((ProcessInfo.processInfo.systemUptime - identityStarted) * 1_000_000)
                )
                let reused = try await owner.resume(connection: Connection(ticket: profile.ticket,
                                                                           identity: identity,
                                                                           invitation: nil, useRelays: true))
                guard let self, selectedProfileId == profile.id, !Task.isCancelled else { return }
                let elapsed = (ProcessInfo.processInfo.systemUptime - started) * 1_000_000
                owner.recordConnectionEvent(phase: .uiConnectReady, value: UInt64(elapsed))
                publish(owner.snapshot())
                if !reused {
                    liveActivities.resendTokens(hostID: profile.id)
                }
                if snapshot.hostName() == nil {
                    perform(.loadHostName(LoadHostName()))
                }
                notice = snapshot.error()
                isConnecting = false
            } catch {
                owner.recordConnectionEvent(
                    phase: Task.isCancelled ? .uiConnectCancelled : .uiConnectFailed,
                    value: UInt64((ProcessInfo.processInfo.systemUptime - started) * 1_000_000)
                )
                guard self?.selectedProfileId == profile.id, !Task.isCancelled else { return }
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
                    guard let self, selectedProfileId == host, !Task.isCancelled else { return }
                    let latest = owner.snapshot()
                    publish(latest)
                    if previous.connected(), !latest.connected(), !isConnecting {
                        // Network changes and foreground activation can retry a failed attempt.
                        connect()
                    }
                    previous = latest
                } catch { return }
            }
        }
    }
}
