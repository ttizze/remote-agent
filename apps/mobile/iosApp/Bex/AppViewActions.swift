import AgentCore
import Foundation
import UniformTypeIdentifiers

struct DraftIdentity: Hashable {
    let profile: String?
    let key: DraftKey
}

typealias SnapshotRequest = (Intent, @escaping (AgentCore.Snapshot, Result<Outcome, Error>) -> Void) -> Void

extension BexAppViewModel {
    var sending: Bool {
        snapshot.operationRunning(key: .submission(draftKey: coreDraftKey))
    }

    var transcribing: Bool {
        snapshot.operationRunning(key: .dictation(draftKey: coreDraftKey))
    }

    var loadingThreads: Bool {
        snapshot.operationRunning(key: .sessionList)
    }

    var loadingHistory: Bool {
        selectedThreadId.map { snapshot.operationRunning(key: .history(session: $0)) } ?? false
    }

    var interruptingTurnId: String? {
        guard let session = selectedThreadId,
              let turn = snapshot.conversation(id: session)?.activeTurnId() else { return nil }
        return snapshot.operationRunning(key: .interrupt(session: session, turn: turn)) ? turn : nil
    }

    func recordScene(_ value: UInt64) {
        store?.recordConnectionEvent(phase: .appScene, value: value)
    }

    func recordListViewUpdate() {
        store?.recordConnectionEvent(phase: .listViewUpdated, value: isConnected ? 1 : 0)
    }

    func browser(_ request: BrowserRequest) async throws -> BrowserFrame {
        guard let owner = store else { throw URLError(.notConnectedToInternet) }
        let host = selectedProfileId
        let frame = try await owner.browser(request: request)
        guard host == selectedProfileId else { throw CancellationError() }
        return frame
    }

    func requestSnapshot(_ intent: Intent, completion: @escaping (AgentCore.Snapshot, Result<Outcome, Error>) -> Void) {
        perform(intent) { [self] result in completion(snapshot, result) }
    }

    var isConnected: Bool {
        snapshot.connected()
    }

    var selectedProfileName: String? {
        profiles.first { $0.id == selectedProfileId }?.name
    }

    var selectedThreadId: SessionRef? {
        snapshot.navigation().threadId
    }

    var isNewThread: Bool {
        selectedThreadId == nil && screen == .thread
    }

    var threadLoadState: LoadState {
        loadingThreads ? .loading : list != nil ? .ready : notice != nil ? .failed : .idle
    }

    var cwd: String {
        snapshot.navigation().cwd
    }

    var selectedDirectory: String {
        snapshot.selectedDirectory()
    }

    var coreDraftKey: DraftKey {
        snapshot.navigation().draftKey
    }

    var draftKey: DraftIdentity {
        DraftIdentity(profile: selectedProfileId, key: coreDraftKey)
    }

    var draft: String {
        get { snapshot.draft(key: coreDraftKey).text }
        set { perform(.setDraftText(threadId: coreDraftKey, text: newValue)) }
    }

    func showProfiles() {
        screen = .profiles; perform(.showThreadList)
    }

    func showThreadList() {
        screen = .threads; perform(.showThreadList); refreshTaskList()
    }

    func refreshTaskList() {
        guard !loadingThreads else { return }
        notice = nil
        perform(.listSessions(ListSessions(query: snapshot.listQuery())))
    }

    func expandTaskList(projects: Bool = false, projectId: String? = nil) {
        perform(.expandThreadList(projectId: projectId, projects: projects))
    }

    func searchTaskList(_ term: String) {
        var query = snapshot.listQuery()
        guard query.searchTerm != term else { return }
        query.searchTerm = term
        perform(.listSessions(ListSessions(query: query)))
    }

    func openNewThread(cwd: String) {
        perform(.newChat(cwd: cwd)); screen = .thread
    }

    func openNewThread(on profileId: String) {
        selectProfile(profileId); openNewThread(cwd: "")
    }

