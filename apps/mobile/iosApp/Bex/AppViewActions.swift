import AgentCore
import Foundation
import UniformTypeIdentifiers

extension BexAppViewModel {
    var state: AppPresentation {
        let nav = snapshot.navigation()
        let selected = profiles.first { $0.id == selectedProfileId }
        return AppPresentation(
            screen: profiles.isEmpty ? .pairing : screen, isConnected: snapshot.connected(),
            isConnecting: isConnecting, profiles: profiles, selectedProfileId: selectedProfileId,
            selectedProfileName: selected?.name, pairingError: pairingError,
            connectionError: connectionError ?? (snapshot.connected() ? nil : snapshot.error()),
            workingDirectory: nav.cwd, projects: list?.projects ?? [],
            threadLoadState: loadingThreads ? .loading : list != nil ? .ready : notice != nil ? .failed : .idle,
            threadLoadError: notice, threads: list?.threads ?? [], hasMoreProjects: list?.hasMoreProjects ?? false,
            visibleProjectCount: list?.projects.count ?? 0, loadingMoreThreads: loadingThreads,
            loadingHistory: loadingHistory, moreProjectIds: Set(list?.moreProjectIds ?? []),
            hasMoreChats: list?.hasMoreChats ?? false, selectedThreadId: nav.threadId,
            isNewThread: nav.threadId == nil && screen == .thread, notice: notice,
            interruptingTurnId: interruptingTurnId
        )
    }

    var cwd: String {
        snapshot.navigation().cwd
    }

    var coreDraftKey: String {
        snapshot.navigation().draftKey
    }

    var draftKey: String {
        (selectedProfileId ?? "") + ":" + coreDraftKey
    }

    var draft: String {
        get { snapshot.draft(key: coreDraftKey).text }
        set { perform(.setDraftText(key: coreDraftKey, text: newValue)) }
    }

    var attachments: [StagedAttachment] {
        snapshot.draft(key: coreDraftKey).attachments.enumerated().map {
            StagedAttachment(id: $0.offset, name: $0.element.name, path: $0.element.path, isImage: $0.element.isImage)
        }
    }

    func openPairing() {
        pairingError = nil; screen = .pairing
    }

    func dismissPairing() {
        screen = .profiles
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
        var query = snapshot.listQuery()
        if query.projectLimit == 0 {
            query.projectLimit = 5
        }
        if query.chatLimit == 0 {
            query.chatLimit = 5
        }
        perform(.listThreads(query: query)) { [weak self] _ in self?.loadingThreads = false }
    }

    func expandTaskList(projects: Bool = false, projectId: String? = nil) {
        var query = snapshot.listQuery()
        if let projectId {
            query.projectThreadLimits[projectId, default: 5] += 10
        } else if projects {
            query.projectLimit += 10
        } else {
            query.chatLimit += 10
        }
        loadingThreads = true
        perform(.listThreads(query: query)) { [weak self] _ in self?.loadingThreads = false }
    }

    func searchTaskList(_ term: String) {
        var query = snapshot.listQuery()
        guard query.searchTerm != term else { return }
        query.searchTerm = term
        perform(.listThreads(query: query))
    }

    func openNewThread(cwd: String) {
        perform(.newChat(cwd: cwd)); screen = .thread
    }

    func openNewThread(on profileId: String) {
        selectProfile(profileId); openNewThread(cwd: "")
    }

    func openThread(_ id: String) {
        let host = selectedProfileId
        let previousScreen = screen
        perform(.openThread(id: id)) { [weak self] result in
            guard let self, case .success = result, selectedProfileId == host,
                  screen == previousScreen, snapshot.navigation().threadId == id else { return }
            screen = .thread
        }
    }

