import ActivityKit
import AgentCore
import Foundation
import OSLog

@MainActor
final class TaskLiveActivities {
    struct Request {
        let hostID: String
        let activityID: String
        let token: Data?
        let intent: Intent
    }

    let requests: AsyncStream<Request>
    private let output: AsyncStream<Request>.Continuation
    private var tokenObservers: [String: Task<Void, Never>] = [:]
    private var stateObservers: [String: Task<Void, Never>] = [:]
    private var tokens: [String: Data] = [:]
    private var remote = Set<String>()
    private struct Candidate: Equatable {
        let attributes: TaskActivityAttributes
        let state: TaskActivityAttributes.ContentState
        let ongoing: Bool
    }

    private struct Input: Equatable {
        let hostID: String?
        let connected: Bool
        let foreground: Bool
        let candidates: [Candidate]
    }

    private var pending: Input?
    private var previous: Input?
    private var worker: Task<Void, Never>?
    // A dismissed activity stays dismissed until that task finishes.
    private var started = Set<TaskActivityAttributes>()
    private let logger = Logger(subsystem: "com.ttizze.b-codex", category: "LiveActivity")

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
        if enabled {
            remote.insert(activityID)
        } else {
            remote.remove(activityID)
        }
    }

    private func register(_ activity: Activity<TaskActivityAttributes>, token: Data) {
        tokens[activity.id] = token
        let identity = activity.attributes
        let environment: PushEnvironment = Bundle.main
            .object(forInfoDictionaryKey: "BexAPNSEnvironment") as? String == "production"
            ? .production : .sandbox
        output.yield(Request(hostID: identity.hostID, activityID: activity.id, token: token,
                             intent: .registerLiveActivity(RegisterLiveActivity(
                                 session: SessionRef(
                                     provider: identity.provider == "codex" ? .codex : .claude,
                                     id: identity.sessionID
                                 ),
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
        output.yield(Request(hostID: activity.attributes.hostID, activityID: activity.id, token: nil,
                             intent: .unregisterLiveActivity(UnregisterLiveActivity(activityId: activity.id))))
    }

    func synchronize(hostID: String?, hostName: String, connected: Bool, foreground: Bool, tasks: [TaskActivity]) {
        let input = Input(hostID: hostID, connected: connected, foreground: foreground, candidates: tasks.map { task in
            Candidate(
                attributes: TaskActivityAttributes(
                    hostID: hostID ?? "",
                    provider: task.session.provider == .codex ? "codex" : "claude", sessionID: task.session.id
                ),
                state: .init(
                    title: task.title,
                    status: task.status,
                    statusLabel: task.statusLabel,
                    connected: connected,
                    hostName: String(hostName.unicodeScalars.prefix(120))
                ),
                ongoing: task.ongoing
            )
        })
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
    }

    private func apply(_ input: Input) async {
        let activities = Activity<TaskActivityAttributes>.activities
        for activity in activities where activity.activityState == .active || activity.activityState == .stale {
            observe(activity)
            let identity = activity.attributes
            started.insert(identity)
            guard activity.attributes.hostID == input.hostID else {
                await activity.end(nil, dismissalPolicy: .immediate)
                started.remove(identity)
                continue
            }
            let candidate = input.candidates.first { $0.attributes == identity }
            // A missing/search-filtered row or offline snapshot is not completion evidence.
            guard input.connected, let candidate, candidate.state.status != "unknown" else {
                if remote.contains(activity.id) {
                    continue
                }
                var state = activity.content.state
                state.connected = false
                await activity.update(ActivityContent(state: state, staleDate: .now))
                continue
            }
            if candidate.ongoing {
                let usesPush = remote.contains(activity.id)
                let updated = content(candidate.state, foreground: input.foreground, remote: usesPush)
                await activity.update(updated)
            } else {
                await activity.end(
                    ActivityContent(state: candidate.state, staleDate: nil),
                    dismissalPolicy: .after(.now.addingTimeInterval(60))
                )
            }
        }
        guard input.connected else { return }
        for candidate in input.candidates where candidate.state.status != "unknown" {
            let identity = candidate.attributes
            if !candidate.ongoing {
                started.remove(identity)
                continue
            }
            guard input.foreground, input.hostID != nil, ActivityAuthorizationInfo().areActivitiesEnabled,
                  !started.contains(identity) else { continue }
            do {
                let activity = try Activity.request(attributes: candidate.attributes,
                                                    content: content(candidate.state, foreground: true),
                                                    pushType: .token)
                started.insert(identity)
                observe(activity)
            } catch {
                logger.error("Could not start task activity: \(error.localizedDescription)")
            }
        }
    }

    private func content(_ state: TaskActivityAttributes.ContentState, foreground: Bool, remote: Bool = false)
        -> ActivityContent<TaskActivityAttributes.ContentState> {
        ActivityContent(state: state, staleDate: foreground ? nil : .now.addingTimeInterval(remote ? 120 : 30),
                        relevanceScore: state.status == "waiting" ? 100 : 50)
    }
}

extension BexAppViewModel {
    func synchronizeLiveActivities(foreground: Bool) {
        liveActivities.synchronize(hostID: selectedProfileId, hostName: selectedProfileName ?? "PC Host",
                                   connected: snapshot.connected(), foreground: foreground,
                                   tasks: snapshot.taskActivities())
    }

    func observeLiveActivityRequests() {
        let requests = liveActivities.requests
        Task { [weak self] in
            for await request in requests {
                guard let self, selectedProfileId == request.hostID, snapshot.connected(),
                      let owner = store else { continue }
                do {
                    let receipt = try owner.dispatch(intent: request.intent)
                    Task { [weak self] in
                        guard let outcome = try? await receipt.wait(),
                              case let .liveActivityRegistered(enabled) = outcome else { return }
                        self?.liveActivities.registrationFinished(
                            activityID: request.activityID,
                            token: request.token,
                            enabled: enabled
                        )
                    }
                } catch {
                    // Registration retries with the current token after reconnecting.
                }
            }
        }
    }
}
