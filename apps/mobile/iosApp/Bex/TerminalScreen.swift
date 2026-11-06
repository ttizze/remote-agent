import AgentCore
import SwiftTerm
import SwiftUI

struct TerminalScreen: View {
    let model: ConversationModel
    let cwd: String
    let handle: String

    var body: some View {
        VStack(spacing: 0) {
            let terminal = model.terminals.view(handle: handle)
            if terminal?.loading ?? true {
                ProgressView().accessibilityIdentifier("terminal.loading")
            } else if let status = terminal?.status {
                AppText(status).font(.caption).foregroundStyle(.secondary)
            }
            NativeTerminalView(session: model.session, handle: handle, cwd: cwd, terminal: terminal)
        }
    }
}

private struct NativeTerminalView: UIViewRepresentable {
    let session: ClientSession?
    let handle: String
    let cwd: String
    let terminal: AgentCore.TerminalView?

    func makeCoordinator() -> Coordinator {
        Coordinator(session: session, handle: handle, cwd: cwd)
    }

    func makeUIView(context: Context) -> HostTerminalView {
        let view = HostTerminalView(frame: .zero)
        view.terminalDelegate = context.coordinator
        view.font = .monospacedSystemFont(ofSize: 13, weight: .regular)
        view.accessibilityIdentifier = "terminal.screen"
        // Replace SwiftTerm's menu-only long press with direct word selection.
        for case let gesture as UILongPressGestureRecognizer in view.gestureRecognizers ?? [] {
            view.removeGestureRecognizer(gesture)
        }
        let longPress = UILongPressGestureRecognizer(
            target: view, action: #selector(HostTerminalView.selectTextOnLongPress(_:))
        )
        longPress.minimumPressDuration = 0.7
        view.addGestureRecognizer(longPress)
        return view
    }

    static func dismantleUIView(_ view: HostTerminalView, coordinator: Coordinator) {
        view.terminalDelegate = nil
        coordinator.dismantled = true
        if coordinator.started {
            coordinator.dispatch(.detachTerminal(DetachTerminal(handle: coordinator.handle)))
        }
    }

    func updateUIView(_ view: HostTerminalView, context: Context) {
        view.copyTitle = localizedUiText(language: context.environment.locale.appLanguage, source: "コピー")
        let coordinator = context.coordinator
        guard let terminal else { return }
        view.accessibilityValue = terminal.acceptsInput && !terminal.loading ? "入力可能" : nil
        let previousSequence = coordinator.sequence
        for chunk in terminal.output where chunk.sequence > coordinator.sequence {
            if let size = chunk.resetSize {
                coordinator.restoring = true
                view.resize(cols: Int(size.cols), rows: Int(size.rows))
            }
            view.feed(byteArray: Array(chunk.data)[...])
            coordinator.restoring = false
            coordinator.sequence = chunk.sequence
        }
        if coordinator.sequence > previousSequence {
            coordinator.dispatch(.acknowledgeTerminal(handle: handle, sequence: coordinator.sequence))
        }
    }

    @MainActor final class Coordinator: NSObject, TerminalViewDelegate {
        // The session owns this terminal; its observer publishes UI changes after
        // UIKit updates. Profile changes must not retarget existing callbacks.
        let session: ClientSession?
        let handle: String
        var sequence: UInt64 = 0
        var restoring = false
        var started = false
        var dismantled = false
        let cwd: String
        init(session: ClientSession?, handle: String, cwd: String) {
            self.session = session; self.handle = handle; self.cwd = cwd
        }

        func dispatch(_ intent: Intent) {
            guard let session, let receipt = try? session.dispatch(intent: intent) else { return }
            // Preserve input order and drain tracked receipts without publishing
            // an ConversationModel snapshot from a UIKit lifecycle callback.
            Task { _ = try? await receipt.wait() }
        }

        func send(source _: SwiftTerm.TerminalView, data: ArraySlice<UInt8>) {
            guard !dismantled, let session,
                  session.snapshot().terminalAcceptsInput(handle: handle) else { return }
            dispatch(.writeTerminal(WriteTerminal(handle: handle, data: Data(data))))
        }

        func sizeChanged(source _: SwiftTerm.TerminalView, newCols: Int, newRows: Int) {
            guard !dismantled, !restoring, newCols > 0, newRows > 0 else { return }
            let size = TerminalSize(
                cols: UInt16(clamping: min(newCols, 500)),
                rows: UInt16(clamping: min(newRows, 250))
            )
            Task { @MainActor in
                guard !dismantled, let session else { return }
                if !started {
                    started = true
                    dispatch(.startTerminal(StartTerminal(handle: handle, cwd: cwd, size: size)))
                } else if session.snapshot().terminalAcceptsInput(handle: handle) {
                    dispatch(.resizeTerminal(ResizeTerminal(handle: handle, size: size)))
                }
            }
        }

        func setTerminalTitle(source _: SwiftTerm.TerminalView, title _: String) {}
        func hostCurrentDirectoryUpdate(source _: SwiftTerm.TerminalView, directory _: String?) {}
        func scrolled(source _: SwiftTerm.TerminalView, position _: Double) {}
        func rangeChanged(source _: SwiftTerm.TerminalView, startY _: Int, endY _: Int) {}
        func requestOpenLink(source _: SwiftTerm.TerminalView, link: String, params _: [String: String]) {
            guard let url = URL(string: link),
                  ["https", "http"].contains(url.scheme?.lowercased() ?? "") else { return }
            UIApplication.shared.open(url)
        }
    }
}

/// Emulator responses are produced by the Host; SwiftTerm still handles native user input.
private final class HostTerminalView: SwiftTerm.TerminalView {
    @TaskLocal private nonisolated static var isPasting = false
    var copyTitle = "コピー"

    @objc func selectTextOnLongPress(_ gesture: UILongPressGestureRecognizer) {
        guard gesture.state == .ended else { return }
        // Seed the selection position through SwiftTerm's public menu API.
        // Reopening the menu must preserve any existing selection range.
        let needsSelection = !hasActiveSelection
        showStandardContextMenu(at: gesture.location(in: self))
        if needsSelection {
            // Match tapping Select: dismiss the initial menu before SwiftTerm
            // selects the word and asynchronously presents its Copy menu.
            UIMenuController.shared.hideMenu()
            select(nil)
        }
    }

    @objc func editMenu(for _: UITextRange, suggestedActions: [UIMenuElement]) -> UIMenu? {
        // Terminal selection belongs to the emulator, while UITextInput's range
        // tracks keyboard composition. Supply Copy from the emulator selection.
        guard hasActiveSelection else { return nil }
        let copy = UIAction(title: copyTitle, image: UIImage(systemName: "doc.on.doc")) { [weak self] _ in
            self?.copy(nil)
        }
        return UIMenu(children: [copy] + splitCopyCommand(suggestedActions).others)
    }

    override func paste(_ sender: Any?) {
        // SwiftTerm sends paste bytes synchronously through the emulator delegate.
        // Scope forwarding to this call, without enabling replies from its parser thread.
        Self.$isPasting.withValue(true) { super.paste(sender) }
    }

    override nonisolated func send(source: SwiftTerm.Terminal, data: ArraySlice<UInt8>) {
        guard Self.isPasting else { return }
        super.send(source: source, data: data)
    }
}
