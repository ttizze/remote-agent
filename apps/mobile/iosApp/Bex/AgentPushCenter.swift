import ActivityKit
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

/// Preferences come from the immutable core snapshot. APNs permission is a
/// separate OS fact and is combined with this value at registration time.
struct AgentPushPreferences: Equatable, Sendable {
    let notificationsEnabled: Bool
    let notifyOnApproval: Bool
    let notifyOnInput: Bool
    let notifyOnCompletion: Bool
    let notifyOnFailure: Bool
    let liveActivitiesEnabled: Bool

    /// Live Activities may register their token without prompting for alerts;
    /// normal alert permission remains opt-in in the core settings.
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
}

/// Owns APNs registration and the single aggregate ActivityKit card. The
/// application model supplies the three Host mutations after it has a live
/// AgentStore connection; keeping those callbacks here makes background token
/// changes independent of the visible SwiftUI screen.
@MainActor
final class AgentPushCenter: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    static let appGroup = "group.com.ttizze.b-codex"
    private static let deviceIdKey = "push.device-id"
    private static let liveActivitiesKey = "push.live-activities-enabled"

    private let notificationCenter = UNUserNotificationCenter.current()
    private var registerHandler: ((AgentPushRegistration) -> Void)?
    private var activeHandler: ((String, Bool) -> Void)?
    private var visibleThread: (() -> String?)?
    private var preferencesProvider: (() -> AgentPushPreferences)?
    private var deviceToken: String?
    private var liveActivityToken: String?
    private var pushToStartToken: String?
    private var pendingDeepLink: String?
    private var notificationEnabled = false
    private var activityTokenTask: Task<Void, Never>?
    private var activityStateTask: Task<Void, Never>?
    private var activityUpdatesTask: Task<Void, Never>?
    private var pushToStartUpdatesTask: Task<Void, Never>?
    private var localActivityId: String?
    private var observedActivityId: String?

    private var defaults: UserDefaults {
        UserDefaults(suiteName: Self.appGroup) ?? .standard
    }

    func configure(
        register: @escaping (AgentPushRegistration) -> Void,
        setActive: @escaping (String, Bool) -> Void,
        visibleThread: @escaping () -> String?,
        preferences: @escaping () -> AgentPushPreferences,
    ) {
        registerHandler = register
        activeHandler = setActive
        self.visibleThread = visibleThread
        preferencesProvider = preferences
        if #available(iOS 16.1, *) {
            observeActivityTokens()
        }
        submitRegistration()
        if let pendingDeepLink {
            self.pendingDeepLink = nil
            NotificationCenter.default.post(name: .agentPushDeepLink, object: pendingDeepLink)
        }
        Task { await requestPermissionAndRegister() }
    }

    /// Re-reads core notification preferences after a settings mutation.
    /// Permission is requested only when the new core value opts into alerts.
    func refreshPreferences() {
        submitRegistration()
        Task { await requestPermissionAndRegister() }
    }

    func application(
        _: UIApplication,
        didFinishLaunchingWithOptions _: [UIApplication.LaunchOptionsKey: Any]? = nil,
    ) -> Bool {
        notificationCenter.delegate = self
        if #available(iOS 16.1, *) {
            observeActivityTokens()
        }
        return true
    }

    nonisolated func application(_: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        let value = deviceToken.map { String(format: "%02x", $0) }.joined()
        Task { @MainActor [weak self] in
            guard let self else { return }
            self.deviceToken = value
            self.submitRegistration()
        }
    }

    nonisolated func application(_: UIApplication, didFailToRegisterForRemoteNotificationsWithError _: Error) {
        Task { @MainActor [weak self] in
            guard let self else { return }
            self.notificationEnabled = false
            // A failed APNs registration cannot deliver either normal pushes
            // or remote ActivityKit starts. Leave the stored registration for
            // a later retry, but make the Host inactive until APNs succeeds.
            self.deviceToken = nil
            self.activeHandler?(self.deviceId, false)
            self.submitRegistration()
        }
    }

    private func requestPermissionAndRegister() async {
        notificationCenter.delegate = self
        let preferences = effectivePreferences()
        if !preferences.notificationsEnabled {
            notificationEnabled = false
            if preferences.liveActivitiesEnabled {
                UIApplication.shared.registerForRemoteNotifications()
            }
            activeHandler?(deviceId, preferences.liveActivitiesEnabled)
            submitRegistration()
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
        if notificationEnabled {
            UIApplication.shared.registerForRemoteNotifications()
        } else if preferences.liveActivitiesEnabled {
            UIApplication.shared.registerForRemoteNotifications()
        }
        activeHandler?(deviceId, isActive(effectivePreferences()))
        submitRegistration()
    }

    private func submitRegistration() {
        guard let deviceToken, !deviceToken.isEmpty else { return }
        let preferences = effectivePreferences()
        registerHandler?(
            AgentPushRegistration(
                deviceId: deviceId,
                token: deviceToken,
                liveActivityToken: liveActivityToken,
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

    private var deviceId: String {
        if let value = defaults.string(forKey: Self.deviceIdKey), !value.isEmpty {
            return value
        }
        let value = UUID().uuidString.lowercased()
        defaults.set(value, forKey: Self.deviceIdKey)
        return value
    }

    private var liveActivitiesEnabled: Bool {
        defaults.object(forKey: Self.liveActivitiesKey) as? Bool ?? true
    }

    func setLiveActivitiesEnabled(_ enabled: Bool) {
        defaults.set(enabled, forKey: Self.liveActivitiesKey)
        if !enabled {
            endAllActivities()
        }
        activeHandler?(deviceId, isActive(effectivePreferences()))
        submitRegistration()
    }

    private func effectivePreferences() -> AgentPushPreferences {
        let provided = preferencesProvider?() ?? .default
        return AgentPushPreferences(
            notificationsEnabled: provided.notificationsEnabled,
            notifyOnApproval: provided.notifyOnApproval,
            notifyOnInput: provided.notifyOnInput,
            notifyOnCompletion: provided.notifyOnCompletion,
            notifyOnFailure: provided.notifyOnFailure,
            liveActivitiesEnabled: provided.liveActivitiesEnabled && liveActivitiesEnabled,
        )
    }

    private func isActive(_ preferences: AgentPushPreferences) -> Bool {
        (notificationEnabled && preferences.notificationsEnabled) || preferences.liveActivitiesEnabled
    }

    func deactivate() {
        activeHandler?(deviceId, false)
        if #available(iOS 16.1, *) {
            endAllActivities()
        }
    }

    func shutdown() {
        activityTokenTask?.cancel()
        activityStateTask?.cancel()
        activityUpdatesTask?.cancel()
        pushToStartUpdatesTask?.cancel()
        activityTokenTask = nil
        activityStateTask = nil
        activityUpdatesTask = nil
        pushToStartUpdatesTask = nil
        localActivityId = nil
        observedActivityId = nil
        activeHandler?(deviceId, false)
        registerHandler = nil
        activeHandler = nil
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

    /// Starts one local ActivityKit card and listens for its push token. The
    /// Host can then continue it while the app is closed through APNs.
    @available(iOS 16.1, *)
    func startActivity(contentState: AgentActivityAttributes.ContentState) async throws -> String {
        let activity = try Activity<AgentActivityAttributes>.request(
            attributes: AgentActivityAttributes(),
            content: ActivityContent(
                state: contentState,
                staleDate: Date().addingTimeInterval(600),
            ),
            pushType: .token,
        )
        observeActivityToken(activity)
        return activity.id
    }

    @available(iOS 16.1, *)
    func updateActivity(
        id: String,
        contentState: AgentActivityAttributes.ContentState,
    ) async {
        guard let activity = Activity<AgentActivityAttributes>.activities.first(where: { $0.id == id }) else {
            return
        }
        await activity.update(
            ActivityContent(
                state: contentState,
                staleDate: Date().addingTimeInterval(600),
            )
        )
    }

    @available(iOS 16.1, *)
    func endActivity(
        id: String,
        contentState: AgentActivityAttributes.ContentState,
    ) async {
        guard let activity = Activity<AgentActivityAttributes>.activities.first(where: { $0.id == id }) else {
            if localActivityId == id {
                localActivityId = nil
                observedActivityId = nil
                liveActivityToken = nil
                submitRegistration()
            }
            return
        }
        await activity.end(
            ActivityContent(state: contentState, staleDate: nil),
            dismissalPolicy: .after(Date().addingTimeInterval(300)),
        )
        if localActivityId == id {
            localActivityId = nil
            liveActivityToken = nil
            observedActivityId = nil
            submitRegistration()
        }
    }

    /// Reconciles the foreground app's local ActivityKit card with the core
    /// awareness projection. On iOS versions without remote push-to-start,
    /// this is the only way to arm a card before the app is backgrounded; once
    /// armed, the Host owns updates through the registered activity token.
    @available(iOS 16.1, *)
    func reconcileActivity(contentState: AgentActivityAttributes.ContentState?) async {
        guard effectivePreferences().liveActivitiesEnabled else {
            endAllActivities()
            localActivityId = nil
            return
        }
        guard let contentState, contentState.activeCount > 0 else {
            if let localActivityId {
                await endActivity(id: localActivityId, contentState: contentState ?? emptyContentState())
            }
            return
        }
        if let localActivityId {
            await updateActivity(id: localActivityId, contentState: contentState)
            return
        }
        guard UIApplication.shared.applicationState == .active else { return }
        do {
            localActivityId = try await startActivity(contentState: contentState)
        } catch {
            // ActivityKit can reject a second card or a device policy. The
            // Host push-to-start path remains available on supported systems.
        }
    }

    @available(iOS 16.1, *)
    private func endAllActivities() {
        localActivityId = nil
        observedActivityId = nil
        liveActivityToken = nil
        activityTokenTask?.cancel()
        activityStateTask?.cancel()
        activityTokenTask = nil
        activityStateTask = nil
        submitRegistration()
        for activity in Activity<AgentActivityAttributes>.activities {
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
                self.observeActivityToken(activity)
            }
        }
        if #available(iOS 17.2, *), pushToStartUpdatesTask == nil {
            pushToStartUpdatesTask = Task { @MainActor [weak self] in
                for await token in Activity<AgentActivityAttributes>.pushToStartTokenUpdates {
                    guard let self else { return }
                    self.pushToStartToken = token.map { String(format: "%02x", $0) }.joined()
                    self.submitRegistration()
                }
            }
        }
        for activity in Activity<AgentActivityAttributes>.activities {
            observeActivityToken(activity)
        }
    }

    @available(iOS 16.1, *)
    private func observeActivityToken(_ activity: Activity<AgentActivityAttributes>) {
        localActivityId = activity.id
        observedActivityId = activity.id
        activityTokenTask?.cancel()
        activityStateTask?.cancel()
        activityTokenTask = Task { @MainActor [weak self] in
            for await token in activity.pushTokenUpdates {
                guard let self else { return }
                self.liveActivityToken = token.map { String(format: "%02x", $0) }.joined()
                self.submitRegistration()
            }
        }
        activityStateTask = Task { @MainActor [weak self] in
            for await state in activity.activityStateUpdates {
                guard let self, self.observedActivityId == activity.id else { return }
                guard state == .ended || state == .dismissed || state == .stale else { continue }
                self.localActivityId = nil
                self.observedActivityId = nil
                self.liveActivityToken = nil
                self.activityTokenTask?.cancel()
                self.activityTokenTask = nil
                self.submitRegistration()
                return
            }
        }
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

    static func deepLink(from userInfo: [AnyHashable: Any]) -> String? {
        if let value = userInfo["deepLink"] as? String, validThreadDeepLink(value) {
            return value
        }
        guard let environment = userInfo["environmentId"] as? String,
              let thread = userInfo["threadId"] as? String,
              !environment.isEmpty,
              !thread.isEmpty,
              let environment = environment.addingPercentEncoding(withAllowedCharacters: Self.pathComponentCharacters),
              let thread = thread.addingPercentEncoding(withAllowedCharacters: Self.pathComponentCharacters)
        else { return nil }
        return "remoteagent://threads/\(environment)/\(thread)"
    }

    static func threadDeepLink(hostId: String, threadId: String) -> String {
        let host = hostId.addingPercentEncoding(withAllowedCharacters: pathComponentCharacters) ?? hostId
        let thread = threadId.addingPercentEncoding(withAllowedCharacters: pathComponentCharacters) ?? threadId
        return "remoteagent://threads/\(host)/\(thread)"
    }

    /// The usage widget has no Host to scope. Keep its route separate from a
    /// thread notification so a malformed or future settings URL cannot open
    /// a different screen by accident.
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
        return queryItems.count == 1
            && queryItems[0].name == "tab"
            && queryItems[0].value == "limits"
    }

    private static func validThreadDeepLink(_ value: String) -> Bool {
        threadTarget(from: value) != nil
    }

    static func threadTarget(from value: String) -> (hostId: String, threadId: String)? {
        guard value.utf16.count <= 512,
              let url = URL(string: value),
              url.scheme == "remoteagent",
              url.host == "threads",
              url.user == nil,
              url.password == nil,
              url.port == nil,
              url.query == nil,
              url.fragment == nil else {
            return nil
        }
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
              validRouteSegment(threadId)
        else { return nil }
        return (hostId, threadId)
    }

    private static func validRouteSegment(_ value: String) -> Bool {
        !value.isEmpty
            && value != "."
            && value != ".."
            && value.unicodeScalars.allSatisfy { scalar in
                scalar.value != 0x2f
                    && scalar.value != 0x5c
                    && !CharacterSet.controlCharacters.contains(scalar)
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
