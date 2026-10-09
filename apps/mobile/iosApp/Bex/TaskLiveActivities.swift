import ActivityKit
import AgentCore
import Foundation
import OSLog

private extension TaskActivityAttributes.Display {
    init(_ source: TaskActivityDisplay) {
        self.init(current: .init(source.current),
                  canStart: source.canStart, ongoing: source.ongoing, urgent: source.urgent)
    }
}

private extension TaskActivityAttributes.View {
    init(_ source: TaskActivityView) {
        let icons = source.icons.map { icon in
            let kind: TaskActivityAttributes.IconKind = switch icon.kind {
            case .running: .running
            case .waiting: .waiting
            case .unknown: .unknown
            case .finished: .finished
            }
            return TaskActivityAttributes.Icon(kind: kind, label: icon.label)
        }
        self.init(total: source.total, label: source.label, icons: icons, overflow: source.overflow)
    }
}

@MainActor
final class TaskLiveActivities {
    enum Request {
        case intent(hostID: String, intent: Intent)
        case flush(CheckedContinuation<Void, Never>)
    }

    let requests: AsyncStream<Request>
    private let output: AsyncStream<Request>.Continuation
    private var tokenObservers: [String: Task<Void, Never>] = [:]
    private var stateObservers: [String: Task<Void, Never>] = [:]
    private var tokens: [String: Data] = [:]
    private var startTokenObserver: Task<Void, Never>?
    private var activityObserver: Task<Void, Never>?
    private struct StartRegistration: Equatable {
        let hostID: String
        let token: Data
        let allow: Bool
    }

    private var startRegistration: StartRegistration?
    private struct Input: Equatable {
        let hostID: String?
        let foreground: Bool
        let connected: Bool
        let state: TaskActivityAttributes.ContentState?
    }

    private var pending: Input?
    private var previous: Input?
    private var worker: Task<Void, Never>?
    /// User dismissal lasts until this host's active task set becomes empty.
    private var started = Set(UserDefaults.standard.stringArray(forKey: "taskActivityStartedHosts") ?? []) {
        didSet { UserDefaults.standard.set(Array(started), forKey: "taskActivityStartedHosts") }
    }

    fileprivate let logger = Logger(
        subsystem: Bundle.main.bundleIdentifier ?? "dev.remoteagent.mobile.ios",
        category: "LiveActivity"
    )

    init() {
        (requests, output) = AsyncStream.makeStream()
        startTokenObserver = Task { [weak self] in
            for await token in Activity<TaskActivityAttributes>.pushToStartTokenUpdates {
                guard !Task.isCancelled, let self else { break }
                if let input = pending ?? previous {
                    registerStartToken(
                        hostID: input.hostID,
                        token: token,
                        connected: input.connected,
                        foreground: input.foreground
                    )
                }
            }
        }
        activityObserver = Task { [weak self] in
            for await activity in Activity<TaskActivityAttributes>.activityUpdates {
                guard !Task.isCancelled, let self else { break }
                let duplicate = Activity<TaskActivityAttributes>.activities.contains {
                    $0.id != activity.id && $0.attributes.hostID == activity.attributes.hostID
                        && ($0.activityState == .active || $0.activityState == .stale)
                        && tokenObservers[$0.id] != nil
                }
                if duplicate {
                    await activity.end(nil, dismissalPolicy: .immediate)
                } else {
                    started.insert(activity.attributes.hostID); observe(activity)
                }
            }
        }
    }

    deinit {
        output.finish()
        startTokenObserver?.cancel()
        activityObserver?.cancel()
        for observer in tokenObservers.values {
            observer.cancel()
        }
        for observer in stateObservers.values {
            observer.cancel()
        }
    }

    func resendTokens(hostID: String) {
        startRegistration = nil
        if let input = pending ?? previous {
            registerStartToken(
                hostID: hostID,
                token: Activity<TaskActivityAttributes>.pushToStartToken,
                connected: input.connected,
                foreground: input.foreground
            )
        }
        for activity in Activity<TaskActivityAttributes>.activities where activity.attributes.hostID == hostID {
            if let token = tokens[activity.id] {
                register(activity, token: token)
            }
        }
    }

    private func registerStartToken(hostID: String?, token: Data?, connected: Bool, foreground: Bool) {
        guard connected, let hostID, let token else { return }
        let allow = !foreground && ActivityAuthorizationInfo().areActivitiesEnabled
        let registration = StartRegistration(hostID: hostID, token: token, allow: allow)
        guard startRegistration != registration else { return }
        startRegistration = registration
        output.yield(.intent(hostID: hostID, intent: .registerLiveActivity(RegisterLiveActivity(
            activityId: nil, allowStart: allow, token: token, environment: pushEnvironment
        ))))
    }

    private var pushEnvironment: PushEnvironment {
        Bundle.main.object(forInfoDictionaryKey: "BexAPNSEnvironment") as? String == "production"
            ? .production : .sandbox
    }

    private func register(_ activity: Activity<TaskActivityAttributes>, token: Data) {
        tokens[activity.id] = token
        logger.info("Registering Live Activity push token (\(token.count, privacy: .public) bytes)")
        output.yield(.intent(hostID: activity.attributes.hostID,
                             intent: .registerLiveActivity(RegisterLiveActivity(
                                 activityId: activity.id, allowStart: false, token: token, environment: pushEnvironment
                             ))))
    }

