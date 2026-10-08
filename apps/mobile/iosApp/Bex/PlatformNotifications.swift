import AgentCore
import AVFoundation
import UIKit
import UserNotifications

enum LocalNotifications {
    private static var authorized: Bool?
    private static var authorizationRequestInFlight = false
    private struct Pending {
        let title: String
        let body: String
        let sound: Bool
        let threadId: String?
        let deepLink: String?
        let kind: String?
        let soundKind: String?
    }

    private static var pending: [Pending] = []
    /// Stable route ownership makes a replacement keep one posted notice and
    /// lets a departed Host retract only its own notices.
    private static var postedIdentifiers: [String: String] = [:]
    private static var foregroundPlayer: AVAudioPlayer?

    private static func schedule(_ pendingRequest: Pending) {
        let key = notificationKey(pendingRequest)
        let center = UNUserNotificationCenter.current()
        if let oldIdentifier = postedIdentifiers.removeValue(forKey: key) {
            center.removeDeliveredNotifications(withIdentifiers: [oldIdentifier])
            center.removePendingNotificationRequests(withIdentifiers: [oldIdentifier])
        }
        let identifier = "remoteagent.local.\(UUID().uuidString)"
        postedIdentifiers[key] = identifier
        let content = UNMutableNotificationContent()
        content.title = pendingRequest.title
        content.body = pendingRequest.body
        content.sound = pendingRequest.sound ? notificationSound(for: pendingRequest.soundKind) : nil
        content.badge = NSNumber(value: postedIdentifiers.count)
        var userInfo: [AnyHashable: Any] = [:]
        if let threadId = pendingRequest.threadId {
            // Keep same-named threads from different Hosts in separate
            // notification groups; the deep link carries the owning route.
            content.threadIdentifier = pendingRequest.deepLink ?? threadId
            userInfo["threadId"] = threadId
        }
        if let deepLink = pendingRequest.deepLink {
            content.threadIdentifier = deepLink
            userInfo["deepLink"] = deepLink
        }
        if let kind = pendingRequest.kind {
            userInfo["kind"] = kind
        }
        if let soundKind = pendingRequest.soundKind {
            userInfo["soundKind"] = soundKind
        }
        if !userInfo.isEmpty {
            content.userInfo = userInfo
        }
        let trigger = UNTimeIntervalNotificationTrigger(timeInterval: 0.1, repeats: false)
        let notificationRequest = UNNotificationRequest(
            identifier: identifier, content: content, trigger: trigger
        )
        center.add(notificationRequest) { error in
            guard error != nil else { return }
            DispatchQueue.main.async {
                if Self.postedIdentifiers[key] == identifier {
                    Self.postedIdentifiers.removeValue(forKey: key)
                    Self.updateBadge()
                }
            }
        }
    }

    private static func notificationSound(for soundKind: String?) -> UNNotificationSound {
        let kind = soundKind?.split(separator: ".").last.map(String.init)?.uppercased()
        let name = kind == "COMPLETION" ? "RemoteAgentCompletion.caf" : "RemoteAgentInput.caf"
        return UNNotificationSound(named: UNNotificationSoundName(name))
    }

