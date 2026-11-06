import AgentCore
import Foundation
import Observation
import UniformTypeIdentifiers

/// One pane owns its native presentation and input. The Host session owns transport and persistence.
@MainActor
@Observable
final class ConversationModel {
    let host: String
    @ObservationIgnored let session: ClientSession
    @ObservationIgnored let pane: AgentPane?
    @ObservationIgnored private(set) var publication = Presentation.empty()
    @ObservationIgnored private(set) var snapshot = AgentCore.Snapshot.empty()
    private(set) var settings = Presentation.empty().settings()
    private(set) var isConnected = false
    private(set) var conversationContext = Presentation.empty().conversationContext()
    private(set) var composer = Presentation.empty().composer()
    private(set) var composerControls = Presentation.empty().composer().controls()
    private(set) var files = AgentCore.Snapshot.empty().files()
    private(set) var terminals = Terminals.empty()
    private(set) var conversation: ConversationPresentation?
    private(set) var agents: [AgentSummary] = []
    var notice: String?
    var sideChatRequest: SideChatRequest?
    var composerFocusRequest: UUID?
    var transferError: String?
    var transferring = false
    var panels = Panels()
    let workbenchInstance = UUID()
    var workbenchSideChats: [UInt64: SideChatRequest] = [:]
    @ObservationIgnored private var observation: Task<Void, Never>?
    @ObservationIgnored private var presentationTask: Task<Void, Never>?
    @ObservationIgnored private var pendingPresentation: ProjectionInput?
    @ObservationIgnored private var recentConversations: [ConversationPresentation] = []

    var store: AgentStore? {
        session.store()
    }

    init(host: String, session: ClientSession, pane: AgentPane? = nil) {
        self.host = host
        self.session = session
        self.pane = pane
        if let value = try? currentPresentation() {
            publish(value)
        }
        if let pane {
            observation = Task { [weak self] in
                do {
                    var previous = try pane.presentation()
                    self?.publish(previous)
                    while !Task.isCancelled {
                        _ = try await pane.nextPresentation(previous: previous)
                        guard !Task.isCancelled else { return }
                        let next = try pane.presentation()
                        self?.publish(next)
                        previous = next
                    }
                } catch {
                    if !Task.isCancelled {
                        self?.notice = error.localizedDescription
                    }
                }
            }
        }
    }

    deinit {
        observation?.cancel()
        pane?.retire()
    }

    private func currentPresentation() throws -> Presentation {
        if let pane {
            return try pane.presentation()
        }
        return session.presentation()
    }

    func perform(_ intent: Intent, completion: @escaping (Result<Outcome, Error>) -> Void = { _ in }) {
        do {
            let receipt = try pane.map { try $0.dispatch(intent: intent) } ?? session.dispatch(intent: intent)
            try publish(currentPresentation())
            Task { [weak self] in
                let result: Result<Outcome, Error>
                do { result = try await .success(receipt.wait()) } catch { result = .failure(error) }
                if let self, let value = try? currentPresentation() {
                    publish(value)
                }
                completion(result)
            }
        } catch { completion(.failure(error)) }
    }

    func publish(_ value: Presentation) {
        let changes = value.changes(previous: publication)
        guard changes.changed else { return }
        let next = value.snapshot()
        migrateWorkbench(to: next)
        publication = value
        if changes.settings {
            settings = value.settings()
        }
        if changes.composer {
            composer = value.composer()
        }
        if changes.composerControls {
            composerControls = value.composer().controls()
        }
        if changes.context {
            conversationContext = value.conversationContext()
        }
        if changes.agents {
            agents = value.agents()
        }
        let nextFiles = next.files()
        if changes.groups.terminals {
            terminals = next.terminals()
        }
        if !nextFiles.unchanged(other: files) {
            files = nextFiles
        }
        snapshot = next
        if isConnected != next.connected() {
            isConnected = next.connected()
        }
        if changes.conversation {
            projectConversation(value.conversation())
        }
    }

    func requestSnapshot(_ intent: Intent, completion: @escaping (AgentCore.Snapshot, Result<Outcome, Error>) -> Void) {
        perform(intent) { [self] result in completion(snapshot, result) }
    }

    func persist() {
        let session = session
        Task { [weak self] in
            do { try await session.flush() } catch { self?.notice = error.localizedDescription }
        }
    }

    func prepareDictation() -> DictationPreparation? {
        pane?.prepareDictation() ?? store?.prepareDictation()
    }

    func openThread(_ id: SessionRef) {
        notice = nil
        perform(.readThread(ReadThread(threadId: id, open: true)))
    }

    func openNewThread(cwd: String) {
        perform(.newChat(cwd: cwd))
    }
}

