import Combine
import Foundation
import RemoteAgentMobile
import UniformTypeIdentifiers

/// Native presentation only: state transitions, cache reconciliation, and RPC
/// orchestration remain inside IosAppController.
@MainActor
final class BexAppViewModel: ObservableObject {
    @Published private(set) var state: IosAppViewState
    let conversation: BexConversationModel
    @Published var isScanning = false
    @Published var transferError: String?
    @Published var transferring = false
    @Published var sending = false
    @Published private(set) var transcribing = false
    @Published private(set) var settings: AgentSettingsState
    private var settingsObservation: HostEventSubscription?
    var settingsActions: IosSettingsActions {
        controller.settings
    }

    @Published private var drafts = UserDefaults.standard
        .dictionary(forKey: "bex.drafts.v4") as? [String: String] ?? [:]
    @Published private var staged: [String: [StagedAttachment]] = {
        guard let data = UserDefaults.standard.data(forKey: "bex.attachments.v4") else { return [:] }
        return (try? JSONDecoder().decode([String: [StagedAttachment]].self, from: data)) ?? [:]
    }() {
        didSet { UserDefaults.standard.set(try? JSONEncoder().encode(staged), forKey: "bex.attachments.v4") }
    }

    var draftKey: String {
        (state.selectedProfileId ?? "") + ":" + (state.selectedThreadId ?? "new:\(state.workingDirectory)")
    }

    var draft: String {
        get { drafts[draftKey] ?? "" }
        set { drafts[draftKey] = newValue; UserDefaults.standard.set(drafts, forKey: "bex.drafts.v4") }
    }

    var attachments: [StagedAttachment] {
        staged[draftKey] ?? []
    }

    var cwd: String {
        state.workingDirectory
    }

    var workspace: IosWorkspaceActions {
        controller.workspace
    }

    private let controller = IosAppController()

    init() {
        settings = controller.settings.currentState()
        state = controller.currentState()
        conversation = BexConversationModel(thread: controller.currentThread())
        settingsObservation = controller.settings.observeSettings { [weak self] state in
            self?.settings = state
        }
        controller.observe { [weak self] state, thread in
            DispatchQueue.main.async {
                guard let self else { return }
                if self.state !== state {
                    self.state = state
                }
                if self.conversation.thread !== thread {
                    self.conversation.thread = thread
                }
            }
        }
    }

    deinit {
        settingsObservation?.cancel()
        controller.close()
    }

    func openPairing() {
        controller.hosts.openPairing()
    }

    func dismissPairing() {
        controller.hosts.dismissPairing()
    }

    func showProfiles() {
        controller.hosts.showProfiles()
    }

    func selectProfile(_ id: String) {
        controller.hosts.selectProfile(hostIdentity: id)
    }

    func pair(_ contents: String) {
        controller.hosts.pair(
            contents: contents,
            nowMs: Int64(Date().timeIntervalSince1970 * 1000)
        )
    }

    func connect() {
        controller.hosts.connect()
    }

    func refreshTaskList() {
        controller.navigation.refreshTaskList()
    }

    func expandTaskList(projects: Bool = false, projectId: String? = nil) {
        controller.navigation.expandTaskList(
            projects: projects,
            projectId: projectId
        )
    }

    func searchTaskList(_ term: String) {
        controller.navigation.searchTaskList(term: term)
    }

    func openNewThread(cwd: String) {
        controller.navigation.openNewThread(cwd: cwd)
    }

    func openNewThread(on profileId: String) {
        guard profileId != state.selectedProfileId else { return }
        controller.hosts.selectProfile(hostIdentity: profileId)
        controller.navigation.openNewThread(cwd: "")
    }

    func openThread(_ id: String) {
        controller.navigation.openThread(threadId: id)
    }

    func loadOlderHistory(_ turnId: String?) {
        controller.navigation.loadOlderHistory(turnId: turnId)
    }

    func showThreadList() {
        controller.navigation.showThreadList()
    }
}

/// Draft submission and dictation
extension BexAppViewModel {
    func send() {
        let submission = captureDraft()
        send(submission.text, from: submission)
    }

    private func captureDraft() -> DraftSubmission {
        DraftSubmission(text: draft, files: attachments, key: draftKey, host: state.selectedProfileId ?? "")
    }

    private func send(_ text: String, from submission: DraftSubmission, dictatedText: String? = nil) {
        sending = true
        controller.conversation.sendTurn(
            text: text,
            attachments: submission.files.map { CodexAttachment(path: $0.path, name: $0.name, isImage: $0.isImage) },
            options: settings.options
        ) { [weak self] accepted, threadId in
            guard let self else { return }
            sending = false
            let destination = threadId.map { submission.host + ":" + $0 } ?? submission.key
            if accepted.boolValue {
                for draftKey in Set([submission.key, destination]) {
                    if drafts[draftKey] == submission.text {
                        drafts[draftKey] = ""
                    }
                    staged[draftKey]?.removeAll { attachment in submission.files.contains { $0.id == attachment.id } }
                }
            } else {
                if destination != submission.key {
                    if drafts[destination, default: ""].isEmpty {
                        drafts[destination] = submission.text
                    }
                    if staged[destination, default: []].isEmpty {
                        staged[destination] = submission.files
                    }
                }
                if let dictatedText {
                    for draftKey in Set([submission.key, destination]) {
                        drafts[draftKey] = Self.appendingDictation(
                            dictatedText,
                            to: drafts[draftKey, default: ""]
                        )
                    }
                }
            }
            UserDefaults.standard.set(drafts, forKey: "bex.drafts.v4")
        }
    }

