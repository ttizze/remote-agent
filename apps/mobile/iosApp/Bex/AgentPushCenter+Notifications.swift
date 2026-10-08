import UserNotifications

extension AgentPushCenter {
    nonisolated func userNotificationCenter(
        _: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        let data = notification.request.content.userInfo
        let deepLink = Self.deepLink(from: data)
        Task { @MainActor [weak self] in
            if deepLink != nil, deepLink == self?.visibleThread?() {
                completionHandler([])
            } else {
                completionHandler([.banner, .list, .sound])
            }
        }
    }

    nonisolated func userNotificationCenter(
        _: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        let deepLink = Self.deepLink(from: response.notification.request.content.userInfo)
        if let deepLink {
            Task { @MainActor [weak self] in
                guard let self else { return }
                pendingDeepLink = deepLink
                if registerHandler != nil {
                    pendingDeepLink = nil
                    NotificationCenter.default.post(name: .agentPushDeepLink, object: deepLink)
                }
            }
        }
        completionHandler()
    }
}
