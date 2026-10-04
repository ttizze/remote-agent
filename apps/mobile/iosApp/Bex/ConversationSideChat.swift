import AgentCore
import SwiftUI

@MainActor
final class SideChatRequest: ObservableObject {
    let id = UUID()
    let text: String
    let host: String
    let originalThreadId: SessionRef
    let originalConversation: ConversationPresentation?
    let cwd: String
    private(set) var threadId: SessionRef?
    private var draftInitialized = false
    private var transition: Task<Void, Never>?
    @Published private(set) var preparing = true
    @Published private(set) var error: String?

    init(text: String, host: String, originalThreadId: SessionRef,
         originalConversation: ConversationPresentation?, cwd: String) {
        self.text = text
        self.host = host
        self.originalThreadId = originalThreadId
        self.originalConversation = originalConversation
        self.cwd = cwd
    }

    func activate(_ active: Bool, model: BexAppViewModel) {
        if active {
            preparing = true
        }
        let previous = transition
        transition = Task {
            await previous?.value
            guard model.screen == .thread, model.selectedProfileId == host,
                  model.sideChatRequest?.id == id,
                  model.selectedThreadId == originalThreadId || model.selectedThreadId == threadId else { return }
            if active {
                await prepare(model)
            } else {
                await restoreOriginal(model)
            }
        }
    }

    private func prepare(_ model: BexAppViewModel) async {
        if model.notice == error {
            model.notice = nil
        }
        error = nil
        do {
            if threadId == nil {
                let result = try await model.outcome(for: .createSession(CreateSession(
                    provider: model.snapshot.modelProviderForDraft(threadId: model.coreDraftKey),
                    cwd: cwd,
                    model: nil
                )))
                guard case let .startedThread(id) = result else { throw CocoaError(.coderInvalidValue) }
                threadId = id
            }
            guard model.selectedProfileId == host, model.sideChatRequest?.id == id, let threadId else { return }
            if !draftInitialized {
                if !text.isEmpty {
                    _ = try await model.outcome(for: .setDraftText(
                        threadId: .session(session: threadId), text: BexAppViewModel.selectionQuote(text)
                    ))
                }
                draftInitialized = true
            }
            guard model.screen == .thread, model.selectedProfileId == host,
                  model.sideChatRequest?.id == id else { return }
            model.composerFocusRequest = UUID()
        } catch {
            self.error = model.snapshot.error() ?? error.localizedDescription
            await restoreOriginal(model)
        }
        preparing = false
    }

    private func restoreOriginal(_ model: BexAppViewModel) async {
        guard model.screen == .thread, model.selectedProfileId == host,
              model.sideChatRequest?.id == id, model.selectedThreadId == threadId else { return }
        do {
            _ = try await model.outcome(for: .readThread(ReadThread(threadId: originalThreadId, open: true)))
            model.persist()
        } catch { model.notice = error.localizedDescription }
    }
}

extension BexAppViewModel {
    var isShowingSideChat: Bool {
        guard let request = sideChatRequest else { return false }
        return selectedProfileId == request.host && selectedThreadId == request.threadId
    }

    func addSelectionToChat(_ text: String) {
        let quote = Self.selectionQuote(text)
        draft = draft.isEmpty ? quote : draft + "\n\n" + quote
        composerFocusRequest = UUID()
    }

    static func selectionQuote(_ text: String) -> String {
        text.components(separatedBy: "\n").map { "> " + $0 }.joined(separator: "\n") + "\n\n"
    }

    func askSelectionInSideChat(_ text: String) {
        guard !isShowingSideChat, let host = selectedProfileId, let thread = selectedThreadId else { return }
        sideChatRequest = SideChatRequest(text: text, host: host, originalThreadId: thread,
                                          originalConversation: conversation, cwd: selectedDirectory)
    }
}

extension ThreadScreen {
    var selectionActions: ConversationSelectionActions {
        let ask: ((String) -> Void)? = isSideChat ? nil : { text in
            model.askSelectionInSideChat(text)
            openTools?(.sideChat, false)
        }
        return ConversationSelectionActions(addToChat: model.addSelectionToChat, askInSideChat: ask)
    }
}

struct ConversationSideChat: View {
    @ObservedObject var model: BexAppViewModel
    @ObservedObject var request: SideChatRequest

    var body: some View {
        VStack(spacing: 0) {
            if request.preparing {
                ProgressView("サイドチャットを準備中…")
            } else if let error = request.error {
                VStack(spacing: 16) {
                    Text(error).foregroundStyle(.red)
                    Button("再試行") { request.activate(true, model: model) }
                }.padding()
            } else {
                ThreadScreen(model: model, conversation: model.conversation, isSideChat: true)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .onAppear { request.activate(true, model: model) }
        .onDisappear { request.activate(false, model: model) }
    }
}
