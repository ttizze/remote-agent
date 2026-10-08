import ActivityKit
import CryptoKit
import Foundation
import UIKit
import UserNotifications

/// The device-side preferences and tokens sent to the Host's direct push
/// registry. Provider credentials never enter this record.
struct AgentPushRegistration: Equatable, Sendable {
    let deviceId: String
    let token: String
    let liveActivityToken: String?
    let pushToStartToken: String?
    let bundleId: String
    let apnsEnvironment: String
    let notificationsEnabled: Bool
    let notifyOnApproval: Bool
    let notifyOnInput: Bool
    let notifyOnCompletion: Bool
    let notifyOnFailure: Bool
    let liveActivitiesEnabled: Bool
}

/// Preferences come from immutable core snapshots. APNs authorization is a
/// separate OS fact and is combined with this value at registration time.
struct AgentPushPreferences: Equatable, Sendable {
    let notificationsEnabled: Bool
    let notifyOnApproval: Bool
    let notifyOnInput: Bool
    let notifyOnCompletion: Bool
    let notifyOnFailure: Bool
    let liveActivitiesEnabled: Bool

    static let `default` = AgentPushPreferences(
        notificationsEnabled: false,
        notifyOnApproval: true,
        notifyOnInput: true,
        notifyOnCompletion: true,
        notifyOnFailure: true,
        liveActivitiesEnabled: true,
    )
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
    private static let liveActivitiesKey = "push.live-activities-enabled"

    private let notificationCenter = UNUserNotificationCenter.current()
    private var registerHandler: ((String, AgentPushRegistration) -> Void)?
    private var activeHandler: ((String, String, Bool) -> Void)?
    private var visibleThread: (() -> String?)?
    private var hostIdsProvider: (() -> [String])?
    private var preferencesProvider: ((String) -> AgentPushPreferences)?
    private var deviceToken: String?
    private var pushToStartToken: String?
    private var pendingDeepLink: String?
    private var notificationEnabled = false
    private var activityUpdatesTask: Task<Void, Never>?
    private var pushToStartUpdatesTask: Task<Void, Never>?
    private var activityTokenTasks: [String: Task<Void, Never>] = [:]
    private var activityStateTasks: [String: Task<Void, Never>] = [:]
    private var activityIds: [String: String] = [:]
    private var activityHosts: [String: String] = [:]
    private var activityTokens: [String: String] = [:]
    private var startingHosts = Set<String>()
    private var pendingActivityStates: [String: AgentActivityAttributes.ContentState]?
    private var activityReconciliationRunning = false
    private static var pendingShortcutType: String?

    private var defaults: UserDefaults {
        UserDefaults(suiteName: Self.appGroup) ?? .standard
    }

    func configure(
        register: @escaping (String, AgentPushRegistration) -> Void,
        setActive: @escaping (String, String, Bool) -> Void,
        hostIds: @escaping () -> [String],
        visibleThread: @escaping () -> String?,
        preferences: @escaping (String) -> AgentPushPreferences,
    ) {
        registerHandler = register
        activeHandler = setActive
        hostIdsProvider = hostIds
        self.visibleThread = visibleThread
        preferencesProvider = preferences
        // Wait until the model has supplied the retained Host ids. Looking at
        // Activity.activities during UIApplication launch would otherwise
        // classify every restored card as unknown and end it before profile
        // restoration finishes.
        if #available(iOS 16.1, *) { observeActivityTokens() }
        submitRegistrations()
        if let pendingDeepLink {
            self.pendingDeepLink = nil
            NotificationCenter.default.post(name: .agentPushDeepLink, object: pendingDeepLink)
        }
        Task { await requestPermissionAndRegister() }
    }

    /// Re-reads core notification preferences after a settings mutation.
    /// Permission is requested only when a Host opts into alerts.
    func refreshPreferences() {
        submitRegistrations()
        Task { await requestPermissionAndRegister() }
    }

    func application(
        _: UIApplication,
        didFinishLaunchingWithOptions _: [UIApplication.LaunchOptionsKey: Any]? = nil,
    ) -> Bool {
        notificationCenter.delegate = self
        return true
    }

    nonisolated func application(_: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        let value = deviceToken.map { String(format: "%02x", $0) }.joined()
        Task { @MainActor [weak self] in
            guard let self else { return }
            self.deviceToken = value
            self.submitRegistrations()
            self.setActiveForKnownHosts()
        }
    }

    nonisolated func application(_: UIApplication, didFailToRegisterForRemoteNotificationsWithError _: Error) {
        Task { @MainActor [weak self] in
            guard let self else { return }
            self.notificationEnabled = false
            self.deviceToken = nil
            self.setActiveForKnownHosts(false)
            self.submitRegistrations()
        }
    }

