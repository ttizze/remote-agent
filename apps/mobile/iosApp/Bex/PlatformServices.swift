import RemoteAgentMobile

@MainActor
final class BexPlatformBridge {
    let discovery = BexBonjourHostDiscovery()

    func start() {
        discovery.onAddressesChanged = { addresses in
            IosBonjourBridge.shared.update(addresses: addresses)
        }
        discovery.start()
    }

    func didEnterBackground() {
        IosLifecycleBridge.shared.didEnterBackground()
    }

    /// iOS may stop the process while backgrounded. Recovery always obtains a
    /// fresh Host snapshot rather than trusting the on-device cache.
    func restoreAfterForeground() {
        discovery.start()
        IosLifecycleBridge.shared.restoreAfterForeground()
    }
}
