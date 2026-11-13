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
    private(set) var conversation: ConversationPresentation?
    var hasConversation: Bool {
        conversation != nil
    }

    private(set) var agents: [AgentSummary] = []
    private(set) var activityDecorations: [String: [ActivityDecoration]] = [:]
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
    @ObservationIgnored private var activityExpansionOverrides = [String: String]()
    @ObservationIgnored private var requestedSubagents = Set<SessionRef>()
    @ObservationIgnored private var activityClock: Task<Void, Never>?

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
        activityClock = Task { [weak self] in
            while !Task.isCancelled {
                do { try await Task.sleep(for: .seconds(1)) } catch { return }
                guard let self else { return }
                if activityDecorations.values.joined().contains(where: {
                    if case let .subagent(_, _, _, _, status, _) = $0 {
                        return status == "running"
                    }
                    return false
                }) {
                    refreshActivityDecorations()
                }
            }
        }
    }

    deinit {
        observation?.cancel()
        activityClock?.cancel()
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
            projectConversation(value.conversation())
        }
        if changes.groups.sessions || changes.settings {
            refreshActivityDecorations()
        }
    }

    private func refreshActivityDecorations() {
        guard let conversation, conversation.scope == snapshot.panelScope(host: host, draft: coreDraftKey) else {
            activityDecorations = [:]; return
        }
        let now = Date().timeIntervalSince1970
        var next: [String: [ActivityDecoration]] = [:]
        var load: [SessionRef] = []
        for row in conversation.rows {
            guard case let .native(_, item?) = row.content else { continue }
            let values = snapshot.activityDecorations(item: item.source, nowSeconds: now)
            if !values.isEmpty {
                next[row.id] = values
            }
            for value in values {
                if case let .subagent(_, target?, _, _, _, _) = value,
                   requestedSubagents.insert(target).inserted, target != selectedThreadId {
                    load.append(target)
                }
            }
        }
        if next != activityDecorations {
            activityDecorations = next
        }
        for target in load {
            perform(.readThread(ReadThread(threadId: target, open: false)))
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

    func toggleActivity(_ id: String) {
        guard let conversation,
              conversation.scope == snapshot.panelScope(host: host, draft: coreDraftKey),
              let change = conversation.projection.toggleActivity(id: id, expansion: activityExpansionOverrides)
        else { return }
        activityExpansionOverrides = change.expansion
        projectConversation(publication.conversation())
        if let params = change.loadItems {
            perform(.loadTurnItems(params))
        }
    }

    private func projectConversation(_ rendered: RenderedConversation?) {
        guard let rendered else {
            conversation = nil
            pendingPresentation = nil
            activityExpansionOverrides.removeAll()
            requestedSubagents.removeAll(); activityDecorations = [:]
            return
        }
        let scope = snapshot.panelScope(host: host, draft: coreDraftKey)
        if conversation?.scope != scope {
            activityExpansionOverrides.removeAll()
            requestedSubagents.removeAll(); activityDecorations = [:]
            conversation = recentConversations.first { $0.scope == scope }
        }
        pendingPresentation = ProjectionInput(
            rendered: rendered,
            scope: scope,
            connection: snapshot.connectionGeneration(),
            expansion: activityExpansionOverrides
        )
        guard presentationTask == nil else { return }
        presentationTask = Task { [weak self] in
            while let self, let input = pendingPresentation {
                pendingPresentation = nil
                let rendered = await Task.detached(priority: .userInitiated) { [previous = conversation] in
                    ConversationPresentation.project(input.rendered, scope: input.scope,
                                                     expansion: input.expansion, previous: previous)
                }.value
                if snapshot.panelScope(host: host, draft: coreDraftKey) == input.scope,
                   snapshot.connectionGeneration() == input.connection,
                   snapshot.uiLanguage() == input.rendered.language(),
                   activityExpansionOverrides == input.expansion,
                   publication.conversation() != nil {
                    conversation = rendered
                    refreshActivityDecorations()
                    // Expansion resets with the scope, so only collapsed rows can be shown on reopen.
                    if rendered.id != nil, input.expansion.isEmpty {
                        recentConversations.removeAll { $0.scope == rendered.scope }
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
    let scope: PanelScope
    let connection: UInt64
    let expansion: [String: String]
}
