import ActivityKit
import CryptoKit
import Foundation
import UIKit
import UserNotifications

/// The device-side tokens and capability facts sent to the Host's direct push
/// registry. Provider credentials never enter this record.
struct AgentPushRegistration: Equatable, Sendable {
    let deviceId: String
    let token: String
    let liveActivityToken: String?
    let pushToStartToken: String?
    let bundleId: String
    let apnsEnvironment: String
    let pushAvailable: Bool
    let notificationsAuthorized: Bool
    let liveActivitiesAvailable: Bool
}

extension Notification.Name {
    static let agentPushDeepLink = Notification.Name("remoteagent.push.deepLink")
    static let remoteAgentShortcut = Notification.Name("remote-agent.shortcut")
}

/// Owns APNs registration and one ActivityKit card per retained Host. A
/// card's content state carries its environment id, so a Host never receives
/// another Host's activity token or overwrites its aggregate.
@MainActor
final class AgentPushCenter: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    static let appGroup = "group.com.ttizze.b-codex"
    private static let deviceIdKey = "push.device-id"

    private let notificationCenter = UNUserNotificationCenter.current()
    var registerHandler: ((String, AgentPushRegistration) -> Void)?
    private var activeHandler: ((String, String, Bool) -> Void)?
    var visibleThread: (() -> String?)?
    private var hostIdsProvider: (() -> [String])?
    private var liveActivitiesProvider: ((String) -> Bool)?
    var deviceToken: String?
    var pushToStartToken: String?
    var pendingDeepLink: String?
    private var notificationEnabled = false
    var activityUpdatesTask: Task<Void, Never>?
    var pushToStartUpdatesTask: Task<Void, Never>?
    var activityTokenTasks: [String: Task<Void, Never>] = [:]
    var activityStateTasks: [String: Task<Void, Never>] = [:]
    var activityIds: [String: String] = [:]
    var activityHosts: [String: String] = [:]
    var activityTokens: [String: String] = [:]
    var startingHosts = Set<String>()
    var pendingActivityStates: [String: AgentActivityAttributes.ContentState]?
    var activityReconciliationRunning = false
    private static var pendingShortcutType: String?

    private var defaults: UserDefaults {
        UserDefaults(suiteName: Self.appGroup) ?? .standard
    }

    func configure(
        register: @escaping (String, AgentPushRegistration) -> Void,
        setActive: @escaping (String, String, Bool) -> Void,
        hostIds: @escaping () -> [String],
        visibleThread: @escaping () -> String?,
        liveActivitiesEnabled: @escaping (String) -> Bool
    ) {
        registerHandler = register
        activeHandler = setActive
        hostIdsProvider = hostIds
        self.visibleThread = visibleThread
        liveActivitiesProvider = liveActivitiesEnabled
        // Wait until the model has supplied the retained Host ids. Looking at
        // Activity.activities during UIApplication launch would otherwise
        // classify every restored card as unknown and end it before profile
        // restoration finishes.
        if #available(iOS 16.1, *) {
            observeActivityTokens()
        }
        submitRegistrations()
        if let pendingDeepLink {
            self.pendingDeepLink = nil
            NotificationCenter.default.post(name: .agentPushDeepLink, object: pendingDeepLink)
        }
        Task { await requestPermissionAndRegister() }
    }

    /// Re-reads the core Live Activities setting and OS authorization after a
    /// settings or foreground transition.
    func refreshPreferences(
        activityStates: [String: AgentActivityAttributes.ContentState]? = nil
    ) {
        submitRegistrations()
        Task {
            await requestPermissionAndRegister()
            if let activityStates, #available(iOS 16.1, *) {
                await reconcileActivities(states: activityStates)
            }
        }
    }

    func application(
        _: UIApplication,
        didFinishLaunchingWithOptions _: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        notificationCenter.delegate = self
        return true
    }

    nonisolated func application(_: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        let value = deviceToken.map { String(format: "%02x", $0) }.joined()
        Task { @MainActor [weak self] in
            guard let self else { return }
            self.deviceToken = value
            submitRegistrations()
        }
    }

    nonisolated func application(_: UIApplication, didFailToRegisterForRemoteNotificationsWithError _: Error) {
        Task { @MainActor [weak self] in
            guard let self else { return }
            notificationEnabled = false
            deviceToken = nil
            setActiveForKnownHosts(false)
            submitRegistrations()
        }
    }

    private func requestPermissionAndRegister() async {
        notificationCenter.delegate = self
        let hosts = knownHostIds()
        let wantsAlerts = !hosts.isEmpty
        let wantsLiveActivities = hosts.contains { liveActivitiesAllowed(for: $0) }
        if !wantsAlerts && !wantsLiveActivities {
            notificationEnabled = false
            submitRegistrations()
            return
        }
        let current = await notificationCenter.notificationSettings()
        if wantsAlerts && current.authorizationStatus == .notDetermined {
            notificationEnabled = await (try? notificationCenter.requestAuthorization(options: [
                .alert,
                .sound,
                .badge
            ])) == true
        } else {
            notificationEnabled = current.authorizationStatus == .authorized
                || current.authorizationStatus == .provisional
                || current.authorizationStatus == .ephemeral
        }
        // ActivityKit push tokens also use APNs registration. Requesting this
        // token does not show an alert when only Live Activities are enabled.
        if notificationEnabled || wantsLiveActivities {
            UIApplication.shared.registerForRemoteNotifications()
        }
        submitRegistrations()
    }

    func knownHostIds() -> [String] {
        Array(Set(hostIdsProvider?() ?? [])).sorted()
    }

    private func liveActivitiesEnabled(for hostId: String) -> Bool {
        liveActivitiesProvider?(hostId) ?? true
    }

    func activityKitAvailable() -> Bool {
        guard #available(iOS 16.1, *) else { return false }
        return ActivityAuthorizationInfo().areActivitiesEnabled
    }

    func liveActivitiesAllowed(for hostId: String) -> Bool {
        liveActivitiesEnabled(for: hostId) && activityKitAvailable()
    }

    private func setActiveForKnownHosts(_ active: Bool) {
        for hostId in knownHostIds() {
            activeHandler?(hostId, deviceId(for: hostId), active)
        }
    }

    func submitRegistrations() {
        guard let deviceToken, !deviceToken.isEmpty else { return }
        for hostId in knownHostIds() {
            submitRegistration(hostId: hostId, token: deviceToken)
        }
    }

    func submitRegistration(hostId: String, token: String) {
        guard !token.isEmpty else { return }
        registerHandler?(
            hostId,
            AgentPushRegistration(
                deviceId: deviceId(for: hostId),
                token: token,
                liveActivityToken: activityTokens[hostId],
                // ActivityKit's push-to-start token is scoped to this app and
                // activity type, rather than to a Host. Every retained Host
                // gets the same token in its own Host-scoped registration;
                // the Host sends only its own content state and the returned
                // per-activity token is associated back by environmentId.
                pushToStartToken: pushToStartToken,
                bundleId: Bundle.main.bundleIdentifier ?? "com.ttizze.b-codex",
                apnsEnvironment: Self.apnsEnvironment,
                pushAvailable: true,
                notificationsAuthorized: notificationEnabled,
                liveActivitiesAvailable: activityKitAvailable()
            )
        )
    }

    private var baseDeviceId: String {
        if let value = defaults.string(forKey: Self.deviceIdKey), !value.isEmpty {
            return value
        }
        let value = UUID().uuidString.lowercased()
        defaults.set(value, forKey: Self.deviceIdKey)
        return value
    }

    private func deviceId(for hostId: String) -> String {
        let candidate = "\(baseDeviceId):\(hostId)"
        guard candidate.utf8.count > 128 else { return candidate }
        let digest = SHA256.hash(data: Data(hostId.utf8)).map { String(format: "%02x", $0) }.joined()
        return "\(baseDeviceId):\(digest)"
    }

    /// Returns the stable per-Host principal even after an in-memory
    /// registration has been discarded during a cold-start profile removal.
    func deviceIdForPush(hostId: String) -> String {
        deviceId(for: hostId)
    }

    func shutdown() {
        activityUpdatesTask?.cancel()
        pushToStartUpdatesTask?.cancel()
        activityUpdatesTask = nil
        pushToStartUpdatesTask = nil
        for task in activityTokenTasks.values {
            task.cancel()
        }
        for task in activityStateTasks.values {
            task.cancel()
        }
        activityTokenTasks.removeAll()
        activityStateTasks.removeAll()
        setActiveForKnownHosts(false)
        registerHandler = nil
        activeHandler = nil
        hostIdsProvider = nil
        visibleThread = nil
        liveActivitiesProvider = nil
    }

    private static var apnsEnvironment: String {
        #if DEBUG
            return "sandbox"
        #else
            return "production"
        #endif
    }

    func application(
        _: UIApplication,
        performActionFor shortcutItem: UIApplicationShortcutItem,
        completionHandler: @escaping (Bool) -> Void
    ) {
        NotificationCenter.default.post(
            name: .remoteAgentShortcut,
            object: nil,
            userInfo: ["type": shortcutItem.type]
        )
        completionHandler(true)
    }

    func application(
        _: UIApplication,
        configurationForConnecting connectingSceneSession: UISceneSession,
        options: UIScene.ConnectionOptions
    ) -> UISceneConfiguration {
        if let shortcut = options.shortcutItem {
            Self.pendingShortcutType = shortcut.type
        }
        UIApplication.shared.shortcutItems = [
            UIApplicationShortcutItem(
                type: "new-thread",
                localizedTitle: "New thread",
                localizedSubtitle: nil,
                icon: UIApplicationShortcutIcon(type: .add),
                userInfo: nil
            )
        ]
        return UISceneConfiguration(name: "Default Configuration", sessionRole: connectingSceneSession.role)
    }

    static func takePendingShortcut() -> String? {
        defer { pendingShortcutType = nil }
        return pendingShortcutType
    }
}