    private func requestPermissionAndRegister() async {
        notificationCenter.delegate = self
        let hosts = knownHostIds()
        let wantsAlerts = hosts.contains { preferences(for: $0).notificationsEnabled }
        if !wantsAlerts {
            notificationEnabled = false
            if liveActivitiesEnabled { UIApplication.shared.registerForRemoteNotifications() }
            submitRegistrations()
            setActiveForKnownHosts()
            return
        }
        let current = await notificationCenter.notificationSettings()
        if current.authorizationStatus == .notDetermined {
            notificationEnabled = await (try? notificationCenter.requestAuthorization(options: [.alert, .sound, .badge])) == true
        } else {
            notificationEnabled = current.authorizationStatus == .authorized
                || current.authorizationStatus == .provisional
                || current.authorizationStatus == .ephemeral
        }
        // ActivityKit push tokens also use APNs registration. Requesting this
        // token does not show an alert when the core preference is disabled.
        if notificationEnabled || liveActivitiesEnabled { UIApplication.shared.registerForRemoteNotifications() }
        submitRegistrations()
        setActiveForKnownHosts()
    }

    private func knownHostIds() -> [String] {
        Array(Set(hostIdsProvider?() ?? [])).sorted()
    }

    private func preferences(for hostId: String) -> AgentPushPreferences {
        var value = preferencesProvider?(hostId) ?? .default
        value = AgentPushPreferences(
            notificationsEnabled: value.notificationsEnabled,
            notifyOnApproval: value.notifyOnApproval,
            notifyOnInput: value.notifyOnInput,
            notifyOnCompletion: value.notifyOnCompletion,
            notifyOnFailure: value.notifyOnFailure,
            liveActivitiesEnabled: value.liveActivitiesEnabled && liveActivitiesEnabled,
        )
        return value
    }

    private func setActiveForKnownHosts(_ active: Bool) {
        for hostId in knownHostIds() {
            activeHandler?(hostId, deviceId(for: hostId), active)
        }
    }

    private func setActiveForKnownHosts() {
        for hostId in knownHostIds() {
            let preferences = preferences(for: hostId)
            let active = deviceToken != nil &&
                (preferences.liveActivitiesEnabled || (notificationEnabled && preferences.notificationsEnabled))
            activeHandler?(hostId, deviceId(for: hostId), active)
        }
    }

    private func submitRegistrations() {
        guard let deviceToken, !deviceToken.isEmpty else { return }
        for hostId in knownHostIds() { submitRegistration(hostId: hostId, token: deviceToken) }
    }

