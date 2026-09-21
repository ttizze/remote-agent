import AgentCore
import Foundation
import UniformTypeIdentifiers

typealias SnapshotRequest = (Intent, @escaping (AgentCore.Snapshot, Result<Outcome, Error>) -> Void) -> Void

extension BexAppViewModel {
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

    var selectedThreadId: String? {
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

    var coreDraftKey: String {
        snapshot.navigation().draftKey
    }

    var draftKey: String {
        (selectedProfileId ?? "") + ":" + coreDraftKey
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
        loadingThreads = true
        notice = nil
        perform(.listThreads(ListThreads(query: snapshot.listQuery()))) { [weak self] _ in
            self?.loadingThreads = false
        }
    }

    func expandTaskList(projects: Bool = false, projectId: String? = nil) {
        loadingThreads = true
        perform(.expandThreadList(projectId: projectId, projects: projects)) { [weak self] _ in
            self?.loadingThreads = false
        }
    }

    func searchTaskList(_ term: String) {
        var query = snapshot.listQuery()
        guard query.searchTerm != term else { return }
        query.searchTerm = term
        perform(.listThreads(ListThreads(query: query)))
    }

    func openNewThread(cwd: String) {
        perform(.newChat(cwd: cwd)); screen = .thread
    }

    func openNewThread(on profileId: String) {
        selectProfile(profileId); openNewThread(cwd: "")
    }

    func openThread(_ id: String) {
        notice = nil
        perform(.readThread(ReadThread(threadId: id, open: true)))
        if selectedThreadId == id {
            screen = .thread
        }
    }

    func loadOlderHistory(_: String?) {
        guard !loadingHistory, let id = selectedThreadId else { return }
        loadingHistory = true
        perform(.readOlder(threadId: id)) { [weak self] _ in
            self?.loadingHistory = false
        }
    }

    func send() {
        sending = true
        perform(.submit(
            threadId: snapshot.navigation().threadId,
            clientUserMessageId: UUID().uuidString
        )) { [weak self] _ in
            self?.persist()
            self?.sending = false
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

    func transcribe(_ audio: Data, draftKey key: String, sendImmediately: Bool) {
        guard !transcribing, key == draftKey else { return }
        transcribing = true
        perform(.transcribe(Dictate(
            draftKey: coreDraftKey,
            audio: audio,
            send: sendImmediately,
            clientUserMessageId: UUID().uuidString
        ))) { [weak self] _ in
            self?.persist()
            self?.transcribing = false
        }
    }

    func interrupt(_ turnId: String) {
        guard let threadId = snapshot.navigation().threadId else { return }
        interruptingTurnId = turnId
        perform(.interrupt(Interrupt(threadId: threadId, turnId: turnId))) { [weak self] _ in
            self?.interruptingTurnId = nil
        }
    }

    func scanned(_ contents: String?) {
        isScanning = false; if let contents {
            pair(contents)
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

    func readItemDetails(threadId: String, turnId: String, itemId: String) async -> String? {
        do {
            _ = try await outcome(for: .readItem(ReadItem(threadId: threadId, turnId: turnId, itemId: itemId)))
            return nil
        } catch { return error.localizedDescription }
    }

    func download(_ path: String) async throws -> URL {
        let host = selectedProfileId
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(
            UUID().uuidString,
            isDirectory: true
        )
        let target = directory.appendingPathComponent(URL(fileURLWithPath: path).lastPathComponent)
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            _ = try await outcome(for: .downloadFile(DownloadFile(source: path, destination: target.path)))
            guard selectedProfileId == host else { throw CancellationError() }
            try Task.checkCancellation()
            return target
        } catch {
            try? FileManager.default.removeItem(at: directory)
            throw error
        }
    }

    func outcome(for intent: Intent) async throws -> Outcome {
        try await withCheckedThrowingContinuation { continuation in
            perform(intent) { continuation.resume(with: $0) }
        }
    }

    var mediaAccess: ConversationMediaAccess {
        let host = selectedProfileId
        let images: (@MainActor () async throws -> [SessionImage])? = conversation.map { thread in
            { [self] in
                guard selectedProfileId == host else { throw CancellationError() }
                let result = try await outcome(for: .loadSessionImages(LoadSessionImages(threadId: thread.id)))
                guard selectedProfileId == host else { throw CancellationError() }
                guard case let .sessionImages(images) = result else {
                    throw NSError(domain: "BexImage", code: 1,
                                  userInfo: [NSLocalizedDescriptionKey: "画像の応答が無効です。"])
                }
                return images
            }
        }
        return ConversationMediaAccess(host: host, cwd: cwd, download: { [self] path in
            guard selectedProfileId == host else { throw CancellationError() }
            return try await download(path)
        }, sessionImages: images, visualization: { [self, cwd] path in
            guard selectedProfileId == host else { throw CancellationError() }
            let result = try await outcome(for: .loadVisualization(LoadVisualization(path: path, cwd: cwd)))
            guard selectedProfileId == host else { throw CancellationError() }
            guard case let .visualization(html) = result else { throw URLError(.badServerResponse) }
            return html
        })
    }

    func forkAndOpen(through turnId: String, completion: @escaping (String?) -> Void) {
        guard let threadId = selectedThreadId, let host = selectedProfileId else { completion(nil); return }
        perform(.forkThread(ForkThread(threadId: threadId, lastTurnId: turnId))) { [self] result in
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