    func loadOlderHistory(_ turnId: String?) {
        guard !loadingHistory, let thread = conversation else { return }
        let cursor = turnId.flatMap { id in thread.source.turns().first { $0.id() == id }?.itemsCursor() } ?? thread
            .source.historyCursor()
        loadingHistory = true
        perform(.readOlder(threadId: thread.id, turnId: turnId, cursor: cursor)) { [weak self] _ in
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

    func removeAttachment(_ id: Int) {
        perform(.removeAttachment(key: coreDraftKey, index: UInt32(id))) { [weak self] _ in self?.persist() }
    }

    func transcribe(_ audio: Data, draftKey key: String, sendImmediately: Bool) {
        guard !transcribing, key == draftKey else { return }
        transcribing = true
        perform(.transcribe(
            key: coreDraftKey,
            audio: audio,
            send: sendImmediately,
            clientUserMessageId: UUID().uuidString
        )) { [weak self] _ in
            self?.persist()
            self?.transcribing = false
        }
    }

    func interrupt(_ turnId: String) {
        guard let threadId = snapshot.navigation().threadId else { return }
        interruptingTurnId = turnId
        perform(.interrupt(threadId: threadId, turnId: turnId)) { [weak self] _ in self?.interruptingTurnId = nil }
    }

    func scanned(_ contents: String?) {
        isScanning = false; if let contents {
            pair(contents)
        }
    }
}

extension BexAppViewModel {
    func attach(_ url: URL, temporaryDirectory: URL? = nil, completion: @escaping () -> Void = {}) {
        let access = url.startAccessingSecurityScopedResource()
        transferring = true
        transferError = nil
        let attachment = Attachment(path: url.path, name: url.lastPathComponent,
                                    isImage: UTType(filenameExtension: url.pathExtension)?.conforms(to: .image) == true)
        perform(.uploadAttachment(key: coreDraftKey, attachment: attachment, directory: cwd)) { [weak self] _ in
            self?.persist()
            if access {
                url.stopAccessingSecurityScopedResource()
            }
            if let temporaryDirectory {
                try? FileManager.default.removeItem(at: temporaryDirectory)
            }
            self?.transferring = false
            completion()
        }
    }

    func respond(_ request: RequestPresentation, answer: Answer, completion: @escaping (String?) -> Void) {
        perform(.respond(requestId: request.source.id, answer: answer)) { result in
            if case let .failure(error) = result {
                completion(error.localizedDescription)
            } else {
                completion(nil)
            }
        }
    }

    func forkThread(_ threadId: String, through turnId: String, completion: @escaping (String?, String?) -> Void) {
        perform(.forkThread(threadId: threadId, lastTurnId: turnId)) { result in
            switch result {
            case let .success(.startedThread(id)): completion(id, nil)
            case let .failure(error): completion(nil, error.localizedDescription)
            default: completion(nil, "会話を分岐できませんでした。")
            }
        }
    }

    func readItemDetails(threadId: String, turnId: String, itemId: String) async -> (String?, String?) {
        await withCheckedContinuation { continuation in
            perform(.readItem(threadId: threadId, turnId: turnId, itemId: itemId)) { [weak self] result in
                if case let .failure(error) = result {
                    continuation.resume(returning: (
                        nil,
                        error.localizedDescription
                    )); return
                }
                let source = self?.snapshot.conversation(id: threadId)?.turns().first { $0.id() == turnId }?
                    .items().first { $0.id() == itemId || $0.clientId() == itemId }
                continuation.resume(returning: (source.map { ConversationItem($0).expandedBody() }, nil))
            }
        }
    }

    func readSessionImages(_ threadId: String, completion: @escaping ([String]?, String?) -> Void) {
        perform(.loadSessionImages(threadId: threadId)) { result in
            switch result {
            case let .success(.sessionImages(images)): completion(
                    images.map { $0.encoded ? "data:image/png;base64," + $0.source : $0.source },
                    nil
                )
            case let .failure(error): completion(nil, error.localizedDescription)
            default: completion(nil, "画像の応答が無効です。")
            }
        }
    }

    func download(_ path: String, completion: @escaping (URL?, String?) -> Void) {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(
            UUID().uuidString,
            isDirectory: true
        )
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        } catch { completion(nil, error.localizedDescription); return }
        let target = directory.appendingPathComponent(URL(fileURLWithPath: path).lastPathComponent)
        perform(.downloadFile(source: path, destination: target.path)) { result in
            switch result {
            case .success: completion(target, nil)
            case let .failure(error):
                try? FileManager.default.removeItem(at: directory)
                completion(nil, error.localizedDescription)
            }
        }
    }
}