    private func submitRegistration(hostId: String, token: String) {
        guard !token.isEmpty else { return }
        let preferences = preferences(for: hostId)
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
                notificationsEnabled: notificationEnabled && preferences.notificationsEnabled,
                notifyOnApproval: preferences.notifyOnApproval,
                notifyOnInput: preferences.notifyOnInput,
                notifyOnCompletion: preferences.notifyOnCompletion,
                notifyOnFailure: preferences.notifyOnFailure,
                liveActivitiesEnabled: preferences.liveActivitiesEnabled,
            )
        )
    }

    private var baseDeviceId: String {
        if let value = defaults.string(forKey: Self.deviceIdKey), !value.isEmpty { return value }
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

    private var liveActivitiesEnabled: Bool {
        defaults.object(forKey: Self.liveActivitiesKey) as? Bool ?? true
    }

    func setLiveActivitiesEnabled(_ enabled: Bool) {
        defaults.set(enabled, forKey: Self.liveActivitiesKey)
        if !enabled {
            if #available(iOS 16.1, *) { endAllActivities() }
        }
        submitRegistrations()
        setActiveForKnownHosts()
    }

    func shutdown() {
        activityUpdatesTask?.cancel()
        pushToStartUpdatesTask?.cancel()
        activityUpdatesTask = nil
        pushToStartUpdatesTask = nil
        for task in activityTokenTasks.values { task.cancel() }
        for task in activityStateTasks.values { task.cancel() }
        activityTokenTasks.removeAll()
        activityStateTasks.removeAll()
        setActiveForKnownHosts(false)
        registerHandler = nil
        activeHandler = nil
        hostIdsProvider = nil
        visibleThread = nil
        preferencesProvider = nil
    }

    private static var apnsEnvironment: String {
#if DEBUG
        return "sandbox"
#else
        return "production"
#endif
    }

    /// Starts one local ActivityKit card for a specific Host and listens for
    /// its push token. The Host can continue that card while the app is closed.
    @available(iOS 16.1, *)
    func startActivity(hostId: String, contentState: AgentActivityAttributes.ContentState) async throws -> String {
        let activity = try Activity<AgentActivityAttributes>.request(
            attributes: AgentActivityAttributes(),
            content: ActivityContent(
                state: contentState,
                staleDate: Date().addingTimeInterval(600),
            ),
            pushType: .token,
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
            dismissalPolicy: .after(Date().addingTimeInterval(300)),
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
        guard liveActivitiesEnabled else { endAllActivities(); return }
        let known = Set(knownHostIds())
        for (hostId, id) in activityIds where !known.contains(hostId) {
            await endActivity(
                hostId: hostId,
                contentState: states[hostId] ?? emptyContentState(),
            )
        }
        for hostId in known {
            let state = states[hostId]
            if let state, state.activeCount > 0 {
                if activityIds[hostId] != nil {
                    await updateActivity(hostId: hostId, contentState: state)
                } else if UIApplication.shared.applicationState == .active,
                          !startingHosts.contains(hostId) {
                    startingHosts.insert(hostId)
                    do { _ = try await startActivity(hostId: hostId, contentState: state) }
                    catch { startingHosts.remove(hostId) }
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
    private func endAllActivities() {
        let activities = Activity<AgentActivityAttributes>.activities
        activityIds.removeAll()
        activityHosts.removeAll()
        activityTokens.removeAll()
        startingHosts.removeAll()
        for task in activityTokenTasks.values { task.cancel() }
        for task in activityStateTasks.values { task.cancel() }
        activityTokenTasks.removeAll()
        activityStateTasks.removeAll()
        pendingActivityStates = nil
        submitRegistrations()
        for activity in activities {
            Task { @MainActor in
                await activity.end(
                    ActivityContent(state: activity.content.state, staleDate: nil),
                    dismissalPolicy: .immediate,
                )
            }
        }
    }

    @available(iOS 16.1, *)
    private func observeActivityTokens() {
        guard activityUpdatesTask == nil else { return }
        activityUpdatesTask = Task { @MainActor [weak self] in
            for await activity in Activity<AgentActivityAttributes>.activityUpdates {
                guard let self else { return }
                let hostId = self.hostId(for: activity)
                guard self.knownHostIds().contains(hostId) else {
                    Task { @MainActor in
                        await activity.end(
                            ActivityContent(state: activity.content.state, staleDate: nil),
                            dismissalPolicy: .immediate,
                        )
                    }
                    continue
                }
                self.activityIds[hostId] = activity.id
                self.activityHosts[activity.id] = hostId
                self.observeActivityToken(activity, hostId: hostId)
            }
        }
        if #available(iOS 17.2, *), pushToStartUpdatesTask == nil {
            pushToStartUpdatesTask = Task { @MainActor [weak self] in
                for await token in Activity<AgentActivityAttributes>.pushToStartTokenUpdates {
                    guard let self else { return }
                    self.pushToStartToken = token.map { String(format: "%02x", $0) }.joined()
                    self.submitRegistrations()
                }
            }
        }
        for activity in Activity<AgentActivityAttributes>.activities {
            let hostId = hostId(for: activity)
            guard knownHostIds().contains(hostId) else {
                Task { @MainActor in
                    await activity.end(
                        ActivityContent(state: activity.content.state, staleDate: nil),
                        dismissalPolicy: .immediate,
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
        if let known = activityHosts[activity.id] { return known }
        return activity.content.state.activities.first?.environmentId ?? "unknown"
    }

    @available(iOS 16.1, *)
    private func observeActivityToken(_ activity: Activity<AgentActivityAttributes>, hostId: String) {
        activityTokenTasks[hostId]?.cancel()
        activityStateTasks[hostId]?.cancel()
        activityTokenTasks[hostId] = Task { @MainActor [weak self] in
            for await token in activity.pushTokenUpdates {
                guard let self else { return }
                guard self.activityIds[hostId] == activity.id else { return }
                self.activityTokens[hostId] = token.map { String(format: "%02x", $0) }.joined()
                self.submitRegistration(hostId: hostId, token: self.deviceToken ?? "")
            }
        }
        activityStateTasks[hostId] = Task { @MainActor [weak self] in
            for await state in activity.activityStateUpdates {
                guard let self else { return }
                guard state == .ended || state == .dismissed || state == .stale else { continue }
                self.removeActivity(hostId: hostId, id: activity.id)
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

    nonisolated func userNotificationCenter(
        _: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void,
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
        withCompletionHandler completionHandler: @escaping () -> Void,
    ) {
        let deepLink = Self.deepLink(from: response.notification.request.content.userInfo)
        if let deepLink {
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.pendingDeepLink = deepLink
                if self.registerHandler != nil {
                    self.pendingDeepLink = nil
                    NotificationCenter.default.post(name: .agentPushDeepLink, object: deepLink)
                }
            }
        }
        completionHandler()
    }

    func application(
        _: UIApplication,
        performActionFor shortcutItem: UIApplicationShortcutItem,
        completionHandler: @escaping (Bool) -> Void,
    ) {
        NotificationCenter.default.post(
            name: .remoteAgentShortcut,
            object: nil,
            userInfo: ["type": shortcutItem.type],
        )
        completionHandler(true)
    }

    func application(
        _: UIApplication,
        configurationForConnecting connectingSceneSession: UISceneSession,
        options: UIScene.ConnectionOptions,
    ) -> UISceneConfiguration {
        if let shortcut = options.shortcutItem { Self.pendingShortcutType = shortcut.type }
        UIApplication.shared.shortcutItems = [
            UIApplicationShortcutItem(
                type: "new-thread",
                localizedTitle: "New thread",
                localizedSubtitle: nil,
                icon: UIApplicationShortcutIcon(type: .add),
                userInfo: nil,
            )
        ]
        return UISceneConfiguration(name: "Default Configuration", sessionRole: connectingSceneSession.role)
    }

    static func takePendingShortcut() -> String? {
        defer { pendingShortcutType = nil }
        return pendingShortcutType
    }

    static func deepLink(from userInfo: [AnyHashable: Any]) -> String? {
        if let value = userInfo["deepLink"] as? String {
            if isActivityOverviewDeepLink(value) { return value }
            if validThreadDeepLink(value) { return value }
        }
        guard let environment = userInfo["environmentId"] as? String,
              let thread = userInfo["threadId"] as? String,
              !environment.isEmpty,
              !thread.isEmpty,
              let environment = environment.addingPercentEncoding(withAllowedCharacters: pathComponentCharacters),
              let thread = thread.addingPercentEncoding(withAllowedCharacters: pathComponentCharacters)
        else { return nil }
        return "remoteagent://threads/\(environment)/\(thread)"
    }

    static func threadDeepLink(hostId: String, threadId: String) -> String {
        let host = hostId.addingPercentEncoding(withAllowedCharacters: pathComponentCharacters) ?? hostId
        let thread = threadId.addingPercentEncoding(withAllowedCharacters: pathComponentCharacters) ?? threadId
        return "remoteagent://threads/\(host)/\(thread)"
    }

    static func isUsageDeepLink(_ value: String) -> Bool {
        guard let components = URLComponents(string: value),
              components.scheme == "remoteagent",
              components.host == "settings",
              components.percentEncodedPath == "/usage",
              components.user == nil,
              components.password == nil,
              components.port == nil,
              components.fragment == nil
        else { return false }
        let queryItems = components.queryItems ?? []
        return queryItems.count == 1 && queryItems[0].name == "tab" && queryItems[0].value == "limits"
    }

    static func isActivityOverviewDeepLink(_ value: String) -> Bool {
        value == AgentCore.agentActivityOverviewDeepLink()
    }

    private static func validThreadDeepLink(_ value: String) -> Bool { threadTarget(from: value) != nil }

    static func threadTarget(from value: String) -> (hostId: String, threadId: String)? {
        guard value.utf16.count <= 512,
              let url = URL(string: value),
              url.scheme == "remoteagent",
              url.host == "threads",
              url.user == nil,
              url.password == nil,
              url.port == nil,
              url.query == nil,
              url.fragment == nil else { return nil }
        let components = url.percentEncodedPath.split(separator: "/", omittingEmptySubsequences: false)
        guard components.count == 3,
              components[0].isEmpty,
              !components[1].isEmpty,
              !components[2].isEmpty,
              let hostId = components[1].removingPercentEncoding,
              let threadId = components[2].removingPercentEncoding,
              !hostId.isEmpty,
              !threadId.isEmpty,
              validRouteSegment(hostId),
              validRouteSegment(threadId) else { return nil }
        return (hostId, threadId)
    }

    private static func validRouteSegment(_ value: String) -> Bool {
        !value.isEmpty && value != "." && value != ".." && value.unicodeScalars.allSatisfy { scalar in
            scalar.value != 0x2f && scalar.value != 0x5c && !CharacterSet.controlCharacters.contains(scalar)
        }
    }

    private static var pathComponentCharacters: CharacterSet {
        var characters = CharacterSet.alphanumerics
        characters.insert(charactersIn: "-._~")
        return characters
    }

    @available(iOS 16.1, *)
    private func emptyContentState() -> AgentActivityAttributes.ContentState {
        AgentActivityAttributes.ContentState(
            title: "Agent activity",
            subtitle: "Agent work completed",
            activeCount: 0,
            updatedAt: ISO8601DateFormatter().string(from: Date()),
            activities: [],
        )
    }
}
