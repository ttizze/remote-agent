import AgentCore
import SwiftUI

struct SideChatRequest: Identifiable {
    let id = UUID()
    let text: String
    let host: String
    let originalThreadId: String
    let originalConversation: ConversationPresentation?
    let cwd: String
}

extension BexAppViewModel {
    func addSelectionToChat(_ text: String) {
        let quote = Self.selectionQuote(text)
        draft = draft.isEmpty ? quote : draft + "\n\n" + quote
        composerFocusRequest = UUID()
    }

    static func selectionQuote(_ text: String) -> String {
        text.components(separatedBy: "\n").map { "> " + $0 }.joined(separator: "\n") + "\n\n"
    }

    func askSelectionInSideChat(_ text: String) {
        guard sideChatRequest == nil, let host = selectedProfileId, let thread = selectedThreadId else { return }
        sideChatRequest = SideChatRequest(text: text, host: host, originalThreadId: thread,
                                          originalConversation: conversation, cwd: selectedDirectory)
    }
}

struct ConversationSideChat: View {
    @ObservedObject var model: BexAppViewModel
    let request: SideChatRequest
    @State private var threadId: String?
    @State private var draftInitialized = false
    @State private var preparing = true
    @State private var error: String?
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Group {
                if preparing {
                    ProgressView("サイドチャットを準備中…")
                } else if let error {
                    VStack(spacing: 16) {
                        Text(error).foregroundStyle(.red)
                        Button("再試行") { Task { await prepare() } }
                    }.padding()
                } else {
                    ThreadScreen(model: model, conversation: model.conversation, isSideChat: true)
                }
            }
            .navigationTitle("サイドチャット")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("閉じる") { dismiss() }.accessibilityIdentifier("side-chat.close")
                }
            }
        }
        .accessibilityIdentifier("side-chat.sheet")
        .task { await prepare() }
        .onDisappear { restoreOriginal() }
    }

    private func run(_ intent: Intent) async throws -> Outcome {
        try await withCheckedThrowingContinuation { continuation in
            model.perform(intent) { continuation.resume(with: $0) }
        }
    }

    private func prepare() async {
        preparing = true
        if model.notice == error {
            model.notice = nil
        }
        error = nil
        do {
            if threadId == nil {
                let result = try await run(.startThread(StartThread(cwd: request.cwd, model: nil)))
                guard case let .startedThread(id) = result else { throw CocoaError(.coderInvalidValue) }
                threadId = id
            }
            if !draftInitialized, let id = threadId {
                let quote = BexAppViewModel.selectionQuote(request.text)
                _ = try await run(.setDraftText(threadId: id, text: quote))
                draftInitialized = true
            }
            guard !Task.isCancelled, model.selectedProfileId == request.host,
                  model.sideChatRequest?.id == request.id, let threadId else { return }
            // A newly started thread has no persisted turns to hydrate yet.
            _ = try await run(.readThread(ReadThread(
                threadId: threadId, open: true
            )))
            if Task.isCancelled || model.sideChatRequest?.id != request.id {
                restoreOriginal(); return
            }
            model.composerFocusRequest = UUID()
        } catch {
            self.error = model.snapshot.error() ?? error.localizedDescription
            restoreOriginal()
        }
        preparing = false
    }

    private func restoreOriginal() {
        guard model.selectedProfileId == request.host, model.selectedThreadId == threadId else { return }
        model.perform(.readThread(ReadThread(threadId: request.originalThreadId, open: true))) { result in
            if case .success = result {
                model.persist()
            }
        }
    }
}
