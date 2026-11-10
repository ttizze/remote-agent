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

    final class Coordinator {
        var handle: UnsafeMutableRawPointer?
        var perform: (String, String) -> Void = { _, _ in }
        deinit {
            if let handle {
                releaseConversation(handle)
            }
        }
    }

    func makeCoordinator() -> Coordinator {
        let coordinator = Coordinator()
        coordinator.perform = perform
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
            if action == "error" {
                NSLog("NSH conversation: %@", text)
            } else {
                coordinator.perform(action, text)
            }
        }
        let handle = host.withCString {
            // The engine stops before the coordinator's reference is released.
            createConversation(session.uniffiCloneHandle(), pane?.uniffiCloneHandle() ?? 0, $0,
                               Unmanaged.passUnretained(coordinator).toOpaque(), callback)
        }
        guard let handle else { return UIView() }
        coordinator.handle = handle
        return conversationView(handle).takeUnretainedValue()
    }

    func updateUIView(_: UIView, context: Context) {
        context.coordinator.perform = perform
    }
}
