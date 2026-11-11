import AgentCore
import Foundation
import Observation

/// One pane owns its native presentation and input. The Host session owns transport and persistence.
@MainActor
@Observable
final class ConversationModel {
    let host: String
    @ObservationIgnored let session: ClientSession
    @ObservationIgnored let pane: AgentPane?
    @ObservationIgnored private(set) var publication: Presentation
    var snapshot: AgentCore.Snapshot {
        publication.snapshot()
    }

    private(set) var settings: SettingsPresentation
    private(set) var isConnected = false
    private(set) var conversationContext: ConversationContext
    private(set) var composer: ComposerPresentation
    private(set) var composerControls: ComposerControls
    /// The shared core summary for the workspace-change badge. Keep this on
    /// the observable model because `publication` is intentionally ignored by
    /// Observation and workspace reviews can change without composer changes.
    private(set) var composerReview: ComposerReview?
    private(set) var files: AgentCore.Files
    private(set) var terminals = Terminals.empty()
    /// The open conversation has rows for its NSH timeline to draw.
    private(set) var hasConversation = false
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

    var store: AgentStore? {
        session.store()
    }

    init(host: String, session: ClientSession, pane: AgentPane? = nil) {
        self.host = host
        self.session = session
        self.pane = pane
        let empty = Presentation.empty(language: session.snapshot().uiLanguage())
        let composer = empty.composer()
        publication = empty
        settings = empty.settings()
        conversationContext = empty.conversationContext()
        self.composer = composer
        composerControls = composer.controls()
        files = empty.snapshot().files(paneId: "")!
        composerReview = empty.composerReview(isSideChat: pane != nil)
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
        let next = value.snapshot()
        guard changes.changed, let nextFiles = next.files(paneId: value.paneId()) else { return }
        migrateWorkbench(to: next, draft: value.conversationContext().navigation().draftKey)
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
        composerReview = value.composerReview(isSideChat: pane != nil)
        if changes.context {
            conversationContext = value.conversationContext()
        }
        if changes.agents {
            agents = value.agents()
        }
        if changes.groups.terminals {
            terminals = next.terminals()
        }
        if !nextFiles.unchanged(other: files) {
            files = nextFiles
        }
        if isConnected != next.connected() {
            isConnected = next.connected()
        }
        if changes.conversation {
            hasConversation = value.conversation() != nil
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

    func send() {
        perform(.submit(
            threadId: navigation.threadId,
            clientUserMessageId: UUID().uuidString,
            alternate: false
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

    func respond(_ requestId: String, _ answer: Answer) {
        perform(.respond(Respond(requestId: requestId, answer: answer)))
    }

    var questionPrompt: QuestionPrompt? {
        composerControls.question
    }

    var dockedRequest: Request? {
        conversationContext.dockedRequest()
    }

    func answerQuestion(_ requestId: String, _ action: QuestionAction) {
        perform(.answerQuestion(requestId: requestId, action: action))
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
}