    private func observe(_ activity: Activity<TaskActivityAttributes>) {
        guard tokenObservers[activity.id] == nil else { return }
        if let token = activity.pushToken {
            register(activity, token: token)
        }
        tokenObservers[activity.id] = Task { [weak self] in
            for await token in activity.pushTokenUpdates {
                guard !Task.isCancelled else { break }
                if self?.tokens[activity.id] != token {
                    self?.register(activity, token: token)
                }
            }
        }
        stateObservers[activity.id] = Task { [weak self] in
            for await state in activity.activityStateUpdates {
                guard !Task.isCancelled else { break }
                if state == .ended || state == .dismissed {
                    if state == .ended {
                        self?.started.remove(activity.attributes.hostID)
                    }
                    self?.retire(activity)
                    break
                }
            }
        }
    }

    private func retire(_ activity: Activity<TaskActivityAttributes>) {
        guard let observer = tokenObservers.removeValue(forKey: activity.id) else { return }
        observer.cancel()
        stateObservers.removeValue(forKey: activity.id)?.cancel()
        tokens.removeValue(forKey: activity.id)
        output.yield(.intent(hostID: activity.attributes.hostID,
                             intent: .unregisterLiveActivity(UnregisterLiveActivity(activityId: activity.id))))
    }

    func synchronize(hostID: String?, hostName: String, connected: Bool, foreground: Bool,
                     display: TaskActivityDisplay?) {
        let input = Input(hostID: hostID, foreground: foreground, connected: connected,
                          state: display.map { .init(display: .init($0),
                                                     hostName: String(hostName.unicodeScalars.prefix(120))) })
        guard input != (pending ?? previous) else { return }
        pending = input
        guard worker == nil else { return }
        worker = Task { [weak self] in
            while let self, let input = pending {
                pending = nil
                await apply(input)
                previous = input
            }
            self?.worker = nil
        }
    }

    func flush() async {
        await worker?.value
        // ActivityKit may produce the first token after request() returns. Keep the
        // background task alive for that token as well as the registration reply.
        let deadline = ContinuousClock.now.advanced(by: .seconds(5))
        while ContinuousClock.now < deadline,
              Activity<TaskActivityAttributes>.activities.contains(where: {
                  ($0.activityState == .active || $0.activityState == .stale) && tokens[$0.id] == nil
              }) {
            do {
                try await Task.sleep(for: .milliseconds(100))
            } catch { break }
        }
        // The background task must also cover Host registration, not just ActivityKit updates.
        await withCheckedContinuation { continuation in
            if case .terminated = output.yield(.flush(continuation)) {
                continuation.resume()
            }
        }
        await worker?.value
    }

    private func apply(_ input: Input) async {
        registerStartToken(
            hostID: input.hostID,
            token: Activity<TaskActivityAttributes>.pushToStartToken,
            connected: input.connected,
            foreground: input.foreground
        )
        for activity in Activity<TaskActivityAttributes>.activities
            where activity.activityState == .active || activity.activityState == .stale {
            observe(activity)
            let host = activity.attributes.hostID
            started.insert(host)
            guard host == input.hostID else {
                await activity.end(nil, dismissalPolicy: .immediate)
                started.remove(host)
                continue
            }
            // No initial reply yet: preserve this host's existing activity.
            guard let state = input.state else { continue }
            if state.display.ongoing {
                await activity.update(content(state))
            } else {
                let dismissal = Date.now.addingTimeInterval(TimeInterval(taskActivityDismissSeconds()))
                await activity.end(ActivityContent(state: state, staleDate: nil),
                                   dismissalPolicy: .after(dismissal))
            }
        }
        guard let state = input.state, input.connected, let host = input.hostID else { return }
        if !state.display.ongoing {
            started.remove(host)
        }
        guard state.display.canStart,
              input.foreground, ActivityAuthorizationInfo().areActivitiesEnabled,
              !started.contains(host) else { return }
        do {
            let activity = try Activity.request(attributes: TaskActivityAttributes(hostID: host),
                                                content: content(state),
                                                pushType: .token)
            started.insert(host)
            observe(activity)
        } catch {
            logger.error("Could not start task activity: \(error.localizedDescription)")
        }
    }

    private func content(_ state: TaskActivityAttributes
        .ContentState) -> ActivityContent<TaskActivityAttributes.ContentState> {
        ActivityContent(state: state, staleDate: nil, relevanceScore: state.display.urgent ? 100 : 50)
    }
}

extension BexAppViewModel {
    func synchronizeLiveActivities(foreground: Bool) {
        liveActivities.synchronize(hostID: selectedProfileId, hostName: selectedProfileName ?? "PC Host",
                                   connected: snapshot.connected(), foreground: foreground,
                                   display: snapshot.taskActivityDisplay())
    }

    func observeLiveActivityRequests() {
        let requests = liveActivities.requests
        Task { [weak self] in
            for await request in requests {
                if case let .flush(continuation) = request {
                    continuation.resume()
                    continue
                }
                guard case let .intent(hostID, intent) = request,
                      let self, selectedProfileId == hostID, snapshot.connected(),
                      let owner = store else { continue }
                do {
                    let receipt = try owner.dispatch(intent: intent)
                    // Finish each request before dispatching a rotated token or unregistering.
                    let outcome = try await receipt.wait()
                    if case let .liveActivityRegistered(enabled) = outcome {
                        liveActivities.logger
                            .info("Host Live Activity push updates enabled: \(enabled, privacy: .public)")
                    }
                } catch {
                    liveActivities.logger.error("Live Activity push registration failed")
                    Task { [weak self] in
                        try? await Task.sleep(for: .seconds(5))
                        guard let self, snapshot.connected(), selectedProfileId == hostID else { return }
                        liveActivities.resendTokens(hostID: hostID)
                    }
                }
            }
        }
    }
}
