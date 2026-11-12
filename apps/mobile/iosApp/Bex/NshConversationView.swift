#if BEX_NSH_CONVERSATION
    import AgentCore
    import SwiftUI
    import UIKit

    private typealias EventCallback = @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<UInt8>?, Int) -> Void

    @_silgen_name("bex_nsh_conversation_create")
    private func createConversation(_ session: UInt64, _ pane: UInt64, _ host: UnsafePointer<CChar>,
                                    _ context: UnsafeMutableRawPointer,
                                    _ callback: EventCallback) -> UnsafeMutableRawPointer?

    @_silgen_name("bex_nsh_conversation_view")
    private func conversationView(_ handle: UnsafeMutableRawPointer) -> Unmanaged<UIView>

    @_silgen_name("bex_nsh_conversation_release")
    private func releaseConversation(_ handle: UnsafeMutableRawPointer)

    /// The conversation's rows drawn by the NSH client the Mac uses, on this
    /// screen's session. The composer and navigation around it stay native.
    struct NshConversationView: UIViewRepresentable {
        let host: String
        let session: ClientSession
        let pane: AgentPane?
        /// What the message text asks for: an `action` (`quote`, `sideChat`,
        /// `explain`, `link`) and its `text`.
        let perform: (_ action: String, _ text: String) -> Void
        let preview: (Result<SharedFile, Error>) -> Void

        final class Coordinator {
            var handle: UnsafeMutableRawPointer?
            var perform: (String, String) -> Void = { _, _ in }
            var preview: (Result<SharedFile, Error>) -> Void = { _ in }
        }

        func makeCoordinator() -> Coordinator {
            let coordinator = Coordinator()
            coordinator.perform = perform
            coordinator.preview = preview
            return coordinator
        }

        func makeUIView(context: Context) -> UIView {
            let coordinator = context.coordinator
            let callback: EventCallback = { context, bytes, count in
                guard let context, let bytes,
                      let event = try? JSONSerialization.jsonObject(
                          with: Data(bytes: bytes, count: count)
                      ) as? [String: Any],
                      let action = event["action"] as? String
                else { return }
                let coordinator = Unmanaged<Coordinator>.fromOpaque(context).takeUnretainedValue()
                let text = event["text"] as? String ?? ""
                if action == "preview" {
                    // NSH lends its download only during the callback. Copy it before
                    // crossing queues; the Swift presentation owns the new directory.
                    let file = Result {
                        guard !text.isEmpty else { throw URLError(.badURL) }
                        return try SharedFile(url: retainChatMedia(URL(fileURLWithPath: text)))
                    }
                    DispatchQueue.main.async { [weak coordinator] in
                        coordinator?.preview(file)
                    }
                } else {
                    DispatchQueue.main.async { [weak coordinator] in
                        coordinator?.perform(action, text)
                    }
                }
            }
            let handle = host.withCString {
                // The engine stops before the coordinator's reference is released.
                createConversation(session.uniffiCloneHandle(), pane?.uniffiCloneHandle() ?? 0, $0,
                                   Unmanaged.passUnretained(coordinator).toOpaque(), callback)
            }
            guard let handle else {
                let failure = UILabel()
                failure.text = localizedUiText(language: session.snapshot().uiLanguage(), source: "会話を開けませんでした。")
                failure.numberOfLines = 0
                return failure
            }
            coordinator.handle = handle
            return conversationView(handle).takeUnretainedValue()
        }

        func updateUIView(_: UIView, context: Context) {
            context.coordinator.perform = perform
            context.coordinator.preview = preview
        }

        static func dismantleUIView(_: UIView, coordinator: Coordinator) {
            if let handle = coordinator.handle {
                releaseConversation(handle)
                coordinator.handle = nil
            }
            coordinator.perform = { _, _ in }
            coordinator.preview = { _ in }
        }
    }

#endif
