import AgentCore
import Foundation

struct DraftIdentity: Hashable {
    let profile: String?
    let key: DraftKey
}

typealias SnapshotRequest = (Intent, @escaping (AgentCore.Snapshot, Result<Outcome, Error>) -> Void) -> Void

extension AppViewModel {
    func recordScene(_ value: UInt64) {
        sessions.recordScene(value: value)
    }

    func recordListViewUpdate() {
        store?.recordConnectionEvent(phase: .listViewUpdated, value: isConnected ? 1 : 0)
    }

    func requestSnapshot(_ intent: Intent, completion: @escaping (AgentCore.Snapshot, Result<Outcome, Error>) -> Void) {
        perform(intent) { [self] result in completion(snapshot, result) }
    }

    var selectedProfileName: String? {
        profiles.first { $0.id == selectedProfileId }?.name
    }

    var threadLoadState: LoadState {
        loadingThreads ? .loading : list != nil ? .ready : notice != nil ? .failed : .idle
    }

    func showSettings() {
        navigationPath = [.settings]
    }

    func showProfiles() {
        navigationPath = [.profiles]; perform(.showThreadList)
    }

    func showThreadList() {
        if navigationPath.isEmpty {
            // The initial Host restore can select a thread while the native path is already at its root.
            threadListDidAppear()
        } else {
            navigationPath = []
        }
    }

    func threadListDidAppear() {
        guard navigationPath.isEmpty else { return }
        cancelPairing()
        // Clearing the selected conversation also rebuilds its history and composer. Keep the
        // outgoing screen intact until UIKit finishes the pop, including a cancelled edge swipe.
        perform(.showThreadList)
    }

    func refreshTaskList() {
        notice = nil
        connectProfiles()
        for id in listedHostIds {
            guard let owner = sessions.session(host: id),
                  owner.snapshot().connected(),
                  !owner.snapshot().operationRunning(key: .sessionList) else { continue }
            perform(.listSessions(ListSessions(query: owner.snapshot().listQuery())), on: id)
        }
    }

    func expandTaskList(projectId: String? = nil, host: String? = nil) {
        for id in host.map({ [$0] }) ?? listedHostIds {
            perform(.expandThreadList(projectId: projectId), on: id)
        }
    }

    func searchTaskList(_ term: String) {
        for id in listedHostIds {
            guard var query = sessions.session(host: id)?.snapshot().listQuery(),
                  query.searchTerm != term else { continue }
            query.searchTerm = term
            perform(.listSessions(ListSessions(query: query)), on: id)
        }
    }

    func openNewThread(cwd: String) {
        perform(.newChat(cwd: cwd)); navigationPath = [.thread]
    }

    func openNewThread(on profileId: String) {
        activateProfile(profileId); openNewThread(cwd: "")
    }

    func openNewThread(cwd: String, on host: String) {
        activateProfile(host)
        openNewThread(cwd: cwd)
    }

    func openThread(_ id: SessionRef, on host: String) {
        activateProfile(host)
        openThread(id)
    }

    var listedHostIds: [String] {
        profiles.filter { allProfiles || $0.id == selectedProfileId }.map(\.id)
    }

    func perform(_ intent: Intent, on host: String) {
        guard let owner = sessions.session(host: host) else { return }
        do {
            let receipt = try owner.dispatch(intent: intent)
            updateTaskList()
            Task { [weak self] in
                do { _ = try await receipt.wait() } catch { self?.notice = error.localizedDescription }
                self?.updateTaskList()
            }
        } catch { notice = error.localizedDescription }
    }

    func openThread(_ id: SessionRef) {
        notice = nil
        perform(.readThread(ReadThread(threadId: id, open: true)))
        if snapshot.navigation().threadId == id {
            navigationPath = [.thread]
        }
    }

    func openTaskActivity(_ url: URL) {
        guard let scheme = TaskActivityAttributes.urlScheme, url.scheme == scheme, url.host == "tasks",
              let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              let host = components.queryItems?.first(where: { $0.name == "host" })?.value,
              profiles.contains(where: { $0.id == host }) else { return }
        if selectedProfileId != host {
            selectProfile(host)
        }
        showThreadList()
    }
}
