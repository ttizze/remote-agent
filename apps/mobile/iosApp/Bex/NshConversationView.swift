#if BEX_NSH_CONVERSATION
    import AgentCore
    import SwiftUI
    import UIKit

    /// The conversation's rows drawn by the NSH client the Mac uses, on this
    /// screen's session. The composer and navigation around it stay native.
    struct NshConversationView: UIViewControllerRepresentable {
        @Environment(\.locale) private var locale
        let host: String
        let session: ClientSession
        let pane: AgentPane?
        let composerInset: CGFloat
        let selectionKey: DraftIdentity
        let allowsSideChat: Bool
        /// What the message text asks for: an `action` (`quote`, `sideChat`,
        /// `explain`, `link`) and its `text`.
        let perform: (_ action: String, _ text: String) -> Void
        let preview: (Result<SharedFile, Error>) -> Void

        final class Coordinator {
            var handle: UnsafeMutableRawPointer?
            weak var menuController: NshSelectionMenuController?
            var selectionKey: DraftIdentity?
            var perform: (String, String) -> Void = { _, _ in }
            var preview: (Result<SharedFile, Error>) -> Void = { _ in }
        }

        func makeCoordinator() -> Coordinator {
            let coordinator = Coordinator()
            coordinator.perform = perform
            coordinator.preview = preview
            return coordinator
        }

        func makeUIViewController(context: Context) -> NshSelectionMenuController {
            let coordinator = context.coordinator
            let controller = NshSelectionMenuController()
            coordinator.menuController = controller
            configure(controller, coordinator: coordinator)
            let callback: BexNshEventCallback = { context, bytes, count in
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
                        if action == "selection" {
                            coordinator?.menuController?.select(text)
                        } else {
                            coordinator?.perform(action, text)
                        }
                    }
                }
            }
            let handle = host.withCString {
                // The engine stops before the coordinator's reference is released.
                bex_nsh_conversation_create(session.uniffiCloneHandle(), pane?.uniffiCloneHandle() ?? 0, $0,
                                            Unmanaged.passUnretained(coordinator).toOpaque(), callback)
            }
            guard let handle else {
                let failure = UILabel()
                failure.text = localizedUiText(language: session.snapshot().uiLanguage(), source: "会話を開けませんでした。")
                failure.numberOfLines = 0
                controller.view = failure
                return controller
            }
            coordinator.handle = handle
            bex_nsh_conversation_set_composer_inset(handle, Float(composerInset))
            controller.view = Unmanaged<UIView>.fromOpaque(bex_nsh_conversation_view(handle)).takeUnretainedValue()
            return controller
        }

        func updateUIViewController(_ controller: NshSelectionMenuController, context: Context) {
            configure(controller, coordinator: context.coordinator)
            context.coordinator.perform = perform
            context.coordinator.preview = preview
            if let handle = context.coordinator.handle {
                bex_nsh_conversation_set_composer_inset(handle, Float(composerInset))
            }
        }

        private func configure(_ controller: NshSelectionMenuController, coordinator: Coordinator) {
            if coordinator.selectionKey != selectionKey {
                controller.resetSelection()
                coordinator.selectionKey = selectionKey
            }
            controller.perform = perform
            controller.allowsSideChat = allowsSideChat
            controller.localize = { localizedUiText(language: locale.appLanguage, source: $0) }
        }

        static func dismantleUIViewController(_ controller: NshSelectionMenuController, coordinator: Coordinator) {
            controller.resetSelection()
            controller.perform = { _, _ in }
            coordinator.menuController = nil
            if let handle = coordinator.handle {
                bex_nsh_conversation_release(handle)
                coordinator.handle = nil
            }
            coordinator.perform = { _, _ in }
            coordinator.preview = { _ in }
        }
    }

#endif
