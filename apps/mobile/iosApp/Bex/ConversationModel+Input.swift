import AgentCore
import Foundation

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
            clientUserMessageId: UUID().uuidString,
            alternate: false
        )) { [weak self] _ in
            self?.persist()
        }
    }

    func resume() {
        for intent in composer.resumeIntents(clientUserMessageId: UUID().uuidString) {
            perform(intent)
        }
        persist()
    }

    func rewind(thread: SessionRef, through turn: String) {
        guard selectedThreadId == thread else { return }
        perform(.rewindSession(RewindSession(threadId: thread, turnId: turn))) { [weak self] result in
            switch result {
            case .success: self?.composerFocusRequest = UUID(); self?.persist()
            case let .failure(error): self?.notice = error.localizedDescription
            }
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

    func readItemDetails(threadId: SessionRef, turnId: String, itemId: String) async -> String? {
        do {
            _ = try await outcome(for: .readItem(ReadItem(threadId: threadId, turnId: turnId, itemId: itemId)))
            return nil
        } catch { return error.localizedDescription }
    }
}