    func openThread(_ id: SessionRef) {
        notice = nil
        perform(.readThread(ReadThread(threadId: id, open: true)))
        if selectedThreadId == id {
            screen = .thread
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

    func loadOlderHistory() {
        guard !loadingHistory, let id = selectedThreadId else { return }
        perform(.readOlder(threadId: id))
    }

    func send() {
        perform(.submit(
            threadId: snapshot.navigation().threadId,
            clientUserMessageId: UUID().uuidString
        )) { [weak self] _ in
            self?.persist()
        }
    }

    func restoreUnknownSubmission(_ id: String) {
        perform(.restoreUnknownSubmission(clientUserMessageId: id)) { [weak self] _ in self?.persist() }
    }

    func discardUnknownSubmission(_ id: String) {
        perform(.discardUnknownSubmission(clientUserMessageId: id)) { [weak self] _ in self?.persist() }
    }

    func removeAttachment(_ id: Int) {
        perform(.removeAttachment(draftKey: coreDraftKey, index: UInt32(id))) { [weak self] _ in self?.persist() }
    }

    func transcribe(_ audio: Data, draftKey key: DraftIdentity, sendImmediately: Bool,
                    preparation: DictationPreparation?) {
        guard !transcribing, key == draftKey else { return }
        perform(.transcribe(Dictate(
            draftKey: coreDraftKey,
            preparation: preparation?.id(),
            audio: audio,
            send: sendImmediately,
            clientUserMessageId: UUID().uuidString
        ))) { [weak self] _ in
            withExtendedLifetime(preparation) {
                self?.persist()
            }
        }
    }

    func interrupt(_ turnId: String) {
        guard let threadId = snapshot.navigation().threadId else { return }
        perform(.interrupt(Interrupt(threadId: threadId, turnId: turnId)))
    }

    func scanned(_ contents: String?) {
        isScanning = false; if let contents {
            preparePairing(contents)
        }
    }
}

extension BexAppViewModel {
    func attach(_ url: URL, temporaryDirectory: URL? = nil) async throws {
        let access = url.startAccessingSecurityScopedResource()
        transferring = true
        transferError = nil
        defer {
            persist()
            if access {
                url.stopAccessingSecurityScopedResource()
            }
            if let temporaryDirectory {
                try? FileManager.default.removeItem(at: temporaryDirectory)
            }
            transferring = false
        }
        let attachment = Attachment(path: url.path, name: url.lastPathComponent,
                                    isImage: UTType(filenameExtension: url.pathExtension)?.conforms(to: .image) == true)
        _ = try await outcome(for: .uploadAttachment(UploadAttachment(
            draftKey: coreDraftKey,
            attachment: attachment,
            directory: cwd
        )))
    }

    func respond(_ request: Request, answer: Answer, completion: @escaping (String?) -> Void) {
        perform(.respond(Respond(requestId: request.id, answer: answer))) { result in
            if case let .failure(error) = result {
                completion(error.localizedDescription)
            } else {
                completion(nil)
            }
        }
    }

    func readItemDetails(threadId: SessionRef, turnId: String, itemId: String) async -> String? {
        do {
            _ = try await outcome(for: .readItem(ReadItem(threadId: threadId, turnId: turnId, itemId: itemId)))
            return nil
        } catch { return error.localizedDescription }
    }

    func download(_ path: String) async throws -> URL {
        let host = selectedProfileId
        return try await inTemporaryDirectory { directory in
            let target = directory.appendingPathComponent(URL(fileURLWithPath: path).lastPathComponent)
            _ = try await outcome(for: .downloadFile(DownloadFile(source: path, destination: target.path)))
            guard selectedProfileId == host else { throw CancellationError() }
            try Task.checkCancellation()
            return target
        }
    }

    func outcome(for intent: Intent) async throws -> Outcome {
        try await withCheckedThrowingContinuation { continuation in
            perform(intent) { continuation.resume(with: $0) }
        }
    }

    var mediaAccess: ConversationMediaAccess {
        let host = selectedProfileId
        return ConversationMediaAccess(host: host, cwd: cwd, download: { [self] path in
            guard selectedProfileId == host else { throw CancellationError() }
            return try await download(path)
        }, visualization: { [self, cwd] path in
            guard selectedProfileId == host else { throw CancellationError() }
            let result = try await outcome(for: .loadVisualization(LoadVisualization(path: path, cwd: cwd)))
            guard selectedProfileId == host else { throw CancellationError() }
            guard case let .visualization(html) = result else { throw URLError(.badServerResponse) }
            return html
        })
    }

    func forkAndOpen(through turnId: String, completion: @escaping (String?) -> Void) {
        guard let threadId = selectedThreadId, let host = selectedProfileId else { completion(nil); return }
        perform(.forkSession(ForkSession(threadId: threadId, lastTurnId: turnId))) { [self] result in
            guard selectedProfileId == host, selectedThreadId == threadId else { completion(nil); return }
            switch result {
            case let .success(.startedThread(id)):
                openThread(id)
                completion(nil)
            case let .failure(error): completion(error.localizedDescription)
            default: completion("会話を分岐できませんでした。")
            }
        }
    }
}
