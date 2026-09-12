import AgentCore
import Foundation
import UniformTypeIdentifiers

extension BexAppViewModel {
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
        let host = selectedProfileId
        let previousScreen = screen
        perform(.readThread(ReadThread(threadId: id, open: true))) { [weak self] result in
            guard let self, case .success = result, selectedProfileId == host,
                  screen == previousScreen, snapshot.navigation().threadId == id else { return }
            screen = .thread
        }
    }

    func loadOlderHistory(_ turnId: String?) {
        guard !loadingHistory, let id = selectedThreadId else { return }
        loadingHistory = true
        perform(.readOlder(ReadOlder(threadId: id, turnId: turnId))) { [weak self] _ in
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
    func attach(_ url: URL, temporaryDirectory: URL? = nil, completion: @escaping () -> Void = {}) {
        let access = url.startAccessingSecurityScopedResource()
        transferring = true
        transferError = nil
        let attachment = Attachment(path: url.path, name: url.lastPathComponent,
                                    isImage: UTType(filenameExtension: url.pathExtension)?.conforms(to: .image) == true)
        perform(.uploadAttachment(UploadAttachment(
            draftKey: coreDraftKey,
            attachment: attachment,
            directory: cwd
        ))) { [weak self] _ in
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

    func respond(_ request: Request, answer: Answer, completion: @escaping (String?) -> Void) {
        perform(.respond(Respond(requestId: request.id, answer: answer))) { result in
            if case let .failure(error) = result {
                completion(error.localizedDescription)
            } else {
                completion(nil)
            }
        }
    }

    func forkThread(_ threadId: String, through turnId: String, completion: @escaping (String?, String?) -> Void) {
        perform(.forkThread(ForkThread(threadId: threadId, lastTurnId: turnId))) { result in
            switch result {
            case let .success(.startedThread(id)): completion(id, nil)
            case let .failure(error): completion(nil, error.localizedDescription)
            default: completion(nil, "会話を分岐できませんでした。")
            }
        }
    }

    func readItemDetails(threadId: String, turnId: String, itemId: String) async -> String? {
        await withCheckedContinuation { continuation in
            perform(.readItem(ReadItem(threadId: threadId, turnId: turnId, itemId: itemId))) { result in
                if case let .failure(error) = result {
                    continuation.resume(returning: error.localizedDescription)
                } else {
                    continuation.resume(returning: nil)
                }
            }
        }
    }

    func readSessionImages(_ threadId: String, completion: @escaping ([String]?, String?) -> Void) {
        perform(.loadSessionImages(LoadSessionImages(threadId: threadId))) { result in
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
        perform(.downloadFile(DownloadFile(source: path, destination: target.path))) { result in
            switch result {
            case .success: completion(target, nil)
            case let .failure(error):
                try? FileManager.default.removeItem(at: directory)
                completion(nil, error.localizedDescription)
            }
        }
    }
}