extension ConversationModel {
    var sending: Bool {
        composerControls.sending
    }

    var transcribing: Bool {
        composerControls.transcribing
    }

    var interruptingTurnId: String? {
        let controls = composerControls
        return controls.interruptPhase == .running ? controls.runningTurn : nil
    }

    var navigation: AgentCore.Navigation {
        conversationContext.navigation()
    }

    var history: HistoryControl? {
        conversationContext.history()
    }

    var hostHomeDirectory: String? {
        conversationContext.home()
    }

    var selectedDirectory: String {
        conversationContext.selectedDirectory()
    }

    var selectedThreadId: SessionRef? {
        navigation.threadId
    }

    var isNewThread: Bool {
        selectedThreadId == nil
    }

    var cwd: String {
        navigation.cwd
    }

    var coreDraftKey: DraftKey {
        navigation.draftKey
    }

    var draftKey: DraftIdentity {
        DraftIdentity(profile: host, key: coreDraftKey)
    }

    var draft: String {
        get { composer.draft().text }
        set { perform(.setDraftText(threadId: coreDraftKey, text: newValue)) }
    }

    func loadOlderHistory(_ id: SessionRef) {
        guard selectedThreadId == id else { return }
        notice = nil
        perform(.readOlder(threadId: id))
    }

    func send() {
        perform(.submit(
            threadId: navigation.threadId,
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
        guard let threadId = navigation.threadId else { return }
        perform(.interrupt(Interrupt(threadId: threadId, turnId: turnId)))
    }

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
            attachment: attachment
        )))
    }

    func respond(_ requestId: String, _ answer: Answer) {
        perform(.respond(Respond(requestId: requestId, answer: answer)))
    }

    var questionPrompt: QuestionPrompt? {
        composerControls.question
    }

    var dockedRequest: Request? {
        composerControls.request
    }

    func answerQuestion(_ requestId: String, _ action: QuestionAction) {
        perform(.answerQuestion(requestId: requestId, action: action))
    }

    func readItemDetails(threadId: SessionRef, turnId: String, itemId: String) async -> String? {
        do {
            _ = try await outcome(for: .readItem(ReadItem(threadId: threadId, turnId: turnId, itemId: itemId)))
            return nil
        } catch { return error.localizedDescription }
    }

    func download(_ path: String) async throws -> URL {
        try await inTemporaryDirectory { directory in
            let target = directory.appendingPathComponent(URL(fileURLWithPath: path).lastPathComponent)
            _ = try await outcome(for: .downloadFile(DownloadFile(source: path, destination: target.path)))
            try Task.checkCancellation()
            return target
        }
    }

    func outcome(for intent: Intent) async throws -> Outcome {
        try await withCheckedThrowingContinuation { continuation in
            perform(intent) { continuation.resume(with: $0) }
        }
    }

    func forkAndOpen(through turnId: String, completion: @escaping (String?) -> Void) {
        guard let threadId = selectedThreadId else { completion(nil); return }
        perform(.forkSession(ForkSession(threadId: threadId, lastTurnId: turnId))) { [self] result in
            guard selectedThreadId == threadId else { completion(nil); return }
            switch result {
            case let .success(.startedThread(id)):
                openThread(id)
                completion(nil)
            case let .failure(error): completion(error.localizedDescription)
            default: completion("会話を分岐できませんでした。")
            }
        }
    }

    private func projectConversation(_ rendered: RenderedConversation?) {
        guard let rendered else {
            conversation = nil
            pendingPresentation = nil
            return
        }
        let id = rendered.source().id()
        if conversation?.id != id {
            conversation = recentConversations.first { $0.id == id }
        }
        pendingPresentation = ProjectionInput(
            rendered: rendered,
            key: coreDraftKey,
            connection: snapshot.connectionGeneration()
        )
        guard presentationTask == nil else { return }
        presentationTask = Task { [weak self] in
            while let self, let input = pendingPresentation {
                pendingPresentation = nil
                let previous = conversation
                let rendered = await Task.detached(priority: .userInitiated) {
                    ConversationPresentation.project(input.rendered, previous: previous)
                }.value
                if coreDraftKey == input.key, snapshot.connectionGeneration() == input.connection {
                    conversation = rendered
                    if rendered.id != nil {
                        recentConversations.removeAll { $0.id == rendered.id }
                        recentConversations.insert(rendered, at: 0)
                        if recentConversations.count > 3 {
                            recentConversations.removeLast()
                        }
                    }
                }
            }
            self?.presentationTask = nil
        }
    }
}

private struct ProjectionInput {
    let rendered: RenderedConversation
    let key: DraftKey
    let connection: UInt64
}
