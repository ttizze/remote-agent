import ActivityKit
import AgentCore
import Foundation
import OSLog
import UIKit

private extension TaskActivityAttributes.Display {
    init(_ source: TaskActivityDisplay) {
        self.init(current: .init(source.current), stale: .init(source.stale),
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
        case intent(hostID: String, activityID: String, token: Data?, intent: Intent)
        case flush(CheckedContinuation<Void, Never>)
    }

    let requests: AsyncStream<Request>
    private let output: AsyncStream<Request>.Continuation
    private var tokenObservers: [String: Task<Void, Never>] = [:]
    private var stateObservers: [String: Task<Void, Never>] = [:]
    private var tokens: [String: Data] = [:]
    private var remote = Set<String>()
    private struct Input: Equatable {
        let hostID: String?
        let foreground: Bool
        let state: TaskActivityAttributes.ContentState
    }

    private var pending: Input?
    private var previous: Input?
    private var worker: Task<Void, Never>?
    // User dismissal lasts until this host's active task set becomes empty.
    private var started = Set<String>()
    private(set) var hostID: String?
    private(set) var sessions: [SessionRef] = []
    fileprivate let logger = Logger(
        subsystem: Bundle.main.bundleIdentifier ?? "dev.remoteagent.mobile.ios",
        category: "LiveActivity"
    )

    init() {
        (requests, output) = AsyncStream.makeStream()
    }

    deinit {
        output.finish()
        for observer in tokenObservers.values {
            observer.cancel()
        }
        for observer in stateObservers.values {
            observer.cancel()
        }
    }

    func resendTokens(hostID: String) {
        for activity in Activity<TaskActivityAttributes>.activities where activity.attributes.hostID == hostID {
            if let token = tokens[activity.id] {
                register(activity, token: token)
            }
        }
    }

    func registrationFinished(activityID: String, token: Data?, enabled: Bool) {
        guard let token, tokens[activityID] == token else { return }
        logger.info("Host Live Activity push updates enabled: \(enabled, privacy: .public)")
        guard remote.contains(activityID) != enabled else { return }
        if enabled {
            remote.insert(activityID)
        } else {
            remote.remove(activityID)
        }
        previous = nil
    }

    private func register(_ activity: Activity<TaskActivityAttributes>, token: Data) {
        if tokens[activity.id] != token, remote.remove(activity.id) != nil {
            previous = nil
        }
        tokens[activity.id] = token
        logger.info("Registering Live Activity push token (\(token.count, privacy: .public) bytes)")
        let environment: PushEnvironment = Bundle.main
            .object(forInfoDictionaryKey: "BexAPNSEnvironment") as? String == "production"
            ? .production : .sandbox
        output.yield(.intent(hostID: activity.attributes.hostID, activityID: activity.id, token: token,
                             intent: .registerLiveActivity(RegisterLiveActivity(
                                 activityId: activity.id, token: token, environment: environment
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
        remote.remove(activity.id)
        output.yield(.intent(hostID: activity.attributes.hostID, activityID: activity.id, token: nil,
                             intent: .unregisterLiveActivity(UnregisterLiveActivity(activityId: activity.id))))
    }

    func synchronize(hostID: String?, hostName: String, connected: Bool, foreground: Bool,
                     overview: TaskActivityOverview) {
        self.hostID = hostID
        sessions = overview.sessions
        let input = Input(hostID: hostID, foreground: foreground,
                          state: .init(display: .init(overview.display), connected: connected,
                                       hostName: String(hostName.unicodeScalars.prefix(120))))
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
        // The background task must also cover Host registration, not just ActivityKit updates.
        await withCheckedContinuation { continuation in
            if case .terminated = output.yield(.flush(continuation)) {
                continuation.resume()
            }
        }
        await worker?.value
    }

    private func apply(_ input: Input) async {
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
            guard input.state.connected else {
                if remote.contains(activity.id) {
                    continue
                }
                var state = activity.content.state
                state.connected = false
                await activity.update(ActivityContent(state: state, staleDate: .now))
                continue
            }
            if input.state.display.ongoing {
                await activity.update(content(input.state, foreground: input.foreground,
                                              remote: remote.contains(activity.id)))
            } else {
                let timing = taskActivityTiming(
                    foreground: input.foreground,
                    remoteUpdates: remote.contains(activity.id)
                )
                let dismissal = Date.now.addingTimeInterval(TimeInterval(timing.dismissAfterSeconds))
                await activity.end(ActivityContent(state: input.state, staleDate: nil),
                                   dismissalPolicy: .after(dismissal))
            }
        }
        guard input.state.connected, let host = input.hostID else { return }
        if !input.state.display.ongoing {
            started.remove(host)
        }
        guard input.state.display.canStart,
              input.foreground, ActivityAuthorizationInfo().areActivitiesEnabled,
              !started.contains(host) else { return }
        do {
            let activity = try Activity.request(attributes: TaskActivityAttributes(hostID: host),
                                                content: content(input.state, foreground: true),
                                                pushType: .token)
            started.insert(host)
            observe(activity)
        } catch {
            logger.error("Could not start task activity: \(error.localizedDescription)")
        }
    }

    private func content(_ state: TaskActivityAttributes.ContentState, foreground: Bool, remote: Bool = false)
        -> ActivityContent<TaskActivityAttributes.ContentState> {
        let timing = taskActivityTiming(foreground: foreground, remoteUpdates: remote)
        let staleDate = timing.staleAfterSeconds.map { Date.now.addingTimeInterval(TimeInterval($0)) }
        return ActivityContent(state: state, staleDate: staleDate,
                               relevanceScore: state.display.urgent ? 100 : 50)
    }
}

extension BexAppViewModel {
    func synchronizeLiveActivities(foreground: Bool) {
        liveActivities.synchronize(hostID: selectedProfileId, hostName: selectedProfileName ?? "PC Host",
                                   connected: snapshot.connected(), foreground: foreground,
                                   overview: snapshot.taskActivityOverview(previousSessions:
                                       liveActivities.hostID == selectedProfileId ? liveActivities.sessions : []))
    }

    func observeLiveActivityRequests() {
        let requests = liveActivities.requests
        Task { [weak self] in
            for await request in requests {
                if case let .flush(continuation) = request {
                    continuation.resume()
                    continue
                }
                guard case let .intent(hostID, activityID, token, intent) = request,
                      let self, selectedProfileId == hostID, snapshot.connected(),
                      let owner = store else { continue }
                do {
                    let receipt = try owner.dispatch(intent: intent)
                    // Finish each request before dispatching a rotated token or unregistering.
                    let outcome = try await receipt.wait()
                    guard case let .liveActivityRegistered(enabled) = outcome else { continue }
                    liveActivities.registrationFinished(
                        activityID: activityID,
                        token: token,
                        enabled: enabled
                    )
                    synchronizeLiveActivities(foreground: UIApplication.shared.applicationState == .active)
                } catch {
                    liveActivities.logger.error("Live Activity push registration failed")
                    // Registration retries with the current token after reconnecting.
                }
            }
        }
    }
}