    static func deliver(
        title: String,
        body: String,
        sound: Bool,
        threadId: String? = nil,
        deepLink: String? = nil,
        kind: String? = nil,
        soundKind: String? = nil
    ) {
        let request = Pending(
            title: title, body: body, sound: sound, threadId: threadId,
            deepLink: deepLink, kind: kind, soundKind: soundKind
        )
        if authorized == true {
            schedule(request)
            return
        }
        guard authorized != false else {
            pending.removeAll()
            return
        }
        if let deepLink {
            pending.removeAll { $0.deepLink == deepLink }
        } else {
            pending.removeAll { $0.threadId == threadId }
        }
        pending.append(request)
        guard authorized == nil, !authorizationRequestInFlight else { return }
        authorizationRequestInFlight = true
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound, .badge]) { granted, _ in
            DispatchQueue.main.async {
                Self.authorizationRequestInFlight = false
                Self.authorized = granted
                if !granted {
                    Self.pending.removeAll()
                }
                Self.flushPendingIfAuthorized()
            }
        }
    }

    static func refreshAuthorization() {
        UNUserNotificationCenter.current().getNotificationSettings { settings in
            DispatchQueue.main.async {
                switch settings.authorizationStatus {
                case .authorized, .provisional:
                    Self.authorized = true
                case .denied:
                    Self.authorized = false
                    Self.pending.removeAll()
                case .notDetermined:
                    // The permission sheet may still be visible. Keep
                    // queued events until the request completion reports a
                    // real grant or denial.
                    if Self.authorized != true {
                        Self.authorized = nil
                    }
                @unknown default:
                    Self.authorized = false
                    Self.pending.removeAll()
                }
                Self.flushPendingIfAuthorized()
            }
        }
    }

    private static func flushPendingIfAuthorized() {
        guard authorized == true else { return }
        let requests = pending
        pending.removeAll()
        requests.forEach(schedule)
    }

    /// The native badge counts successfully posted notices, including a
    /// completed notice until focus and excluding in-app toasts.
    static func updateBadge() {
        UIApplication.shared.applicationIconBadgeNumber = postedIdentifiers.count
    }

    static func clearDelivered() {
        let center = UNUserNotificationCenter.current()
        var identifiers = Set(postedIdentifiers.values)
        postedIdentifiers.removeAll()
        pending.removeAll()
        center.getDeliveredNotifications { notifications in
            identifiers.formUnion(
                notifications
                    .map(\.request.identifier)
                    .filter { $0.hasPrefix("remoteagent.local.") }
            )
            let values = Array(identifiers)
            guard !values.isEmpty else { return }
            center.removeDeliveredNotifications(withIdentifiers: values)
            center.removePendingNotificationRequests(withIdentifiers: values)
        }
        center.getPendingNotificationRequests { requests in
            let values = requests
                .map(\.identifier)
                .filter { $0.hasPrefix("remoteagent.local.") }
            guard !values.isEmpty else { return }
            center.removePendingNotificationRequests(withIdentifiers: values)
        }
        updateBadge()
    }

    static func acknowledge(_ deepLink: String?) {
        guard let deepLink else { return }
        let key = notificationKey(deepLink: deepLink, threadId: nil, title: nil, body: nil)
        guard let identifier = postedIdentifiers.removeValue(forKey: key) else { return }
        let center = UNUserNotificationCenter.current()
        center.removeDeliveredNotifications(withIdentifiers: [identifier])
        center.removePendingNotificationRequests(withIdentifiers: [identifier])
        updateBadge()
    }

    static func removeEnvironment(_ environmentId: String) {
        let keys = postedIdentifiers.keys.filter {
            AgentPushCenter.threadTarget(from: $0.replacingOccurrences(of: "route:", with: ""))?.hostId == environmentId
        }
        let identifiers = keys.compactMap { postedIdentifiers.removeValue(forKey: $0) }
        pending.removeAll {
            guard let deepLink = $0.deepLink else { return false }
            return AgentPushCenter.threadTarget(from: deepLink)?.hostId == environmentId
        }
        let center = UNUserNotificationCenter.current()
        center.removeDeliveredNotifications(withIdentifiers: identifiers)
        center.removePendingNotificationRequests(withIdentifiers: identifiers)
        updateBadge()
    }

    private static func notificationKey(_ request: Pending) -> String {
        notificationKey(
            deepLink: request.deepLink,
            threadId: request.threadId,
            title: request.title,
            body: request.body
        )
    }

    private static func notificationKey(
        deepLink: String?,
        threadId: String?,
        title: String?,
        body: String?
    ) -> String {
        if let deepLink {
            return "route:\(deepLink)"
        }
        if let threadId {
            return "thread:\(threadId)"
        }
        return "body:\(title ?? "")\n\(body ?? "")"
    }

    static func playSound(soundKind: String? = nil) {
        // The foreground path has no UNNotificationContent to carry the
        // event's sound kind, so select the same bundled cue as OS delivery.
        let kind = soundKind?.split(separator: ".").last.map(String.init)?.uppercased()
        let resource = kind == "COMPLETION" ? "RemoteAgentCompletion" : "RemoteAgentInput"
        guard let url = Bundle.main.url(forResource: resource, withExtension: "caf"),
              let player = try? AVAudioPlayer(contentsOf: url) else { return }
        foregroundPlayer = player
        player.prepareToPlay()
        player.play()
    }
}
