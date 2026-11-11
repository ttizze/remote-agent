import AgentCore
import Observation
import SwiftUI

@MainActor
@Observable
final class SideChatRequest {
    let id = UUID()
    let text: String
    let host: String
    let originalThreadId: SessionRef
    let cwd: String
    @ObservationIgnored private let session: ClientSession
    @ObservationIgnored private let provider: ProviderKind?
    private(set) var chat: ConversationModel?
    private(set) var preparing = false
    private(set) var error: String?

    init(text: String, host: String, originalThreadId: SessionRef, cwd: String,
         session: ClientSession, provider: ProviderKind?) {
        self.text = text
        self.host = host
        self.originalThreadId = originalThreadId
        self.cwd = cwd
        self.session = session
        self.provider = provider
    }

    func prepare() async {
        guard !preparing else { return }
        preparing = true
        error = nil
        defer { preparing = false }
        do {
            let pane: AgentPane
            if let existing = chat?.pane {
                pane = existing
            } else {
                let store = try await session.ready()
                pane = try await store.openPane(cwd: cwd)
                chat = ConversationModel(host: host, session: session, pane: pane)
            }
            try await pane.prepareSideChat(provider: provider, text: text)
            chat?.composerFocusRequest = UUID()
        } catch {
            if !Task.isCancelled {
                self.error = error.localizedDescription
            }
        }
    }
}

extension ConversationModel {
    func addSelectionToChat(_ text: String) {
        draft = appendSelectionToDraft(draft: draft, selection: text)
        composerFocusRequest = UUID()
    }

    func askSelectionInSideChat(_ text: String) {
        guard let thread = selectedThreadId else { return }
        sideChatRequest = SideChatRequest(text: text, host: host, originalThreadId: thread,
                                          cwd: selectedDirectory, session: session,
                                          provider: snapshot.modelProviderForDraft(threadId: coreDraftKey))
    }
}

struct ConversationSideChat: View {
    let request: SideChatRequest
    let environment: ConversationEnvironment

    var body: some View {
        VStack(spacing: 0) {
            if request.preparing {
                ProgressView("サイドチャットを準備中…")
            } else if let error = request.error {
                VStack(spacing: 16) {
                    ErrorTextView(source: error).foregroundStyle(.red)
                    Button("再試行") { Task { await request.prepare() } }
                }.padding()
            } else if let chat = request.chat {
                ThreadScreen(model: chat, environment: environment, isSideChat: true)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .task { await request.prepare() }
    }
}