    func removeAttachment(_ id: UUID) {
        staged[draftKey]?.removeAll { $0.id == id }
    }

    func transcribe(_ audio: Data, draftKey key: String, sendImmediately: Bool) {
        guard !transcribing, key == draftKey else { return }
        let submission = captureDraft()
        transcribing = true
        transferError = nil
        controller.conversation.transcribeAudio(audio: audio.base64EncodedString()) { [weak self] text, error in
            guard let self else { return }
            transcribing = false
            if let text {
                if sendImmediately, draftKey == key {
                    send(Self.appendingDictation(text, to: submission.text), from: submission, dictatedText: text)
                } else {
                    drafts[key] = Self.appendingDictation(text, to: drafts[key, default: ""])
                    UserDefaults.standard.set(drafts, forKey: "bex.drafts.v4")
                    if sendImmediately {
                        transferError = "会話が切り替わったため送信せず、元の会話の下書きに文字起こしを保存しました。"
                    }
                }
            } else if draftKey == key {
                transferError = error ?? "文字起こしできませんでした。"
            }
        }
    }

    private static func appendingDictation(_ text: String, to draft: String) -> String {
        draft + (draft.isEmpty || draft.last?.isWhitespace == true ? "" : "\n") + text
    }
}

/// Attachments and host requests
extension BexAppViewModel {
    func attach(_ url: URL, temporaryDirectory: URL? = nil, completion: @escaping () -> Void = {}) {
        let key = draftKey
        let directory = cwd
        let params: String
        do {
            params = try jsonString([
                "direction": "upload",
                "source": url.path,
                "directory": directory,
                "fileName": url.lastPathComponent
            ])
        } catch {
            transferError = error.localizedDescription
            if let temporaryDirectory {
                try? FileManager.default.removeItem(at: temporaryDirectory)
            }
            completion()
            return
        }
        let access = url.startAccessingSecurityScopedResource()
        transferring = true
        transferError = nil
        controller.workspace.transfer(paramsJson: params) { [weak self] result, error in
            if access {
                url.stopAccessingSecurityScopedResource()
            }
            if let temporaryDirectory {
                try? FileManager.default.removeItem(at: temporaryDirectory)
            }
            defer { completion() }
            guard let self else { return }
            transferring = false
            transferError = error
            if let result, let path = jsonObject(result)["path"] as? String {
                let type = UTType(filenameExtension: url.pathExtension)
                staged[key, default: []].append(StagedAttachment(
                    name: url.lastPathComponent,
                    path: path,
                    isImage: type?.conforms(to: .image) == true
                ))
            }
        }
    }

    func respond(_ request: IosTurnRequestView, answer: RequestAnswer, completion: @escaping (String?) -> Void) {
        controller.conversation.respond(requestJson: request.requestJson, answer: answer, completion: completion)
    }

    func forkThread(
        _ threadId: String,
        through turnId: String,
        completion: @escaping ([String: Any]?, String?) -> Void
    ) {
        controller.conversation.forkThread(threadId: threadId, lastTurnId: turnId) { result, error in
            completion(result.map(jsonObject), error)
        }
    }

    func readItemDetails(threadId: String, turnId: String, itemId: String) async -> (String?, String?) {
        await withCheckedContinuation { continuation in
            controller.conversation.readItemDetails(threadId: threadId, turnId: turnId, itemId: itemId) { body, error in
                continuation.resume(returning: (body, error))
            }
        }
    }

    func readSessionImages(_ threadId: String, completion: @escaping ([String]?, String?) -> Void) {
        controller.conversation.readSessionImages(threadId: threadId, completion: completion)
    }

    func download(_ path: String, completion: @escaping (URL?, String?) -> Void) {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(
            UUID().uuidString,
            isDirectory: true
        )
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        } catch {
            completion(nil, error.localizedDescription)
            return
        }
        let target = directory.appendingPathComponent(URL(fileURLWithPath: path).lastPathComponent)
        do {
            try controller.workspace.transfer(paramsJson: jsonString([
                "direction": "download",
                "source": path,
                "destination": target.path
            ])) { result, error in
                completion(result == nil ? nil : target, error)
            }
        } catch {
            try? FileManager.default.removeItem(at: directory)
            completion(nil, error.localizedDescription)
        }
    }

    func interrupt(_ turnId: String) {
        controller.conversation.interrupt(turnId: turnId)
    }

    func scanned(_ contents: String?) {
        isScanning = false
        guard let contents, !contents.isEmpty else { return }
        pair(contents)
    }
}
